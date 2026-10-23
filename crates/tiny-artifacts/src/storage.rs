//! Collections owned by the artifact library.

use tinystoragedrivers_types::CollectionSpec;

/// Documents containing versioned artifact records.
pub const ARTIFACTS_COLLECTION: &str = "artifacts.records";

/// The per-owner migration checkpoint.
pub(crate) const MIGRATIONS_COLLECTION: &str = "artifacts.migrations";
pub(crate) const MIGRATIONS_ID: &str = "schema";
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// Return the document collections the host should ensure in its bound scope.
///
/// Migration behavior remains owned by [`crate::DocumentArtifactStore::open`].
#[must_use]
pub fn collections() -> Vec<CollectionSpec> {
    vec![
        CollectionSpec::new(ARTIFACTS_COLLECTION),
        CollectionSpec::new(MIGRATIONS_COLLECTION),
    ]
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
