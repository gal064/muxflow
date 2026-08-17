//! Create, rename, move, duplicate, and delete, and the confirmations and
//! cleanup guarantees they carry.

use super::*;

/// Hidden means "not shown", never "not there".
///
/// The destructive-confirmation gates count what is really in a directory,
/// and must keep doing so: a directory holding nothing but a `.git` is a
/// directory holding a repository, and deleting it without asking because
/// the tree does not draw its contents would be a far worse defect than a
/// prompt about something invisible. This is the test that fails if the
/// hiding predicate is ever wired into `mutations.rs` for tidiness.
#[test]
fn a_directory_that_looks_empty_is_still_not_empty_to_a_delete() {
    let (root, service) = fixture();
    let repository = root.join("repo");
    fs::create_dir(&repository).unwrap();
    fs::create_dir(repository.join(".git")).unwrap();
    fs::write(repository.join(".git/config"), "kept").unwrap();

    let listed = service
        .list_directory(root.to_str().unwrap(), "repo", "watch")
        .unwrap();
    assert!(listed.entries.is_empty(), "the tree draws nothing in it");

    let error = service
        .mutate(&v1::FileServiceRequest {
            operation_id: "delete-repo".into(),
            root: root.to_string_lossy().into_owned(),
            mutation: v1::FileMutationKind::Delete.into(),
            path: repository.to_string_lossy().into_owned(),
            non_empty_confirmed: false,
            ..Default::default()
        })
        .expect_err("a repository must not be deleted without confirmation");
    assert!(
        error.to_string().contains("confirmation_required"),
        "got {error}"
    );
    assert!(repository.join(".git/config").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn symlink_escape_and_destructive_confirmations_are_enforced() {
    let (root, service) = fixture();
    let outside = std::env::temp_dir().join(format!("ade-outside-{}", Uuid::new_v4()));
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret"), "no").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    assert!(
        service
            .open_file_stream(root.to_str().unwrap(), "escape/secret")
            .is_err()
    );
    fs::write(root.join("kept"), "inside").unwrap();
    std::os::unix::fs::symlink(root.join("kept"), root.join("inside-link")).unwrap();
    service
        .mutate(&v1::FileServiceRequest {
            operation_id: "delete-link".into(),
            root: root.to_string_lossy().into_owned(),
            path: "inside-link".into(),
            mutation: v1::FileMutationKind::Delete.into(),
            ..Default::default()
        })
        .unwrap();
    assert!(
        root.join("kept").exists(),
        "deleting a symlink must retain its target"
    );
    fs::create_dir(root.join("full")).unwrap();
    fs::write(root.join("full/item"), "x").unwrap();
    let request = v1::FileServiceRequest {
        operation_id: "delete-1".into(),
        root: root.to_string_lossy().into_owned(),
        path: "full".into(),
        mutation: v1::FileMutationKind::Delete.into(),
        ..Default::default()
    };
    assert!(
        service
            .mutate(&request)
            .unwrap_err()
            .to_string()
            .contains("confirmation_required")
    );
    let mut confirmed = request;
    confirmed.non_empty_confirmed = true;
    service.mutate(&confirmed).unwrap();
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside).unwrap();
}

