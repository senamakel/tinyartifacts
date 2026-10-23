use super::*;

#[test]
fn exposes_only_artifact_owned_document_collections() {
    let names: Vec<_> = collections()
        .into_iter()
        .map(|spec| spec.name.clone())
        .collect();

    assert_eq!(names, ["artifacts.records", "artifacts.migrations"]);
}
