//! The [`ArtifactStore`] port: durable storage for versioned artifacts.

use anyhow::Result;
use async_trait::async_trait;

use crate::types::ArtifactRecord;

/// Durable storage for artifacts. Writes replace whole records — a caller
/// mutates the record (e.g. `push_version`) and upserts the result.
#[async_trait]
pub trait ArtifactStore: Send + Sync {
    /// Lists artifacts, most-recently-updated first.
    async fn list(&self) -> Result<Vec<ArtifactRecord>>;
    /// Fetches one artifact by id, with its full version history.
    async fn get(&self, id: &str) -> Result<Option<ArtifactRecord>>;
    /// Inserts or replaces an artifact by id.
    async fn upsert(&self, artifact: &ArtifactRecord) -> Result<()>;
    /// Deletes an artifact by id; returns whether one was removed.
    async fn delete(&self, id: &str) -> Result<bool>;
}

/// Reject ids that could escape the artifact namespace or collide with path
/// separators used by versioned file keys.
pub(crate) fn validate_id(id: &str) -> anyhow::Result<()> {
    use anyhow::bail;

    if id.is_empty() || id == "." || id == ".." {
        bail!("invalid artifact id {id:?}");
    }
    if id.contains(['/', '\\', '\0']) {
        bail!("artifact id must not contain path separators: {id:?}");
    }
    if id.starts_with('.') {
        bail!("artifact id must not start with '.': {id:?}");
    }
    Ok(())
}
