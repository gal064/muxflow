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

#[tokio::test]
async fn ignored_directory_status_uses_a_canonical_repository_path() {
    let fixture = Fixture::new("ignored-directory");
    fixture.write(".gitignore", b"ignored-directory/\n");
    fixture.git(&["add", ".gitignore"]);
    fixture.git(&["commit", "-qm", "ignore directory"]);
    fs::create_dir(fixture.root.join("ignored-directory")).unwrap();
    fixture.write("ignored-directory/file", b"ignored\n");

    let status = GitService::new(Arc::new(AtomicBool::new(false)), 0)
        .status(&fixture.request(), None)
        .await
        .unwrap();
    assert!(status.authoritative);
    assert!(status.entries.iter().any(|entry| {
        entry.ignored
            && entry.path == b"ignored-directory"
            && entry.display_path == "ignored-directory"
    }));
}

#[tokio::test]
async fn status_models_initial_raw_ignored_mode_symlink_binary_and_rename_delete() {
    let fixture = Fixture::new("status");
    let initial = GitService::new(Arc::new(AtomicBool::new(false)), 0)
        .status(&fixture.request(), None)
        .await
        .unwrap();
    let initial_repository = initial.repository.unwrap();
    assert!(initial_repository.initial);
    // Porcelain writes `(initial)` where an object id would go. That is a
    // state, not an id, and must never reach the wire as one.
    assert_eq!(initial_repository.head_oid, "");
    assert!(!initial_repository.detached_head);
    fixture.write(".gitignore", b"ignored*\n");
    fixture.write("tracked", b"base\n");
    fixture.write("binary", b"a\0b");
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("ignored-one", b"ignored");
    #[cfg(target_os = "linux")]
    let raw = b"raw-\xff".to_vec();
    #[cfg(target_os = "macos")]
    let raw = "raw-é".as_bytes().to_vec();
    fs::write(
        fixture.root.join(std::ffi::OsString::from_vec(raw.clone())),
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
    let status = GitService::new(Arc::new(AtomicBool::new(false)), 0)
        .status(&fixture.request(), None)
        .await
        .unwrap();
    let raw_entry = status
        .entries
        .iter()
        .find(|entry| entry.path == raw)
        .unwrap();
    // Entries carry the repository-relative byte path and nothing more. The
    // absolute path is that path joined to the root the snapshot already names
    // once, which is what keeps a large repository inside the encoded bound.
    assert_eq!(raw_entry.path, raw);
    assert_eq!(
        status.repository.as_ref().unwrap().worktree_root,
        fixture.root.to_str().unwrap()
    );
    assert!(status.entries.iter().any(|entry| entry.ignored));
    assert!(
        status
            .entries
            .iter()
            .any(|entry| entry.path == b"ignored-one")
    );
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

#[tokio::test]
async fn status_accepts_deleted_file_with_a_missing_parent_directory() {
    let fixture = Fixture::new("deleted-parent");
    fixture.write("removed/nested/file", b"tracked\n");
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "base"]);
    fs::remove_dir_all(fixture.root.join("removed")).unwrap();

    let status = GitService::new(Arc::new(AtomicBool::new(false)), 0)
        .status(&fixture.request(), None)
        .await
        .unwrap();

    assert!(status.authoritative);
    assert!(
        status
            .entries
            .iter()
            .any(|entry| { entry.path == b"removed/nested/file" && entry.worktree_status == "D" })
    );
}

#[tokio::test]
async fn diff_preserves_crlf_missing_eof_binary_and_symlink_target() {
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
    let service = GitService::new(Arc::new(AtomicBool::new(false)), 0);
    let status = service.status(&fixture.request(), None).await.unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    request.path = b"text".to_vec();
    let text = service.diff_only(&request, None).await.unwrap();
    assert_eq!(text.old_content, b"one\r\ntwo");
    assert_eq!(text.new_content, b"one\r\nchanged");
    // The client response carries content, not the patch the editor never read.
    assert!(text.patch.is_empty());
    let marker = b"\\ No newline at end of file";
    assert!(
        service
            .diff_with_patch(&request)
            .await
            .unwrap()
            .patch
            .windows(marker.len())
            .any(|value| value == marker)
    );
    request.path = b"binary".to_vec();
    assert!(service.diff_only(&request, None).await.unwrap().binary);
    request.path = b"link".to_vec();
    assert_eq!(
        service.diff_only(&request, None).await.unwrap().new_content,
        b"/etc/passwd"
    );
}

#[tokio::test]
async fn large_diff_returns_bounded_metadata_instead_of_crossing_control_lane() {
    let fixture = Fixture::new("large-diff");
    let large = vec![b'a'; MAX_DIFF_CONTENT + 1];
    fs::write(fixture.root.join("large"), &large).unwrap();
    fixture.git(&["add", "large"]);
    fixture.git(&["commit", "-qm", "large"]);
    fs::write(fixture.root.join("large"), vec![b'b'; large.len()]).unwrap();
    let service = GitService::new(Arc::new(AtomicBool::new(false)), 0);
    let status = service.status(&fixture.request(), None).await.unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.path = b"large".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    let diff = service.diff_only(&request, None).await.unwrap();
    assert!(diff.too_large);
    assert!(diff.patch.is_empty());
    assert!(diff.old_content.is_empty());
    assert!(diff.new_content.is_empty());
    assert!(!diff.source_generation.is_empty());
    fs::write(fixture.root.join("large"), vec![b'c'; large.len()]).unwrap();
    request.expected_status_generation = service.status(&request, None).await.unwrap().generation;
    let same_size_edit = service.diff_only(&request, None).await.unwrap();
    assert_ne!(diff.source_generation, same_size_edit.source_generation);
}

