# Previous filesystem writer fixture

`previous-release-1.json` was written by the public `FsArtifactStore::upsert`
implementation from tiny-artifacts commit
`704e9c3062512fad1d309280fb8782c8ce3c9034`. That source's lockfile resolves
`tinystoragedrivers-core` to `be94f7568fe7650b0729be55128f48270414b9a7`.
The fixture is a real serialized `ArtifactRecord`; the current document owner
imports it through `DocumentArtifactStore::import_legacy_record_json` after
opening and applying its own schema migration.

Regenerate it from this repository with the checked-in public-API generator:

```sh
mkdir -p target/legacy-artifact-writer
git archive 704e9c3062512fad1d309280fb8782c8ce3c9034 \
  | tar -x -C target/legacy-artifact-writer
mkdir -p target/legacy-artifact-writer/crates/tiny-artifacts/examples
cp crates/tiny-artifacts/tests/fixtures/generate_legacy_artifact.rs \
  target/legacy-artifact-writer/crates/tiny-artifacts/examples/
cargo run --locked \
  --manifest-path target/legacy-artifact-writer/crates/tiny-artifacts/Cargo.toml \
  --example generate_legacy_artifact -- \
  crates/tiny-artifacts/tests/fixtures
```

The generated file's SHA-256 is
`b388ce1f98e1fad9582b3797b526fe97d7ef6bc824f7ba102cd367f4ae0c9cb8`.
