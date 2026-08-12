use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn cleanup_result_never_claims_removed_after_unlink_or_connection_failure() {
    assert_eq!(
        classify_upload_cleanup(Ok("unlink denied".into())),
        (CleanupStatus::Failed, Some("unlink denied".into()))
    );
    assert_eq!(
        classify_upload_cleanup(Err("connection closed".into())),
        (
            CleanupStatus::ConnectionClosed,
            Some("connection closed".into())
        )
    );
    assert_eq!(
        classify_upload_cleanup(Ok(String::new())),
        (CleanupStatus::Removed, None)
    );
}

#[test]
fn failed_upload_cleanup_is_not_downgraded_by_noop_cancel() {
    let mut failure = TransferFailure::new(
        TransferOutcome::NotPublished,
        TransferFailureKind::Cleanup,
        CleanupStatus::Failed,
        "upload rejected after cleanup failure",
        Some("unlink denied".into()),
    );
    let (status, error) = classify_upload_cleanup(Ok(String::new()));
    failure.merge_cleanup(status, error);
    assert_eq!(failure.cleanup_status, CleanupStatus::Failed);
    assert_eq!(failure.cleanup_error.as_deref(), Some("unlink denied"));
}

#[test]
fn local_path_inspection_is_ordered_opaque_and_no_follow() {
    let root = std::env::temp_dir().join(format!("inspect-upload-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let names = [
        "with spaces",
        "quote'\"",
        "line\nbreak",
        "-leading",
        "東京.png",
    ];
    let mut paths = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let path = root.join(name);
        std::fs::write(&path, vec![b'x'; index + 1]).unwrap();
        paths.push(path.to_string_lossy().into_owned());
    }
    let inspected = inspect_local_terminal_paths(paths.clone()).unwrap();
    assert_eq!(inspected.len(), names.len());
    for (index, result) in inspected.iter().enumerate() {
        assert_eq!(result.path, paths[index]);
        assert_eq!(result.name, names[index]);
        assert_eq!(result.size_bytes, (index + 1).to_string());
    }

    let target = root.join("target");
    std::fs::write(&target, b"target").unwrap();
    let link = root.join("link");
    symlink(&target, &link).unwrap();
    assert!(inspect_local_terminal_paths(vec![link.to_string_lossy().into_owned()]).is_err());
    assert!(inspect_local_terminal_paths(vec![root.to_string_lossy().into_owned()]).is_err());
    assert!(
        inspect_local_terminal_paths(vec![root.join("missing").to_string_lossy().into_owned()])
            .is_err()
    );
    assert!(inspect_local_terminal_paths(vec!["relative".into()]).is_err());
    let unreadable = root.join("unreadable");
    std::fs::write(&unreadable, b"private").unwrap();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    if unsafe { libc::geteuid() } != 0 {
        assert!(
            inspect_local_terminal_paths(vec![unreadable.to_string_lossy().into_owned()]).is_err()
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn upload_basename_preserves_shell_sensitive_names_as_opaque_data() {
    let source = Path::new("/tmp/source");
    for name in [
        "with spaces",
        "quote'\"",
        "line\nbreak",
        "-leading",
        "東京.png",
    ] {
        assert_eq!(upload_basename(source, name).unwrap(), name);
    }
    for name in ["", ".", "..", "a/b", "/tmp/x"] {
        if name.is_empty() {
            assert_eq!(upload_basename(source, name).unwrap(), "source");
        } else {
            assert!(upload_basename(source, name).is_err());
        }
    }
}

#[test]
fn png_contract_is_signature_and_size_bounded() {
    let path = std::env::temp_dir().join(format!("clipboard-{}.png", Uuid::new_v4()));
    std::fs::write(&path, PNG_SIGNATURE).unwrap();
    let (mut file, identity) = open_regular_source(&path).unwrap();
    validate_png_if_requested(&mut file, &identity, true).unwrap();
    std::fs::remove_file(path).unwrap();
}
