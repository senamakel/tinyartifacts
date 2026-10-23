//! Durable artifact bytes over a host-bound blob handle.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use tinystoragedrivers_api::{BlobStore as TsdBlobStore, Storage};

/// Storage-independent metadata returned when bytes are written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    /// Number of bytes stored.
    pub size_bytes: u64,
    /// Media type guessed from the file name, when known.
    pub media_type: Option<String>,
}

/// Durable file bytes addressed by opaque keys.
#[async_trait]
pub trait FileStore: Send + Sync {
    /// Store bytes at `key`, replacing any existing object.
    ///
    /// # Errors
    /// Returns an error when the key is invalid or storage fails.
    async fn put(&self, key: &str, bytes: &[u8], media_type: Option<&str>) -> Result<FileMetadata>;
    /// Read the complete object at `key`.
    ///
    /// # Errors
    /// Returns an error when the key is invalid, the object is missing, or
    /// storage fails.
    async fn read(&self, key: &str) -> Result<Vec<u8>>;
    /// Return metadata for `key`, or `None` when it does not exist.
    ///
    /// # Errors
    /// Returns an error when the key is invalid or storage fails.
    async fn metadata(&self, key: &str) -> Result<Option<FileMetadata>>;
    /// Remove every object below the directory `prefix`, which must end in `/`.
    ///
    /// # Errors
    /// Returns an error when the prefix is invalid or storage fails.
    async fn delete_prefix(&self, prefix: &str) -> Result<()>;
}

/// [`FileStore`] adapter for a host-provided blob handle.
pub struct BlobFileStore {
    blobs: std::sync::Arc<dyn TsdBlobStore>,
}

impl BlobFileStore {
    /// Adapt the blobs in the host-bound `storage` bundle to the artifact
    /// file-store port.
    #[must_use]
    pub fn new(storage: &Storage) -> Self {
        Self {
            blobs: storage.blobs().clone(),
        }
    }
}

impl std::fmt::Debug for BlobFileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobFileStore")
            .field("blobs", &self.blobs)
            .finish()
    }
}

#[async_trait]
impl FileStore for BlobFileStore {
    async fn put(&self, key: &str, bytes: &[u8], media_type: Option<&str>) -> Result<FileMetadata> {
        validate_key(key)?;
        let content_type = media_type.or_else(|| crate::media_type_for(Path::new(key)));
        let meta = self.blobs.put(key, bytes.to_vec(), content_type).await?;
        Ok(FileMetadata {
            size_bytes: meta.len,
            media_type: meta.content_type,
        })
    }

    async fn read(&self, key: &str) -> Result<Vec<u8>> {
        validate_key(key)?;
        self.blobs
            .get(key)
            .await?
            .map(|blob| blob.bytes)
            .with_context(|| format!("file `{key}` does not exist"))
    }

    async fn metadata(&self, key: &str) -> Result<Option<FileMetadata>> {
        validate_key(key)?;
        Ok(self.blobs.head(key).await?.map(|meta| FileMetadata {
            size_bytes: meta.len,
            media_type: meta.content_type,
        }))
    }

    async fn delete_prefix(&self, prefix: &str) -> Result<()> {
        validate_prefix(prefix)?;
        for blob in self.blobs.list(prefix).await? {
            self.blobs.delete(&blob.key).await?;
        }
        Ok(())
    }
}

/// Files kept below `root` using path-safe storage keys.
#[derive(Clone, Debug)]
pub struct FsFileStore {
    root: PathBuf,
}

impl FsFileStore {
    /// Store files under `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory containing stored file objects.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Replace the bytes of an existing regular file below the store root.
    ///
    /// The file is opened and truncated in place, preserving its inode and
    /// filesystem metadata such as permissions. The write is not atomic: an
    /// I/O error can leave the target partially written. The root, parent
    /// directories, and target must already exist. A final symlink is allowed
    /// only when it resolves to a regular file inside the root.
    ///
    /// Unlike [`FileStore::put`], this operation does not create or atomically
    /// replace a target. It is intended for callers that must respect the
    /// operating system's write permission for an already existing file.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is invalid, the root or target is
    /// missing, the target is not a regular file, it escapes the root, or the
    /// target cannot be opened or written.
    pub async fn overwrite_existing(
        &self,
        key: &str,
        bytes: &[u8],
        media_type: Option<&str>,
    ) -> Result<FileMetadata> {
        self.overwrite_existing_with_hook(key, bytes, media_type, || {})
            .await
    }