#[test]
fn failed_overwrite_restores_the_original_destination() {
    let (root, service) = fixture();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(source.join("unsupported.sock")).unwrap();
    fs::write(root.join("destination"), "original").unwrap();
    let result = service.mutate(&v1::FileServiceRequest {
        operation_id: "duplicate-failure".into(),
        root: root.to_string_lossy().into_owned(),
        path: "source".into(),
        destination: "destination".into(),
        mutation: v1::FileMutationKind::Duplicate.into(),
        overwrite_confirmed: true,
        ..Default::default()
    });
    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(root.join("destination")).unwrap(),
        "original"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn case_only_rename_succeeds_without_overwrite_confirmation_and_reports_truthfully() {
    let (root, service) = fixture();
    fs::write(root.join("Case.txt"), "preserved").unwrap();
    let metadata = service
        .mutate(&v1::FileServiceRequest {
            operation_id: "case-only-rename".into(),
            root: root.to_string_lossy().into_owned(),
            path: "Case.txt".into(),
            destination: "case.txt".into(),
            mutation: v1::FileMutationKind::Rename.into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(metadata.name, "case.txt");
    assert_eq!(metadata.path, root.join("case.txt").to_string_lossy());
    assert_eq!(
        fs::read_to_string(root.join("case.txt")).unwrap(),
        "preserved"
    );
    let names = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, [OsString::from("case.txt")]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_duplicate_preserves_dot_git_and_symlinks() {
    let (root, service) = fixture();
    fs::create_dir(root.join("source")).unwrap();
    fs::create_dir(root.join("source/.git")).unwrap();
    fs::write(root.join("source/.git/config"), "kept").unwrap();
    fs::write(root.join("source/file"), "body").unwrap();
    std::os::unix::fs::symlink("file", root.join("source/link")).unwrap();
    service
        .mutate(&v1::FileServiceRequest {
            operation_id: "duplicate-all".into(),
            root: root.to_string_lossy().into_owned(),
            root_token: root_token(root.to_str().unwrap()).unwrap(),
            path: "source".into(),
            destination: "copy".into(),
            mutation: v1::FileMutationKind::Duplicate.into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("copy/.git/config")).unwrap(),
        "kept"
    );
    assert_eq!(
        fs::read_link(root.join("copy/link")).unwrap(),
        PathBuf::from("file")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn timed_out_recursive_duplicate_is_cooperative_and_cleans_partial() {
    let (root, service) = fixture();
    fs::create_dir(root.join("source")).unwrap();
    let body = vec![0x5a; 1024 * 1024];
    for index in 0..64 {
        fs::write(root.join("source").join(index.to_string()), &body).unwrap();
    }
    let cancellation = Arc::new(AtomicBool::new(false));
    let timeout = Arc::clone(&cancellation);
    let timer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(5));
        timeout.store(true, Ordering::Release);
    });
    let result = service.mutate_cancellable(
        &v1::FileServiceRequest {
            operation_id: "duplicate-timeout".into(),
            root: root.to_string_lossy().into_owned(),
            root_token: root_token(root.to_str().unwrap()).unwrap(),
            path: "source".into(),
            destination: "copy".into(),
            mutation: v1::FileMutationKind::Duplicate.into(),
            ..Default::default()
        },
        &cancellation,
    );
    timer.join().unwrap();
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert!(!root.join("copy").exists());
    assert!(!fs::read_dir(&root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".partial")
    }));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn disconnected_recursive_delete_leaves_no_app_owned_partial() {
    let (root, service) = fixture();
    fs::create_dir(root.join("victim")).unwrap();
    for index in 0..10_000 {
        fs::write(root.join("victim").join(index.to_string()), b"x").unwrap();
    }
    let cancellation = Arc::new(AtomicBool::new(false));
    let disconnected = Arc::clone(&cancellation);
    let timer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2));
        disconnected.store(true, Ordering::Release);
    });
    let result = service.mutate_cancellable(
        &v1::FileServiceRequest {
            operation_id: "delete-disconnect".into(),
            root: root.to_string_lossy().into_owned(),
            root_token: root_token(root.to_str().unwrap()).unwrap(),
            path: "victim".into(),
            mutation: v1::FileMutationKind::Delete.into(),
            non_empty_confirmed: true,
            ..Default::default()
        },
        &cancellation,
    );
    timer.join().unwrap();
    if let Err(error) = result {
        assert!(error.to_string().contains("cancelled"));
        assert!(root.join("victim").exists());
    } else {
        assert!(!root.join("victim").exists());
    }
    assert!(!fs::read_dir(&root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".partial")
    }));
    fs::remove_dir_all(root).unwrap();
}
