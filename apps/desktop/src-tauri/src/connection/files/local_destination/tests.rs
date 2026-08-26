use super::*;
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Barrier},
};

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
fn destination_diagnostic_classifies_without_recording_the_path() {
    let root = std::env::temp_dir().join(format!("ade-dl-diagnostic-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let destination = root.join("private-report-name.pdf");
    let directory = File::open(&root).unwrap();
    let diagnostic = destination_parent_diagnostic(
        &destination,
        None,
        ParentOpenDiagnostic {
            attempts: 1,
            first_errno: None,
            final_errno: None,
        },
        Some(&directory),
    );
    let serialized = serde_json::to_string(&diagnostic).unwrap();

    assert_eq!(diagnostic["parentClass"], "temporaryDirectory");
    assert_eq!(diagnostic["parentExists"], true);
    assert_eq!(diagnostic["parentKind"], "directory");
    assert!(!serialized.contains(root.to_str().unwrap()));
    assert!(!serialized.contains("private-report-name.pdf"));
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn destination_diagnostic_preserves_original_and_retry_denial_facts() {
    let root = std::env::temp_dir().join(format!("ade-dl-missing-parent-{}", Uuid::new_v4()));
    let destination = root.join("report.pdf");
    let diagnostic = destination_parent_diagnostic(
        &destination,
        Some("destination parent is unavailable or unsafe: Operation not permitted (os error 1)"),
        ParentOpenDiagnostic {
            attempts: 2,
            first_errno: Some(libc::EPERM),
            final_errno: Some(libc::EPERM),
        },
        None,
    );

    assert!(diagnostic["parentExists"].is_null());
    assert_eq!(diagnostic["parentKind"], "notProbed");
    assert!(diagnostic["readAccess"].is_null());
    assert_eq!(diagnostic["parentOpenAttempts"], 2);
    assert_eq!(diagnostic["parentOpenFirstErrno"], libc::EPERM);
    assert_eq!(diagnostic["parentOpenFinalErrno"], libc::EPERM);
    assert_eq!(diagnostic["parentOpenRecovered"], false);
    assert_eq!(
        diagnostic["admissionErrorClass"],
        "parentUnavailableOrUnsafe"
    );
}

#[test]
fn rejected_relative_destination_is_not_probed_against_the_process_cwd() {
    let diagnostic = destination_parent_diagnostic(
        Path::new("renderer-controlled/report.pdf"),
        Some("download destination path must be absolute"),
        ParentOpenDiagnostic::default(),
        None,
    );

    assert_eq!(diagnostic["parentAbsolute"], false);
    assert_eq!(diagnostic["parentClass"], "relativeRejected");
    assert_eq!(diagnostic["parentKind"], "notProbed");
    assert!(diagnostic["readAccess"].is_null());
    assert_eq!(diagnostic["statvfsSucceeded"], false);
}

#[test]
fn transient_eperm_retries_the_identical_safe_parent_open_once() {
    let root = std::env::temp_dir().join(format!("ade-dl-eperm-retry-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    inject_parent_open_eperm(1);

    let reserved = ReservedDestination::reserve_observed(
        &root.join("report.pdf"),
        DownloadCollisionPolicy::Fail,
        Arc::default(),
    )
    .unwrap();
    let diagnostic = reserved.parent_open_diagnostic();

    assert_eq!(diagnostic.attempts, 2);
    assert_eq!(diagnostic.first_errno, Some(libc::EPERM));
    assert_eq!(diagnostic.final_errno, None);
    drop(reserved);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn repeated_eperm_fails_after_one_retry_and_preserves_both_errno_values() {
    let root = std::env::temp_dir().join(format!("ade-dl-eperm-stop-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let destination = root.join("report.pdf");
    inject_parent_open_eperm(2);

    let failure = match ReservedDestination::reserve_observed(
        &destination,
        DownloadCollisionPolicy::Fail,
        Arc::default(),
    ) {
        Ok(_) => panic!("two injected denials must exhaust the bounded retry"),
        Err(failure) => failure,
    };
    let parent_open = failure.parent_open_diagnostic();
    let diagnostic =
        destination_parent_diagnostic(&destination, Some(failure.as_str()), parent_open, None);

    assert_eq!(parent_open.attempts, 2);
    assert_eq!(parent_open.first_errno, Some(libc::EPERM));
    assert_eq!(parent_open.final_errno, Some(libc::EPERM));
    assert_eq!(diagnostic["parentOpenRecovered"], false);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_rename_leases_choose_distinct_exact_final_leaves() {
    let root = std::env::temp_dir().join(format!("ade-dl-leases-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let reservations = Arc::new(DestinationReservations::default());
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let root = root.clone();
        let reservations = Arc::clone(&reservations);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            ReservedDestination::reserve(
                &root.join("report.pdf"),
                DownloadCollisionPolicy::Rename,
                reservations,
            )
            .unwrap()
        }));
    }
    barrier.wait();
    let mut leases: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    let mut paths: Vec<_> = leases
        .iter()
        .map(|lease| lease.final_path().to_owned())
        .collect();
    paths.sort();

    assert_eq!(
        paths,
        [root.join("report (1).pdf"), root.join("report.pdf")]
    );
    assert_eq!(reservations.len(), 2);
    leases.clear();
    assert_eq!(reservations.len(), 0);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn saturated_reservations_share_one_incremental_semantic_namespace() {
    let root = std::env::temp_dir().join(format!("ade-dl-saturated-leases-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let reservations = Arc::new(DestinationReservations::default());
    let mut leases = Vec::new();
    for index in 0..128 {
        leases.push(
            ReservedDestination::reserve(
                &root.join(format!("report-{index}.pdf")),
                DownloadCollisionPolicy::Fail,
                Arc::clone(&reservations),
            )
            .unwrap(),
        );
    }

    assert_eq!(reservations.len(), 128);
    assert_eq!(reservations.semantic_namespace_count(), 1);
    let namespace = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
    assert!(namespace.is_dir());
    assert_eq!(fs::read_dir(namespace).unwrap().count(), 128);

    drop(leases);
    assert_eq!(reservations.len(), 0);
    assert_eq!(reservations.semantic_namespace_count(), 0);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir(root).unwrap();
}

#[test]
fn reservation_uses_destination_filesystem_case_and_normalization_semantics() {
    let root = std::env::temp_dir().join(format!("ade-dl-semantics-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();

    for (first_name, alias_name) in [
        ("case-probe.bin", "CASE-PROBE.BIN"),
        ("r\u{e9}sum\u{e9}.bin", "re\u{301}sume\u{301}.bin"),
    ] {
        let probe = root.join(first_name);
        fs::write(&probe, b"probe").unwrap();
        let aliases = fs::metadata(root.join(alias_name)).is_ok();
        fs::remove_file(&probe).unwrap();

        let reservations = Arc::new(DestinationReservations::default());
        let first = ReservedDestination::reserve(
            &root.join(first_name),
            DownloadCollisionPolicy::Fail,
            Arc::clone(&reservations),
        )
        .unwrap();
        let alias = ReservedDestination::reserve(
            &root.join(alias_name),
            DownloadCollisionPolicy::Fail,
            Arc::clone(&reservations),
        );
        if aliases {
            assert!(alias.is_err(), "filesystem-equivalent leaves must collide");
        } else {
            assert!(
                alias.is_ok(),
                "distinct filesystem leaves may both be leased"
            );
        }
        drop(alias);
        drop(first);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reservation_never_touches_a_foreign_private_coordination_namespace() {
    let root = std::env::temp_dir().join(format!("ade-dl-foreign-{}", Uuid::new_v4()));
    let foreign = root.join(".tmux-agent-destination-reservations");
    fs::create_dir_all(&foreign).unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o700)).unwrap();
    let marker = foreign.join("report.pdf");
    fs::write(&marker, b"foreign bytes must survive").unwrap();
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();

    let reservations = Arc::new(DestinationReservations::default());
    let lease = ReservedDestination::reserve(
        &root.join("report.pdf"),
        DownloadCollisionPolicy::Fail,
        reservations,
    )
    .unwrap();
    drop(lease);

    assert_eq!(fs::read(&marker).unwrap(), b"foreign bytes must survive");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exact_destination_rejects_duplicates_and_only_owner_releases() {
    let root = std::env::temp_dir().join(format!("ade-dl-owner-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let reservations = Arc::new(DestinationReservations::default());
    let prepared = ReservedDestination::reserve(
        &root.join("result.bin"),
        DownloadCollisionPolicy::Fail,
        Arc::clone(&reservations),
    )
    .unwrap();
    let duplicate = ReservedDestination::reserve(
        &root.join(".").join("result.bin"),
        DownloadCollisionPolicy::Fail,
        Arc::clone(&reservations),
    )
    .err()
    .unwrap();
    assert!(duplicate.contains("already reserved"));

    prepared.release_as(Uuid::new_v4());
    assert_eq!(reservations.len(), 1);
    assert!(
        ReservedDestination::reserve(
            &root.join("result.bin"),
            DownloadCollisionPolicy::Fail,
            Arc::clone(&reservations),
        )
        .is_err()
    );
    drop(prepared);
    assert_eq!(reservations.len(), 0);
    ReservedDestination::reserve(
        &root.join("result.bin"),
        DownloadCollisionPolicy::Fail,
        Arc::clone(&reservations),
    )
    .unwrap();
    fs::remove_dir_all(root).unwrap();
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