    async fn overwrite_existing_with_hook<F>(
        &self,
        key: &str,
        bytes: &[u8],
        media_type: Option<&str>,
        before_open: F,
    ) -> Result<FileMetadata>
    where
        F: FnOnce() + Send + 'static,
    {
        validate_key(key)?;
        let root_path = self.root.clone();
        let key = key.to_owned();
        let bytes = bytes.to_vec();
        let media_type = media_type.map(str::to_owned);
        let file = tokio::task::spawn_blocking(move || -> Result<FileMetadata> {
            use cap_std::ambient_authority;
            use cap_std::fs::{Dir, OpenOptions};
            use std::io::Write;

            let root_path = std::fs::canonicalize(&root_path).with_context(|| {
                format!("cannot resolve file store root {}", root_path.display())
            })?;
            let root = Dir::open_ambient_dir(&root_path, ambient_authority())
                .with_context(|| format!("cannot open file store root {}", root_path.display()))?;
            let relative = canonical_relative_target(&root_path, &key)
                .with_context(|| format!("cannot resolve existing file {key}"))?;
            let metadata = root
                .metadata(&relative)
                .with_context(|| format!("cannot resolve existing file {key}"))?;
            if !metadata.is_file() {
                bail!("file key does not name a regular file: {key:?}");
            }
            before_open();
            let mut options = OpenOptions::new();
            options.write(true).truncate(true);
            let mut file = root
                .open_with(&relative, &options)
                .with_context(|| format!("cannot open {key} for writing"))?;
            file.write_all(&bytes)
                .with_context(|| format!("cannot write {key}"))?;
            file.flush()
                .with_context(|| format!("cannot flush {key}"))?;

            Ok(FileMetadata {
                size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                media_type: media_type
                    .or_else(|| crate::media_type_for(Path::new(&key)).map(str::to_owned)),
            })
        })
        .await
        .context("file-store overwrite task failed")??;
        Ok(file)
    }

    async fn read_with_hook<F>(&self, key: &str, before_open: F) -> Result<Vec<u8>>
    where
        F: FnOnce() + Send + 'static,
    {
        validate_key(key)?;
        let root_path = self.root.clone();
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            use cap_std::ambient_authority;
            use cap_std::fs::Dir;
            use std::io::Read;

            let root_path = std::fs::canonicalize(&root_path).with_context(|| {
                format!("cannot resolve file store root {}", root_path.display())
            })?;
            let root = Dir::open_ambient_dir(&root_path, ambient_authority())
                .with_context(|| format!("cannot open file store root {}", root_path.display()))?;
            let relative = canonical_relative_target(&root_path, &key)
                .with_context(|| format!("cannot resolve file {key}"))?;
            before_open();
            let mut file = root
                .open(&relative)
                .with_context(|| format!("cannot read {key}"))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .with_context(|| format!("cannot read {key}"))?;
            Ok(bytes)
        })
        .await
        .context("file-store read task failed")?
    }
}

#[async_trait]
impl FileStore for FsFileStore {
    async fn put(&self, key: &str, bytes: &[u8], media_type: Option<&str>) -> Result<FileMetadata> {
        validate_key(key)?;
        tokio::fs::create_dir_all(&self.root)
            .await
            .with_context(|| format!("cannot create file store root {}", self.root.display()))?;
        let root_path = self.root.clone();
        let key = key.to_owned();
        let bytes = bytes.to_vec();
        let media_type = media_type.map(str::to_owned);
        tokio::task::spawn_blocking(move || -> Result<FileMetadata> {
            use cap_std::ambient_authority;
            use cap_std::fs::Dir;
            use std::io::Write;

            let root_path = std::fs::canonicalize(&root_path).with_context(|| {
                format!("cannot resolve file store root {}", root_path.display())
            })?;
            let root = Dir::open_ambient_dir(&root_path, ambient_authority())
                .with_context(|| format!("cannot open file store root {}", root_path.display()))?;
            let relative = Path::new(&key);
            let parent = relative
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let directory = create_parent_directory(&root, &root_path, parent)
                .with_context(|| format!("cannot open parent directory for {key}"))?;
            let file_name = relative.file_name().context("file key has no name")?;
            let staging = format!(".{}.tmp", uuid::Uuid::new_v4());
            write_staging_file(&directory, &staging, |file| file.write_all(&bytes))
                .with_context(|| format!("cannot write staging file for {key}"))?;
            if let Err(error) = directory.rename(&staging, &directory, file_name) {
                let _ = directory.remove_file(&staging);
                return Err(error).with_context(|| format!("cannot save {key}"));
            }
            Ok(FileMetadata {
                size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                media_type: media_type
                    .or_else(|| crate::media_type_for(Path::new(&key)).map(str::to_owned)),
            })
        })
        .await
        .context("file-store write task failed")?
    }

    async fn read(&self, key: &str) -> Result<Vec<u8>> {
        self.read_with_hook(key, || {}).await
    }

