# tiny-artifacts

Versioned artifacts for AI agents: the documents, pages, images and files an
agent makes and then revises — and the edits a human makes in between.

An artifact is a record with an ordered list of versions. Nothing is ever
overwritten: a revision appends a version, written either by an agent or by a
human (an *operator*). That history is what makes the most useful quality
signal answerable at any time: **what did the human change before using the
agent's work?** (`ArtifactRecord::human_edit_diff`).

The crate owns what an artifact is and how it changes. A host — an app, a
server — owns where records live, how they are shown, and how they are shared,
through four small ports.

## The model

| Type | What it is |
| --- | --- |
| `ArtifactRecord` | id, title, kind, versions (oldest first), status, timestamps |
| `ArtifactVersion` | 1-based number, body, author (`agent` / `operator`) and author id, an optional note, an optional `FileRef`, and the link it was shared under |
| `ArtifactKind` | `text`, `markdown`, `html` hold their content; `image`, `file` hold a reference to a file |
| `ArtifactStatus` | `pending` (being produced), `ready`, `failed` (with an error) |
| `FileRef` | a stored file: opaque key, size, media type |

Every field added since the first release defaults when absent, so records
written by earlier versions keep loading.

## Using it

All creation and change goes through one service, `Artifacts`:

```rust
use std::sync::Arc;
use tinystoragedrivers_api::Storage;
use tiny_artifacts::{
    Artifacts, ArtifactKind, Author, BlobFileStore, Content, DocumentArtifactStore, FileRoots,
};

async fn demo(storage: &Storage) -> anyhow::Result<()> {
let store = DocumentArtifactStore::open(storage).await?;
let artifacts = Artifacts::new(Arc::new(store))
    .with_file_roots(FileRoots::new(["/path/to/agent/workspace".into()]))
    .with_file_store(Arc::new(BlobFileStore::new(storage)));

// A text artifact, then a human edit of it.
let saved = artifacts
    .create(ArtifactKind::Markdown, "Launch notes", Content::Text("# v1".into()), &Author::agent("writer"), None)
    .await?;
artifacts
    .revise(&saved.record.id, Content::Text("# v1, edited".into()), &Author::operator("sam"), None)
    .await?;

// A file artifact: a path inside the allowed folders.
artifacts
    .create(ArtifactKind::File, "Q3 deck", Content::File("decks/q3.pptx".into()), &Author::agent("writer"), None)
    .await?;
Ok(()) }
```

The service enforces the rules, whoever is calling:

- **Content fits the kind.** Text kinds take text; file kinds take a path,
  which must name an existing regular file inside the host's `FileRoots`
  (canonicalized, so `..` and symlinks cannot reach outside).
- **Revisions append.** A text revision identical to the newest version
  appends nothing.
- **Slow producers** open an artifact with `begin` (status `pending`, no
  version yet), then `revise` to complete it or `fail` it with a reason.
- **File versions keep their bytes** through `FileStore`: each version is
  written under `artifacts/files/<id>/v<N>.<ext>` and its record carries the
  storage key, so a producer that overwrites its input file does not change
  earlier versions. Deleting an artifact removes every object under its key
  prefix.
- **Sharing** (`publish`) checks the stored object through `FileStore`, records
  the link on the version it shared and reuses a live link instead of sharing
  again. A host publisher can read the same object by its `FileRef` key.

## Ports a host implements

| Port | Role |
| --- | --- |
| `ArtifactStore` | Where records are kept. `DocumentArtifactStore` adapts the host-bound document port and applies the artifact-owned schema migration on open. |
| `ArtifactObserver` | Told after every write (`pending`, `ready`, `failed`, `deleted`) — show a card, copy the file where the user can open it. If it fails, the write stands and the caller gets a notice. |
| `ArtifactPublisher` | How a version is shared (e.g. an upload and a link). Optional. |
| `FileStore` | Durable file bytes behind `FileRef` keys. Use `BlobFileStore` to adapt a host's `BlobStore`, or `FsFileStore` to keep files beneath a local root. |
| `FileRoots` | Allowed input folders. Paths are canonicalized and checked before a file store reads them; `with_guard` can refuse paths inside them (credential stores, its own data). |

Hosts provide an already-bound `tinystoragedrivers_api::Storage` bundle. The
library exposes `collections()` for host setup, while `DocumentArtifactStore::open`
ensures its own collections and runs its migration before returning. A host
that imports old filesystem records can pass each record's serialized bytes to
`import_legacy_record_json`; locating source files and copying legacy file
bytes remain host responsibilities. Legacy `FileRef.path` values still
deserialize so host import code can resolve them with its existing policy.

`FsFileStore` creates its root and parent directories as needed and rejects
keys that escape that root, including symlink targets outside it. Its ordinary
`put` writes a staging file before replacing the destination. The separate
`overwrite_existing` method updates an existing file in place, preserving its
inode and permissions; it is not atomic and can leave partial bytes after an
I/O failure. Use `BlobFileStore` when file bytes must live in the host's
bound storage backend.

## Agent tools

`registered_tools(artifacts, author)` returns the default set, with versions
attributed to `author` (a tool run carries no agent identity, so a host that
builds tools per agent passes each its own):

| Tool | Does |
| --- | --- |
| `create_artifact` | New artifact: `body` for text kinds, `path` for `image` / `file` |
| `update_artifact` | Append a version |
| `read_artifact` | One record with its full history |
| `list_artifacts` | Every record, most recently updated first |

Two more are opt-in, because a host must decide to offer them:
`DeleteArtifactTool` (`delete_artifact`, destructive) and `ShareArtifactTool`
(`share_artifact`, only useful with a publisher).

The tools follow the `tinytools` convention: a malformed call (a bad kind, a
missing body) is an error; a refusal about the world (no such artifact, a
missing file) is a tool result the agent can act on.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
```

The integration tests in `crates/tiny-artifacts/tests/` cover the tools and
service (file references, lifecycle, observer, publishing); storage adapter
tests use the real in-memory storage ports from the development-only testkit.

## Releasing

Run the **Release** workflow on `main` from the Actions tab with a `patch`,
`minor`, or `major` bump. It re-runs every check, sets the new version in the
root `Cargo.toml` and `Cargo.lock`, commits and tags `vX.Y.Z` on `main`, and
creates a GitHub release with generated notes. Do not hand-edit the version. A
change to a public signature is a `minor` bump while the crate is `0.x`.

Hosts consume the crate by git tag or revision (OpenHuman vendors it as a
submodule), so a release ships no binaries.

## Documentation

- [`AGENTS.md`](AGENTS.md) — repository guidelines for humans and agents
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — how to propose a change
- [`SECURITY.md`](SECURITY.md) — how to report a vulnerability

## License

GPL-3.0-only. See [LICENSE](LICENSE).
