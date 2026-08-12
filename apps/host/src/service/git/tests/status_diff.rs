use super::*;

#[test]
fn porcelain_parser_preserves_raw_rename_conflict_and_ignored_records() {
    let data = b"2 R. S.M. 100644 100644 100644 aaa bbb R100 new\xff\0old name\0u UU N... 100644 100644 100644 100644 aaa bbb ccc conflict\0? raw\xff\0! ignored/\0";
    let parsed = parse_porcelain_v2_z(data).unwrap();
    assert_eq!(parsed.len(), 4);
    assert_eq!(parsed[0].path, b"new\xff");
    assert_eq!(parsed[0].original_path, b"old name");
    assert_eq!(parsed[0].rename_score, "R100");
    assert!(parsed[0].submodule);
    assert_eq!(parsed[0].submodule_state, "S.M.");
    assert!(parsed[1].conflicted);
    assert_eq!(parsed[1].conflict_code, "UU");
    assert!(parsed[2].untracked);
    assert!(parsed[3].ignored);
    assert_eq!(parsed[3].path, b"ignored");
}

#[test]
fn ignored_directory_status_uses_a_canonical_repository_path() {
    let fixture = Fixture::new("ignored-directory");
    fixture.write(".gitignore", b"ignored-directory/\n");
    fixture.git(&["add", ".gitignore"]);
    fixture.git(&["commit", "-qm", "ignore directory"]);
    fs::create_dir(fixture.root.join("ignored-directory")).unwrap();
    fixture.write("ignored-directory/file", b"ignored\n");

    let status = GitService::new().status(&fixture.request()).unwrap();
    assert!(status.authoritative);
    assert!(status.entries.iter().any(|entry| {
        entry.ignored
            && entry.path == b"ignored-directory"
            && entry.display_path == "ignored-directory"
    }));
}

#[test]
fn status_models_initial_raw_ignored_mode_symlink_binary_and_rename_delete() {
    let fixture = Fixture::new("status");
    let initial = GitService::new().status(&fixture.request()).unwrap();
    assert!(initial.repository.unwrap().initial);
    fixture.write(".gitignore", b"ignored*\n");
    fixture.write("tracked", b"base\n");
    fixture.write("binary", b"a\0b");
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("ignored-one", b"ignored");
    fs::write(
        fixture
            .root
            .join(std::ffi::OsString::from_vec(b"raw-\xff".to_vec())),
        b"raw",
    )
    .unwrap();
    std::os::unix::fs::symlink("tracked", fixture.root.join("link")).unwrap();
    let mut mode = fs::metadata(fixture.root.join("tracked"))
        .unwrap()
        .permissions();
    mode.set_mode(0o755);
    fs::set_permissions(fixture.root.join("tracked"), mode).unwrap();
    fixture.git(&["mv", "binary", "renamed"]);
    fs::remove_file(fixture.root.join("renamed")).unwrap();
    let status = GitService::new().status(&fixture.request()).unwrap();
    assert!(status.entries.iter().any(|entry| entry.path == b"raw-\xff"));
    assert!(status.entries.iter().any(|entry| entry.ignored));
    assert!(
        status
            .entries
            .iter()
            .any(|entry| entry.path == b"link" && entry.symlink)
    );
    assert!(
        status
            .entries
            .iter()
            .any(|entry| entry.worktree_mode == 0o100755)
    );
    assert!(status.entries.iter().any(|entry| {
        entry.path == b"renamed" && entry.index_status == "R" && entry.worktree_status == "D"
    }));
}

