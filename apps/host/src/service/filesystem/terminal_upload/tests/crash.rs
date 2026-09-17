use super::*;

#[test]
fn cross_process_lock_holder() {
    let Ok(directory) = std::env::var("ADE_PHASE7_LOCK_HOLDER_DIR") else {
        return;
    };
    let name = std::env::var_os("ADE_PHASE7_LOCK_HOLDER_NAME").unwrap();
    let ready = PathBuf::from(std::env::var_os("ADE_PHASE7_LOCK_READY").unwrap());
    let release = PathBuf::from(std::env::var_os("ADE_PHASE7_LOCK_RELEASE").unwrap());
    let staging = StagingDirectory::open(Path::new(&directory)).unwrap();
    let file = staging.open_readonly(OsStr::new(&name)).unwrap();
    lock_exclusive(&file).unwrap();
    fs::write(&ready, b"locked").unwrap();
    for _ in 0..1_000 {
        if release.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("cross-process lock holder timed out");
}

#[test]
fn upload_transaction_crash_worker() {
    let Ok(role) = std::env::var("ADE_PHASE7_CRASH_WORKER_ROLE") else {
        return;
    };
    let transfer_id = std::env::var("ADE_PHASE7_CRASH_TRANSFER_ID").unwrap();
    let target = std::env::var("ADE_PHASE7_CRASH_TARGET").unwrap();
    if role == "commit" {
        let collision = match std::env::var("ADE_PHASE7_CRASH_COLLISION")
            .unwrap()
            .as_str()
        {
            "new" => v1::CollisionPolicy::Fail,
            "overwrite" => v1::CollisionPolicy::OverwriteConfirmed,
            value => panic!("unexpected crash collision {value}"),
        };
        let replacement = b"verified replacement after real process crash";
        let service = FileService::new();
        service
            .prepare_terminal_upload(
                &transfer_id,
                &target,
                replacement.len() as u64,
                collision,
                false,
                false,
            )
            .unwrap();
        service
            .write_terminal_upload_chunk(&transfer_id, 0, replacement)
            .unwrap();
        let digest = blake3::hash(replacement).to_hex().to_string();
        let result = service.commit_terminal_upload(&transfer_id, &digest);
        panic!("crash point returned instead of SIGKILL: {result:?}");
    }

    assert_eq!(role, "recover");
    let result_path = PathBuf::from(std::env::var_os("ADE_PHASE7_CRASH_RESULT").unwrap());
    let service = FileService::new();
    let probe_id = Uuid::new_v4().to_string();
    let probe_name = format!("recovery-probe-{probe_id}.bin");
    service
        .prepare_terminal_upload(
            &probe_id,
            &probe_name,
            0,
            v1::CollisionPolicy::Fail,
            false,
            false,
        )
        .unwrap();
    service.cancel_terminal_upload(&probe_id).unwrap();
    let reconciled = service.reconcile_terminal_upload(&transfer_id).ok();
    fs::write(
        result_path,
        serde_json::to_vec(&serde_json::json!({
            "published": reconciled.is_some(),
            "verified": reconciled.as_ref().is_some_and(|value| value.verified),
            "blake3": reconciled.map(|value| value.blake3),
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn sigkill_restart_matrix_converges_every_upload_transaction_boundary() {
    let common_points = [
        "prepared-journal-durable",
        "target-renamed",
        "target-directory-fsynced",
        "published-journal-written",
        "published-journal-fsynced",
        "published-journal-renamed",
        "published-journal-directory-fsynced",
        "cleanup-complete",
    ];
    let overwrite_only = ["backup-removed", "backup-removal-directory-fsynced"];
    let replacement = b"verified replacement after real process crash";
    let replacement_digest = blake3::hash(replacement).to_hex().to_string();

    for collision in ["new", "overwrite"] {
        for point in common_points.iter().copied().chain(
            (collision == "overwrite")
                .then_some(overwrite_only)
                .into_iter()
                .flatten(),
        ) {
            let home = temp_dir();
            let staging = StagingDirectory::open_upload_cache(&home).unwrap();
            let transfer_id = Uuid::new_v4().to_string();
            let target = format!("crash-{collision}-{point}.bin");
            if collision == "overwrite" {
                let mut original = staging.create_private(OsStr::new(&target)).unwrap();
                original.write_all(b"original bytes").unwrap();
                original.sync_all().unwrap();
            }
            let foreign = home.join("foreign-sentinel");
            fs::write(&foreign, b"foreign unchanged").unwrap();
            let foreign_link = staging.path().join("foreign-link");
            symlink(&foreign, &foreign_link).unwrap();

            let status = Command::new(std::env::current_exe().unwrap())
                .arg("service::filesystem::terminal_upload::tests::crash::upload_transaction_crash_worker")
                .arg("--exact")
                .arg("--nocapture")
                .env("HOME", &home)
                .env("ADE_PHASE7_CRASH_WORKER_ROLE", "commit")
                .env("ADE_PHASE7_CRASH_TRANSFER_ID", &transfer_id)
                .env("ADE_PHASE7_CRASH_TARGET", &target)
                .env("ADE_PHASE7_CRASH_COLLISION", collision)
                .env("ADE_PHASE7_UPLOAD_CRASH_POINT", point)
                .status()
                .unwrap();
            assert_eq!(
                status.signal(),
                Some(libc::SIGKILL),
                "{collision}/{point} did not die by SIGKILL: {status}"
            );

            let result_path = home.join("recovery-result.json");
            let recovery = Command::new(std::env::current_exe().unwrap())
                .arg("service::filesystem::terminal_upload::tests::crash::upload_transaction_crash_worker")
                .arg("--exact")
                .arg("--nocapture")
                .env("HOME", &home)
                .env("ADE_PHASE7_CRASH_WORKER_ROLE", "recover")
                .env("ADE_PHASE7_CRASH_TRANSFER_ID", &transfer_id)
                .env("ADE_PHASE7_CRASH_TARGET", &target)
                .env("ADE_PHASE7_CRASH_RESULT", &result_path)
                .status()
                .unwrap();
            assert!(recovery.success(), "{collision}/{point} recovery failed");
            let recovered: serde_json::Value =
                serde_json::from_slice(&fs::read(&result_path).unwrap()).unwrap();
            let published = point != "prepared-journal-durable";
            assert_eq!(recovered["published"], published, "{collision}/{point}");
            assert_eq!(recovered["verified"], published, "{collision}/{point}");
            if published {
                assert_eq!(
                    recovered["blake3"], replacement_digest,
                    "{collision}/{point}"
                );
                assert_eq!(
                    fs::read(staging.path().join(&target)).unwrap(),
                    replacement,
                    "{collision}/{point}"
                );
                assert!(
                    staging
                        .path()
                        .join(owned_manifest_name(&transfer_id))
                        .exists(),
                    "{collision}/{point} manifest did not converge"
                );
                let manifest: OwnedUploadManifest = serde_json::from_slice(
                    &fs::read(staging.path().join(owned_manifest_name(&transfer_id))).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    manifest.transaction_state,
                    UploadJournalState::Reconciled,
                    "{collision}/{point} journal state did not converge"
                );
            } else if collision == "overwrite" {
                assert_eq!(
                    fs::read(staging.path().join(&target)).unwrap(),
                    b"original bytes",
                    "{collision}/{point}"
                );
                assert!(
                    !staging
                        .path()
                        .join(owned_manifest_name(&transfer_id))
                        .exists()
                );
            } else {
                assert!(
                    !staging.path().join(&target).exists(),
                    "{collision}/{point}"
                );
                assert!(
                    !staging
                        .path()
                        .join(owned_manifest_name(&transfer_id))
                        .exists()
                );
            }
            let partial = staging
                .path()
                .join(format!("{PARTIAL_PREFIX}{transfer_id}{PARTIAL_SUFFIX}"));
            let manifest_partial = staging.path().join(format!(
                "{OWNED_MANIFEST_PREFIX}{transfer_id}{OWNED_MANIFEST_SUFFIX}.partial"
            ));
            let reservation = staging
                .path()
                .join(reservation_leaf_name(OsStr::new(&target)));
            assert!(!partial.exists(), "{collision}/{point} leaked partial");
            assert!(
                !manifest_partial.exists(),
                "{collision}/{point} leaked journal"
            );
            assert!(
                !reservation.exists(),
                "{collision}/{point} leaked reservation"
            );
            assert_eq!(fs::read(&foreign).unwrap(), b"foreign unchanged");
            assert!(foreign_link.is_symlink());
            fs::remove_dir_all(home).unwrap();
        }
    }
}