#[tokio::test]
async fn invalid_byte_status_expansion_returns_bounded_placeholder_and_service_stays_usable() {
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
    let next = GitService::new(Arc::new(AtomicBool::new(false)), 0)
        .status(&fixture.request(), None)
        .await
        .unwrap();
    assert!(next.authoritative);
    assert_eq!(next.entries.len(), 1);
}

#[tokio::test]
async fn status_generation_tracks_same_porcelain_content_and_copy_binary_enrichment_is_batched() {
    let fixture = Fixture::new("content-generation");
    fixture.write("tracked", b"base\n");
    fixture.git(&["add", "tracked"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("tracked", b"aaaa\n");
    fixture.write("binary-untracked", b"x\0y");
    fixture.write("copy", b"base\n");
    fixture.git(&["add", "copy"]);
    let service = GitService::new(Arc::new(AtomicBool::new(false)), 0);
    let first = service.status(&fixture.request(), None).await.unwrap();
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
    let second = service.status(&fixture.request(), None).await.unwrap();
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

#[tokio::test]
async fn aggregate_and_binary_diff_preclassification_stays_below_the_frame_budget() {
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
    let service = GitService::new(Arc::new(AtomicBool::new(false)), 0);
    let status = service.status(&fixture.request(), None).await.unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.as_ref().unwrap().repository_id.clone();
    request.expected_status_generation = status.generation;
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    request.path = b"wide".to_vec();
    let text = service.diff_only(&request, None).await.unwrap();
    assert!(text.too_large);
    assert!(text.patch.is_empty() && text.old_content.is_empty() && text.new_content.is_empty());
    request.path = b"binary-wide".to_vec();
    let binary = service.diff_only(&request, None).await.unwrap();
    assert!(binary.binary);
    assert!(
        binary.patch.is_empty() && binary.old_content.is_empty() && binary.new_content.is_empty()
    );
    assert!(prost::Message::encoded_len(&binary) < tmux_agent_protocol::MAX_FRAME_BYTES);
}

#[tokio::test]
async fn diff_requires_repository_and_fresh_status_and_honors_pre_cancel() {
    let fixture = Fixture::new("diff-authority");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    fixture.write("file", b"changed\n");
    let service = GitService::new(Arc::new(AtomicBool::new(false)), 0);
    let status = service.status(&fixture.request(), None).await.unwrap();
    let mut request = fixture.request();
    request.path = b"file".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    request.expected_status_generation = status.generation;
    assert!(
        service
            .diff_only(&request, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("repository identity")
    );
    request.repository_id = status.repository.unwrap().repository_id;
    fixture.write("file", b"newer!!\n");
    // A diff no longer needs the client's status expectation to still hold: the
    // response states the authoritative status it was actually read against, so
    // the client reconciles instead of paying another round trip to retry.
    let (diff, carried) = service.diff(&request, true, None).await.unwrap();
    assert_eq!(diff.new_content, b"newer!!\n");
    assert!(carried.authoritative);
    assert_ne!(carried.generation, request.expected_status_generation);
    assert!(
        carried
            .entries
            .iter()
            .any(|entry| entry.path == b"file" && entry.worktree_status == "M")
    );

    request.expected_status_generation = carried.generation;
    assert!(
        service
            .diff(&request, true, Some(Arc::new(AtomicBool::new(true))))
            .await
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
    #[cfg(target_os = "linux")]
    let raw = b"new\n\xff".to_vec();
    #[cfg(target_os = "macos")]
    let raw = "new\né".as_bytes().to_vec();
    fs::write(
        fixture.root.join(std::ffi::OsString::from_vec(raw.clone())),
        b"first\nsecond\n",
    )
    .unwrap();
    let service = Arc::new(GitService::new(Arc::new(AtomicBool::new(false)), 0));
    let status = service.status(&fixture.request(), None).await.unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.as_ref().unwrap().repository_id.clone();
    request.expected_status_generation = status.generation;
    request.connection_epoch = 61;
    request.path = raw.clone();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();
    let diff = service.diff_only(&request, None).await.unwrap();
    assert_eq!(diff.hunk_count, 1);
    assert!(
        service
            .diff_with_patch(&request)
            .await
            .unwrap()
            .patch
            .windows(3)
            .any(|window| window == b"@@ ")
    );
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

#[tokio::test]
async fn real_conflict_and_submodule_worktree_states_are_reported() {
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
    let conflicted = GitService::new(Arc::new(AtomicBool::new(false)), 0)
        .status(&conflict.request(), None)
        .await
        .unwrap();
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
    let status = GitService::new(Arc::new(AtomicBool::new(false)), 0)
        .status(&parent.request(), None)
        .await
        .unwrap();
    assert!(
        status
            .entries
            .iter()
            .any(|entry| entry.path == b"module" && entry.submodule)
    );
}

#[tokio::test]
async fn large_text_bodies_are_referenced_for_the_bulk_lane_and_stream_back_exactly() {
    let fixture = Fixture::new("bulk-diff-content");
    let base: Vec<u8> = (0..40_000_u32)
        .flat_map(|line| format!("line {line}\n").into_bytes())
        .collect();
    let changed: Vec<u8> = base
        .iter()
        .copied()
        .chain(b"tail\n".iter().copied())
        .collect();
    fs::write(fixture.root.join("wide"), &base).unwrap();
    fixture.git(&["add", "wide"]);
    fixture.git(&["commit", "-qm", "base"]);
    fs::write(fixture.root.join("wide"), &changed).unwrap();
    let service = Arc::new(GitService::new(Arc::new(AtomicBool::new(false)), 0));
    let status = service.status(&fixture.request(), None).await.unwrap();
    let mut request = fixture.request();
    request.repository_id = status.repository.unwrap().repository_id;
    request.expected_status_generation = status.generation;
    request.path = b"wide".to_vec();
    request.diff_target = v1::GitDiffTarget::Unstaged.into();

    let diff = service.diff_only(&request, None).await.unwrap();
    assert!(
        diff.old_content.is_empty(),
        "large bodies leave the control lane"
    );
    assert!(diff.new_content.is_empty());
    let old_ref = diff
        .old_content_ref
        .clone()
        .expect("old side is referenced");
    let new_ref = diff
        .new_content_ref
        .clone()
        .expect("new side is referenced");
    assert_eq!(old_ref.size, base.len() as u64);
    assert_eq!(new_ref.size, changed.len() as u64);
    assert_eq!(
        new_ref.content_digest,
        blake3::hash(&changed).to_hex().to_string()
    );

    let mut streamed = Vec::new();
    loop {
        let mut chunk_request = request.clone();
        chunk_request.content = Some(v1::GitDiffContentRequest {
            side: v1::GitDiffContentSide::New.into(),
            expected_content_digest: new_ref.content_digest.clone(),
            expected_size: new_ref.size,
            offset: streamed.len() as u64,
            length: 64 * 1024,
        });
        let chunk = service.diff_content(&chunk_request, None).await.unwrap();
        assert_eq!(chunk.offset, streamed.len() as u64);
        assert_eq!(chunk.total_size, changed.len() as u64);
        streamed.extend_from_slice(&chunk.data);
        if chunk.last {
            break;
        }
    }
    assert_eq!(streamed, changed);

    // A body that no longer matches what the control response classified is
    // refused rather than silently substituted.
    fs::write(fixture.root.join("wide"), b"replaced\n").unwrap();
    let mut stale = request.clone();
    stale.content = Some(v1::GitDiffContentRequest {
        side: v1::GitDiffContentSide::New.into(),
        expected_content_digest: new_ref.content_digest.clone(),
        expected_size: new_ref.size,
        offset: 0,
        length: 1024,
    });
    assert!(
        service
            .diff_content(&stale, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("changed since it was classified")
    );

    // The old side is an immutable object, so it still streams.
    let mut old_request = request.clone();
    old_request.content = Some(v1::GitDiffContentRequest {
        side: v1::GitDiffContentSide::Old.into(),
        expected_content_digest: old_ref.content_digest.clone(),
        expected_size: old_ref.size,
        offset: 0,
        length: 1024,
    });
    let head = service.diff_content(&old_request, None).await.unwrap();
    assert_eq!(head.data, base[..1024]);
    assert!(!head.last);
}

#[tokio::test]
async fn repository_discovery_runs_one_batched_rev_parse_and_reuses_it() {
    let fixture = Fixture::new("batched-discovery");
    fixture.write("file", b"base\n");
    fixture.git(&["add", "file"]);
    fixture.git(&["commit", "-qm", "base"]);
    let service = Arc::new(GitService::new(Arc::new(AtomicBool::new(false)), 0));
    let first = service.status(&fixture.request(), None).await.unwrap();
    let repository = first.repository.clone().unwrap();
    // Branch and HEAD come from the porcelain read, not from extra processes.
    assert_eq!(repository.head_name, "master");
    assert!(!repository.head_oid.is_empty());
    assert!(!repository.initial);
    assert!(!repository.detached_head);

    fixture.write("file", b"changed\n");
    let mut refreshed = fixture.request();
    refreshed.repository_id = repository.repository_id.clone();
    let second = service.status(&refreshed, None).await.unwrap();
    assert_ne!(second.source_generation, first.source_generation);
    // The second refresh pays no discovery at all: the identity was cached and
    // revalidated with `fstat` rather than another `rev-parse`.
    let observation = service.observation();
    assert_eq!(observation.discoveries, 1);
    assert_eq!(observation.status_pipelines, 2);
}
