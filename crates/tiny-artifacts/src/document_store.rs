//! Artifact record persistence over a host-bound document handle.

use std::cmp::Reverse;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tinystoragedrivers_api::{DocumentStore, DocumentStoreExt, Storage};
use tinystoragedrivers_types::{ErrorKind, Precondition, Query, StorageError, WriteOptions};

use crate::storage::{
    collections, ARTIFACTS_COLLECTION, MIGRATIONS_COLLECTION, MIGRATIONS_ID, SCHEMA_VERSION,
};
use crate::store::validate_id;
use crate::{ArtifactRecord, ArtifactStore};

/// [`ArtifactStore`] backed by documents in a host-provided storage scope.
#[derive(Clone)]
pub struct DocumentArtifactStore {
    documents: Arc<dyn DocumentStore>,
}

impl std::fmt::Debug for DocumentArtifactStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentArtifactStore")
            .finish_non_exhaustive()
    }
}

impl DocumentArtifactStore {
    /// Open the artifact owner over an already-bound storage bundle, ensure
    /// its collections, and persist its schema checkpoint before returning a
    /// usable store.
    ///
    /// # Errors
    /// Returns an error when collections cannot be ensured or the migration
    /// checkpoint cannot be read or written.
    pub async fn open(storage: &Storage) -> Result<Self> {
        let documents = Arc::clone(storage.documents());
        for spec in collections() {
            documents.ensure_collection(&spec).await?;
        }
        migrate(documents.as_ref()).await?;
        Ok(Self { documents })
    }

    /// Import one record encoded by the previous filesystem store writer.
    ///
    /// The host owns locating and reading source files; this method accepts
    /// their bytes, validates the old record shape, and inserts it only when
    /// its id is not already present. An exact body/schema replay is safe; an
    /// id already holding different content is a conflict.
    ///
    /// # Errors
    /// Returns an error when the record is malformed, its id is unsafe, a
    /// conflicting id already holds another record, or the document backend
    /// refuses the write.
    pub async fn import_legacy_record_json(&self, bytes: &[u8]) -> Result<bool> {
        let record: ArtifactRecord =
            serde_json::from_slice(bytes).context("legacy artifact record is not valid JSON")?;
        validate_id(&record.id)?;
        let body = serde_json::to_value(&record)?;
        match self
            .documents
            .put_with(
                ARTIFACTS_COLLECTION,
                &record.id,
                body.clone(),
                Precondition::Absent,
                WriteOptions::new().schema(SCHEMA_VERSION),
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::Conflict => {
                let Some(existing) = self.documents.get(ARTIFACTS_COLLECTION, &record.id).await?
                else {
                    return Err(error.into());
                };
                if existing.meta.schema == SCHEMA_VERSION
                    && existing.meta.expires_ms.is_none()
                    && existing.doc == body
                {
                    Ok(false)
                } else {
                    Err(StorageError::conflict(
                        "legacy artifact id already contains a different record",
                    )
                    .into())
                }
            }
            Err(error) => Err(error.into()),
        }
    }
}

async fn migrate(documents: &dyn DocumentStore) -> Result<()> {
    let current = documents.get(MIGRATIONS_COLLECTION, MIGRATIONS_ID).await?;
    let current_version = current
        .as_ref()
        .and_then(|record| record.doc.get("version"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    if current_version >= u64::from(SCHEMA_VERSION)
        && current
            .as_ref()
            .is_some_and(|record| record.meta.schema == SCHEMA_VERSION)
    {
        return Ok(());
    }

    let next = json!({ "version": current_version.max(u64::from(SCHEMA_VERSION)) });
    let precondition = current.map_or(Precondition::Absent, |record| {
        Precondition::Version(record.version)
    });
    match documents
        .put_with(
            MIGRATIONS_COLLECTION,
            MIGRATIONS_ID,
            next,
            precondition,
            WriteOptions::new().schema(SCHEMA_VERSION),
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::Conflict => {
            let latest = documents.get(MIGRATIONS_COLLECTION, MIGRATIONS_ID).await?;
            let completed = latest
                .as_ref()
                .and_then(|record| record.doc.get("version"))
                .and_then(Value::as_u64)
                .is_some_and(|version| version >= u64::from(SCHEMA_VERSION))
                && latest
                    .as_ref()
                    .is_some_and(|record| record.meta.schema == SCHEMA_VERSION);
            if completed {
                Ok(())
            } else {
                Err(error.into())
            }
        }
        Err(error) => Err(error.into()),
    }
}

#[async_trait]
impl ArtifactStore for DocumentArtifactStore {
    async fn list(&self) -> Result<Vec<ArtifactRecord>> {
        let query = Query::all();
        let records = self
            .documents
            .query_all(ARTIFACTS_COLLECTION, &query)
            .await?;
        let mut artifacts = Vec::with_capacity(records.len());
        for record in records {
            match serde_json::from_value(record.doc) {
                Ok(artifact) => artifacts.push(artifact),
                Err(error) => tracing::warn!(
                    target: "tiny-artifacts",
                    artifact_id = %record.id,
                    err = %error,
                    "skipping an unreadable artifact record"
                ),
            }
        }
        artifacts.sort_by_key(|record: &ArtifactRecord| {
            (Reverse(record.updated_at_millis), record.id.clone())
        });
        Ok(artifacts)
    }

    async fn get(&self, id: &str) -> Result<Option<ArtifactRecord>> {
        validate_id(id)?;
        self.documents
            .get(ARTIFACTS_COLLECTION, id)
            .await?
            .map(|record| {
                serde_json::from_value(record.doc)
                    .with_context(|| format!("artifact {id} is not a valid record"))
            })
            .transpose()
    }

    async fn upsert(&self, artifact: &ArtifactRecord) -> Result<()> {
        validate_id(&artifact.id)?;
        let body = serde_json::to_value(artifact)?;
        self.documents
            .put_with(
                ARTIFACTS_COLLECTION,
                &artifact.id,
                body,
                Precondition::None,
                WriteOptions::new().schema(SCHEMA_VERSION),
            )
            .await?;
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        validate_id(id)?;
        self.documents
            .delete(ARTIFACTS_COLLECTION, id, Precondition::None)
            .await
            .map_err(Into::into)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "document_store_tests.rs"]
mod tests;
