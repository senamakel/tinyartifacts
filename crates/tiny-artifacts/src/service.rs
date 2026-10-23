//! [`Artifacts`]: the one way artifacts are created and changed.
//!
//! The agent's tools and a host's own producers (an image generator, a deck
//! builder) all go through this service, so every artifact follows the same
//! rules whoever makes it:
//!
//! * text kinds take text, file kinds take a file checked by [`FileRoots`];
//! * a revision appends a version and never edits one (an identical text
//!   revision appends nothing);
//! * a slow producer opens the artifact pending and completes or fails it;
//! * after every write the host's [`ArtifactObserver`] hears about it, so the
//!   record is always written first and anything the host shows is derived
//!   from it.

use std::sync::Arc;

use uuid::Uuid;

use crate::events::{ArtifactEvent, ArtifactEventKind, ArtifactObserver};
use crate::file_ref::{FileRef, FileRefError, FileRoots};
use crate::file_store::FileStore;
use crate::publish::{ArtifactPublisher, PublishOptions};
use crate::store::ArtifactStore;
use crate::types::{
    ArtifactAuthor, ArtifactKind, ArtifactRecord, ArtifactStatus, ArtifactVersion, PublishedLink,
};

/// What a version contains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    /// Source text, for `text`, `markdown` and `html` artifacts.
    Text(String),
    /// A path to a file, for `image` and `file` artifacts. Checked against the
    /// service's [`FileRoots`] before anything is written.
    File(String),
}

/// Who is writing a version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Author {
    /// Agent or human.
    pub kind: ArtifactAuthor,
    /// The agent id or the operator's label (display only).
    pub id: String,
}

impl Author {
    /// An agent, by id.
    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self {
            kind: ArtifactAuthor::Agent,
            id: id.into(),
        }
    }

    /// A human, by label.
    #[must_use]
    pub fn operator(id: impl Into<String>) -> Self {
        Self {
            kind: ArtifactAuthor::Operator,
            id: id.into(),
        }
    }
}

/// The result of a write.
#[derive(Clone, Debug)]
pub struct Saved {
    /// The record as stored.
    pub record: ArtifactRecord,
    /// The newest version number (0 for a pending artifact with no version).
    pub version: u32,
    /// Whether this call appended a version.
    pub appended: bool,
    /// Set when the host's observer failed after the write. The write stands.
    pub notice: Option<String>,
}

/// The result of [`Artifacts::publish`].
#[derive(Clone, Debug)]
pub struct Published {
    /// Where the version can be reached.
    pub link: PublishedLink,
    /// The version shared.
    pub version: u32,
    /// Whether an existing live link was returned instead of sharing again.
    pub reused: bool,
}

/// Why an artifact operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactsError {
    /// No artifact has this id.
    #[error("artifact `{0}` not found")]
    NotFound(String),
    /// The content does not fit the artifact's kind.
    #[error("a `{kind}` artifact takes {expected}")]
    WrongContent {
        /// The artifact's kind.
        kind: &'static str,
        /// What it takes instead.
        expected: &'static str,
    },
    /// Text content was empty.
    #[error("the artifact content is empty")]
    EmptyText,
    /// The file could not be referenced.
    #[error(transparent)]
    File(#[from] FileRefError),
    /// The artifact has no version to act on.
    #[error("artifact `{id}` has no version {version}")]
    NoVersion {
        /// The artifact id.
        id: String,
        /// The version asked for (0 = newest).
        version: u32,
    },
    /// The host has no way to share artifacts.
    #[error("this host cannot share artifacts")]
    NoPublisher,
    /// An older version's file was replaced since, so it cannot be shared as
    /// it was.
    #[error("version {version} of `{id}` points at a file that has changed since; share the newest version instead")]
    FileChanged {
        /// The artifact id.
        id: String,
        /// The version asked for.
        version: u32,
    },
    /// The store or the publisher failed.
    #[error(transparent)]
    Backend(#[from] anyhow::Error),
    /// File content needs a configured durable file store.
    #[error("this host has no file store for artifact files")]
    NoFileStore,
}

/// The artifact service: one store, plus the host's observer, publisher and
/// allowed file folders.
#[derive(Clone)]
pub struct Artifacts {
    store: Arc<dyn ArtifactStore>,
    observer: Option<Arc<dyn ArtifactObserver>>,
    publisher: Option<Arc<dyn ArtifactPublisher>>,
    files: FileRoots,
    file_store: Option<Arc<dyn FileStore>>,
}

impl std::fmt::Debug for Artifacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Artifacts")
            .field("observer", &self.observer.is_some())
            .field("publisher", &self.publisher.is_some())
            .field("files", &self.files)
            .field("file_store", &self.file_store.is_some())
            .finish_non_exhaustive()
    }
}

