//! Durable bytes behind [`FileRef`] keys.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use tinystoragedrivers_core::{BlobStore as TsdBlobStore, ScopedStorage};
use tokio::io::AsyncWriteExt;

use crate::file_ref::{FileRef, FileRoots};
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
    ///
    /// Returns an error when the key is invalid or storage fails.
    async fn put(&self, key: &str, bytes: &[u8], media_type: Option<&str>) -> Result<FileMetadata>;
    /// Read the complete object at `key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is invalid, the object is missing, or
    /// storage fails.
    async fn read(&self, key: &str) -> Result<Vec<u8>>;
    /// Return metadata for `key`, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is invalid or storage fails.
    async fn metadata(&self, key: &str) -> Result<Option<FileMetadata>>;
    /// Remove every object below the directory `prefix`, which must end in `/`.
    ///
    /// # Errors
    ///
    /// Returns an error when the prefix is invalid or storage fails.
    async fn delete_prefix(&self, prefix: &str) -> Result<()>;
}

/// [`FileStore`] adapter for a host-provided scoped storage handle.
pub struct BlobFileStore {
    blobs: std::sync::Arc<dyn TsdBlobStore>,
}

impl BlobFileStore {
    /// Adapt the blobs in `storage` to the artifact file-store port.
    #[must_use]
    pub fn new(storage: &ScopedStorage) -> Self {
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
        let path = self.existing_path(key).await?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .await
            .with_context(|| format!("cannot open {} for writing", path.display()))?;
        file.write_all(bytes)
            .await
            .with_context(|| format!("cannot write {}", path.display()))?;
        file.flush()
            .await
            .with_context(|| format!("cannot flush {}", path.display()))?;

        Ok(FileMetadata {
            size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            media_type: media_type
                .map(str::to_owned)
                .or_else(|| crate::media_type_for(Path::new(key)).map(str::to_owned)),
        })
    }

    async fn existing_path(&self, key: &str) -> Result<PathBuf> {
        validate_key(key)?;
        let root = tokio::fs::canonicalize(&self.root)
            .await
            .with_context(|| format!("cannot resolve file store root {}", self.root.display()))?;
        let path = root.join(key);
        let parent = path.parent().context("file key has no parent")?;
        let canonical_parent = tokio::fs::canonicalize(parent)
            .await
            .with_context(|| format!("cannot resolve parent of {}", path.display()))?;
        if !canonical_parent.starts_with(&root) {
            bail!("file key escapes the configured store root: {key:?}");
        }

        let candidate = canonical_parent.join(path.file_name().context("file key has no name")?);
        let canonical = tokio::fs::canonicalize(&candidate)
            .await
            .with_context(|| format!("cannot resolve existing file {}", candidate.display()))?;
        if !canonical.starts_with(&root) {
            bail!("file key escapes the configured store root: {key:?}");
        }
        let metadata = tokio::fs::metadata(&canonical)
            .await
            .with_context(|| format!("cannot inspect {}", canonical.display()))?;
        if !metadata.is_file() {
            bail!("file key does not name a regular file: {key:?}");
        }
        Ok(canonical)
    }

    async fn path(&self, key: &str, create_parent: bool) -> Result<PathBuf> {
        validate_key(key)?;
        tokio::fs::create_dir_all(&self.root).await?;
        let root = tokio::fs::canonicalize(&self.root).await?;
        let path = root.join(key);
        let parent = path.parent().context("file key has no parent")?;
        if create_parent {
            create_parent_directories(&root, key).await?;
        }
        let canonical_parent = match tokio::fs::canonicalize(parent).await {
            Ok(parent) => parent,
            Err(error) if !create_parent && error.kind() == std::io::ErrorKind::NotFound => {
                let mut ancestor = parent;
                let canonical_ancestor = loop {
                    match tokio::fs::canonicalize(ancestor).await {
                        Ok(canonical) => break canonical,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            ancestor = ancestor
                                .parent()
                                .context("file key parent is outside the store")?;
                        }
                        Err(error) => return Err(error.into()),
                    }
                };
                if !canonical_ancestor.starts_with(&root) {
                    bail!("file key escapes the configured store root: {key:?}");
                }
                let missing = parent
                    .strip_prefix(ancestor)
                    .context("file key parent is outside the store")?;
                return Ok(canonical_ancestor
                    .join(missing)
                    .join(path.file_name().context("file key has no name")?));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot resolve parent of {}", path.display()));
            }
        };
        if !canonical_parent.starts_with(&root) {
            bail!("file key escapes the configured store root: {key:?}");
        }
        Ok(canonical_parent.join(path.file_name().context("file key has no name")?))
    }
}

