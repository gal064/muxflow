use super::*;

#[test]
fn upload_rename_suffix_respects_name_max_and_utf8_boundaries() {
    let stem = "界".repeat(83);
    let candidate = renamed_basename(&stem, Some("png"), 12, 255).unwrap();
    assert!(candidate.len() <= 255);
    assert!(candidate.ends_with(" (12).png"));
    assert!(candidate.is_char_boundary(candidate.len()));
}
use std::os::unix::fs::symlink;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::thread;

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!("terminal-upload-test-{}", Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn basename_policy_accepts_shell_metacharacters_without_invoking_a_shell() {
    for name in [
        "with spaces.txt",
        "quote'\".txt",
        "line\nbreak",
        "-leading",
        "unicodé-東京.png",
    ] {
        validate_destination_basename(name).unwrap();
    }
    for name in [
        "",
        ".",
        "..",
        "a/b",
        "/absolute",
        ".tmux-agent-upload-owned.partial",
    ] {
        assert!(
            validate_destination_basename(name).is_err(),
            "accepted {name:?}"
        );
    }
}

#[test]
fn durable_journal_recovers_prepared_and_published_new_uploads() {
    for published in [false, true] {
        let path = temp_dir();
        let directory = StagingDirectory::open(&path).unwrap();
        let id = Uuid::new_v4().to_string();
        let partial = OsString::from(format!("{PARTIAL_PREFIX}{id}{PARTIAL_SUFFIX}"));
        let target = OsString::from("journal-target.bin");
        let mut bytes = directory.create_private(&partial).unwrap();
        bytes.write_all(b"verified").unwrap();
        bytes.sync_all().unwrap();
        let digest = blake3::hash(b"verified").to_hex().to_string();
        let staged =
            stage_completed_upload_manifest(&directory, &id, &target, &bytes, &digest, None, None)
                .unwrap();
        if published {
            directory.rename_noreplace(&partial, &target).unwrap();
            directory.sync().unwrap();
        }
        drop(staged);
        drop(bytes);

        recover_upload_transactions(&directory).unwrap();
        if published {
            assert_eq!(fs::read(path.join(&target)).unwrap(), b"verified");
            assert!(path.join(owned_manifest_name(&id)).exists());
        } else {
            assert!(!path.join(&target).exists());
            assert!(!path.join(&partial).exists());
            assert!(!path.join(owned_manifest_name(&id)).exists());
        }
        assert!(
            !path
                .join(format!("{}{}.json.partial", OWNED_MANIFEST_PREFIX, id))
                .exists()
        );
        fs::remove_dir_all(path).unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn durable_journal_recovers_overwrite_and_removes_only_original_backup() {
    let path = temp_dir();
    let directory = StagingDirectory::open(&path).unwrap();
    let id = Uuid::new_v4().to_string();
    let partial = OsString::from(format!("{PARTIAL_PREFIX}{id}{PARTIAL_SUFFIX}"));
    let target = OsString::from("overwrite-journal.bin");
    let mut original = directory.create_private(&target).unwrap();
    original.write_all(b"original").unwrap();
    original.sync_all().unwrap();
    let original_metadata = original.metadata().unwrap();
    drop(original);
    let mut replacement = directory.create_private(&partial).unwrap();
    replacement.write_all(b"replacement").unwrap();
    replacement.sync_all().unwrap();
    let digest = blake3::hash(b"replacement").to_hex().to_string();
    let staged = stage_completed_upload_manifest(
        &directory,
        &id,
        &target,
        &replacement,
        &digest,
        Some((original_metadata.dev(), original_metadata.ino())),
        None,
    )
    .unwrap();
    directory.exchange(&partial, &target).unwrap();
    directory.sync().unwrap();
    drop(staged);
    drop(replacement);

    recover_upload_transactions(&directory).unwrap();
    assert_eq!(fs::read(path.join(&target)).unwrap(), b"replacement");
    assert!(!path.join(&partial).exists());
    assert!(path.join(owned_manifest_name(&id)).exists());
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn upload_is_private_digest_verified_and_atomic() {
    let directory = temp_dir();
    let service = FileService::new();
    let id = Uuid::new_v4().to_string();
    service
        .prepare_terminal_upload_in(
            &directory,
            &id,
            "result file.txt",
            5,
            v1::CollisionPolicy::Fail,
            false,
            false,
        )
        .unwrap();
    service
        .write_terminal_upload_chunk(&id, 0, b"hello")
        .unwrap();
    let digest = blake3::hash(b"hello").to_hex().to_string();
    let result = service.commit_terminal_upload(&id, &digest).unwrap();
    assert!(result.verified);
    assert_eq!(fs::read(&result.final_path).unwrap(), b"hello");
    assert_eq!(
        fs::metadata(&result.final_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        !directory
            .join(format!("{PARTIAL_PREFIX}{id}{PARTIAL_SUFFIX}"))
            .exists()
    );
    fs::remove_dir_all(directory).unwrap();
}

fn prepared_upload(
    directory: &Path,
    destination: &str,
    collision: v1::CollisionPolicy,
    bytes: &[u8],
) -> (FileService, String, String) {
    let service = FileService::new();
    let id = Uuid::new_v4().to_string();
    service
        .prepare_terminal_upload_in(
            directory,
            &id,
            destination,
            bytes.len() as u64,
            collision,
            false,
            false,
        )
        .unwrap();
    service.write_terminal_upload_chunk(&id, 0, bytes).unwrap();
    (service, id, blake3::hash(bytes).to_hex().to_string())
}

#[test]
fn manifest_faults_never_publish_upload_bytes() {
    for fault in [
        PublicationFault::ManifestCreate,
        PublicationFault::ManifestWrite,
        PublicationFault::ManifestFsync,
    ] {
        let directory = temp_dir();
        let (service, id, digest) = prepared_upload(
            &directory,
            "transaction.bin",
            v1::CollisionPolicy::Fail,
            b"replacement",
        );
        let error = service
            .commit_terminal_upload_with_fault(&id, &digest, Some(fault))
            .unwrap_err()
            .to_string();
        assert!(error.contains("not_published"), "{fault:?}: {error}");
        assert!(!directory.join("transaction.bin").exists());
        assert!(!directory.join(owned_manifest_name(&id)).exists());
        assert!(
            !directory
                .join(format!("{PARTIAL_PREFIX}{id}{PARTIAL_SUFFIX}"))
                .exists()
        );
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn post_rename_fsync_fault_rolls_back_new_file_and_preserves_overwrite() {
    for overwrite in [false, true] {
        let directory = temp_dir();
        if overwrite {
            fs::write(directory.join("transaction.bin"), b"original").unwrap();
        }
        let collision = if overwrite {
            v1::CollisionPolicy::OverwriteConfirmed
        } else {
            v1::CollisionPolicy::Fail
        };
        let (service, id, digest) =
            prepared_upload(&directory, "transaction.bin", collision, b"replacement");
        let error = service
            .commit_terminal_upload_with_fault(
                &id,
                &digest,
                Some(PublicationFault::PostRenameDirectoryFsync),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("not_published"), "{error}");
        if overwrite {
            assert_eq!(
                fs::read(directory.join("transaction.bin")).unwrap(),
                b"original"
            );
        } else {
            assert!(!directory.join("transaction.bin").exists());
        }
        assert!(!directory.join(owned_manifest_name(&id)).exists());
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn cleanup_fault_reports_retained_backup_but_keeps_verified_replacement() {
    let directory = temp_dir();
    fs::write(directory.join("transaction.bin"), b"original").unwrap();
    let (service, id, digest) = prepared_upload(
        &directory,
        "transaction.bin",
        v1::CollisionPolicy::OverwriteConfirmed,
        b"replacement",
    );
    let result = service
        .commit_terminal_upload_with_fault(&id, &digest, Some(PublicationFault::Cleanup))
        .unwrap();
    assert_eq!(
        fs::read(directory.join("transaction.bin")).unwrap(),
        b"replacement"
    );
    assert!(result.verified);
    assert!(result.cleanup_error.contains("retained"));
    assert_eq!(
        fs::read(directory.join(format!("{PARTIAL_PREFIX}{id}{PARTIAL_SUFFIX}"))).unwrap(),
        b"original"
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn post_publication_journal_failures_report_published_cleanup_and_keep_verified_bytes() {
    for fault in [
        PublicationFault::ReconciledJournalWrite,
        PublicationFault::ReconciledJournalFsync,
    ] {
        let directory = temp_dir();
        fs::write(directory.join("journal-cleanup.bin"), b"original").unwrap();
        let (service, id, digest) = prepared_upload(
            &directory,
            "journal-cleanup.bin",
            v1::CollisionPolicy::OverwriteConfirmed,
            b"replacement",
        );
        let result = service
            .commit_terminal_upload_with_fault(&id, &digest, Some(fault))
            .unwrap();
        assert!(result.verified);
        assert!(result.cleanup_error.contains("reconciliation failed"));
        assert_eq!(
            fs::read(directory.join("journal-cleanup.bin")).unwrap(),
            b"replacement"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn pre_authoritative_journal_faults_restore_original_with_not_published_outcome() {
    for fault in [
        PublicationFault::PublishedJournalWrite,
        PublicationFault::PublishedJournalFsync,
        PublicationFault::PostJournalRenameDirectoryFsync,
    ] {
        let directory = temp_dir();
        fs::write(directory.join("journal-rollback.bin"), b"original").unwrap();
        let (service, id, digest) = prepared_upload(
            &directory,
            "journal-rollback.bin",
            v1::CollisionPolicy::OverwriteConfirmed,
            b"replacement",
        );
        let error = service
            .commit_terminal_upload_with_fault(&id, &digest, Some(fault))
            .unwrap_err();
        assert_eq!(error.outcome, PublicationOutcome::NotPublished);
        assert_eq!(
            fs::read(directory.join("journal-rollback.bin")).unwrap(),
            b"original"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn rollback_substitution_reports_unknown_and_preserves_original_and_replacement() {
    let directory = temp_dir();
    fs::write(directory.join("transaction.bin"), b"original").unwrap();
    let (service, id, digest) = prepared_upload(
        &directory,
        "transaction.bin",
        v1::CollisionPolicy::OverwriteConfirmed,
        b"replacement",
    );
    let error = service
        .commit_terminal_upload_with_fault(
            &id,
            &digest,
            Some(PublicationFault::RollbackSubstitution),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("outcome_unknown"), "{error}");
    assert_eq!(
        fs::read(directory.join(format!("{PARTIAL_PREFIX}{id}{PARTIAL_SUFFIX}"))).unwrap(),
        b"original",
        "unknown rollback must retain the exchanged original inode"
    );
    let retained = fs::read_dir(&directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read(entry.path()).ok())
        .collect::<Vec<_>>();
    assert!(retained.iter().any(|bytes| bytes == b"replacement"));
    assert!(retained.iter().any(|bytes| bytes == b"foreign-substitute"));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn lost_commit_response_can_be_reconciled_from_verified_owned_manifest() {
    let directory = temp_dir();
    let service = FileService::new();
    let id = Uuid::new_v4().to_string();
    service
        .prepare_terminal_upload_in(
            &directory,
            &id,
            "reconcile.bin",
            5,
            v1::CollisionPolicy::Fail,
            false,
            false,
        )
        .unwrap();
    service
        .write_terminal_upload_chunk(&id, 0, b"owned")
        .unwrap();
    let digest = blake3::hash(b"owned").to_hex().to_string();
    let committed = service.commit_terminal_upload(&id, &digest).unwrap();
    let reconciled = service
        .reconcile_terminal_upload_in(&directory, &id)
        .unwrap();
    assert_eq!(reconciled.final_path, committed.final_path);
    assert_eq!(reconciled.total_bytes, 5);
    assert_eq!(reconciled.blake3, digest);
    assert!(reconciled.verified);

    fs::write(&committed.final_path, b"other").unwrap();
    assert!(
        service
            .reconcile_terminal_upload_in(&directory, &id)
            .unwrap_err()
            .to_string()
            .contains("verified bytes")
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn concurrent_rename_uploads_reserve_distinct_destinations_across_services() {
    let directory = temp_dir();
    let first = FileService::new();
    let second = FileService::new();
    let first_id = Uuid::new_v4().to_string();
    let second_id = Uuid::new_v4().to_string();
    let first_descriptor = first
        .prepare_terminal_upload_in(
            &directory,
            &first_id,
            "same name.txt",
            5,
            v1::CollisionPolicy::Rename,
            false,
            false,
        )
        .unwrap();
    let second_descriptor = second
        .prepare_terminal_upload_in(
            &directory,
            &second_id,
            "same name.txt",
            6,
            v1::CollisionPolicy::Rename,
            false,
            false,
        )
        .unwrap();
    assert_eq!(first_descriptor.destination_name, "same name.txt");
    assert_eq!(second_descriptor.destination_name, "same name (1).txt");

    first
        .write_terminal_upload_chunk(&first_id, 0, b"first")
        .unwrap();
    second
        .write_terminal_upload_chunk(&second_id, 0, b"second")
        .unwrap();
    let first_result = first
        .commit_terminal_upload(&first_id, blake3::hash(b"first").to_hex().as_ref())
        .unwrap();
    let second_result = second
        .commit_terminal_upload(&second_id, blake3::hash(b"second").to_hex().as_ref())
        .unwrap();
    assert_ne!(first_result.final_path, second_result.final_path);
    assert_eq!(fs::read(first_result.final_path).unwrap(), b"first");
    assert_eq!(fs::read(second_result.final_path).unwrap(), b"second");
    assert!(!directory.read_dir().unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(RESERVATION_PREFIX)
    }));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn failures_and_cancellation_never_publish_a_complete_destination() {
    let directory = temp_dir();
    let service = FileService::new();
    for cancel in [false, true] {
        let id = Uuid::new_v4().to_string();
        let name = if cancel { "cancelled" } else { "bad-digest" };
        service
            .prepare_terminal_upload_in(
                &directory,
                &id,
                name,
                5,
                v1::CollisionPolicy::Fail,
                false,
                false,
            )
            .unwrap();
        service
            .write_terminal_upload_chunk(&id, 0, b"hello")
            .unwrap();
        if cancel {
            service.cancel_terminal_upload(&id).unwrap();
        } else {
            assert!(service.commit_terminal_upload(&id, "wrong").is_err());
        }
        assert!(!directory.join(name).exists());
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cancellation_reports_unlink_failure_and_retains_owned_partial() {
    let directory = temp_dir();
    let service = FileService::new();
    let id = Uuid::new_v4().to_string();
    service
        .prepare_terminal_upload_in(
            &directory,
            &id,
            "cancel-unlink-fault.bin",
            1,
            v1::CollisionPolicy::Fail,
            false,
            false,
        )
        .unwrap();
    let cleanup = service
        .cancel_terminal_upload_with_fault(&id, true)
        .unwrap();
    assert!(cleanup.contains("injected partial unlink failure"));
    assert!(
        directory
            .join(format!("{PARTIAL_PREFIX}{id}{PARTIAL_SUFFIX}"))
            .exists()
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn staging_parent_swap_fails_closed_and_cleans_through_original_descriptor() {
    let directory = temp_dir();
    let backup = directory.with_extension("backup");
    let attacker = directory.with_extension("attacker");
    let service = FileService::new();
    let id = Uuid::new_v4().to_string();
    let destination = format!("parent-swap-{id}");
    service
        .prepare_terminal_upload_in(
            &directory,
            &id,
            &destination,
            4,
            v1::CollisionPolicy::Fail,
            false,
            false,
        )
        .unwrap();
    service
        .write_terminal_upload_chunk(&id, 0, b"swap")
        .unwrap();
    fs::rename(&directory, &backup).unwrap();
    fs::create_dir(&attacker).unwrap();
    symlink(&attacker, &directory).unwrap();
    let digest = blake3::hash(b"swap").to_hex().to_string();
    assert!(service.commit_terminal_upload(&id, &digest).is_err());
    assert!(!attacker.join(&destination).exists());
    assert!(!backup.join(&destination).exists());
    assert!(
        !backup
            .join(format!("{PARTIAL_PREFIX}{id}{PARTIAL_SUFFIX}"))
            .exists(),
        "cleanup must unlink through the originally opened directory"
    );
    fs::remove_file(&directory).unwrap();
    fs::rename(&backup, &directory).unwrap();
    fs::remove_dir(&attacker).unwrap();
    fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn limits_and_cleanup_ownership_fail_closed() {
    let directory = temp_dir();
    let service = FileService::new();
    let id = Uuid::new_v4().to_string();
    assert!(
        service
            .prepare_terminal_upload_in(
                &directory,
                &id,
                "large",
                LARGE_UPLOAD_BYTES + 1,
                v1::CollisionPolicy::Fail,
                false,
                false
            )
            .is_err()
    );
    assert!(
        service
            .prepare_terminal_upload_in(
                &directory,
                &id,
                "image.png",
                MAX_IMAGE_BYTES + 1,
                v1::CollisionPolicy::Fail,
                true,
                true
            )
            .is_err()
    );
    fs::write(directory.join("foreign.partial"), b"keep").unwrap();
    let staging = StagingDirectory::open(&directory).unwrap();
    let active_id = Uuid::new_v4();
    let active_name = OsString::from(format!("{PARTIAL_PREFIX}{active_id}{PARTIAL_SUFFIX}"));
    let active = directory.join(&active_name);
    let active_file = staging.create_private(&active_name).unwrap();
    active_file.set_len(STALE_PARTIAL_TOTAL_BYTES + 1).unwrap();
    lock_exclusive(&active_file).unwrap();
    let stale_id = Uuid::new_v4();
    let stale_name = OsString::from(format!("{PARTIAL_PREFIX}{stale_id}{PARTIAL_SUFFIX}"));
    let stale = directory.join(&stale_name);
    staging
        .create_private(&stale_name)
        .unwrap()
        .set_len(STALE_PARTIAL_TOTAL_BYTES + 1)
        .unwrap();
    cleanup_owned_staging(&staging).unwrap();
    assert!(directory.join("foreign.partial").exists());
    assert!(
        active.exists(),
        "cleanup must never delete an active partial"
    );
    assert!(
        !stale.exists(),
        "owned excess-size partial should be removed"
    );
    drop(active_file);
    drop(staging);
    fs::remove_dir_all(directory).unwrap();
}

#[path = "tests/crash.rs"]
mod crash;

#[test]
fn fresh_sparse_active_partial_survives_cleanup_from_another_process() {
    let directory = temp_dir();
    let staging = StagingDirectory::open(&directory).unwrap();
    let active_id = Uuid::new_v4();
    let active_name = OsString::from(format!("{PARTIAL_PREFIX}{active_id}{PARTIAL_SUFFIX}"));
    let active_path = directory.join(&active_name);
    staging
        .create_private(&active_name)
        .unwrap()
        .set_len(8 * 1024 * 1024 * 1024 + 1)
        .unwrap();
    let stale_id = Uuid::new_v4();
    let stale_name = OsString::from(format!("{PARTIAL_PREFIX}{stale_id}{PARTIAL_SUFFIX}"));
    let stale_path = directory.join(&stale_name);
    staging
        .create_private(&stale_name)
        .unwrap()
        .set_len(STALE_PARTIAL_TOTAL_BYTES + 1)
        .unwrap();
    let ready = directory.with_extension("ready");
    let release = directory.with_extension("release");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("service::filesystem::terminal_upload::tests::crash::cross_process_lock_holder")
        .arg("--exact")
        .arg("--nocapture")
        .env("ADE_PHASE7_LOCK_HOLDER_DIR", &directory)
        .env("ADE_PHASE7_LOCK_HOLDER_NAME", &active_name)
        .env("ADE_PHASE7_LOCK_READY", &ready)
        .env("ADE_PHASE7_LOCK_RELEASE", &release)
        .spawn()
        .unwrap();
    for _ in 0..500 {
        if ready.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "lock-holder child did not become ready");
    cleanup_owned_partials(&staging).unwrap();
    assert!(
        active_path.exists(),
        "active cross-process partial was deleted"
    );
    assert!(
        !stale_path.exists(),
        "inactive excess-size partial was retained"
    );
    fs::write(&release, b"release").unwrap();
    assert!(child.wait().unwrap().success());
    cleanup_owned_partials(&staging).unwrap();
    assert!(!active_path.exists(), "released partial was not collected");
    drop(staging);
    fs::remove_file(ready).unwrap();
    fs::remove_file(release).unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn completed_retention_uses_private_manifest_and_preserves_replaced_foreign_inode() {
    let directory = temp_dir();
    let staging = StagingDirectory::open(&directory).unwrap();

    let large_id = Uuid::new_v4().to_string();
    let large_name = OsString::from("owned-large");
    let large = staging.create_private(&large_name).unwrap();
    large.set_len(COMPLETED_STAGING_TOTAL_BYTES + 1).unwrap();
    record_completed_upload(
        &staging,
        &large_id,
        &large_name,
        &large,
        blake3::hash(&[]).to_hex().as_ref(),
    )
    .unwrap();
    drop(large);
    let large_manifest_path = directory.join(owned_manifest_name(&large_id));
    let mut large_manifest: OwnedUploadManifest =
        serde_json::from_slice(&fs::read(&large_manifest_path).unwrap()).unwrap();
    large_manifest.created_unix_seconds = 0;
    fs::write(
        &large_manifest_path,
        serde_json::to_vec(&large_manifest).unwrap(),
    )
    .unwrap();

    let replaced_id = Uuid::new_v4().to_string();
    let replaced_name = OsString::from("replaced-by-user");
    let mut replaced = staging.create_private(&replaced_name).unwrap();
    replaced.write_all(b"owned").unwrap();
    replaced.sync_all().unwrap();
    record_completed_upload(
        &staging,
        &replaced_id,
        &replaced_name,
        &replaced,
        blake3::hash(b"owned").to_hex().as_ref(),
    )
    .unwrap();
    drop(replaced);
    staging.unlink(&replaced_name).unwrap();
    let mut foreign = staging.create_private(&replaced_name).unwrap();
    foreign.write_all(b"foreign").unwrap();
    foreign.sync_all().unwrap();
    drop(foreign);

    cleanup_completed_uploads(&staging).unwrap();
    assert!(staging.metadata(&large_name).is_err());
    assert_eq!(
        fs::read(directory.join(&replaced_name)).unwrap(),
        b"foreign"
    );
    assert!(staging.metadata(&owned_manifest_name(&large_id)).is_err());
    assert!(
        staging
            .metadata(&owned_manifest_name(&replaced_id))
            .is_err()
    );
    drop(staging);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn existing_cache_parent_mode_is_validated_but_never_chmodded() {
    let root = temp_dir();
    let cache = root.join(".cache");
    fs::create_dir(&cache).unwrap();
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
    ensure_cache_parent(&cache).unwrap();
    assert_eq!(
        fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
        0o755
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn upload_cache_symlink_substitution_preserves_foreign_inode_and_mode() {
    let root = temp_dir();
    let cache = root.join(".cache");
    let foreign = root.join("foreign");
    fs::create_dir(&cache).unwrap();
    fs::create_dir(&foreign).unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&foreign, cache.join("muxflow")).unwrap();
    assert!(StagingDirectory::open_upload_cache(&root).is_err());
    assert_eq!(
        fs::metadata(&foreign).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(fs::read_dir(&foreign).unwrap().next().is_none());
    fs::remove_dir_all(root).unwrap();
}