impl Artifacts {
    /// A service over `store`, with no observer, no publisher and no file
    /// folders (file artifacts are refused until [`Self::with_file_roots`]).
    #[must_use]
    pub fn new(store: Arc<dyn ArtifactStore>) -> Self {
        Self {
            store,
            observer: None,
            publisher: None,
            files: FileRoots::default(),
            file_store: None,
        }
    }

    /// Tell `observer` about every write.
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn ArtifactObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Share versions through `publisher`.
    #[must_use]
    pub fn with_publisher(mut self, publisher: Arc<dyn ArtifactPublisher>) -> Self {
        self.publisher = Some(publisher);
        self
    }

    /// Let file artifacts reference files inside `files`.
    #[must_use]
    pub fn with_file_roots(mut self, files: FileRoots) -> Self {
        self.files = files;
        self
    }

    /// Store file artifacts in `file_store` under generated artifact keys.
    #[must_use]
    pub fn with_file_store(mut self, file_store: Arc<dyn FileStore>) -> Self {
        self.file_store = Some(file_store);
        self
    }

    /// The underlying store, for reads.
    #[must_use]
    pub fn store(&self) -> &Arc<dyn ArtifactStore> {
        &self.store
    }

    /// Whether a publisher is configured.
    #[must_use]
    pub fn can_publish(&self) -> bool {
        self.publisher.is_some()
    }

    /// One artifact with its history.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactsError::Backend`] when the store fails.
    pub async fn get(&self, id: &str) -> Result<Option<ArtifactRecord>, ArtifactsError> {
        Ok(self.store.get(id).await?)
    }

    /// Every artifact, most recently updated first.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactsError::Backend`] when the store fails.
    pub async fn list(&self) -> Result<Vec<ArtifactRecord>, ArtifactsError> {
        Ok(self.store.list().await?)
    }

    /// Create an artifact with its first version. `id` lets a host keep an id
    /// it already uses; a fresh one is minted otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error when the content does not fit `kind`, a file cannot be
    /// referenced, or the store fails.
    pub async fn create(
        &self,
        kind: ArtifactKind,
        title: &str,
        content: Content,
        author: &Author,
        id: Option<String>,
    ) -> Result<Saved, ArtifactsError> {
        let at = crate::now_millis();
        let id = id.unwrap_or_else(new_id);
        let version = self.version_for(kind, content, author, at).await?;
        let mut record = ArtifactRecord::pending(id, title.trim(), kind, at);
        record.push_version(version);
        self.keep_version_file(&mut record).await?;
        record.status = ArtifactStatus::Ready;
        self.store.upsert(&record).await?;
        Ok(self.saved(record, true, ArtifactEventKind::Ready).await)
    }

