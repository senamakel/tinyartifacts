use super::*;

use serde_json::Value;
use std::sync::Arc;
use tinystoragedrivers_api::{async_trait, DocumentStore, Storage, StorageBackend};
use tinystoragedrivers_testkit::{memory_backend, Failing, FaultPlan};
use tinystoragedrivers_types::{
    Capabilities, Capability, CollectionSpec, ErrorKind, Filter, Page, Precondition, Query, Scope,
    SearchHit, Sort, StorageError, Version, Versioned, WriteOptions,
};

fn storage(scope: &str) -> Storage {
    memory_backend()
        .for_scope(&Scope::new(scope).unwrap())
        .unwrap()
}

fn record(id: &str, updated: u64) -> ArtifactRecord {
    let mut value =
        ArtifactRecord::new(id, id, crate::ArtifactKind::Markdown, "# body", "writer", 1);
    value.updated_at_millis = updated;
    value
}

#[tokio::test]
async fn opening_applies_owned_schema_and_document_store_round_trips_records() {
    let storage = storage("document-artifact-store-test");
    let store = DocumentArtifactStore::open(&storage).await.unwrap();

    let marker = storage
        .documents()
        .get(MIGRATIONS_COLLECTION, MIGRATIONS_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(marker.doc["version"], SCHEMA_VERSION);
    assert_eq!(marker.meta.schema, SCHEMA_VERSION);
    store.upsert(&record("older", 1)).await.unwrap();
    store.upsert(&record("newer", 2)).await.unwrap();
    for id in ["older", "newer"] {
        let stored = storage
            .documents()
            .get(ARTIFACTS_COLLECTION, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.meta.schema, SCHEMA_VERSION);
    }

    let listed = store.list().await.unwrap();
    assert_eq!(listed, [record("newer", 2), record("older", 1)]);
    assert_eq!(store.get("older").await.unwrap(), Some(record("older", 1)));
    assert!(store.delete("older").await.unwrap());
    assert!(!store.delete("older").await.unwrap());

    let reopened = DocumentArtifactStore::open(&storage).await.unwrap();
    assert_eq!(
        reopened.get("newer").await.unwrap(),
        Some(record("newer", 2))
    );
}

#[tokio::test]
async fn legacy_record_import_is_idempotent_and_does_not_replace_existing_records() {
    let store = DocumentArtifactStore::open(&storage("artifact-import-test"))
        .await
        .unwrap();
    let legacy = include_bytes!("../tests/fixtures/previous-release-1.json");

    assert!(store.import_legacy_record_json(legacy).await.unwrap());
    assert!(!store.import_legacy_record_json(legacy).await.unwrap());
    let imported = store.get("previous-release-1").await.unwrap().unwrap();
    assert_eq!(imported.title, "Previous release record");
    assert_eq!(
        imported.latest().unwrap().body,
        "# Preserved artifact\n\nWritten by the 0.1.0 filesystem store."
    );
    assert_eq!(imported.created_at_millis, 1_704_067_200_000);
    let stored = store
        .documents
        .get(ARTIFACTS_COLLECTION, "previous-release-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.meta.schema, SCHEMA_VERSION);

    assert!(store
        .import_legacy_record_json(br#"{"id":"../outside"}"#)
        .await
        .is_err());
    assert!(store.import_legacy_record_json(b"not json").await.is_err());
}

#[tokio::test]
async fn legacy_import_conflict_only_accepts_identical_schema_and_body() {
    let store = DocumentArtifactStore::open(&storage("artifact-import-collision-test"))
        .await
        .unwrap();
    let legacy = include_bytes!("../tests/fixtures/previous-release-1.json");
    let mut conflicting = serde_json::from_slice::<ArtifactRecord>(legacy).unwrap();
    conflicting.title = "A different record with the same id".to_owned();
    store.upsert(&conflicting).await.unwrap();

    let error = store.import_legacy_record_json(legacy).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<StorageError>().unwrap().kind(),
        ErrorKind::Conflict
    );
    assert_eq!(
        store.get("previous-release-1").await.unwrap(),
        Some(conflicting)
    );
}

#[tokio::test]
async fn legacy_import_rejects_matching_target_with_expiry() {
    let storage = storage("artifact-import-expiry-test");
    let store = DocumentArtifactStore::open(&storage).await.unwrap();
    let legacy = include_bytes!("../tests/fixtures/previous-release-1.json");
    let source_dir = std::env::current_dir()
        .unwrap()
        .join("target/test-fixtures")
        .join(format!("artifact-import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&source_dir).unwrap();
    let source_path = source_dir.join("previous-release-1.json");
    std::fs::write(&source_path, legacy).unwrap();
    let source_bytes = std::fs::read(&source_path).unwrap();
    let record: ArtifactRecord = serde_json::from_slice(legacy).unwrap();
    let body = serde_json::to_value(&record).unwrap();
    storage
        .documents()
        .put_with(
            ARTIFACTS_COLLECTION,
            &record.id,
            body.clone(),
            Precondition::Absent,
            WriteOptions::new()
                .schema(SCHEMA_VERSION)
                .expires_ms(i64::MAX),
        )
        .await
        .unwrap();

    let error = store
        .import_legacy_record_json(&source_bytes)
        .await
        .unwrap_err();

    assert_eq!(
        error.downcast_ref::<StorageError>().unwrap().kind(),
        ErrorKind::Conflict
    );
    let existing = storage
        .documents()
        .get(ARTIFACTS_COLLECTION, &record.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(existing.doc, body);
    assert_eq!(existing.meta.schema, SCHEMA_VERSION);
    assert_eq!(existing.meta.expires_ms, Some(i64::MAX));
    assert_eq!(std::fs::read(source_path).unwrap(), source_bytes);
    std::fs::remove_dir_all(source_dir).unwrap();
}

#[tokio::test]
async fn legacy_import_rejects_identical_body_without_declared_schema() {
    let storage = storage("artifact-import-undeclared-schema-test");
    let store = DocumentArtifactStore::open(&storage).await.unwrap();
    let legacy = include_bytes!("../tests/fixtures/previous-release-1.json");
    let record: ArtifactRecord = serde_json::from_slice(legacy).unwrap();
    storage
        .documents()
        .put(
            ARTIFACTS_COLLECTION,
            &record.id,
            serde_json::to_value(&record).unwrap(),
            Precondition::Absent,
        )
        .await
        .unwrap();

    let error = store.import_legacy_record_json(legacy).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<StorageError>().unwrap().kind(),
        ErrorKind::Conflict
    );
    let existing = storage
        .documents()
        .get(ARTIFACTS_COLLECTION, &record.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(existing.doc, serde_json::to_value(record).unwrap());
    assert_eq!(existing.meta.schema, 0);
}

#[tokio::test]
async fn document_store_debug_output_omits_records_and_import_rejects_unsafe_ids() {
    let store = DocumentArtifactStore::open(&storage("artifact-debug-import-test"))
        .await
        .unwrap();
    let output = format!("{store:?}");
    assert!(output.contains("DocumentArtifactStore"));
    assert!(!output.contains("secret record content"));

    let unsafe_record = record("../outside", 1);
    let bytes = serde_json::to_vec(&unsafe_record).unwrap();
    assert!(store.import_legacy_record_json(&bytes).await.is_err());
}

#[tokio::test]
async fn list_skips_corrupt_records_while_get_reports_them() {
    let storage = storage("artifact-corrupt-record-test");
    let store = DocumentArtifactStore::open(&storage).await.unwrap();
    storage
        .documents()
        .put(
            ARTIFACTS_COLLECTION,
            "corrupt",
            serde_json::json!({"title": "not an artifact record"}),
            Precondition::Absent,
        )
        .await
        .unwrap();

    assert_eq!(store.list().await.unwrap(), Vec::<ArtifactRecord>::new());
    assert!(store.get("corrupt").await.is_err());
}

#[tokio::test]
async fn import_reports_backend_write_failure_and_unexplained_absence_conflict() {
    let backing = storage("artifact-import-failure-test");
    let faults = FaultPlan::new();
    let wrapped = with_fault_plan(&backing, faults.clone());
    let store = DocumentArtifactStore::open(&wrapped).await.unwrap();
    let legacy = include_bytes!("../tests/fixtures/previous-release-1.json");

    faults.fail_operation_nth("put_with", 1, ErrorKind::Unavailable);
    let backend_error = store.import_legacy_record_json(legacy).await.unwrap_err();
    assert_eq!(storage_error(&backend_error).kind(), ErrorKind::Unavailable);

    faults.fail_operation_nth("put_with", 1, ErrorKind::Conflict);
    let conflict = store.import_legacy_record_json(legacy).await.unwrap_err();
    assert_eq!(storage_error(&conflict).kind(), ErrorKind::Conflict);
}

#[tokio::test]
async fn migration_refuses_to_claim_completion_after_checkpoint_conflict() {
    let backing = storage("artifact-migration-conflict-test");
    backing
        .documents()
        .ensure_collection(&CollectionSpec::new(MIGRATIONS_COLLECTION))
        .await
        .unwrap();
    backing
        .documents()
        .put(
            MIGRATIONS_COLLECTION,
            MIGRATIONS_ID,
            serde_json::json!({"version": SCHEMA_VERSION}),
            Precondition::Absent,
        )
        .await
        .unwrap();

    let faults = FaultPlan::new();
    faults.fail_operation_nth("put_with", 1, ErrorKind::Conflict);
    let error = DocumentArtifactStore::open(&with_fault_plan(&backing, faults))
        .await
        .unwrap_err();
    assert_eq!(storage_error(&error).kind(), ErrorKind::Conflict);
}

#[tokio::test]
async fn records_remain_isolated_by_the_host_bound_scope() {
    let first = DocumentArtifactStore::open(&storage("artifact-scope-a"))
        .await
        .unwrap();
    let second = DocumentArtifactStore::open(&storage("artifact-scope-b"))
        .await
        .unwrap();

    first.upsert(&record("same-id", 1)).await.unwrap();
    assert!(second.get("same-id").await.unwrap().is_none());
}

#[tokio::test]
async fn document_store_rejects_invalid_ids() {
    let store = DocumentArtifactStore::open(&storage("artifact-id-validation"))
        .await
        .unwrap();
    for id in ["", ".", "..", "../x", "a/b", "a\\b", ".hidden"] {
        assert!(store.get(id).await.is_err(), "{id}");
        assert!(store.upsert(&record(id, 1)).await.is_err(), "{id}");
        assert!(store.delete(id).await.is_err(), "{id}");
    }
}

#[tokio::test]
async fn open_propagates_collection_schema_and_checkpoint_failures() {
    let backing = storage("artifact-open-failure-test");
    let ensure_error =
        DocumentArtifactStore::open(&with_failing_documents(&backing, FailurePoint::Ensure))
            .await
            .unwrap_err();
    assert_eq!(storage_error(&ensure_error).kind(), ErrorKind::Backend);

    let read_error =
        DocumentArtifactStore::open(&with_failing_documents(&backing, FailurePoint::Get))
            .await
            .unwrap_err();
    assert_eq!(storage_error(&read_error).kind(), ErrorKind::Backend);

    let metadata_error = DocumentArtifactStore::open(&with_failing_documents(
        &backing,
        FailurePoint::MetadataWrite,
    ))
    .await
    .unwrap_err();
    assert_eq!(
        storage_error(&metadata_error).kind(),
        ErrorKind::Unsupported(Capability::RecordMetadata)
    );
}

fn storage_error(error: &anyhow::Error) -> &StorageError {
    error.downcast_ref::<StorageError>().unwrap()
}

fn with_failing_documents(storage: &Storage, fail_at: FailurePoint) -> Storage {
    let faults = FaultPlan::new();
    let (operation, kind) = match fail_at {
        FailurePoint::Ensure => ("ensure_collection", ErrorKind::Backend),
        FailurePoint::Get => ("get", ErrorKind::Backend),
        FailurePoint::MetadataWrite => (
            "put_with",
            ErrorKind::Unsupported(Capability::RecordMetadata),
        ),
    };
    faults.fail_operation_nth(operation, 1, kind);
    with_fault_plan(storage, faults)
}

fn with_fault_plan(storage: &Storage, faults: FaultPlan) -> Storage {
    let documents = Arc::new(FailingDocuments {
        failing: Failing::with_plan(Arc::clone(storage.documents()), faults),
    });
    Storage::new(
        documents,
        Arc::clone(storage.streams()),
        Arc::clone(storage.kv()),
        Arc::clone(storage.blobs()),
        Arc::clone(storage.queues()),
        Arc::clone(storage.secrets()),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailurePoint {
    Ensure,
    Get,
    MetadataWrite,
}

#[derive(Debug)]
struct FailingDocuments {
    failing: Failing<dyn DocumentStore>,
}

#[async_trait]
impl DocumentStore for FailingDocuments {
    fn capabilities(&self) -> Capabilities {
        self.failing.inner.capabilities()
    }

    async fn ensure_collection(
        &self,
        spec: &CollectionSpec,
    ) -> tinystoragedrivers_types::Result<()> {
        self.failing
            .call(
                "ensure_collection",
                self.failing.inner.ensure_collection(spec),
            )
            .await
    }

    async fn get(
        &self,
        collection: &str,
        id: &str,
    ) -> tinystoragedrivers_types::Result<Option<Versioned<Value>>> {
        self.failing
            .call("get", self.failing.inner.get(collection, id))
            .await
    }

    async fn put(
        &self,
        collection: &str,
        id: &str,
        doc: Value,
        precondition: Precondition,
    ) -> tinystoragedrivers_types::Result<Version> {
        self.failing
            .call(
                "put",
                self.failing.inner.put(collection, id, doc, precondition),
            )
            .await
    }

    async fn put_with(
        &self,
        collection: &str,
        id: &str,
        doc: Value,
        precondition: Precondition,
        options: WriteOptions,
    ) -> tinystoragedrivers_types::Result<Version> {
        self.failing
            .call(
                "put_with",
                self.failing
                    .inner
                    .put_with(collection, id, doc, precondition, options),
            )
            .await
    }

    async fn delete(
        &self,
        collection: &str,
        id: &str,
        precondition: Precondition,
    ) -> tinystoragedrivers_types::Result<bool> {
        self.failing
            .call(
                "delete",
                self.failing.inner.delete(collection, id, precondition),
            )
            .await
    }

    async fn query(
        &self,
        collection: &str,
        query: &Query,
    ) -> tinystoragedrivers_types::Result<Page<Versioned<Value>>> {
        self.failing
            .call("query", self.failing.inner.query(collection, query))
            .await
    }

    async fn count(
        &self,
        collection: &str,
        filter: &Filter,
    ) -> tinystoragedrivers_types::Result<u64> {
        self.failing
            .call("count", self.failing.inner.count(collection, filter))
            .await
    }

    async fn delete_where(
        &self,
        collection: &str,
        filter: &Filter,
    ) -> tinystoragedrivers_types::Result<u64> {
        self.failing
            .call(
                "delete_where",
                self.failing.inner.delete_where(collection, filter),
            )
            .await
    }

    async fn claim(
        &self,
        collection: &str,
        filter: &Filter,
        sort: &[Sort],
        patch: &Value,
    ) -> tinystoragedrivers_types::Result<Option<Versioned<Value>>> {
        self.failing
            .call(
                "claim",
                self.failing.inner.claim(collection, filter, sort, patch),
            )
            .await
    }

    async fn drop_collection(&self, collection: &str) -> tinystoragedrivers_types::Result<()> {
        self.failing
            .call(
                "drop_collection",
                self.failing.inner.drop_collection(collection),
            )
            .await
    }

    async fn collections(&self, prefix: &str) -> tinystoragedrivers_types::Result<Vec<String>> {
        self.failing
            .call("collections", self.failing.inner.collections(prefix))
            .await
    }

    async fn search(
        &self,
        collection: &str,
        text: &str,
        limit: usize,
    ) -> tinystoragedrivers_types::Result<Vec<SearchHit>> {
        self.failing
            .call("search", self.failing.inner.search(collection, text, limit))
            .await
    }
}