async fn create_parent_directories(root: &Path, key: &str) -> Result<()> {
    let mut directory = root.to_path_buf();
    let mut segments = key.split('/').peekable();
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            break;
        }
        let candidate = directory.join(segment);
        match tokio::fs::create_dir(&candidate).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot create {}", candidate.display()));
            }
        }
        directory = tokio::fs::canonicalize(&candidate)
            .await
            .with_context(|| format!("cannot resolve {}", candidate.display()))?;
        if !directory.starts_with(root) {
            bail!("file key escapes the configured store root: {key:?}");
        }
    }
    Ok(())
}

#[async_trait]
impl FileStore for FsFileStore {
    async fn put(&self, key: &str, bytes: &[u8], media_type: Option<&str>) -> Result<FileMetadata> {
        let path = self.path(key, true).await?;
        let parent = path.parent().context("file key has no parent")?;
        let staging = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        tokio::fs::write(&staging, bytes)
            .await
            .with_context(|| format!("cannot write {}", staging.display()))?;
        if let Err(error) = tokio::fs::rename(&staging, &path).await {
            let _ = tokio::fs::remove_file(&staging).await;
            return Err(error).with_context(|| format!("cannot save {}", path.display()));
        }
        Ok(FileMetadata {
            size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            media_type: media_type
                .map(str::to_owned)
                .or_else(|| crate::media_type_for(Path::new(key)).map(str::to_owned)),
        })
    }

    async fn read(&self, key: &str) -> Result<Vec<u8>> {
        let path = self.path(key, false).await?;
        if let Ok(canonical) = tokio::fs::canonicalize(&path).await {
            let root = tokio::fs::canonicalize(&self.root).await?;
            if !canonical.starts_with(root) {
                bail!("file key escapes the configured store root: {key:?}");
            }
        }
        tokio::fs::read(&path)
            .await
            .with_context(|| format!("cannot read {}", path.display()))
    }

    async fn metadata(&self, key: &str) -> Result<Option<FileMetadata>> {
        let path = self.path(key, false).await?;
        if let Ok(canonical) = tokio::fs::canonicalize(&path).await {
            let root = tokio::fs::canonicalize(&self.root).await?;
            if !canonical.starts_with(root) {
                bail!("file key escapes the configured store root: {key:?}");
            }
        }
        match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => Ok(Some(FileMetadata {
                size_bytes: metadata.len(),
                media_type: crate::media_type_for(&path).map(str::to_owned),
            })),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("cannot inspect {}", path.display())),
        }
    }

    async fn delete_prefix(&self, prefix: &str) -> Result<()> {
        validate_prefix(prefix)?;
        let path = self.path(prefix.trim_end_matches('/'), false).await?;
        if let Ok(canonical) = tokio::fs::canonicalize(&path).await {
            let root = tokio::fs::canonicalize(&self.root).await?;
            if !canonical.starts_with(root) {
                bail!("file key prefix escapes the configured store root: {prefix:?}");
            }
        }
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("cannot remove {}", path.display())),
        }
    }
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

/// Copy a legacy path-based reference into any configured file store.
///
/// The source path is resolved against the supplied roots before it is read;
/// this also applies the host's guard to desktop data being imported.
///
/// # Errors
///
/// Returns an error if the old path is refused, unreadable, or the new object
/// cannot be written.
pub async fn import_legacy_file(
    store: &dyn FileStore,
    roots: &FileRoots,
    legacy: &FileRef,
    key: &str,
) -> Result<FileRef> {
    let source = roots.resolve(&legacy.key).await?;
    let bytes = tokio::fs::read(&source.path).await?;
    let media_type = legacy
        .media_type
        .as_deref()
        .or(source.media_type.as_deref());
    let metadata = store.put(key, &bytes, media_type).await?;
    Ok(FileRef {
        key: key.to_owned(),
        size_bytes: metadata.size_bytes,
        media_type: metadata.media_type,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "file_store_tests.rs"]
mod tests;
