//! File artifacts made from bytes the host already holds (`Content::Bytes`),
//! with no path and so no folder to check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use serde_json::json;
use tiny_artifacts::{
    ArtifactKind, ArtifactRecord, ArtifactStore, Artifacts, ArtifactsError, Author, BlobFileStore,
    Content, CreateArtifactTool, FileStore, UpdateArtifactTool,
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

/// A service with a file store and, deliberately, no file folders: bytes need
/// none.
fn service() -> (Arc<Artifacts>, Arc<BlobFileStore>) {
    let storage = memory_storage(&Scope::new("artifact-bytes-test").unwrap());
    let files = Arc::new(BlobFileStore::new(&storage));
    let artifacts = Artifacts::new(Arc::new(MemStore::default())).with_file_store(files.clone());
    (Arc::new(artifacts), files)
}

fn bytes(name: &str, content: &[u8]) -> Content {
    Content::Bytes {
        name: name.into(),
        bytes: content.to_vec(),
    }
}

#[tokio::test]
async fn an_image_is_filed_from_bytes_with_no_folder_to_check() {
    let (artifacts, files) = service();

    let saved = artifacts
        .create(
            ArtifactKind::Image,
            "Chart",
            bytes("charts/q3.PNG", b"\x89PNG fake"),
            &Author::agent("agent"),
            None,
        )
        .await
        .unwrap();

    let file = saved.record.latest().unwrap().file.clone().unwrap();
    assert_eq!(
        file.key,
        format!("artifacts/files/{}/v1.PNG", saved.record.id)
    );
    assert_eq!(file.size_bytes, 9);
    assert_eq!(file.media_type.as_deref(), Some("image/png"));
    assert_eq!(saved.record.latest().unwrap().body, file.key);
    assert_eq!(files.read(&file.key).await.unwrap(), b"\x89PNG fake");
    let stored = artifacts.get(&saved.record.id).await.unwrap().unwrap();
    assert_eq!(stored.latest().unwrap().file.as_ref(), Some(&file));
}

#[tokio::test]
async fn a_revision_from_bytes_appends_a_version_and_keeps_the_old_bytes() {
    let (artifacts, files) = service();
    let author = Author::agent("agent");
    let first = artifacts
        .create(
            ArtifactKind::File,
            "Report",
            bytes("report.csv", b"a,b"),
            &author,
            None,
        )
        .await
        .unwrap();

    let second = artifacts
        .revise(
            &first.record.id,
            bytes("report.pdf", b"%PDF new"),
            &author,
            Some("as a pdf".into()),
        )
        .await
        .unwrap();

    assert!(second.appended);
    assert_eq!(second.version, 2);
    let versions = &second.record.versions;
    let (v1, v2) = (
        versions[0].file.clone().unwrap(),
        versions[1].file.clone().unwrap(),
    );
    assert!(v1.key.ends_with("/v1.csv") && v2.key.ends_with("/v2.pdf"));
    assert_eq!(v2.media_type.as_deref(), Some("application/pdf"));
    assert_eq!(files.read(&v1.key).await.unwrap(), b"a,b");
    assert_eq!(files.read(&v2.key).await.unwrap(), b"%PDF new");
}

#[tokio::test]
async fn a_name_cannot_put_anything_but_a_plain_extension_in_the_storage_key() {
    let (artifacts, _files) = service();
    for name in [
        "../../etc/passwd",
        "noext",
        "weird.t\\xt",
        "",
        "x.aaaaaaaaaaaaaaaaaaaa",
    ] {
        let saved = artifacts
            .create(
                ArtifactKind::File,
                "F",
                bytes(name, b"data"),
                &Author::agent("agent"),
                None,
            )
            .await
            .expect("a name never stops bytes being filed");
        let key = saved.record.latest().unwrap().file.clone().unwrap().key;
        assert_eq!(
            key,
            format!("artifacts/files/{}/v1", saved.record.id),
            "`{name}` gives no extension"
        );
    }
}

#[tokio::test]
async fn bytes_need_a_file_store_and_a_file_kind() {
    let (artifacts, _files) = service();
    let author = Author::agent("agent");
    let err = artifacts
        .create(
            ArtifactKind::Markdown,
            "Doc",
            bytes("a.png", b"x"),
            &author,
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ArtifactsError::WrongContent { .. }), "{err}");

    let bare = Artifacts::new(Arc::new(MemStore::default()));
    let err = bare
        .create(
            ArtifactKind::Image,
            "Logo",
            bytes("a.png", b"x"),
            &author,
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ArtifactsError::NoFileStore), "{err}");
    assert!(bare.list().await.unwrap().is_empty(), "nothing is filed");
}

#[tokio::test]
async fn the_tools_file_bytes_the_host_hands_them_in_place_of_a_path() {
    let (artifacts, files) = service();
    let author = Author::agent("agent");
    let create = CreateArtifactTool::new(artifacts.clone(), author.clone());
    let update = UpdateArtifactTool::new(artifacts.clone(), author);

    let made = create
        .execute_with_content(
            json!({ "kind": "image", "title": "Chart", "path": "ignored.txt" }),
            bytes("chart.png", b"one"),
        )
        .await
        .unwrap();
    assert!(!made.is_error, "{}", made.text());
    let made: serde_json::Value = serde_json::from_str(&made.text()).unwrap();
    assert_eq!(made["version"], 1);
    assert!(made["file_key"].as_str().unwrap().ends_with("/v1.png"));
    let id = made["artifact_id"].as_str().unwrap();

    let revised = update
        .execute_with_content(
            json!({ "artifact_id": id, "note": "bigger" }),
            bytes("chart.png", b"two"),
        )
        .await
        .unwrap();
    assert!(!revised.is_error, "{}", revised.text());
    let revised: serde_json::Value = serde_json::from_str(&revised.text()).unwrap();
    assert_eq!(revised["version"], 2);
    let key = revised["file_key"].as_str().unwrap();
    assert_eq!(files.read(key).await.unwrap(), b"two");
    let record = artifacts.get(id).await.unwrap().unwrap();
    assert_eq!(record.versions[1].note.as_deref(), Some("bigger"));

    // The call's own arguments are still checked.
    let missing = update
        .execute_with_content(json!({ "artifact_id": "nope" }), bytes("a.png", b"x"))
        .await
        .unwrap();
    assert!(missing.is_error);
    assert!(create
        .execute_with_content(json!({ "kind": "image" }), bytes("a.png", b"x"))
        .await
        .is_err());
}
