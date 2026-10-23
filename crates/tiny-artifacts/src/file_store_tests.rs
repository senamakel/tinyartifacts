use super::*;

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!("tiny-artifacts-files-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn a_failed_staging_write_removes_its_partial_file() {
    use cap_std::ambient_authority;
    use cap_std::fs::Dir;
    use std::io::Write;

    let root = temp_dir();
    let directory = Dir::open_ambient_dir(&root, ambient_authority()).unwrap();
    let result = write_staging_file(&directory, ".partial.tmp", |file| {
        file.write_all(b"partial")?;
        Err(std::io::Error::other("injected write failure"))
    });

    assert!(result.is_err());
    assert!(!root.join(".partial.tmp").exists());
}

use tinystoragedrivers_api::Storage;
use tinystoragedrivers_testkit::memory_storage;
use tinystoragedrivers_types::Scope;

fn storage() -> Storage {
    memory_storage(&Scope::new("artifact-files-test").unwrap())
}

#[tokio::test]
async fn blob_adapter_reads_metadata_and_deletes_only_the_requested_prefix() {
    let storage = storage();
    let first = BlobFileStore::new(&storage);
    let second = BlobFileStore::new(&storage);
    assert!(format!("{first:?}").contains("BlobFileStore"));
    let metadata = first
        .put("artifacts/files/a/v1.png", b"png bytes", None)
        .await
        .unwrap();

    assert_eq!(metadata.size_bytes, 9);
    assert_eq!(metadata.media_type.as_deref(), Some("image/png"));
    assert_eq!(
        second.read("artifacts/files/a/v1.png").await.unwrap(),
        b"png bytes"
    );
    assert_eq!(
        second.metadata("artifacts/files/a/v1.png").await.unwrap(),
        Some(metadata)
    );
    second
        .put("artifacts/files/ab/v1.png", b"sibling", None)
        .await
        .unwrap();
    second.delete_prefix("artifacts/files/a/").await.unwrap();
    assert_eq!(
        first.metadata("artifacts/files/a/v1.png").await.unwrap(),
        None
    );
    assert_eq!(
        first.read("artifacts/files/ab/v1.png").await.unwrap(),
        b"sibling"
    );
}

#[tokio::test]
async fn overwrite_existing_preserves_the_file_and_replaces_its_bytes() {
    let root = temp_dir();
    let path = root.join("existing.txt");
    std::fs::write(&path, b"before").unwrap();
    let store = FsFileStore::new(&root);

    #[cfg(unix)]
    let before = {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        (metadata.dev(), metadata.ino(), metadata.mode())
    };

    let metadata = store
        .overwrite_existing("existing.txt", b"after", None)
        .await
        .unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), b"after");
    assert_eq!(metadata.size_bytes, 5);
    assert_eq!(metadata.media_type.as_deref(), Some("text/plain"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!((after.dev(), after.ino(), after.mode()), before);
    }
}

#[tokio::test]
async fn overwrite_existing_refuses_missing_targets_without_creating_paths() {
    let root = temp_dir();
    let store = FsFileStore::new(&root);

    assert!(store
        .overwrite_existing("missing.txt", b"bytes", None)
        .await
        .is_err());
    assert!(store
        .overwrite_existing("missing/child.txt", b"bytes", None)
        .await
        .is_err());
    assert!(!root.join("missing").exists());
    assert!(!root.join("missing.txt").exists());

    let missing_root = root.join("missing-root");
    assert!(FsFileStore::new(&missing_root)
        .overwrite_existing("file.txt", b"bytes", None)
        .await
        .is_err());
    assert!(!missing_root.exists());
}