    /// Open an artifact that is still being produced (status `Pending`, no
    /// version). Complete it with [`Self::revise`] or end it with
    /// [`Self::fail`].
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactsError::Backend`] when the store fails.
    pub async fn begin(
        &self,
        kind: ArtifactKind,
        title: &str,
        id: Option<String>,
    ) -> Result<Saved, ArtifactsError> {
        let record = ArtifactRecord::pending(
            id.unwrap_or_else(new_id),
            title.trim(),
            kind,
            crate::now_millis(),
        );
        self.store.upsert(&record).await?;
        Ok(self.saved(record, false, ArtifactEventKind::Pending).await)
    }

    /// Append a version (completing a pending artifact). A text revision
    /// identical to the newest version appends nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when the artifact does not exist, the content does not
    /// fit its kind, a file cannot be referenced, or the store fails.
    pub async fn revise(
        &self,
        id: &str,
        content: Content,
        author: &Author,
        note: Option<String>,
    ) -> Result<Saved, ArtifactsError> {
        let mut record = self.require(id).await?;
        if record.status == ArtifactStatus::Ready {
            if let (Content::Text(text), Some(latest)) = (&content, record.latest()) {
                if latest.body == *text && !record.holds_file() {
                    return Ok(Saved {
                        version: latest.version,
                        record,
                        appended: false,
                        notice: None,
                    });
                }
            }
        }
        let mut version = self
            .version_for(record.kind, content, author, crate::now_millis())
            .await?;
        version.note = note.filter(|n| !n.trim().is_empty());
        record.push_version(version);
        self.keep_version_file(&mut record).await?;
        record.status = ArtifactStatus::Ready;
        record.error = None;
        self.store.upsert(&record).await?;
        Ok(self.saved(record, true, ArtifactEventKind::Ready).await)
    }

    /// Mark production failed, with the reason.
    ///
    /// # Errors
    ///
    /// Returns an error when the artifact does not exist or the store fails.
    pub async fn fail(&self, id: &str, error: &str) -> Result<Saved, ArtifactsError> {
        let mut record = self.require(id).await?;
        record.status = ArtifactStatus::Failed;
        record.error = Some(error.to_string());
        record.updated_at_millis = crate::now_millis();
        self.store.upsert(&record).await?;
        Ok(self.saved(record, false, ArtifactEventKind::Failed).await)
    }

    /// Delete an artifact. Returns whether one was removed.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactsError::Backend`] when the store fails.
    pub async fn delete(&self, id: &str) -> Result<(bool, Option<String>), ArtifactsError> {
        let Some(record) = self.store.get(id).await? else {
            return Ok((false, None));
        };
        let removed = self.store.delete(id).await?;
        if removed {
            self.drop_version_files(id).await;
        }
        let notice = if removed {
            self.notify(ArtifactEventKind::Deleted, &record).await
        } else {
            None
        };
        Ok((removed, notice))
    }

    /// Share a version (`version = None`: the newest) and record the link on
    /// it. A version that already has a live link of the same kind reuses it
    /// unless `options.fresh` asks for a new one.
    ///
    /// # Errors
    ///
    /// Returns an error when no publisher is configured, the artifact or
    /// version does not exist, an older version's file has changed since, or
    /// sharing fails.
    pub async fn publish(
        &self,
        id: &str,
        version: Option<u32>,
        options: &PublishOptions,
    ) -> Result<Published, ArtifactsError> {
        let publisher = self.publisher.as_ref().ok_or(ArtifactsError::NoPublisher)?;
        let mut record = self.require(id).await?;
        let newest = record.latest().map(|v| v.version);
        let wanted = match version {
            Some(n) => record.version(n),
            None => record.latest(),
        }
        .cloned()
        .ok_or_else(|| ArtifactsError::NoVersion {
            id: id.to_string(),
            version: version.unwrap_or(0),
        })?;

        if !options.fresh {
            if let Some(link) = wanted.published.as_ref() {
                if link.public == options.public && link.is_live(crate::now_millis()) {
                    return Ok(Published {
                        link: link.clone(),
                        version: wanted.version,
                        reused: true,
                    });
                }
            }
        }
        // A file is replaced in place by later versions: an older version can
        // only be shared while its file is still the one it recorded.
        if Some(wanted.version) != newest {
            if let Some(file) = wanted.file.as_ref() {
                let unchanged = if let Some(file_store) = &self.file_store {
                    file_store
                        .metadata(&file.key)
                        .await?
                        .is_some_and(|meta| meta.size_bytes == file.size_bytes)
                } else {
                    false
                };
                if !unchanged {
                    return Err(ArtifactsError::FileChanged {
                        id: id.to_string(),
                        version: wanted.version,
                    });
                }
            }
        }

        let link = publisher.publish(&record, &wanted, options).await?;
        if let Some(stored) = record
            .versions
            .iter_mut()
            .find(|v| v.version == wanted.version)
        {
            stored.published = Some(link.clone());
        }
        self.store.upsert(&record).await?;
        Ok(Published {
            link,
            version: wanted.version,
            reused: false,
        })
    }

    /// Persist the newest file version under a stable key and update its ref.
    async fn keep_version_file(&self, record: &mut ArtifactRecord) -> Result<(), ArtifactsError> {
        let Some(file_store) = &self.file_store else {
            return if record.kind.holds_file() {
                Err(ArtifactsError::NoFileStore)
            } else {
                Ok(())
            };
        };
        let id = record.id.clone();
        crate::store::validate_id(&id)?;
        let holds_file = record.kind.holds_file();
        let Some(version) = record.versions.last_mut() else {
            return Ok(());
        };
        if !holds_file || version.file.is_some() {
            return Ok(());
        }
        let source = self.files.resolve(&version.body).await?;
        let name = match source.path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) => format!("v{}.{ext}", version.version),
            None => format!("v{}", version.version),
        };
        let key = format!("artifacts/files/{id}/{name}");
        let bytes = tokio::fs::read(&source.path)
            .await
            .map_err(anyhow::Error::from)?;
        let metadata = file_store
            .put(&key, &bytes, source.media_type.as_deref())
            .await?;
        if metadata.size_bytes != source.size_bytes {
            return Err(ArtifactsError::Backend(anyhow::anyhow!(
                "file changed while artifact version {} was being stored",
                version.version
            )));
        }
        let kept = FileRef {
            key: key.clone(),
            size_bytes: metadata.size_bytes,
            media_type: metadata.media_type,
        };
        version.body = key;
        version.file = Some(kept);
        Ok(())
    }

    /// Remove a deleted artifact's stored files. Best-effort: the record is
    /// already gone.
    async fn drop_version_files(&self, id: &str) {
        let Some(file_store) = &self.file_store else {
            return;
        };
        if crate::store::validate_id(id).is_err() {
            return;
        }
        match file_store
            .delete_prefix(&format!("artifacts/files/{id}/"))
            .await
        {
            Ok(()) => {}
            Err(err) => tracing::warn!(
                target: "tiny-artifacts",
                artifact_id = id,
                err = %err,
                "could not remove an artifact's version files"
            ),
        }
    }

    async fn require(&self, id: &str) -> Result<ArtifactRecord, ArtifactsError> {
        self.store
            .get(id)
            .await?
            .ok_or_else(|| ArtifactsError::NotFound(id.to_string()))
    }

    async fn version_for(
        &self,
        kind: ArtifactKind,
        content: Content,
        author: &Author,
        at: u64,
    ) -> Result<ArtifactVersion, ArtifactsError> {
        match (kind.holds_file(), content) {
            (false, Content::Text(text)) => {
                if text.trim().is_empty() {
                    return Err(ArtifactsError::EmptyText);
                }
                Ok(ArtifactVersion::new(
                    0,
                    text,
                    author.kind,
                    author.id.clone(),
                    at,
                ))
            }
            (true, Content::File(path)) => {
                let file = self.files.resolve(&path).await?;
                Ok(ArtifactVersion::new(
                    0,
                    file.path.to_string_lossy(),
                    author.kind,
                    author.id.clone(),
                    at,
                ))
            }
            (false, Content::File(_)) => Err(ArtifactsError::WrongContent {
                kind: kind.as_str(),
                expected: "text, not a file path",
            }),
            (true, Content::Text(_)) => Err(ArtifactsError::WrongContent {
                kind: kind.as_str(),
                expected: "a file path, not text",
            }),
        }
    }

    async fn saved(
        &self,
        record: ArtifactRecord,
        appended: bool,
        kind: ArtifactEventKind,
    ) -> Saved {
        let notice = self.notify(kind, &record).await;
        Saved {
            version: record.latest().map_or(0, |v| v.version),
            record,
            appended,
            notice,
        }
    }

    async fn notify(&self, kind: ArtifactEventKind, record: &ArtifactRecord) -> Option<String> {
        let observer = self.observer.as_ref()?;
        let event = ArtifactEvent {
            kind,
            record: record.clone(),
        };
        observer
            .on_event(&event)
            .await
            .err()
            .map(|err| format!("saved, but the host could not show it: {err}"))
    }
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}