    async fn metadata(&self, key: &str) -> Result<Option<FileMetadata>> {
        validate_key(key)?;
        let root_path = self.root.clone();
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || -> Result<Option<FileMetadata>> {
            use cap_std::ambient_authority;
            use cap_std::fs::Dir;

            let root_path = std::fs::canonicalize(&root_path).with_context(|| {
                format!("cannot resolve file store root {}", root_path.display())
            })?;
            let root = Dir::open_ambient_dir(&root_path, ambient_authority())
                .with_context(|| format!("cannot open file store root {}", root_path.display()))?;
            let relative = match canonical_relative_target(&root_path, &key) {
                Ok(relative) => relative,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            match root.metadata(&relative) {
                Ok(metadata) if metadata.is_file() => Ok(Some(FileMetadata {
                    size_bytes: metadata.len(),
                    media_type: crate::media_type_for(Path::new(&key)).map(str::to_owned),
                })),
                Ok(_) => Ok(None),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error).with_context(|| format!("cannot inspect {key}")),
            }
        })
        .await
        .context("file-store metadata task failed")?
    }

    async fn delete_prefix(&self, prefix: &str) -> Result<()> {
        validate_prefix(prefix)?;
        let root_path = self.root.clone();
        let relative = prefix.trim_end_matches('/').to_owned();
        tokio::task::spawn_blocking(move || -> Result<()> {
            use cap_std::ambient_authority;
            use cap_std::fs::Dir;

            let root_path = match std::fs::canonicalize(&root_path) {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            let root = Dir::open_ambient_dir(&root_path, ambient_authority())
                .with_context(|| format!("cannot open file store root {}", root_path.display()))?;
            if let Err(error) = canonical_relative_target(&root_path, &relative) {
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                {
                    return Ok(());
                }
                return Err(error);
            }
            match root.remove_dir_all(&relative) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error).with_context(|| format!("cannot remove {relative}")),
            }
        })
        .await
        .context("file-store deletion task failed")?
    }
}

fn canonical_relative_target(root_path: &Path, key: &str) -> Result<PathBuf> {
    let target = std::fs::canonicalize(root_path.join(key))
        .with_context(|| format!("cannot resolve file {key}"))?;
    if !target.starts_with(root_path) {
        bail!("file key escapes the configured store root: {key:?}");
    }
    target
        .strip_prefix(root_path)
        .map(Path::to_path_buf)
        .context("resolved file is outside the configured store root")
}

fn create_parent_directory(
    root: &cap_std::fs::Dir,
    root_path: &Path,
    parent: &Path,
) -> Result<cap_std::fs::Dir> {
    let mut directory = root.open_dir(".")?;
    let mut resolved = PathBuf::new();
    for component in parent.components() {
        let Component::Normal(segment) = component else {
            continue;
        };
        match std::fs::canonicalize(root_path.join(&resolved).join(segment)) {
            Ok(canonical) => {
                if !canonical.starts_with(root_path) {
                    bail!(
                        "file key escapes the configured store root: {}",
                        parent.display()
                    );
                }
                resolved = canonical
                    .strip_prefix(root_path)
                    .context("resolved parent is outside the configured store root")?
                    .to_path_buf();
                directory = root.open_dir(&resolved)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match directory.create_dir(segment) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                directory = directory.open_dir(segment)?;
                resolved.push(segment);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(directory)
}

fn write_staging_file<F>(directory: &cap_std::fs::Dir, staging: &str, write_body: F) -> Result<()>
where
    F: FnOnce(&mut cap_std::fs::File) -> std::io::Result<()>,
{
    use cap_std::fs::OpenOptions;

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = directory.open_with(staging, &options)?;
    if let Err(error) = write_body(&mut file) {
        drop(file);
        let _ = directory.remove_file(staging);
        return Err(error).with_context(|| format!("cannot write staging file {staging}"));
    }
    if let Err(error) = file.sync_all() {
        drop(file);
        let _ = directory.remove_file(staging);
        return Err(error).with_context(|| format!("cannot sync staging file {staging}"));
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.starts_with('/') || key.contains('\\') || key.contains('\0') {
        bail!("invalid file key {key:?}");
    }
    if key
        .split('/')
        .any(|segment| segment.is_empty() || segment == ".")
    {
        bail!("file key must use canonical path segments: {key:?}");
    }
    if Path::new(key).components().any(|part| {
        matches!(
            part,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("file key must stay inside the store: {key:?}");
    }
    Ok(())
}

fn validate_prefix(prefix: &str) -> Result<()> {
    if prefix.is_empty() {
        bail!("file key prefix is empty");
    }
    if !prefix.ends_with('/') {
        bail!("file key directory prefix must end with `/`: {prefix:?}");
    }
    validate_key(&prefix[..prefix.len() - 1])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "file_store_tests.rs"]
mod tests;