#[tokio::test]
async fn overwrite_existing_rejects_non_files_and_symlinks_outside_the_root() {
    let root = temp_dir();
    let store = FsFileStore::new(&root);
    std::fs::create_dir(root.join("directory")).unwrap();
    assert!(store
        .overwrite_existing("directory", b"bytes", None)
        .await
        .is_err());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = temp_dir().join("outside.txt");
        std::fs::write(&outside, b"keep").unwrap();
        symlink(&outside, root.join("outside-link.txt")).unwrap();

        assert!(store
            .overwrite_existing("outside-link.txt", b"replace", None)
            .await
            .is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"keep");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn overwrite_existing_follows_symlinks_that_resolve_inside_the_root() {
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::MetadataExt;
    let root = temp_dir();
    let path = root.join("target.txt");
    std::fs::write(&path, b"before").unwrap();
    symlink(&path, root.join("link.txt")).unwrap();
    let inode = std::fs::metadata(&path).unwrap().ino();

    FsFileStore::new(&root)
        .overwrite_existing("link.txt", b"after", None)
        .await
        .unwrap();

    let metadata = std::fs::metadata(&path).unwrap();
    assert_eq!(metadata.ino(), inode);
    assert_eq!(std::fs::read(path).unwrap(), b"after");
}

#[cfg(unix)]
#[tokio::test]
async fn overwrite_existing_does_not_follow_a_parent_replaced_with_an_external_symlink() {
    use std::os::unix::fs::symlink;

    let root = temp_dir();
    let parent = root.join("parent");
    std::fs::create_dir(&parent).unwrap();
    std::fs::write(parent.join("target.txt"), b"inside").unwrap();
    let outside = temp_dir();
    std::fs::write(outside.join("target.txt"), b"outside").unwrap();
    let store = FsFileStore::new(&root);
    let moved_parent = root.join("parent-moved");
    let hook_parent = parent.clone();
    let hook_outside = outside.clone();

    let result = store
        .overwrite_existing_with_hook("parent/target.txt", b"changed", None, move || {
            std::fs::rename(&hook_parent, moved_parent).unwrap();
            symlink(hook_outside, hook_parent).unwrap();
        })
        .await;

    assert!(result.is_err(), "the parent swap must invalidate the open");
    assert_eq!(
        std::fs::read(outside.join("target.txt")).unwrap(),
        b"outside"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn read_does_not_follow_a_parent_replaced_with_an_external_symlink() {
    use std::os::unix::fs::symlink;

    let root = temp_dir();
    let parent = root.join("parent");
    std::fs::create_dir(&parent).unwrap();
    std::fs::write(parent.join("target.txt"), b"inside").unwrap();
    let outside = temp_dir();
    std::fs::write(outside.join("target.txt"), b"outside").unwrap();
    let store = FsFileStore::new(&root);
    let moved_parent = root.join("parent-moved");
    let hook_parent = parent.clone();
    let hook_outside = outside.clone();

    let result = store
        .read_with_hook("parent/target.txt", move || {
            std::fs::rename(&hook_parent, moved_parent).unwrap();
            symlink(hook_outside, hook_parent).unwrap();
        })
        .await;

    assert!(result.is_err(), "the parent swap must invalidate the read");
}

#[cfg(unix)]
#[tokio::test]
async fn overwrite_existing_obeys_filesystem_write_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_dir();
    let path = root.join("read-only.txt");
    std::fs::write(&path, b"preserve").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();

    let write_is_allowed = std::fs::OpenOptions::new().write(true).open(&path).is_ok();
    let result = FsFileStore::new(&root)
        .overwrite_existing("read-only.txt", b"replace", None)
        .await;

    if write_is_allowed {
        result.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"replace");
    } else {
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve");
    }
}

#[tokio::test]
async fn filesystem_store_round_trips_and_deletes_only_the_requested_prefix() {
    let root = temp_dir().join("not-created-yet");
    let store = FsFileStore::new(&root);
    assert_eq!(store.root(), root);

    let metadata = store
        .put(
            "artifacts/files/a/v1.txt",
            b"first version",
            Some("text/custom"),
        )
        .await
        .unwrap();
    assert_eq!(metadata.size_bytes, 13);
    assert_eq!(metadata.media_type.as_deref(), Some("text/custom"));
    assert_eq!(
        store.read("artifacts/files/a/v1.txt").await.unwrap(),
        b"first version"
    );
    assert_eq!(
        store.metadata("artifacts/files/a/v1.txt").await.unwrap(),
        Some(FileMetadata {
            size_bytes: 13,
            media_type: Some("text/plain".to_owned()),
        })
    );

    store
        .put("artifacts/files/ab/v1.txt", b"sibling", None)
        .await
        .unwrap();
    store.delete_prefix("artifacts/files/a/").await.unwrap();

    assert!(store.read("artifacts/files/a/v1.txt").await.is_err());
    assert_eq!(
        store.metadata("artifacts/files/a/v1.txt").await.unwrap(),
        None
    );
    assert_eq!(
        store.read("artifacts/files/ab/v1.txt").await.unwrap(),
        b"sibling"
    );
    store
        .delete_prefix("artifacts/files/missing/")
        .await
        .unwrap();
}

#[tokio::test]
async fn filesystem_store_rejects_invalid_paths_and_escaping_symlinks() {
    let root = temp_dir();
    let store = FsFileStore::new(&root);
    for key in [
        "",
        "../outside",
        "nested//file",
        "nested/./file",
        "/absolute",
        "bad\\key",
    ] {
        assert!(store.put(key, b"bad", None).await.is_err(), "{key}");
        assert!(store.read(key).await.is_err(), "{key}");
        assert!(store.metadata(key).await.is_err(), "{key}");
    }
    for prefix in ["", "nested", "nested//", "../"] {
        assert!(store.delete_prefix(prefix).await.is_err(), "{prefix}");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = temp_dir();
        std::fs::write(outside.join("kept.txt"), b"outside").unwrap();
        symlink(&outside, root.join("linked-dir")).unwrap();
        symlink(outside.join("kept.txt"), root.join("linked-file.txt")).unwrap();
        assert!(store
            .put("linked-dir/kept.txt", b"replace", None)
            .await
            .is_err());
        assert!(store.read("linked-dir/kept.txt").await.is_err());
        assert!(store.metadata("linked-dir/kept.txt").await.is_err());
        assert!(store.delete_prefix("linked-dir/").await.is_err());
        assert!(store.read("linked-file.txt").await.is_err());
        assert!(store.metadata("linked-file.txt").await.is_err());
        assert_eq!(std::fs::read(outside.join("kept.txt")).unwrap(), b"outside");
    }
}

#[tokio::test]
async fn blob_adapter_rejects_invalid_keys_and_prefixes_without_touching_siblings() {
    let storage = storage();
    let store = BlobFileStore::new(&storage);
    assert!(store.read("artifacts/files/missing.txt").await.is_err());
    assert_eq!(
        store.metadata("artifacts/files/missing.txt").await.unwrap(),
        None
    );
    for key in [
        "../outside",
        "artifacts/files/a//v1",
        "artifacts/files/a/./v1",
        "/absolute",
        "bad\\key",
    ] {
        assert!(store.put(key, b"bad", None).await.is_err(), "{key}");
        assert!(store.read(key).await.is_err(), "{key}");
        assert!(store.metadata(key).await.is_err(), "{key}");
    }
    store
        .put("artifacts/files/a/v1.txt", b"a", None)
        .await
        .unwrap();
    store
        .put("artifacts/files/ab/v1.txt", b"ab", None)
        .await
        .unwrap();

    for prefix in [
        "",
        "artifacts/files/a",
        "artifacts/files/a//",
        "artifacts/files/a/./",
        "../",
    ] {
        assert!(store.delete_prefix(prefix).await.is_err(), "{prefix}");
        assert_eq!(store.read("artifacts/files/a/v1.txt").await.unwrap(), b"a");
        assert_eq!(
            store.read("artifacts/files/ab/v1.txt").await.unwrap(),
            b"ab"
        );
    }
    store
        .delete_prefix("artifacts/files/missing/")
        .await
        .unwrap();
}
