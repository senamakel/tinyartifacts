//! Shared storage behavior across independent artifact-service instances.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tiny_artifacts::{
    ArtifactKind, ArtifactPublisher, ArtifactRecord, ArtifactStore, ArtifactVersion, Artifacts,
    Author, BlobFileStore, Content, FileRoots, FileStore, PublishOptions, PublishedLink,
};
use tinystoragedrivers_api::{BlobStore, StorageBackend};
use tinystoragedrivers_testkit::memory_backend;
use tinystoragedrivers_types::Scope;

#[derive(Clone, Default)]
struct Records(Arc<Mutex<HashMap<String, ArtifactRecord>>>);

#[async_trait::async_trait]
impl ArtifactStore for Records {
    async fn list(&self) -> anyhow::Result<Vec<ArtifactRecord>> {
        Ok(self.0.lock().unwrap().values().cloned().collect())
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<ArtifactRecord>> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }

    async fn upsert(&self, record: &ArtifactRecord) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(record.id.clone(), record.clone());
        Ok(())
    }

    async fn delete(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self.0.lock().unwrap().remove(id).is_some())
    }
}

struct ReadingPublisher(Arc<dyn BlobStore>);

#[async_trait::async_trait]
impl ArtifactPublisher for ReadingPublisher {
    async fn publish(
        &self,
        record: &ArtifactRecord,
        version: &ArtifactVersion,
        options: &PublishOptions,
    ) -> anyhow::Result<PublishedLink> {
        let file = version.file.as_ref().unwrap();
        let bytes = self
            .0
            .get(&file.key)
            .await?
            .expect("published version is present in shared storage")
            .bytes;
        assert_eq!(bytes, b"version one");
        Ok(PublishedLink {
            url: format!("https://share.example/{}/v{}", record.id, version.version),
            expires_at_millis: Some(u64::MAX),
            published_at_millis: 1,
            public: options.public,
        })
    }
}

fn temp_dir() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("tiny-artifact-share-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[tokio::test]
async fn separate_services_read_share_and_delete_shared_file_versions() {
    let input = temp_dir();
    std::fs::write(input.join("report.pdf"), b"version one").unwrap();
    let records = Records::default();
    let backend = memory_backend();
    let scope = Scope::new("artifact-shared-files").unwrap();
    let first_storage = backend.for_scope(&scope).unwrap();
    let first_blobs = first_storage.blobs().clone();
    let first_files = Arc::new(BlobFileStore::new(&first_storage));
    let first = Artifacts::new(Arc::new(records.clone()))
        .with_file_roots(FileRoots::new([input.clone()]))
        .with_file_store(first_files.clone());

    let id = first
        .create(
            ArtifactKind::File,
            "Report",
            Content::File("report.pdf".into()),
            &Author::agent("writer"),
            Some("shared-report".into()),
        )
        .await
        .unwrap()
        .record
        .id;
    let first_key = first
        .get(&id)
        .await
        .unwrap()
        .unwrap()
        .version(1)
        .unwrap()
        .file
        .as_ref()
        .unwrap()
        .key
        .clone();

    std::fs::write(input.join("report.pdf"), b"version two").unwrap();
    first
        .revise(
            &id,
            Content::File("report.pdf".into()),
            &Author::agent("writer"),
            None,
        )
        .await
        .unwrap();
    std::fs::remove_dir_all(&input).unwrap();

    let second_storage = backend.for_scope(&scope).unwrap();
    let second_blobs = second_storage.blobs().clone();
    let second_files = Arc::new(BlobFileStore::new(&second_storage));
    let second = Artifacts::new(Arc::new(records.clone()))
        .with_file_store(second_files.clone())
        .with_publisher(Arc::new(ReadingPublisher(second_blobs.clone())));
    let history = second.get(&id).await.unwrap().unwrap();
    assert_eq!(history.versions.len(), 2);
    assert_eq!(second_files.read(&first_key).await.unwrap(), b"version one");
    assert_eq!(
        second
            .publish(&id, Some(1), &PublishOptions::default())
            .await
            .unwrap()
            .version,
        1
    );

    assert!(second.delete(&id).await.unwrap().0);
    assert_eq!(
        first_blobs
            .list("artifacts/files/shared-report/")
            .await
            .unwrap()
            .len(),
        0
    );
}
