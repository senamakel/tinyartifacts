//! Sharing an older file version: allowed while the stored file is still the
//! one that version recorded, refused once it has changed or cannot be checked.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tiny_artifacts::{
    ArtifactKind, ArtifactPublisher, ArtifactRecord, ArtifactStore, ArtifactVersion, Artifacts,
    ArtifactsError, Author, BlobFileStore, Content, FileRoots, FileStore, PublishOptions,
    PublishedLink,
};
use tinystoragedrivers_testkit::memory_storage;
use tinystoragedrivers_types::Scope;

#[derive(Default)]
struct MemStore(Mutex<Vec<ArtifactRecord>>);

#[async_trait::async_trait]
impl ArtifactStore for MemStore {
    async fn list(&self) -> anyhow::Result<Vec<ArtifactRecord>> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn get(&self, id: &str) -> anyhow::Result<Option<ArtifactRecord>> {
        Ok(self.0.lock().unwrap().iter().find(|r| r.id == id).cloned())
    }
    async fn upsert(&self, artifact: &ArtifactRecord) -> anyhow::Result<()> {
        let mut store = self.0.lock().unwrap();
        store.retain(|r| r.id != artifact.id);
        store.push(artifact.clone());
        Ok(())
    }
    async fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let mut store = self.0.lock().unwrap();
        let before = store.len();
        store.retain(|r| r.id != id);
        Ok(store.len() < before)
    }
}

struct LinkPublisher;

#[async_trait::async_trait]
impl ArtifactPublisher for LinkPublisher {
    async fn publish(
        &self,
        record: &ArtifactRecord,
        version: &ArtifactVersion,
        options: &PublishOptions,
    ) -> anyhow::Result<PublishedLink> {
        Ok(PublishedLink {
            url: format!("https://share.example/{}/v{}", record.id, version.version),
            expires_at_millis: Some(u64::MAX),
            published_at_millis: 1,
            public: options.public,
        })
    }
}

fn workdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiny-artifacts-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn an_older_file_version_is_refused_once_its_stored_file_changed() {
    let root = workdir();
    std::fs::write(root.join("deck.pptx"), b"v1").unwrap();
    let store = Arc::new(MemStore::default());
    let files = Arc::new(BlobFileStore::new(&memory_storage(
        &Scope::new("artifact-publish-test").unwrap(),
    )));
    let artifacts = Artifacts::new(store.clone())
        .with_publisher(Arc::new(LinkPublisher))
        .with_file_roots(FileRoots::new([root.clone()]))
        .with_file_store(files.clone());
    let author = Author::agent("a");
    let id = artifacts
        .create(
            ArtifactKind::File,
            "Deck",
            Content::File("deck.pptx".into()),
            &author,
            None,
        )
        .await
        .unwrap()
        .record
        .id;
    std::fs::write(root.join("deck.pptx"), b"v2-longer").unwrap();
    artifacts
        .revise(&id, Content::File("deck.pptx".into()), &author, None)
        .await
        .unwrap();
    let fresh = PublishOptions {
        fresh: true,
        ..PublishOptions::default()
    };

    // The stored v1 file is still the one v1 recorded.
    assert_eq!(
        artifacts
            .publish(&id, Some(1), &fresh)
            .await
            .unwrap()
            .version,
        1
    );

    // A host without a file store cannot check it, so it refuses.
    let unchecked = Artifacts::new(store.clone()).with_publisher(Arc::new(LinkPublisher));
    assert!(matches!(
        unchecked.publish(&id, Some(1), &fresh).await,
        Err(ArtifactsError::FileChanged { version: 1, .. })
    ));

    // Once the stored v1 file is replaced with different bytes, v1 is refused.
    let record = artifacts.get(&id).await.unwrap().unwrap();
    let key = record
        .version(1)
        .unwrap()
        .file
        .as_ref()
        .unwrap()
        .key
        .clone();
    files.put(&key, b"replaced, longer", None).await.unwrap();
    assert!(matches!(
        artifacts.publish(&id, Some(1), &fresh).await,
        Err(ArtifactsError::FileChanged { version: 1, .. })
    ));
    // The newest version is unaffected.
    assert_eq!(
        artifacts.publish(&id, None, &fresh).await.unwrap().version,
        2
    );
}
