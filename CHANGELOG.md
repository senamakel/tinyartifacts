# Changelog

## Unreleased

- Add `Content::Bytes`, and `execute_with_content` on `CreateArtifactTool` and
  `UpdateArtifactTool`, so a host that already holds a file's bytes (read out
  of a sandbox, say) can file them as an `image` or `file` artifact without
  writing them to disk first. This adds an enum variant: exhaustive matches on
  `Content` need an arm for it.
- Back artifact records with the host-bound document port and apply
  artifact-owned migrations on open.
- Adapt file bytes through the host-bound blob port, and restore `FsFileStore`
  for callers that explicitly manage a local filesystem root.
- Accept serialized legacy record bytes for host-managed imports.

## 0.2.0

- Add the `FileStore` port with filesystem and blob-store adapters so file
  versions can be shared across host processes.
- Store `FileRef` values as opaque keys with size and media type, and preserve
  deserialization of legacy path-based references for import.
- Configure file storage with `Artifacts::with_file_store`.
- This changes the public `FileRef` fields and file-storage setup API. Hosts
  must provide a file store for file artifacts.
