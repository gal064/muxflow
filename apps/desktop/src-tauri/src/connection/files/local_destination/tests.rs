use super::*;
use std::{fs, io::Write, os::unix::fs::PermissionsExt};

fn write_crafted_journal(
    root: &Path,
    id: Uuid,
    final_name: &[u8],
    partial_name: &[u8],
    mode: u32,
) -> PathBuf {
    let path = root.join(expected_journal_name(id).to_str().unwrap());
    let value = LocalPublishJournal {
        schema_version: 1,
        state: LocalJournalState::Prepared,
        final_name: final_name.to_vec(),
        partial_name: partial_name.to_vec(),
        new_identity: FileIdentity {
            device: u64::MAX,
            inode: u64::MAX,
        },
        original_identity: None,
    };
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

#[test]
fn recovery_ignores_untrusted_journals_and_never_deletes_outside_leaf() {
    let root = std::env::temp_dir().join(format!("ade-dl-hostile-journal-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o1777)).unwrap();
    let victim = root
        .parent()
        .unwrap()
        .join(format!("victim-{}", Uuid::new_v4()));
    fs::write(&victim, b"untouched victim").unwrap();
    let victim_mode = fs::metadata(&victim).unwrap().permissions().mode();

    let traversal = Uuid::new_v4();
    write_crafted_journal(
        &root,
        traversal,
        b"../victim",
        expected_partial_name(traversal).as_bytes(),
        0o600,
    );
    let slash = Uuid::new_v4();
    write_crafted_journal(
        &root,
        slash,
        b"nested/victim",
        expected_partial_name(slash).as_bytes(),
        0o600,
    );
    let wrong_partial = Uuid::new_v4();
    write_crafted_journal(&root, wrong_partial, b"safe.bin", b"../victim", 0o600);
    let public_mode = Uuid::new_v4();
    let public_path = write_crafted_journal(
        &root,
        public_mode,
        b"safe.bin",
        expected_partial_name(public_mode).as_bytes(),
        0o644,
    );
    let symlink_id = Uuid::new_v4();
    let symlink_path = root.join(expected_journal_name(symlink_id).to_str().unwrap());
    std::os::unix::fs::symlink(&victim, &symlink_path).unwrap();
    let foreign_uid_path = if unsafe { libc::geteuid() } == 0 {
        let id = Uuid::new_v4();
        let path = write_crafted_journal(
            &root,
            id,
            b"safe.bin",
            expected_partial_name(id).as_bytes(),
            0o600,
        );
        let path_c = c_string(path.as_os_str(), "foreign-UID journal").unwrap();
        assert_eq!(unsafe { libc::chown(path_c.as_ptr(), 65534, 65534) }, 0);
        Some(path)
    } else {
        None
    };

    let directory_name = c_string(root.as_os_str(), "hostile journal directory").unwrap();
    let fd = unsafe {
        libc::open(
            directory_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    assert!(fd >= 0);
    let directory = unsafe { File::from_raw_fd(fd) };
    recover_local_transactions(&directory).unwrap();

    assert_eq!(fs::read(&victim).unwrap(), b"untouched victim");
    assert_eq!(
        fs::metadata(&victim).unwrap().permissions().mode(),
        victim_mode
    );
    assert_eq!(
        fs::metadata(&public_path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert!(symlink_path.is_symlink());
    assert_eq!(fs::read_link(&symlink_path).unwrap(), victim);
    if let Some(path) = foreign_uid_path {
        assert!(path.exists());
        assert_eq!(fs::metadata(path).unwrap().uid(), 65534);
    }
    assert!(
        root.join(expected_journal_name(traversal).to_str().unwrap())
            .exists()
    );
    assert!(
        root.join(expected_journal_name(slash).to_str().unwrap())
            .exists()
    );
    assert!(
        root.join(expected_journal_name(wrong_partial).to_str().unwrap())
            .exists()
    );

    fs::remove_dir_all(root).unwrap();
    fs::remove_file(victim).unwrap();
}

#[test]
fn rename_suffix_respects_name_max_and_utf8_boundaries() {
    let requested = format!("{}.tar.gz", "界".repeat(81));
    let requested = OsStr::new(&requested);
    let candidate = renamed_name_bytes(requested, 1, 255).unwrap();
    assert!(candidate.len() <= 255);
    let candidate = std::str::from_utf8(&candidate).unwrap();
    assert!(candidate.ends_with(" (1).gz"));
}

#[test]
fn parent_namespace_swap_fails_closed_and_cleans_original_directory() {
    let root = std::env::temp_dir().join(format!("ade-dl-parent-swap-{}", Uuid::new_v4()));
    let parent = root.join("destination");
    fs::create_dir_all(&parent).unwrap();
    let prepared =
        PreparedDestination::open(&parent.join("result.bin"), DownloadCollisionPolicy::Fail)
            .unwrap();
    prepared
        .create_partial()
        .unwrap()
        .write_all(b"owned")
        .unwrap();
    let original = root.join("original");
    fs::rename(&parent, &original).unwrap();
    let attacker = root.join("attacker");
    fs::create_dir(&attacker).unwrap();
    std::os::unix::fs::symlink(&attacker, &parent).unwrap();

    assert!(
        prepared
            .publish()
            .unwrap_err()
            .to_string()
            .contains("parent changed")
    );
    prepared.cleanup_partial().unwrap();
    assert!(
        !original
            .join(prepared.partial_name.to_str().unwrap())
            .exists()
    );
    assert!(!attacker.join("result.bin").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn overwrite_substitution_is_rejected_by_inode() {
    let root = std::env::temp_dir().join(format!("ade-dl-overwrite-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("result.bin"), b"confirmed").unwrap();
    let prepared = PreparedDestination::open(
        &root.join("result.bin"),
        DownloadCollisionPolicy::OverwriteConfirmed,
    )
    .unwrap();
    prepared
        .create_partial()
        .unwrap()
        .write_all(b"download")
        .unwrap();
    fs::rename(root.join("result.bin"), root.join("old.bin")).unwrap();
    fs::write(root.join("result.bin"), b"substitute").unwrap();

    assert!(
        prepared
            .publish()
            .unwrap_err()
            .to_string()
            .contains("substituted")
    );
    assert_eq!(fs::read(root.join("result.bin")).unwrap(), b"substitute");
    prepared.cleanup_partial().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn post_rename_fsync_fault_is_not_published_and_restores_original() {
    for overwrite in [false, true] {
        let root = std::env::temp_dir().join(format!("ade-dl-rollback-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        if overwrite {
            fs::write(root.join("result.bin"), b"original").unwrap();
        }
        let policy = if overwrite {
            DownloadCollisionPolicy::OverwriteConfirmed
        } else {
            DownloadCollisionPolicy::Fail
        };
        let prepared = PreparedDestination::open(&root.join("result.bin"), policy).unwrap();
        let mut partial = prepared.create_partial().unwrap();
        partial.write_all(b"replacement").unwrap();
        partial.sync_all().unwrap();
        drop(partial);
        let error = prepared
            .publish_with_fault(Some(LocalPublishFault::DirectoryFsync))
            .unwrap_err()
            .to_string();
        assert!(error.contains("not_published"), "{error}");
        if overwrite {
            assert_eq!(fs::read(root.join("result.bin")).unwrap(), b"original");
        } else {
            assert!(!root.join("result.bin").exists());
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn cleanup_fault_publishes_replacement_and_retains_original_backup() {
    let root = std::env::temp_dir().join(format!("ade-dl-cleanup-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("result.bin"), b"original").unwrap();
    let prepared = PreparedDestination::open(
        &root.join("result.bin"),
        DownloadCollisionPolicy::OverwriteConfirmed,
    )
    .unwrap();
    let mut partial = prepared.create_partial().unwrap();
    partial.write_all(b"replacement").unwrap();
    partial.sync_all().unwrap();
    drop(partial);
    let result = prepared
        .publish_with_fault(Some(LocalPublishFault::Cleanup))
        .unwrap();
    assert!(
        result
            .cleanup_error
            .as_deref()
            .unwrap()
            .contains("retained")
    );
    assert_eq!(fs::read(root.join("result.bin")).unwrap(), b"replacement");
    assert_eq!(
        fs::read(root.join(prepared.partial_name.to_str().unwrap())).unwrap(),
        b"original"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_substitution_is_unknown_and_retains_original_and_replacement() {
    let root = std::env::temp_dir().join(format!("ade-dl-substitute-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("result.bin"), b"original").unwrap();
    let prepared = PreparedDestination::open(
        &root.join("result.bin"),
        DownloadCollisionPolicy::OverwriteConfirmed,
    )
    .unwrap();
    let mut partial = prepared.create_partial().unwrap();
    partial.write_all(b"replacement").unwrap();
    partial.sync_all().unwrap();
    drop(partial);
    let error = prepared
        .publish_with_fault(Some(LocalPublishFault::RollbackSubstitution))
        .unwrap_err();
    assert_eq!(error.outcome, PublicationOutcome::Unknown);
    assert_eq!(
        fs::read(root.join(prepared.partial_name.to_str().unwrap())).unwrap(),
        b"original"
    );
    let retained = fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read(entry.path()).ok())
        .collect::<Vec<_>>();
    assert!(retained.iter().any(|bytes| bytes == b"replacement"));
    assert!(retained.iter().any(|bytes| bytes == b"foreign-substitute"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_unlink_failure_restores_original_and_retains_verified_replacement() {
    let root = std::env::temp_dir().join(format!("ade-dl-rollback-cleanup-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("result.bin"), b"original").unwrap();
    let prepared = PreparedDestination::open(
        &root.join("result.bin"),
        DownloadCollisionPolicy::OverwriteConfirmed,
    )
    .unwrap();
    let mut partial = prepared.create_partial().unwrap();
    partial.write_all(b"replacement").unwrap();
    partial.sync_all().unwrap();
    drop(partial);
    let error = prepared
        .publish_with_fault(Some(LocalPublishFault::RollbackCleanup))
        .unwrap_err();
    assert_eq!(error.outcome, PublicationOutcome::NotPublished);
    assert!(error.message.contains("quarantine cleanup failure"));
    assert_eq!(fs::read(root.join("result.bin")).unwrap(), b"original");
    let retained = fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read(entry.path()).ok())
        .any(|bytes| bytes == b"replacement");
    assert!(retained);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn durable_download_journal_recovers_prepared_and_published_transactions() {
    for overwrite in [false, true] {
        let root = std::env::temp_dir().join(format!("ade-dl-journal-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        if overwrite {
            fs::write(root.join("result.bin"), b"original").unwrap();
        }
        let policy = if overwrite {
            DownloadCollisionPolicy::OverwriteConfirmed
        } else {
            DownloadCollisionPolicy::Fail
        };
        let prepared = PreparedDestination::open(&root.join("result.bin"), policy).unwrap();
        let mut partial = prepared.create_partial().unwrap();
        partial.write_all(b"replacement").unwrap();
        partial.sync_all().unwrap();
        let metadata = partial.metadata().unwrap();
        let identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        let journal = prepared.create_transaction_journal(identity).unwrap();
        if overwrite {
            exchange_at(
                &prepared.directory,
                &prepared.partial_name,
                &prepared.final_name,
            )
            .unwrap();
            prepared.directory.sync_all().unwrap();
        }
        drop(journal);
        drop(partial);

        recover_local_transactions(&prepared.directory).unwrap();
        if overwrite {
            assert_eq!(fs::read(root.join("result.bin")).unwrap(), b"replacement");
            assert!(!root.join(prepared.partial_name.to_str().unwrap()).exists());
        } else {
            assert!(!root.join("result.bin").exists());
            assert!(!root.join(prepared.partial_name.to_str().unwrap()).exists());
        }
        assert!(
            !root
                .join(prepared.transaction_name.to_str().unwrap())
                .exists()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