#[test]
fn diff_preserves_crlf_missing_eof_binary_and_symlink_target() {
    let fixture = Fixture::new("diff");
    fixture.write("text", b"one\r\ntwo");
    fixture.write("binary", b"a\0b");
    std::os::unix::fs::symlink("text", fixture.root.join("link")).unwrap();
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("text", b"one\r\nchanged");
    fixture.write("binary", b"z\0y");
    fs::remove_file(fixture.root.join("link")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", fixture.root.join("link")).unwrap();
    let service = GitService::new();
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    request.path = b"text".to_vec();
    let text = service.diff(&request).unwrap();
    assert_eq!(text.old_content, b"one\r\ntwo");
    assert_eq!(text.new_content, b"one\r\nchanged");
    let marker = b"\\ No newline at end of file";
    assert!(
        text.patch
            .windows(marker.len())
            .any(|value| value == marker)
    );
    request.path = b"binary".to_vec();
    assert!(service.diff(&request).unwrap().binary);
    request.path = b"link".to_vec();
    assert_eq!(service.diff(&request).unwrap().new_content, b"/etc/passwd");
}

#[test]
fn large_diff_returns_bounded_metadata_instead_of_crossing_control_lane() {
    let fixture = Fixture::new("large-diff");
    let large = vec![b'a'; MAX_DIFF_CONTENT + 1];
    fs::write(fixture.root.join("large"), &large).unwrap();
    fixture.git(&["add", "large"]);
    fixture.git(&["commit", "-qm", "large"]);
    fs::write(fixture.root.join("large"), vec![b'b'; large.len()]).unwrap();
    let service = GitService::new();
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.path = b"large".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    let diff = service.diff(&request).unwrap();
    assert!(diff.too_large);
    assert!(diff.patch.is_empty());
    assert!(diff.old_content.is_empty());
    assert!(diff.new_content.is_empty());
    assert!(!diff.source_generation.is_empty());
    fs::write(fixture.root.join("large"), vec![b'c'; large.len()]).unwrap();
    request.expected_status_generation = service.status(&request).unwrap().generation;
    let same_size_edit = service.diff(&request).unwrap();
    assert_ne!(diff.source_generation, same_size_edit.source_generation);
}

#[test]
fn invalid_byte_status_expansion_returns_bounded_placeholder_and_service_stays_usable() {
    let entries = (0_usize..11_000)
        .map(|index| v1::GitStatusEntry {
            path: [index.to_le_bytes().as_slice(), &[0xff; 1024]].concat(),
            display_path: "�".repeat(1024),
            untracked: true,
            ..Default::default()
        })
        .collect();
    let bounded = status::bound_status_snapshot(v1::GitStatusSnapshot {
        repository: Some(v1::GitRepository {
            repository_id: "bounded-repository".into(),
            ..Default::default()
        }),
        entries,
        authoritative: true,
        ..Default::default()
    });
    assert!(bounded.oversized);
    assert!(!bounded.authoritative);
    assert_eq!(bounded.total_entry_count, 11_000);
    assert!(bounded.entries.is_empty());
    let response = v1::Response {
        ok: true,
        git: Some(v1::GitResponse {
            status: Some(bounded),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(prost::Message::encoded_len(&response) < tmux_agent_protocol::MAX_FRAME_BYTES);
    let mut sequencer = super::super::super::ProtocolSequencer::default();
    let bounded_frame = sequencer.frame(super::super::super::SequencerControl::Response {
        request_id: 90,
        response,
        snapshot_barrier: false,
    });
    tmux_agent_protocol::encode_frame(&bounded_frame).unwrap();
    // A subsequent terminal-control acknowledgement still traverses the same
    // sequencer/control lane after the bounded Git response.
    let terminal_frame = sequencer.frame(super::super::super::SequencerControl::Response {
        request_id: 91,
        response: v1::Response {
            ok: true,
            ..Default::default()
        },
        snapshot_barrier: false,
    });
    tmux_agent_protocol::encode_frame(&terminal_frame).unwrap();

    let fixture = Fixture::new("status-after-oversized");
    fixture.write("ordinary", b"still alive\n");
    let next = GitService::new().status(&fixture.request()).unwrap();
    assert!(next.authoritative);
    assert_eq!(next.entries.len(), 1);
}

#[test]
fn status_generation_tracks_same_porcelain_content_and_copy_binary_enrichment_is_batched() {
    let fixture = Fixture::new("content-generation");
    fixture.write("tracked", b"base\n");
    fixture.git(&["add", "tracked"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("tracked", b"aaaa\n");
    fixture.write("binary-untracked", b"x\0y");
    fixture.write("copy", b"base\n");
    fixture.git(&["add", "copy"]);
    let service = GitService::new();
    let first = service.status(&fixture.request()).unwrap();
    assert!(
        first
            .entries
            .iter()
            .any(|entry| { entry.path == b"binary-untracked" && entry.untracked && entry.binary })
    );
    assert!(first.entries.iter().any(|entry| {
        entry.path == b"copy" && entry.index_status == "C" && entry.original_path == b"tracked"
    }));
    fixture.write("tracked", b"bbbb\n");
    let second = service.status(&fixture.request()).unwrap();
    assert_ne!(first.generation, second.generation);
    assert_ne!(first.source_generation, second.source_generation);
    let tracked_first = first
        .entries
        .iter()
        .find(|entry| entry.path == b"tracked")
        .unwrap();
    let tracked_second = second
        .entries
        .iter()
        .find(|entry| entry.path == b"tracked")
        .unwrap();
    assert_eq!(
        tracked_first.worktree_status,
        tracked_second.worktree_status
    );
    assert_eq!(tracked_first.index_status, tracked_second.index_status);
}

#[test]
fn copy_candidate_bound_is_explicit_instead_of_silently_complete() {
    let fixture = Fixture::new("copy-candidate-bound");
    let mut entries: Vec<_> = (0..513)
        .map(|index| v1::GitStatusEntry {
            path: format!("candidate-{index}").into_bytes(),
            index_status: "A".into(),
            ..Default::default()
        })
        .collect();
    assert!(
        status::apply_copy_detection(fixture.root.to_str().unwrap(), &mut entries, None,).unwrap()
    );
}

#[test]
fn aggregate_and_binary_diff_preclassification_stays_below_the_frame_budget() {
    let fixture = Fixture::new("aggregate-diff");
    fixture.write("wide", &vec![b'a'; 9 * 1024 * 1024]);
    fixture.write("binary-wide", &vec![0; 10 * 1024 * 1024]);
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "large"]);
    fixture.write("wide", &vec![b'b'; 9 * 1024 * 1024]);
    fixture.write("binary-wide", &vec![1; 10 * 1024 * 1024]);
    // Keep a NUL in the binary sample after changing the full file.
    let binary = fs::OpenOptions::new()
        .write(true)
        .open(fixture.root.join("binary-wide"))
        .unwrap();
    use std::os::unix::fs::FileExt as _;
    binary.write_at(&[0], 0).unwrap();
    let service = GitService::new();
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.as_ref().unwrap().repository_id.clone();
    request.expected_status_generation = status.generation;
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    request.path = b"wide".to_vec();
    let text = service.diff(&request).unwrap();
    assert!(text.too_large);
    assert!(text.patch.is_empty() && text.old_content.is_empty() && text.new_content.is_empty());
    request.path = b"binary-wide".to_vec();
    let binary = service.diff(&request).unwrap();
    assert!(binary.binary);
    assert!(
        binary.patch.is_empty() && binary.old_content.is_empty() && binary.new_content.is_empty()
    );
    assert!(prost::Message::encoded_len(&binary) < tmux_agent_protocol::MAX_FRAME_BYTES);
}

#[test]
fn diff_requires_repository_and_fresh_status_and_honors_pre_cancel() {
    let fixture = Fixture::new("diff-authority");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("file", b"changed\n");
    let service = GitService::new();
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.path = b"file".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    request.expected_status_generation = status.generation;
    assert!(
        service
            .diff(&request)
            .unwrap_err()
            .to_string()
            .contains("repository identity")
    );
    request.repository_id = status.repository.unwrap().repository_id;
    fixture.write("file", b"newer!!\n");
    assert!(
        service
            .diff(&request)
            .unwrap_err()
            .to_string()
            .contains("stale Git status")
    );
    request.expected_status_generation = service.status(&request).unwrap().generation;
    let cancelled = AtomicBool::new(true);
    assert!(
        service
            .diff_cancellable(&request, Some(&cancelled))
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
}

#[tokio::test]
async fn descriptor_pinned_untracked_diff_generates_an_applicable_repo_relative_hunk() {
    let fixture = Fixture::new("untracked-hunk");
    fixture.write("base", b"base\n");
    fixture.git(&["add", "base"]);
    fixture.git(&["commit", "-qm", "base"]);
    let raw = b"new\n\xff".to_vec();
    fs::write(
        fixture.root.join(std::ffi::OsString::from_vec(raw.clone())),
        b"first\nsecond\n",
    )
    .unwrap();
    let service = Arc::new(GitService::new());
    let status = service.status(&fixture.request()).unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.as_ref().unwrap().repository_id.clone();
    request.expected_status_generation = status.generation;
    request.connection_epoch = 61;
    request.path = raw.clone();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    let diff = service.diff(&request).unwrap();
    assert_eq!(diff.hunk_count, 1);
    assert!(diff.patch.windows(3).any(|window| window == b"@@ "));
    request.expected_source_generation = diff.source_generation;
    request.mutation = v1::GitMutationKind::StageHunk.into();
    let staged = service
        .mutate(request, 61, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert!(staged.applied);
    assert!(
        staged
            .status
            .unwrap()
            .entries
            .iter()
            .any(|entry| { entry.path == raw && entry.index_status == "A" })
    );
}

#[test]
fn real_conflict_and_submodule_worktree_states_are_reported() {
    let conflict = Fixture::new("conflict");
    conflict.write("file", b"base\n");
    conflict.git(&["add", "file"]);
    conflict.git(&["commit", "-qm", "base"]);
    let primary = String::from_utf8(conflict.git(&["branch", "--show-current"]).stdout).unwrap();
    conflict.git(&["checkout", "-qb", "other"]);
    conflict.write("file", b"other\n");
    conflict.git(&["commit", "-qam", "other"]);
    conflict.git(&["checkout", "-q", primary.trim()]);
    conflict.write("file", b"primary\n");
    conflict.git(&["commit", "-qam", "primary"]);
    let merge = Command::new("git")
        .arg("-C")
        .arg(&conflict.root)
        .args(["merge", "other"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(!merge.status.success());
    let conflicted = GitService::new().status(&conflict.request()).unwrap();
    assert!(
        conflicted.entries.iter().any(|entry| {
            entry.path == b"file" && entry.conflicted && entry.conflict_code == "UU"
        })
    );

    let child = Fixture::new("submodule-child");
    child.write("nested", b"one\n");
    child.git(&["add", "nested"]);
    child.git(&["commit", "-qm", "nested"]);
    let parent = Fixture::new("submodule-parent");
    let added = Command::new("git")
        .arg("-C")
        .arg(&parent.root)
        .args([
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            child.root.to_str().unwrap(),
            "module",
        ])
        .output()
        .unwrap();
    assert!(added.status.success());
    parent.git(&["commit", "-qm", "submodule"]);
    fs::write(parent.root.join("module/nested"), b"changed\n").unwrap();
    let status = GitService::new().status(&parent.request()).unwrap();
    assert!(
        status
            .entries
            .iter()
            .any(|entry| entry.path == b"module" && entry.submodule)
    );
}
