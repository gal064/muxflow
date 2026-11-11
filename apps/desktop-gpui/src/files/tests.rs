//! Ports of `listingModel`, `directoryState`, `directoryCache`, `watchLeases`,
//! `useWorkspaceFiles`, `ignoredPaths`, `fileIcons`, `downloadStatus`,
//! `downloadFlow`, `useConnectionTransfers`, `terminalFileOpenRoute`, the file
//! half of `useWorkspaceDomainController` and the wire half of `api.test.ts`,
//! plus the Explorer against a scripted file service.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use desktop_core::{
    app_state::{AppTabRecord, PersistedAppState},
    platform::{EventBody, EventSink},
};
use futures::{
    channel::oneshot,
    future::{BoxFuture, FutureExt},
};
use gpui_kit::{AppContext as _, Context, Entity, TestAppContext};
use serde_json::{Value, json};

use super::{
    client::{DownloadStart, FileTransport},
    dialog::MutationDialog,
    downloads::{
        ActiveDownload, DownloadIntent, DownloadKind, DownloadList, DownloadStatus, decode_event,
        reconcile_status, suggested_name,
    },
    explorer::{
        DownloadOrigin, Explorer, ExplorerEvent, Inputs, RECOVERY_COALESCE, ROOT_BACKSTOP,
        ROOT_SETTLED_MULTIPLIER, ROOT_STABLE_PROBES, WATCH_RETRY_BASE,
    },
    icons::{Shape, Tint, file_icon},
    ignored_paths,
    listing::{
        Installed, ListingCache, Patched, Recovery, TreeState, append_page, cache_scope,
        compare_entries, is_patchable, parent_path, patch_entry, reachable_watch_targets,
        remove_entry,
    },
    relocate_file_tabs,
    view::{MutationAction, entry_tooltip, flatten},
    wire::{
        self, ActiveRoot, DirectoryListing, FileEntry, FileKind, FileMutation, FileScope,
        FileSelection, PaneRoute, Relocation, TargetKind, WorkspaceEvent, live_file_scope,
    },
};
use crate::{
    agents::view::ActiveRoot as AgentRoot,
    editor::{FileNotice, Root, WatchTarget},
    terminal::paint_gate::{PANE_PAINT_TIMEOUT, PanePaintGate},
    transfers::{TransferCleanupStatus, TransferFailureKind, TransferOutcome, TransferState},
};

// Fixtures.

fn entry(path: &str, directory: bool) -> FileEntry {
    FileEntry {
        path: path.into(),
        name: path.rsplit('/').next().unwrap_or(path).into(),
        kind: if directory {
            FileKind::Directory
        } else {
            FileKind::File
        },
        size_bytes: "1".into(),
        modified_millis: 1,
        generation: "1".into(),
        executable: false,
        symlink_target: None,
        target_kind: None,
        expandable: directory,
    }
}

fn listing(directory: &str, entries: Vec<FileEntry>) -> DirectoryListing {
    DirectoryListing {
        root_token: "root".into(),
        directory: directory.into(),
        revision: "5".into(),
        entries,
        recovered_from_overflow: false,
        next_page_token: None,
        complete: true,
    }
}

fn partial(directory: &str, entries: Vec<FileEntry>, token: &str) -> DirectoryListing {
    DirectoryListing {
        complete: false,
        next_page_token: Some(token.into()),
        ..listing(directory, entries)
    }
}

fn names(listing: &DirectoryListing) -> Vec<&str> {
    listing
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect()
}

fn active_root(token: &str, path: &str, pane: &str) -> ActiveRoot {
    ActiveRoot {
        token: token.into(),
        pane_id: pane.into(),
        path: path.into(),
        git_worktree: true,
        revision: "1".into(),
    }
}

fn changed(patched: Patched) -> DirectoryListing {
    match patched {
        Patched::Changed(listing) => listing,
        other => panic!("expected a patch, got {other:?}"),
    }
}

fn set_of(paths: &[&str]) -> HashSet<String> {
    paths.iter().map(|path| path.to_string()).collect()
}

fn by_directory(listings: Vec<DirectoryListing>) -> HashMap<String, Rc<DirectoryListing>> {
    listings
        .into_iter()
        .map(|listing| (listing.directory.clone(), Rc::new(listing)))
        .collect()
}

// Listings.

#[test]
fn inserts_a_new_entry_in_the_hosts_own_order() {
    let held = listing("/r", vec![entry("/r/src", true), entry("/r/b.txt", false)]);
    let patched = changed(patch_entry(Some(&held), &entry("/r/a.txt", false)));
    assert_eq!(names(&patched), ["src", "a.txt", "b.txt"]);
}

#[test]
fn replaces_an_entry_in_place_without_moving_it() {
    let held = listing(
        "/r",
        vec![entry("/r/a.txt", false), entry("/r/b.txt", false)],
    );
    let mut updated = entry("/r/b.txt", false);
    updated.size_bytes = "99".into();
    let patched = changed(patch_entry(Some(&held), &updated));
    assert_eq!(names(&patched), ["a.txt", "b.txt"]);
    assert_eq!(patched.entries[1].size_bytes, "99");
}

#[test]
fn patches_a_paginated_listing_inside_the_rows_it_holds() {
    assert_eq!(
        patch_entry(None, &entry("/r/a.txt", false)),
        Patched::Recover(Recovery::Missing)
    );
    assert_eq!(
        remove_entry(None, "/r/a.txt"),
        Patched::Recover(Recovery::Missing)
    );
    let held = partial(
        "/r",
        vec![entry("/r/a.txt", false), entry("/r/m.txt", false)],
        "opaque",
    );
    assert!(
        !is_patchable(&held),
        "a partial listing is not worth caching"
    );
    let inserted = changed(patch_entry(Some(&held), &entry("/r/b.txt", false)));
    assert_eq!(names(&inserted), ["a.txt", "b.txt", "m.txt"]);
    assert!(!inserted.complete);
    assert_eq!(inserted.next_page_token.as_deref(), Some("opaque"));
    // Past the last row held: a page nobody asked for.
    assert_eq!(
        patch_entry(Some(&held), &entry("/r/z.txt", false)),
        Patched::Unchanged
    );
    assert_eq!(
        names(&changed(remove_entry(Some(&held), "/r/a.txt"))),
        ["m.txt"]
    );
    assert_eq!(remove_entry(Some(&held), "/r/never"), Patched::Unchanged);
}

#[test]
fn orders_directories_first_then_raw_name_bytes() {
    let mut sorted = [
        entry("/r/z.txt", false),
        entry("/r/a.txt", false),
        entry("/r/lib", true),
    ];
    sorted.sort_by(compare_entries);
    assert_eq!(
        sorted
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["lib", "a.txt", "z.txt"]
    );
    // UTF-8 byte order is code point order: an astral name sorts above U+E000,
    // where UTF-16 code units would put it below.
    let emoji = entry("/r/\u{1F600}.txt", false);
    let private_use = entry("/r/\u{E000}.txt", false);
    assert_eq!(
        compare_entries(&emoji, &private_use),
        std::cmp::Ordering::Greater
    );
    let held = partial("/r", vec![entry("/r/a.txt", false), private_use], "opaque");
    assert_eq!(patch_entry(Some(&held), &emoji), Patched::Unchanged);
}

#[test]
fn asks_for_a_recovery_when_a_name_has_no_computable_position() {
    let lossy = || vec![entry("/r/a.txt", false), entry("/r/m\u{FFFD}.txt", false)];
    let held = partial("/r", lossy(), "opaque");
    assert_eq!(
        patch_entry(Some(&held), &entry("/r/z.txt", false)),
        Patched::Recover(Recovery::Unmappable)
    );
    // A complete listing holds every row, so position does not matter.
    let patched = changed(patch_entry(
        Some(&listing("/r", lossy())),
        &entry("/r/z.txt", false),
    ));
    assert_eq!(names(&patched), ["a.txt", "m\u{FFFD}.txt", "z.txt"]);
}

#[test]
fn appends_a_later_page_keeping_the_rows_shown() {
    let first = partial("/r", vec![entry("/r/a.txt", false)], "one");
    let second = DirectoryListing {
        recovered_from_overflow: true,
        ..listing(
            "/r",
            vec![entry("/r/a.txt", false), entry("/r/b.txt", false)],
        )
    };
    let joined = append_page(&first, second);
    assert_eq!(names(&joined), ["a.txt", "b.txt"]);
    assert!(joined.complete && joined.recovered_from_overflow);
}

#[test]
fn owes_a_watch_only_to_reachable_directories() {
    let listings = by_directory(vec![
        listing("/r", vec![entry("/r/src", true), entry("/r/docs", true)]),
        listing("/r/src", vec![entry("/r/src/deep", true)]),
        listing("/r/src/deep", vec![]),
    ]);
    assert_eq!(
        reachable_watch_targets(
            "/r",
            &listings,
            &set_of(&["/r", "/r/src", "/r/src/deep", "/r/docs"])
        ),
        ["/r", "/r/src", "/r/src/deep", "/r/docs"]
    );
    // Expansion below a collapsed parent is remembered, not watched.
    assert_eq!(
        reachable_watch_targets("/r", &listings, &set_of(&["/r", "/r/src/deep"])),
        ["/r"]
    );
}

#[test]
fn reads_a_parent_without_inventing_one_above_the_root() {
    assert_eq!(parent_path("/r/a.txt"), "/r");
    assert_eq!(parent_path("/a"), "/");
    assert_eq!(parent_path("/"), "/");
}

// The tree state.

fn tree(listings: Vec<DirectoryListing>) -> TreeState {
    TreeState {
        scope_key: "scope".into(),
        root: Some(active_root("root", "/repo", "%1")),
        expanded: set_of(&["/repo"]),
        listings: by_directory(listings),
        ..TreeState::default()
    }
}

fn repo(names: &[&str]) -> DirectoryListing {
    listing(
        "/repo",
        names
            .iter()
            .map(|name| entry(&format!("/repo/{name}"), false))
            .collect(),
    )
}

fn revised(revision: &str, listing: DirectoryListing) -> DirectoryListing {
    DirectoryListing {
        revision: revision.into(),
        ..listing
    }
}

#[test]
fn refuses_a_transition_under_a_replaced_root_or_a_hidden_directory() {
    let state = tree(vec![]);
    assert!(state.on_directory(&active_root("root", "/repo", "%1"), "/repo"));
    assert!(!state.on_directory(&active_root("other", "/elsewhere", "%1"), "/repo"));
    assert!(!state.on_directory(&active_root("root", "/repo", "%1"), "/repo/src"));
}

#[test]
fn keeps_the_newer_of_two_listings_and_never_orders_what_it_cannot() {
    let mut state = tree(vec![revised("9", repo(&["new"]))]);
    state.loading.insert("/repo".into());
    assert_eq!(
        state.install_listing("/repo", revised("4", repo(&["old"])), false, false),
        Installed::Done
    );
    assert_eq!(names(&state.listings["/repo"]), ["new"]);
    assert!(
        !state.loading.contains("/repo"),
        "the wait state comes back either way"
    );
    let mut state = tree(vec![revised("not-a-number", repo(&["held"]))]);
    state.install_listing("/repo", repo(&["incoming"]), false, false);
    assert_eq!(names(&state.listings["/repo"]), ["incoming"]);
}

#[test]
fn never_shortens_a_paginated_directory_and_says_what_it_owes() {
    let mut state = tree(vec![repo(&["a", "b", "c", "d"])]);
    let rescan = partial(
        "/repo",
        vec![entry("/repo/a", false), entry("/repo/b", false)],
        "p2",
    );
    assert_eq!(
        state.install_listing("/repo", rescan.clone(), false, false),
        Installed::Restore(4)
    );
    assert_eq!(state.listings["/repo"].entries.len(), 4);
    // A restore that fell short is installed rather than queued again.
    assert_eq!(
        state.install_listing("/repo", rescan, false, true),
        Installed::Done
    );
    assert_eq!(state.listings["/repo"].entries.len(), 2);
}

#[test]
fn appends_only_a_page_of_the_listing_it_holds() {
    let mut state = tree(vec![revised("9", repo(&["a"]))]);
    state.install_listing("/repo", repo(&["gone"]), true, false);
    assert_eq!(names(&state.listings["/repo"]), ["a"]);
    let mut state = tree(vec![partial("/repo", vec![entry("/repo/a", false)], "p2")]);
    state.install_listing("/repo", repo(&["b"]), true, false);
    assert_eq!(names(&state.listings["/repo"]), ["a", "b"]);
    assert!(state.listings["/repo"].complete);
}

#[test]
fn prunes_a_deleted_subtree_and_nothing_beside_it() {
    let mut state = tree(vec![
        repo(&["src"]),
        listing("/repo/src", vec![]),
        listing("/repo/src/deep", vec![]),
        listing("/repo/srcery", vec![]),
    ]);
    state
        .expanded
        .extend(set_of(&["/repo/src", "/repo/src/deep", "/repo/srcery"]));
    state.loading.insert("/repo/src/deep".into());
    state.prune_subtree("/repo/src");
    let mut kept: Vec<&String> = state.listings.keys().collect();
    kept.sort();
    assert_eq!(kept, ["/repo", "/repo/srcery"]);
    assert_eq!(state.expanded, set_of(&["/repo", "/repo/srcery"]));
    assert!(state.loading.is_empty());
}

// The cache.

#[test]
fn caches_only_complete_listings_of_the_exact_connection_and_root() {
    let mut cache = ListingCache::default();
    let root = active_root("root", "/r", "%1");
    let scope = cache_scope("c", &root);
    cache.set(&scope, "root", &Rc::new(partial("/r", vec![], "opaque")));
    assert!(cache.get(&scope, "/r").is_none());
    let foreign = DirectoryListing {
        root_token: "other".into(),
        ..listing("/r", vec![])
    };
    cache.set(&scope, "root", &Rc::new(foreign));
    assert!(cache.get(&scope, "/r").is_none());
    cache.set(&scope, "root", &Rc::new(listing("/r", vec![])));
    assert!(cache.get(&scope, "/r").is_some());
    assert!(
        cache
            .get(&cache_scope("reconnected", &root), "/r")
            .is_none()
    );
    let regenerated = ActiveRoot {
        revision: "2".into(),
        ..root
    };
    assert!(cache.get(&cache_scope("c", &regenerated), "/r").is_none());
}

#[test]
fn drops_what_a_replaced_root_or_a_deleted_directory_no_longer_authorises() {
    let mut cache = ListingCache::default();
    let scope = cache_scope("c", &active_root("root", "/r", "%1"));
    for directory in ["/r", "/r/src", "/r/src/deep", "/r/srcfile"] {
        cache.set(&scope, "root", &Rc::new(listing(directory, vec![])));
    }
    cache.invalidate_subtree(&scope, "/r/src");
    assert!(cache.get(&scope, "/r/src").is_none());
    assert!(cache.get(&scope, "/r/src/deep").is_none());
    assert!(cache.get(&scope, "/r/srcfile").is_some());
    let next_scope = cache_scope("c", &active_root("next", "/other", "%1"));
    let next = DirectoryListing {
        root_token: "next".into(),
        ..listing("/other", vec![])
    };
    cache.set(&next_scope, "next", &Rc::new(next));
    cache.invalidate_other_roots(&next_scope);
    assert_eq!(cache.held(), 1);
}

#[test]
fn bounds_the_cache_by_evicting_the_least_recently_used() {
    let mut cache = ListingCache::default();
    let scope = cache_scope("c", &active_root("root", "/r", "%1"));
    for index in 0..300 {
        cache.set(
            &scope,
            "root",
            &Rc::new(listing(&format!("/r/{index}"), vec![])),
        );
    }
    assert_eq!(cache.held(), 256);
    assert!(cache.get(&scope, "/r/0").is_none());
    assert!(cache.get(&scope, "/r/299").is_some());
}

// The flattened tree.

#[test]
fn flattens_with_per_level_positions_and_hides_ignored_subtrees() {
    let listings = by_directory(vec![
        listing(
            "/r",
            vec![
                entry("/r/src", true),
                entry("/r/target", true),
                entry("/r/a.txt", false),
            ],
        ),
        partial("/r/src", vec![entry("/r/src/main.rs", false)], "p2"),
        listing("/r/target", vec![entry("/r/target/debug", true)]),
    ]);
    let expanded = set_of(&["/r", "/r/src", "/r/target"]);
    let rows = flatten("/r", &listings, &expanded, None);
    let keys: Vec<String> = rows.iter().map(|row| row.key()).collect();
    assert_eq!(
        keys,
        [
            "/r/src",
            "/r/src/main.rs",
            "/r/src\0more",
            "/r/target",
            "/r/target/debug",
            "/r/a.txt"
        ]
    );
    assert_eq!((rows[0].position, rows[0].size, rows[0].depth), (1, 3, 0));
    assert_eq!((rows[2].position, rows[2].size, rows[2].depth), (2, 2, 1));
    let hidden = set_of(&["/r/target"]);
    let rows = flatten("/r", &listings, &expanded, Some(&hidden));
    assert!(rows.iter().all(|row| !row.key().starts_with("/r/target")));
    assert_eq!(rows[0].size, 2);
}

#[test]
fn describes_a_row_in_its_tooltip() {
    let mut link = entry("/repo/link", false);
    link.kind = FileKind::Symlink;
    link.modified_millis = 0;
    link.symlink_target = Some("/elsewhere".into());
    assert_eq!(
        entry_tooltip(&link),
        "/repo/link\nsymlink · 1 bytes\nSymlink → /elsewhere"
    );
}

// Icons.

#[test]
fn maps_file_types_to_a_shape_and_a_tint() {
    let file = |name: &str| file_icon(name, FileKind::File, None, false);
    assert_eq!(
        file_icon("src", FileKind::Directory, None, false),
        (Shape::Folder, Tint::Term(4))
    );
    assert_eq!(
        file_icon("src", FileKind::Directory, None, true),
        (Shape::FolderOpen, Tint::Term(4))
    );
    for (name, shape, tint) in [
        ("main.ts", Shape::Code, Tint::Term(4)),
        ("main.js", Shape::Code, Tint::Term(3)),
        ("lib.rs", Shape::Code, Tint::Term(1)),
        ("setup.py", Shape::Code, Tint::Term(2)),
        ("Cargo.toml", Shape::Data, Tint::Term(3)),
        ("README.md", Shape::Markdown, Tint::Term(4)),
        ("build.sh", Shape::Shell, Tint::Term(2)),
        ("logo.svg", Shape::Image, Tint::Term(5)),
        ("Cargo.lock", Shape::Lock, Tint::Faint),
        ("notes.txt", Shape::Text, Tint::Dim),
    ] {
        assert_eq!(file(name), (shape, tint), "{name}");
    }
    assert_eq!(file("PHOTO.PNG"), file("photo.png"));
    for name in [
        ".gitignore",
        ".env.local",
        ".eslintrc.js",
        "vite.config.ts",
        "vitest.config.mts",
    ] {
        assert_eq!(file(name), (Shape::Config, Tint::Dim), "{name}");
    }
    for name in ["Makefile", "LICENSE", "archive.unknownext", "trailing."] {
        assert_eq!(file(name), (Shape::File, Tint::Dim), "{name}");
    }
    assert_eq!(
        file_icon("link.rs", FileKind::Symlink, Some(TargetKind::File), false),
        (Shape::Code, Tint::Term(6))
    );
    assert_eq!(
        file_icon(
            "vendor",
            FileKind::Symlink,
            Some(TargetKind::Directory),
            true
        ),
        (Shape::FolderOpen, Tint::Term(6))
    );
}

// Ignored paths.

#[test]
fn names_ignored_entries_under_the_worktree_root_and_leaves_host_names_alone() {
    let entries = [("target", true), ("dist", true), ("src/main.rs", false)];
    let expected = Some(set_of(&["/repo/target", "/repo/dist"]));
    assert_eq!(ignored_paths(true, false, "/repo", entries), expected);
    assert_eq!(ignored_paths(true, false, "/repo/", entries), expected);
    assert_eq!(ignored_paths(false, false, "/repo", entries), None);
    assert_eq!(ignored_paths(true, true, "/repo", entries), None);
    assert_eq!(
        ignored_paths(true, false, "/repo", [("src/main.rs", false)]),
        Some(HashSet::new())
    );
    let owned = [
        ("node_modules", true),
        ("apps/desktop/node_modules", true),
        ("target", true),
        (".DS_Store", true),
        ("build output", true),
    ];
    assert_eq!(
        ignored_paths(true, false, "/repo", owned),
        Some(set_of(&["/repo/target", "/repo/build output"]))
    );
}

// Downloads.

fn download(id: &str, state: TransferState, outcome: Option<TransferOutcome>) -> DownloadStatus {
    DownloadStatus {
        id: id.into(),
        scope_key: "scope".into(),
        path: "/repo/image.png".into(),
        destination: Some("/downloads/image.png".into()),
        kind: DownloadKind::File,
        state,
        outcome,
        failure_kind: None,
        completed_bytes: 12,
        total_bytes: Some(12),
        bytes_per_second: None,
        eta_seconds: None,
        error: None,
        cleanup_error: None,
        cleanup_status: None,
    }
}

fn banner(id: &str, word: &str) -> ActiveDownload {
    ActiveDownload {
        id: id.into(),
        path: "/repo/image.png".into(),
        banner: format!("Download {word}: /repo/image.png"),
    }
}

#[test]
fn announces_a_finished_download_by_what_it_left_the_user() {
    let completed = download(
        "download-1",
        TransferState::Completed,
        Some(TransferOutcome::Published),
    );
    let active = banner("download-1", "running");
    let reconciled = reconcile_status(
        &active.banner,
        Some(&active),
        std::slice::from_ref(&completed),
    );
    assert_eq!(reconciled.status, "Download complete: /downloads/image.png");
    assert_eq!(
        reconciled.completion.as_deref(),
        Some("/downloads/image.png")
    );
    assert_eq!(reconciled.active, None);
    for (state, word) in [
        (TransferState::Failed, "failed"),
        (TransferState::Cancelled, "cancelled"),
    ] {
        let ended = DownloadStatus {
            state,
            outcome: Some(TransferOutcome::NotPublished),
            ..completed.clone()
        };
        let reconciled = reconcile_status(&active.banner, Some(&active), &[ended]);
        assert_eq!(
            reconciled.status,
            format!("Download {word}: /repo/image.png")
        );
        assert_eq!(reconciled.completion, None);
    }
    let nowhere = DownloadStatus {
        destination: None,
        ..completed
    };
    assert_eq!(
        reconcile_status(&active.banner, Some(&active), &[nowhere]).status,
        "Download completed: /repo/image.png"
    );
}

#[test]
fn leaves_a_newer_status_alone_but_not_routine_chatter() {
    let completed = download(
        "download-1",
        TransferState::Completed,
        Some(TransferOutcome::Published),
    );
    let list = std::slice::from_ref(&completed);
    assert_eq!(
        reconcile_status("File rename completed.", None, list).status,
        "File rename completed."
    );
    let active = banner("download-1", "running");
    let newer = reconcile_status("File rename completed.", Some(&active), list);
    assert_eq!(
        (newer.status.as_str(), newer.active),
        ("File rename completed.", None)
    );
    let queued = banner("download-1", "queued");
    for chatter in ["Live", "Topology changed; reconciling…", ""] {
        assert_eq!(
            reconcile_status(chatter, Some(&queued), list).status,
            "Download complete: /downloads/image.png"
        );
    }
    // History for the same source path does not finish a newer transfer.
    let newer_transfer = banner("download-2", "running");
    let held = reconcile_status(&newer_transfer.banner, Some(&newer_transfer), list);
    assert_eq!(held.active, Some(newer_transfer));
}

#[test]
fn names_a_download_and_a_folder_archive() {
    let intent = |path: &str, kind| DownloadIntent {
        path: path.into(),
        kind,
    };
    assert_eq!(
        suggested_name(&intent("/repo/a.pdf", DownloadKind::File)),
        "a.pdf"
    );
    assert_eq!(
        suggested_name(&intent("/repo/src/", DownloadKind::Folder)),
        "src.tar"
    );
    assert_eq!(
        suggested_name(&intent("/repo/x.tar", DownloadKind::Folder)),
        "x.tar"
    );
    assert_eq!(suggested_name(&intent("/", DownloadKind::File)), "download");
}

fn selection(pane: &str) -> FileSelection {
    FileSelection {
        host_profile_id: "local".into(),
        server_identity: "server".into(),
        session_id: "$1".into(),
        pane_id: pane.into(),
    }
}

fn scope() -> FileScope {
    live_file_scope(true, Some("client"), 41, 7, Some(&selection("%1"))).unwrap()
}

fn file_intent() -> DownloadIntent {
    DownloadIntent {
        path: "/repo/image.png".into(),
        kind: DownloadKind::File,
    }
}

fn download_event(overrides: Value) -> String {
    let mut event = json!({
        "transferId": "t1", "state": "running", "transferredBytes": "5", "totalBytes": "10",
        "totalKnown": true, "throughputBytesPerSecond": 2.7, "serverIdentity": "server",
        "expectedServerIdentity": "server", "connectionEpoch": "41", "terminal": false,
    });
    for (key, value) in overrides.as_object().unwrap() {
        event[key] = value.clone();
    }
    event.to_string()
}

#[test]
fn reads_a_download_event_and_fails_closed_on_one_that_does_not_hang_together() {
    let scope = scope();
    let read = |overrides: Value| {
        decode_event(
            &download_event(overrides),
            &file_intent(),
            "/downloads/image.png",
            &scope,
        )
    };
    let running = read(json!({}));
    assert_eq!(running.state, TransferState::Running);
    assert_eq!(
        (
            running.completed_bytes,
            running.total_bytes,
            running.bytes_per_second
        ),
        (5, Some(10), Some(2))
    );
    assert_eq!(running.destination.as_deref(), Some("/downloads/image.png"));
    assert_eq!(running.progress_permille(), Some(500));
    let done = read(json!({
        "state": "completed", "terminal": true, "outcome": "published",
        "destination": "/downloads/image (1).png",
    }));
    assert_eq!(done.state, TransferState::Completed);
    assert_eq!(
        done.destination.as_deref(),
        Some("/downloads/image (1).png")
    );
    for (overrides, error) in [
        (
            json!({"connectionEpoch": "40"}),
            "Host returned a download event for a stale connection scope.",
        ),
        (
            json!({"state": "exploded"}),
            "Unknown transfer state: exploded",
        ),
        (
            json!({"terminal": true}),
            "Host returned an inconsistent download terminal marker.",
        ),
        (
            json!({"transferredBytes": "05"}),
            "Host returned an invalid downloaded byte count.",
        ),
        (
            json!({"state": "failed", "terminal": true}),
            "Terminal transfer state failed omitted its outcome.",
        ),
    ] {
        let refused = read(overrides);
        assert_eq!(refused.state, TransferState::Failed);
        assert_eq!(refused.outcome, Some(TransferOutcome::NotPublished));
        assert_eq!(refused.error.as_deref(), Some(error));
    }
    let unknown = read(json!({
        "state": "failed", "terminal": true, "outcome": "unknown",
        "failureKind": "outcomeUnknown", "cleanupStatus": "failed", "cleanupError": "busy",
    }));
    assert_eq!(unknown.outcome, Some(TransferOutcome::Unknown));
    assert_eq!(unknown.cleanup_error.as_deref(), Some("busy"));
}

#[test]
fn retires_in_flight_downloads_when_their_connection_is_replaced() {
    let mut list = DownloadList::default();
    list.set_connection("a");
    let owned = |id, state, outcome| DownloadStatus {
        scope_key: "a".into(),
        ..download(id, state, outcome)
    };
    list.record(owned("1", TransferState::Running, None));
    list.record(owned("2", TransferState::Verifying, None));
    list.record(owned(
        "3",
        TransferState::Completed,
        Some(TransferOutcome::Published),
    ));
    // A frame from another connection is not this list's.
    list.record(download("4", TransferState::Running, None));
    assert_eq!(list.items.len(), 3);
    // A late frame never weakens a finished one.
    list.record(owned("3", TransferState::Running, None));
    assert_eq!(list.get("3").unwrap().state, TransferState::Completed);
    list.set_connection("b");
    let state = |id: &str| {
        let item = list.get(id).unwrap();
        (item.state, item.outcome, item.failure_kind)
    };
    assert_eq!(
        state("1"),
        (
            TransferState::Failed,
            Some(TransferOutcome::NotPublished),
            Some(TransferFailureKind::StaleScope)
        )
    );
    assert_eq!(state("2").1, Some(TransferOutcome::Unknown));
    assert_eq!(state("3").0, TransferState::Completed);
    list.clear_finished();
    assert!(list.items.is_empty());
}

// The wire.

#[test]
fn maps_file_service_events_and_refuses_a_malformed_one() {
    let payload = json!({
        "operationId": "op",
        "rootToken": "root",
        "activeRoot": {"paneId": "%1", "root": "/repo", "rootToken": "root", "rootGeneration": "1"},
        "directory": {"path": "/repo", "generation": "3", "entries": [], "complete": true},
        "metadata": {"path": "/repo/a.txt", "name": "a.txt", "kind": "file", "size": "4"},
    });
    let events = wire::decode_events(&payload.to_string()).unwrap();
    assert!(matches!(&events[0], WorkspaceEvent::RootChanged(root) if root.token == "root"));
    assert!(matches!(
        &events[1],
        WorkspaceEvent::DirectorySnapshot { listing, .. } if listing.revision == "3"
    ));
    assert!(matches!(
        &events[2],
        WorkspaceEvent::FileChanged { entry: Some(entry), .. } if entry.size_bytes == "4"
    ));
    let undrawable = json!({
        "operationId": "op", "rootToken": "root",
        "metadata": {"path": "/repo/a", "kind": "unspecified"},
    });
    assert!(matches!(
        &wire::decode_events(&undrawable.to_string()).unwrap()[0],
        WorkspaceEvent::FileChanged { entry: None, .. }
    ));
    let deleted = json!({
        "operationId": "op", "rootToken": "root", "deleted": true,
        "metadata": {"path": "/repo/a"},
    });
    assert!(matches!(
        &wire::decode_events(&deleted.to_string()).unwrap()[0],
        WorkspaceEvent::FileDeleted { path, .. } if path == "/repo/a"
    ));
    assert!(wire::decode_events(r#"{"rootToken":"root"}"#).is_err());
    assert!(wire::decode_events("not json").is_err());
}

#[test]
fn refuses_a_search_match_outside_the_directory_asked_about() {
    let answer = |path: &str, relative: &str| {
        json!({"search": {"complete": true, "matches": [
            {"path": path, "relativePath": relative, "score": 3}
        ]}})
    };
    let found = wire::decode_search(answer("/repo/src/a.rs", "src/a.rs"), "/repo/").unwrap();
    assert_eq!(found.matches[0].relative_path, "src/a.rs");
    for (path, relative) in [
        ("/etc/passwd", "../etc/passwd"),
        ("/repo/a", "/repo/a"),
        ("/other/a", "a"),
    ] {
        assert!(
            wire::decode_search(answer(path, relative), "/repo").is_err(),
            "{path}"
        );
    }
    assert!(wire::decode_search(json!({"search": {"matches": []}}), "/repo").is_err());
}

fn pane(overrides: Value) -> tmux_control::Pane {
    let mut value = json!({
        "id": "%1", "sessionId": "$1", "windowId": "@1", "index": 0, "active": true,
        "width": 80, "height": 24, "left": 0, "top": 0, "currentPath": "/repo",
        "currentCommand": "zsh", "panePid": 1, "startCommand": "", "title": "",
    });
    for (key, field) in overrides.as_object().unwrap() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).unwrap()
}

#[test]
fn opens_a_terminal_path_only_while_its_pane_is_where_it_was() {
    let captured = pane(json!({}));
    let live = [captured.clone()];
    assert!(wire::current_terminal_file_pane(&captured, &live, 7, 7).is_some());
    // An unrelated topology change advances the generation without moving it.
    assert!(wire::current_terminal_file_pane(&captured, &live, 7, 9).is_some());
    assert!(wire::current_terminal_file_pane(&captured, &live, 8, 7).is_none());
    for moved in [
        json!({"currentPath": "/other"}),
        json!({"windowId": "@2"}),
        json!({"sessionId": "$2"}),
    ] {
        assert!(wire::current_terminal_file_pane(&captured, &[pane(moved)], 7, 7).is_none());
    }
    assert!(wire::current_terminal_file_pane(&captured, &[], 7, 7).is_none());
}

#[test]
fn makes_no_request_scope_in_the_snapshot_before_connected_gap() {
    let picked = selection("%1");
    assert_eq!(
        live_file_scope(false, Some("client"), 41, 7, Some(&picked)),
        None
    );
    let live = live_file_scope(true, Some("client"), 41, 7, Some(&picked)).unwrap();
    assert_eq!(
        (
            live.client_id.as_str(),
            live.generation,
            live.terminal_epoch
        ),
        ("client", 7, 41)
    );
    assert_eq!(live_file_scope(true, None, 41, 7, Some(&picked)), None);
    assert_eq!(live_file_scope(true, Some(""), 41, 7, Some(&picked)), None);
    assert_eq!(
        live_file_scope(true, Some("client"), 0, 7, Some(&picked)),
        None
    );
    assert_eq!(live_file_scope(true, Some("client"), 41, 7, None), None);
}

#[test]
fn keys_a_scope_by_its_connection_and_a_download_by_less() {
    let base = scope();
    let moved = FileScope {
        generation: 9,
        ..base.clone()
    };
    assert_eq!(
        base.key(),
        moved.key(),
        "a topology generation is not a new scope"
    );
    let reconnected = FileScope {
        client_id: "other".into(),
        ..base.clone()
    };
    assert_ne!(base.key(), reconnected.key());
    assert_ne!(base.transfer_key(), reconnected.transfer_key());
    let other_pane = FileScope {
        pane_id: "%2".into(),
        ..base.clone()
    };
    assert_ne!(base.key(), other_pane.key());
    assert_eq!(base.transfer_key(), other_pane.transfer_key());
}

#[test]
fn builds_each_mutation_the_dialog_describes() {
    let root = active_root("root", "/repo", "%1");
    let folder = entry("/repo/src", true);
    let file = entry("/repo/a.txt", false);
    let build = |action, entry: Option<&FileEntry>, value: &str| {
        MutationDialog::mutation(action, entry, &root, value, true, false)
    };
    assert_eq!(
        build(MutationAction::NewFile, Some(&folder), " b.txt "),
        Some(FileMutation::CreateFile {
            parent: "/repo/src".into(),
            name: "b.txt".into()
        })
    );
    assert_eq!(
        build(MutationAction::NewDirectory, Some(&file), "lib"),
        Some(FileMutation::CreateDirectory {
            parent: "/repo".into(),
            name: "lib".into()
        })
    );
    assert_eq!(build(MutationAction::NewFile, None, "  "), None);
    assert_eq!(
        build(MutationAction::Delete, Some(&folder), ""),
        Some(FileMutation::Delete {
            path: "/repo/src".into(),
            confirmed_non_empty: true
        })
    );
    assert_eq!(
        build(MutationAction::Move, Some(&file), "/repo/b.txt"),
        Some(FileMutation::Move(Relocation {
            path: "/repo/a.txt".into(),
            destination: "/repo/b.txt".into(),
            overwrite: true,
            confirmed_non_empty: false,
        }))
    );
    assert_eq!(build(MutationAction::Rename, None, "x"), None);
    let created = build(MutationAction::NewFile, None, "n").unwrap();
    let command = wire::command::mutate(&root, &created, "op");
    assert_eq!(
        (
            &command["operation"],
            &command["mutation"],
            &command["path"]
        ),
        (&json!("mutate"), &json!("create"), &json!("/repo/n"))
    );
    assert_eq!(command["rootToken"], "root");
}

#[test]
fn relocates_open_file_tabs_below_a_moved_path() {
    let tab = |id: &str, resource: &str, kind: &str| -> AppTabRecord {
        serde_json::from_value(json!({
            "id": id, "hostProfileId": "local", "serverIdentity": "server", "sessionId": "$1",
            "sessionName": "main", "kind": kind, "resource": resource, "title": "x", "order": 0,
        }))
        .unwrap()
    };
    let mut state = PersistedAppState {
        app_tabs: vec![
            tab("a", "/repo/src/a.rs", "file"),
            tab("b", "/repo/src", "markdown"),
            tab("c", "/repo/srcery/c.rs", "file"),
            tab("d", "/repo/src/d.rs", "gitDiff"),
        ],
        ..PersistedAppState::default()
    };
    assert!(relocate_file_tabs(
        &mut state,
        "local",
        "server",
        "/repo",
        "/repo/src",
        "./lib/"
    ));
    let tabs: Vec<(&str, &str)> = state
        .app_tabs
        .iter()
        .map(|tab| (tab.resource.as_str(), tab.title.as_str()))
        .collect();
    assert_eq!(
        tabs,
        [
            ("/repo/lib/a.rs", "a.rs"),
            ("/repo/lib", "lib"),
            ("/repo/srcery/c.rs", "x"),
            ("/repo/src/d.rs", "x")
        ]
    );
    assert!(!relocate_file_tabs(
        &mut state,
        "other",
        "server",
        "/repo",
        "/repo/lib",
        "/x"
    ));
}

// The Explorer, against a scripted host.

type Reply = oneshot::Sender<Result<Value, String>>;

/// Every command the Explorer sent, and the answers still owed.
#[derive(Default)]
struct Host {
    asked: RefCell<Vec<Value>>,
    owed: RefCell<Vec<(Value, Option<Reply>)>>,
    cancelled: RefCell<Vec<String>>,
    started: RefCell<Vec<(DownloadStart, Arc<dyn EventSink>)>>,
    refuse_start: RefCell<Option<String>>,
}

fn operation(command: &Value) -> &str {
    command["operation"].as_str().unwrap_or_default()
}

fn path(command: &Value) -> &str {
    command["path"].as_str().unwrap_or_default()
}

impl Host {
    /// The paths asked about by one kind of command, in order.
    fn asked(&self, kind: &str) -> Vec<String> {
        self.asked
            .borrow()
            .iter()
            .filter(|command| operation(command) == kind)
            .map(|command| path(command).to_owned())
            .collect()
    }

    fn last(&self, kind: &str) -> Value {
        let asked = self.asked.borrow();
        let found = asked
            .iter()
            .rev()
            .find(|command| operation(command) == kind);
        found
            .cloned()
            .unwrap_or_else(|| panic!("no {kind} was asked"))
    }

    /// The paths of the commands of one kind still waiting on the host.
    fn owed(&self, kind: &str) -> Vec<String> {
        self.owed
            .borrow()
            .iter()
            .filter(|(command, reply)| {
                operation(command) == kind
                    && reply.as_ref().is_some_and(|reply| !reply.is_canceled())
            })
            .map(|(command, _)| path(command).to_owned())
            .collect()
    }

    /// `operation:path` of every read the Explorer told the host to stop.
    fn stopped(&self) -> Vec<String> {
        let asked = self.asked.borrow();
        self.cancelled
            .borrow()
            .iter()
            .filter_map(|id| asked.iter().find(|command| command["operationId"] == **id))
            .map(|command| format!("{}:{}", operation(command), path(command)))
            .collect()
    }

    /// Answers the oldest command of this kind still owed for this path.
    fn answer(&self, kind: &str, at: &str, answer: Result<Value, String>) {
        let reply = self
            .owed
            .borrow_mut()
            .iter_mut()
            .find(|(command, reply)| {
                operation(command) == kind
                    && path(command) == at
                    && reply.as_ref().is_some_and(|reply| !reply.is_canceled())
            })
            .and_then(|(_, reply)| reply.take())
            .unwrap_or_else(|| panic!("nothing owed for {kind} {at}"));
        reply.send(answer).ok();
    }
}

struct Transport(Rc<Host>);

impl FileTransport for Transport {
    fn request(
        &self,
        _client_id: String,
        command: Value,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let (reply, answer) = oneshot::channel();
        self.0.asked.borrow_mut().push(command.clone());
        self.0.owed.borrow_mut().push((command, Some(reply)));
        async move { answer.await.unwrap_or_else(|_| Err("dropped".into())) }.boxed()
    }

    fn cancel(&self, _client_id: String, operation_id: String) {
        self.0.cancelled.borrow_mut().push(operation_id);
    }

    fn start_download(
        &self,
        request: DownloadStart,
        events: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        if let Some(error) = self.0.refuse_start.borrow().clone() {
            return Err(error);
        }
        self.0.started.borrow_mut().push((request, events));
        Ok("t1".into())
    }

    fn cancel_download(&self, _transfer_id: String) -> Result<(), String> {
        Ok(())
    }

    fn suggest_destination(&self, file_name: String) -> Result<String, String> {
        Ok(format!("/downloads/{file_name}"))
    }

    fn open_download(&self, _path: String) -> Result<(), String> {
        Ok(())
    }

    fn reveal_download(&self, _path: String) -> Result<(), String> {
        Ok(())
    }
}

fn meta(path: &str, directory: bool) -> Value {
    json!({
        "path": path, "name": path.rsplit('/').next().unwrap(),
        "kind": if directory { "directory" } else { "file" }, "size": "1",
        "modifiedUnixMillis": 1, "mode": 420, "expandable": directory, "generation": "1",
    })
}

fn page(path: &str, entries: &[(&str, bool)], next: Option<&str>) -> Value {
    json!({"directory": {
        "path": path, "generation": "5",
        "entries": entries.iter().map(|(entry, directory)| meta(entry, *directory)).collect::<Vec<_>>(),
        "complete": next.is_none(), "nextPageToken": next.unwrap_or_default(),
    }})
}

fn directory(path: &str, entries: &[(&str, bool)]) -> Value {
    page(path, entries, None)
}

fn root_answer(token: &str, path: &str, pane: &str) -> Value {
    json!({"activeRoot": {
        "paneId": pane, "root": path, "rootToken": token, "gitWorktree": true,
        "serverIdentity": "server", "topologyGeneration": "7", "rootGeneration": "1",
    }})
}

fn inputs(pane: &str, cwd: &str, client: &str) -> Inputs {
    let picked = selection(pane);
    Inputs {
        scope: live_file_scope(true, Some(client), 41, 7, Some(&picked)),
        route: Some(PaneRoute {
            session_id: "$1".into(),
            window_id: "@1".into(),
            cwd: cwd.into(),
        }),
        selection: Some(picked),
        can_mutate: true,
    }
}

fn update<R>(
    explorer: &Entity<Explorer>,
    cx: &mut TestAppContext,
    f: impl FnOnce(&mut Explorer, &mut Context<Explorer>) -> R,
) -> R {
    let result = explorer.update(cx, f);
    cx.run_until_parked();
    result
}

fn set(explorer: &Entity<Explorer>, inputs: Inputs, cx: &mut TestAppContext) {
    update(explorer, cx, |explorer, cx| explorer.set_inputs(inputs, cx));
}

fn read<R>(
    explorer: &Entity<Explorer>,
    cx: &mut TestAppContext,
    f: impl FnOnce(&Explorer) -> R,
) -> R {
    explorer.read_with(cx, |explorer, _| f(explorer))
}

fn rows(explorer: &Entity<Explorer>, cx: &mut TestAppContext) -> Vec<String> {
    read(explorer, cx, |explorer| {
        explorer.view.rows.iter().map(|row| row.key()).collect()
    })
}

fn unwatches(host: &Host) -> usize {
    host.asked
        .borrow()
        .iter()
        .filter(|command| operation(command) == "unwatchDirectory")
        .count()
}

/// An Explorer on `/repo`, its root listed by the watch that arms it.
fn opened(cx: &mut TestAppContext) -> (Rc<Host>, Entity<Explorer>) {
    let host = Rc::new(Host::default());
    let explorer = cx.new(|cx| Explorer::new(Rc::new(Transport(host.clone())), cx));
    set(&explorer, inputs("%1", "/repo", "client"), cx);
    assert_eq!(host.last("resolveActiveRoot")["knownRootToken"], "");
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("root", "/repo", "%1")),
    );
    cx.run_until_parked();
    assert_eq!(host.owed("watchDirectory"), ["/repo"]);
    host.answer(
        "watchDirectory",
        "/repo",
        Ok(directory(
            "/repo",
            &[("/repo/src", true), ("/repo/a.txt", false)],
        )),
    );
    cx.run_until_parked();
    (host, explorer)
}

#[gpui_kit::test]
fn publishes_the_resolved_root_for_agent_launches_and_withdraws_it(cx: &mut TestAppContext) {
    let host = Rc::new(Host::default());
    let explorer = cx.new(|cx| Explorer::new(Rc::new(Transport(host.clone())), cx));
    // As `init` wires it.
    cx.update(|cx| {
        cx.observe(&explorer, |explorer, cx| super::publish_root(&explorer, cx))
            .detach()
    });
    let held = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            cx.try_global::<AgentRoot>()
                .map(|root| (root.path.clone(), root.token.clone(), root.git_worktree))
        })
    };
    set(&explorer, inputs("%1", "/repo", "client"), cx);
    assert_eq!(
        held(cx),
        None,
        "nothing to launch under before the root resolves"
    );
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("root", "/repo", "%1")),
    );
    cx.run_until_parked();
    assert_eq!(held(cx), Some(("/repo".into(), "root".into(), true)));
    set(&explorer, inputs("%2", "/other", "client"), cx);
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("other", "/other", "%2")),
    );
    cx.run_until_parked();
    assert_eq!(held(cx), Some(("/other".into(), "other".into(), true)));
    set(&explorer, Inputs::default(), cx);
    assert_eq!(held(cx), None, "a lost root is withdrawn");
}

#[gpui_kit::test]
fn lists_the_root_through_its_watch_and_nothing_else(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    assert_eq!(rows(&explorer, cx), ["/repo/src", "/repo/a.txt"]);
    assert!(host.asked("listDirectory").is_empty());
    assert!(read(&explorer, cx, |explorer| explorer
        .tree
        .loading
        .is_empty()));
}

#[gpui_kit::test]
fn expands_and_collapses_through_watches_alone(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    assert!(read(&explorer, cx, |explorer| explorer
        .tree
        .loading
        .contains("/repo/src")));
    host.answer(
        "watchDirectory",
        "/repo/src",
        Ok(directory("/repo/src", &[("/repo/src/main.rs", false)])),
    );
    cx.run_until_parked();
    assert_eq!(
        rows(&explorer, cx),
        ["/repo/src", "/repo/src/main.rs", "/repo/a.txt"]
    );
    assert_eq!(read(&explorer, cx, |explorer| explorer.cached()), 2);
    // Collapsing gives the watch back: one unwatch, no list.
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    assert_eq!(unwatches(&host), 1);
    assert_eq!(
        read(&explorer, cx, |explorer| explorer.watched()),
        ["/repo"]
    );
    // Expanding again paints at once and revalidates behind the paint.
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    assert_eq!(
        rows(&explorer, cx),
        ["/repo/src", "/repo/src/main.rs", "/repo/a.txt"]
    );
    assert!(!read(&explorer, cx, |explorer| explorer
        .tree
        .loading
        .contains("/repo/src")));
    assert_eq!(host.owed("watchDirectory"), ["/repo/src"]);
    assert!(host.asked("listDirectory").is_empty());
}

#[gpui_kit::test]
fn stops_a_bootstrap_the_tree_stopped_wanting(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    assert_eq!(host.stopped(), ["watchDirectory:/repo/src"]);
    // The host registers a watch before it lists, so the unwatch waits for
    // the request to settle, however it settles.
    assert_eq!(unwatches(&host), 0);
    host.answer("watchDirectory", "/repo/src", Err("cancelled".into()));
    cx.run_until_parked();
    assert_eq!(unwatches(&host), 1, "a watch still arming is given back");
    assert!(read(&explorer, cx, |explorer| explorer
        .tree
        .error
        .is_none()));
    assert_eq!(
        read(&explorer, cx, |explorer| explorer.watched()),
        ["/repo"]
    );
}

#[gpui_kit::test]
fn keeps_the_tree_across_a_same_root_pane_switch_and_refuses_interaction_meanwhile(
    cx: &mut TestAppContext,
) {
    let (host, explorer) = opened(cx);
    set(&explorer, inputs("%2", "/repo", "client"), cx);
    assert!(!read(&explorer, cx, |explorer| explorer.paints_selection()));
    assert_eq!(
        rows(&explorer, cx),
        ["/repo/src", "/repo/a.txt"],
        "the tree stays painted"
    );
    // Watches are withheld while stale, and nothing a person does acts.
    assert!(read(&explorer, cx, |explorer| explorer
        .watched()
        .is_empty()));
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    assert!(!read(&explorer, cx, |explorer| explorer
        .tree
        .expanded
        .contains("/repo/src")));
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("root", "/repo", "%2")),
    );
    cx.run_until_parked();
    assert!(read(&explorer, cx, |explorer| explorer.paints_selection()));
    assert_eq!(
        rows(&explorer, cx),
        ["/repo/src", "/repo/a.txt"],
        "adopted without a repaint"
    );
    assert_eq!(host.owed("watchDirectory"), ["/repo"]);
    assert!(host.asked("listDirectory").is_empty());
}

#[gpui_kit::test]
fn swaps_to_a_genuinely_different_root_once_it_resolves(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    set(&explorer, inputs("%2", "/other", "client"), cx);
    assert_eq!(rows(&explorer, cx), ["/repo/src", "/repo/a.txt"]);
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("other", "/other", "%2")),
    );
    cx.run_until_parked();
    read(&explorer, cx, |explorer| {
        assert_eq!(explorer.tree.root.as_ref().unwrap().path, "/other");
        assert!(explorer.tree.listings.is_empty());
        assert!(explorer.tree.loading.contains("/other"));
        assert_eq!(
            explorer.cached(),
            0,
            "nothing the old root authorised survives"
        );
    });
    assert!(rows(&explorer, cx).is_empty());
    assert_eq!(host.owed("watchDirectory"), ["/other"]);
}

#[gpui_kit::test]
fn keeps_the_tree_through_a_transport_gap_and_revalidates_when_connected(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| explorer.refresh(None, cx));
    assert_eq!(
        read(&explorer, cx, |explorer| explorer.requested_reads()),
        1
    );
    let mut gap = inputs("%1", "/repo", "client");
    gap.scope = None;
    set(&explorer, gap, cx);
    assert_eq!(rows(&explorer, cx), ["/repo/src", "/repo/a.txt"]);
    assert_eq!(
        read(&explorer, cx, |explorer| explorer.requested_reads()),
        0
    );
    assert!(host.stopped().contains(&"listDirectory:/repo".to_owned()));
    assert!(host.owed("resolveActiveRoot").is_empty());
    set(&explorer, inputs("%1", "/repo", "reconnected"), cx);
    assert_eq!(host.owed("resolveActiveRoot").len(), 1);
    assert_eq!(host.last("resolveActiveRoot")["knownRootToken"], "root");
}

#[gpui_kit::test]
fn patches_from_precise_events_and_lists_only_when_it_cannot(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let changed = |path: &str, entry: Option<FileEntry>| WorkspaceEvent::FileChanged {
        root_token: "root".into(),
        path: path.into(),
        entry,
        operation_id: String::new(),
        generation: String::new(),
    };
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            changed("/repo/b.txt", Some(entry("/repo/b.txt", false))),
            cx,
        );
        explorer.apply_event(
            changed("/repo/c.txt", Some(entry("/repo/c.txt", false))),
            cx,
        );
        explorer.apply_event(
            WorkspaceEvent::FileDeleted {
                root_token: "root".into(),
                path: "/repo/a.txt".into(),
            },
            cx,
        );
        // Another root's event is not this tree's.
        explorer.apply_event(
            WorkspaceEvent::FileChanged {
                root_token: "other".into(),
                path: "/repo/z.txt".into(),
                entry: Some(entry("/repo/z.txt", false)),
                operation_id: String::new(),
                generation: String::new(),
            },
            cx,
        );
    });
    assert_eq!(
        rows(&explorer, cx),
        ["/repo/src", "/repo/b.txt", "/repo/c.txt"]
    );
    assert!(host.asked("listDirectory").is_empty());
    // An entry the host could not describe is a gap: one coalesced list.
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(changed("/repo/d", None), cx);
        explorer.apply_event(changed("/repo/e", None), cx);
    });
    assert!(host.asked("listDirectory").is_empty());
    cx.executor().advance_clock(RECOVERY_COALESCE);
    cx.run_until_parked();
    assert_eq!(host.asked("listDirectory"), ["/repo"]);
}

#[gpui_kit::test]
fn collapsing_cancels_a_recovery_not_yet_issued(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    host.answer(
        "watchDirectory",
        "/repo/src",
        Ok(directory("/repo/src", &[])),
    );
    cx.run_until_parked();
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::FileChanged {
                root_token: "root".into(),
                path: "/repo/src/x".into(),
                entry: None,
                operation_id: String::new(),
                generation: String::new(),
            },
            cx,
        );
        explorer.toggle_directory("/repo/src", cx);
    });
    cx.executor().advance_clock(RECOVERY_COALESCE * 2);
    cx.run_until_parked();
    assert!(host.asked("listDirectory").is_empty());
}

#[gpui_kit::test]
fn prunes_a_deleted_directory_and_stops_reading_it(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    host.answer(
        "watchDirectory",
        "/repo/src",
        Ok(directory("/repo/src", &[])),
    );
    cx.run_until_parked();
    update(&explorer, cx, |explorer, cx| {
        explorer.refresh(Some("/repo/src".into()), cx)
    });
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::FileDeleted {
                root_token: "root".into(),
                path: "/repo/src".into(),
            },
            cx,
        )
    });
    assert_eq!(rows(&explorer, cx), ["/repo/a.txt"]);
    assert!(
        host.stopped()
            .contains(&"listDirectory:/repo/src".to_owned())
    );
    assert_eq!(
        read(&explorer, cx, |explorer| explorer.watched()),
        ["/repo"]
    );
    assert_eq!(read(&explorer, cx, |explorer| explorer.cached()), 1);
}

#[gpui_kit::test]
fn lists_a_refused_watch_and_retries_it_after_the_backoff(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    host.answer(
        "watchDirectory",
        "/repo/src",
        Err("watch limit reached".into()),
    );
    cx.run_until_parked();
    assert_eq!(
        unwatches(&host),
        1,
        "the host counts a watch it failed to list"
    );
    assert_eq!(
        host.asked("listDirectory"),
        ["/repo/src"],
        "a refused watch still gets its listing"
    );
    host.answer(
        "listDirectory",
        "/repo/src",
        Ok(directory("/repo/src", &[("/repo/src/main.rs", false)])),
    );
    cx.run_until_parked();
    assert_eq!(
        rows(&explorer, cx),
        ["/repo/src", "/repo/src/main.rs", "/repo/a.txt"]
    );
    // Nothing asks the host again inside the backoff.
    update(&explorer, cx, |explorer, cx| {
        explorer.refresh(Some("/repo".into()), cx)
    });
    assert_eq!(host.asked("watchDirectory"), ["/repo", "/repo/src"]);
    cx.executor()
        .advance_clock(WATCH_RETRY_BASE + Duration::from_millis(10));
    cx.run_until_parked();
    assert_eq!(
        host.asked("watchDirectory"),
        ["/repo", "/repo/src", "/repo/src"]
    );
}

#[gpui_kit::test]
fn shows_the_failed_listing_of_a_refused_watch_as_the_watch_error(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    host.answer(
        "watchDirectory",
        "/repo/src",
        Err("watch limit reached".into()),
    );
    cx.run_until_parked();
    host.answer(
        "listDirectory",
        "/repo/src",
        Err("permission denied".into()),
    );
    cx.run_until_parked();
    read(&explorer, cx, |explorer| {
        assert_eq!(explorer.tree.error.as_deref(), Some("watch limit reached"));
        assert!(!explorer.tree.loading.contains("/repo/src"));
    });
}

#[gpui_kit::test]
fn restores_the_pages_an_authoritative_rescan_would_have_dropped(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let rescan = wire::decode_listing(page("/repo", &[("/repo/a", false)], Some("p2")), "root");
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::DirectorySnapshot {
                root_token: "root".into(),
                listing: rescan.unwrap(),
            },
            cx,
        )
    });
    assert_eq!(
        rows(&explorer, cx),
        ["/repo/src", "/repo/a.txt"],
        "no rows are dropped meanwhile"
    );
    host.answer(
        "listDirectory",
        "/repo",
        Ok(page("/repo", &[("/repo/a", false)], Some("p2"))),
    );
    cx.run_until_parked();
    host.answer(
        "listDirectory",
        "/repo",
        Ok(directory("/repo", &[("/repo/b", false)])),
    );
    cx.run_until_parked();
    let tokens: Vec<Value> = host
        .asked
        .borrow()
        .iter()
        .filter(|command| operation(command) == "listDirectory")
        .map(|command| command["pageToken"].clone())
        .collect();
    assert_eq!(tokens, [json!(""), json!("p2")]);
    assert_eq!(rows(&explorer, cx), ["/repo/a", "/repo/b"]);
}

#[gpui_kit::test]
fn prefetches_exactly_one_more_page(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    host.answer(
        "watchDirectory",
        "/repo/src",
        Ok(page("/repo/src", &[("/repo/src/a", false)], Some("p2"))),
    );
    cx.run_until_parked();
    assert_eq!(host.owed("listDirectory"), ["/repo/src"]);
    host.answer(
        "listDirectory",
        "/repo/src",
        Ok(page("/repo/src", &[("/repo/src/b", false)], Some("p3"))),
    );
    cx.run_until_parked();
    assert!(
        host.owed("listDirectory").is_empty(),
        "never a second speculative page"
    );
    assert_eq!(
        rows(&explorer, cx),
        [
            "/repo/src",
            "/repo/src/a",
            "/repo/src/b",
            "/repo/src\0more",
            "/repo/a.txt"
        ]
    );
    update(&explorer, cx, |explorer, cx| {
        explorer.load_more("/repo/src", cx)
    });
    assert_eq!(host.owed("listDirectory"), ["/repo/src"]);
    assert_eq!(host.last("listDirectory")["pageToken"], "p3");
}

#[gpui_kit::test]
fn slows_the_root_backstop_once_settled_and_probes_on_a_cd(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let probes = |host: &Host| host.asked("resolveActiveRoot").len();
    let tick = |cx: &mut TestAppContext| {
        cx.executor().advance_clock(ROOT_BACKSTOP);
        cx.run_until_parked();
    };
    for count in 1..=ROOT_STABLE_PROBES as usize {
        tick(cx);
        assert_eq!(probes(&host), 1 + count);
        host.answer(
            "resolveActiveRoot",
            "",
            Ok(root_answer("root", "/repo", "%1")),
        );
        cx.run_until_parked();
    }
    let settled = probes(&host);
    for _ in ROOT_STABLE_PROBES + 1..ROOT_SETTLED_MULTIPLIER {
        tick(cx);
    }
    assert_eq!(
        probes(&host),
        settled,
        "a settled root is checked less often"
    );
    tick(cx);
    assert_eq!(probes(&host), settled + 1, "but never not at all");
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("root", "/repo", "%1")),
    );
    cx.run_until_parked();
    set(&explorer, inputs("%1", "/repo/src", "client"), cx);
    assert_eq!(
        probes(&host),
        settled + 2,
        "a cd in the pane is checked at once"
    );
}

#[gpui_kit::test]
fn treats_a_root_announcement_as_a_reason_to_check(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::RootChanged(active_root("moved", "/moved", "%1")),
            cx,
        )
    });
    assert_eq!(host.owed("resolveActiveRoot").len(), 1);
    let held = read(&explorer, cx, |explorer| {
        explorer.tree.root.clone().unwrap()
    });
    assert_eq!(held.token, "root", "the broadcast itself is never adopted");
}

#[gpui_kit::test]
fn refuses_a_change_while_read_only_and_follows_a_rename(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let moves = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        let moves = moves.clone();
        cx.subscribe(&explorer, move |_, event: &ExplorerEvent, _| {
            if let ExplorerEvent::Relocated {
                source,
                destination,
                ..
            } = event
            {
                moves
                    .borrow_mut()
                    .push((source.clone(), destination.clone()));
            }
        })
        .detach();
    });
    let mut frozen = inputs("%1", "/repo", "client");
    frozen.can_mutate = false;
    set(&explorer, frozen, cx);
    let refused = update(&explorer, cx, |explorer, cx| {
        explorer.mutate(
            FileMutation::Delete {
                path: "/repo/a.txt".into(),
                confirmed_non_empty: false,
            },
            cx,
        )
    });
    assert_eq!(
        cx.foreground_executor().block_test(refused),
        Err("File changes are unavailable while the host is disconnected.".into())
    );
    assert!(host.asked("mutate").is_empty());
    set(&explorer, inputs("%1", "/repo", "client"), cx);
    let renamed = update(&explorer, cx, |explorer, cx| {
        explorer.mutate(
            FileMutation::Rename(Relocation {
                path: "/repo/src/a.rs".into(),
                destination: "/repo/src/b.rs".into(),
                overwrite: false,
                confirmed_non_empty: false,
            }),
            cx,
        )
    });
    host.answer("mutate", "/repo/src/a.rs", Ok(json!({})));
    cx.run_until_parked();
    assert_eq!(cx.foreground_executor().block_test(renamed), Ok(()));
    assert_eq!(
        *moves.borrow(),
        [("/repo/src/a.rs".to_owned(), "/repo/src/b.rs".to_owned())]
    );
    assert_eq!(
        host.owed("listDirectory"),
        ["/repo/src"],
        "the directory it touched is read again"
    );
    assert_eq!(
        read(&explorer, cx, |explorer| explorer.requested_reads()),
        1
    );
}

#[gpui_kit::test]
fn the_keyboard_follows_a_renamed_file(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, _| {
        explorer.view.focus_key = Some("/repo/a.txt".into());
    });
    let renamed = update(&explorer, cx, |explorer, cx| {
        explorer.mutate(
            FileMutation::Rename(Relocation {
                path: "/repo/a.txt".into(),
                destination: "/repo/z.txt".into(),
                overwrite: false,
                confirmed_non_empty: false,
            }),
            cx,
        )
    });
    host.answer("mutate", "/repo/a.txt", Ok(json!({})));
    cx.run_until_parked();
    assert_eq!(cx.foreground_executor().block_test(renamed), Ok(()));
    // Another file took the renamed one's place in the list.
    host.answer(
        "listDirectory",
        "/repo",
        Ok(directory(
            "/repo",
            &[
                ("/repo/src", true),
                ("/repo/m.txt", false),
                ("/repo/z.txt", false),
            ],
        )),
    );
    cx.run_until_parked();
    assert_eq!(
        rows(&explorer, cx),
        ["/repo/src", "/repo/m.txt", "/repo/z.txt"]
    );
    let focused = read(&explorer, cx, |explorer| explorer.view.focus_key.clone());
    assert_eq!(focused.as_deref(), Some("/repo/z.txt"));
}

#[gpui_kit::test]
fn shows_a_failed_change_and_reads_nothing(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let created = update(&explorer, cx, |explorer, cx| {
        explorer.mutate(
            FileMutation::CreateFile {
                parent: "/repo".into(),
                name: "a.txt".into(),
            },
            cx,
        )
    });
    host.answer("mutate", "/repo/a.txt", Err("already exists".into()));
    cx.run_until_parked();
    assert_eq!(
        cx.foreground_executor().block_test(created),
        Err("already exists".into())
    );
    assert!(host.asked("listDirectory").is_empty());
}

fn statuses(explorer: &Entity<Explorer>, cx: &mut TestAppContext) -> Rc<RefCell<Vec<String>>> {
    let statuses = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        let statuses = statuses.clone();
        cx.subscribe(explorer, move |_, event: &ExplorerEvent, _| {
            if let ExplorerEvent::Status(message) = event {
                statuses.borrow_mut().push(message.clone());
            }
        })
        .detach();
    });
    statuses
}

#[gpui_kit::test]
fn downloads_through_the_live_connection_and_follows_its_events(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let statuses = statuses(&explorer, cx);
    let root = read(&explorer, cx, |explorer| {
        explorer.tree.root.clone().unwrap()
    });
    let begin = |cx: &mut TestAppContext, origin| {
        let root = root.clone();
        update(&explorer, cx, move |explorer, cx| {
            explorer.begin_download(&root, origin, cx)
        })
    };
    let scope = begin(cx, DownloadOrigin::Explorer).unwrap();
    assert!(
        begin(cx, DownloadOrigin::Explorer).is_none(),
        "one save panel at a time"
    );
    let picked = Ok(Some("/downloads/image.png".to_owned()));
    let chosen = root.clone();
    update(&explorer, cx, move |explorer, cx| {
        explorer.finish_picking(
            scope,
            chosen,
            file_intent(),
            DownloadOrigin::Explorer,
            picked,
            cx,
        )
    });
    let sink = {
        let started = host.started.borrow();
        let (request, events) = &started[0];
        assert!(request.overwrite, "the panel showed this exact name");
        assert_eq!(
            (request.source.as_str(), request.folder),
            ("/repo/image.png", false)
        );
        events.clone()
    };
    assert_eq!(
        statuses.borrow().last().unwrap(),
        "Download queued: /repo/image.png"
    );
    sink.send(EventBody::Json(download_event(json!({}))))
        .unwrap();
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    let state = |cx: &mut TestAppContext| {
        read(&explorer, cx, |explorer| {
            let item = explorer.downloads.get("t1").unwrap();
            (item.state, item.failure_kind)
        })
    };
    assert_eq!(state(cx), (TransferState::Running, None));
    // A pane switch keeps the connection's downloads; a reconnect retires them.
    set(&explorer, inputs("%2", "/repo", "client"), cx);
    assert_eq!(state(cx), (TransferState::Running, None));
    set(&explorer, inputs("%2", "/repo", "reconnected"), cx);
    assert_eq!(
        state(cx),
        (TransferState::Failed, Some(TransferFailureKind::StaleScope))
    );
    // A connection that moved while the panel was open starts nothing.
    let scope = begin(cx, DownloadOrigin::TabMenu).unwrap();
    set(&explorer, inputs("%2", "/repo", "again"), cx);
    let picked = Ok(Some("/downloads/other.png".to_owned()));
    update(&explorer, cx, move |explorer, cx| {
        explorer.finish_picking(
            scope,
            root,
            file_intent(),
            DownloadOrigin::TabMenu,
            picked,
            cx,
        )
    });
    assert_eq!(host.started.borrow().len(), 1);
    assert_eq!(
        statuses.borrow().last().map(String::as_str),
        Some(super::explorer::DOWNLOAD_CANCELLED)
    );
}

#[gpui_kit::test]
fn records_a_download_the_host_refused_to_start(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    *host.refuse_start.borrow_mut() = Some("destination exists".into());
    let root = read(&explorer, cx, |explorer| {
        explorer.tree.root.clone().unwrap()
    });
    let begun = root.clone();
    let scope = update(&explorer, cx, move |explorer, cx| {
        explorer.begin_download(&begun, DownloadOrigin::Explorer, cx)
    })
    .unwrap();
    let folder = DownloadIntent {
        path: "/repo/src".into(),
        kind: DownloadKind::Folder,
    };
    update(&explorer, cx, move |explorer, cx| {
        explorer.finish_picking(
            scope,
            root,
            folder,
            DownloadOrigin::Explorer,
            Ok(Some("/downloads/src".into())),
            cx,
        )
    });
    read(&explorer, cx, |explorer| {
        let refused = &explorer.downloads.items[0];
        assert_eq!(refused.state, TransferState::Failed);
        assert_eq!(refused.error.as_deref(), Some("destination exists"));
    });
}

#[gpui_kit::test]
fn needs_the_live_workspace_to_download(cx: &mut TestAppContext) {
    let host = Rc::new(Host::default());
    let explorer = cx.new(|cx| Explorer::new(Rc::new(Transport(host.clone())), cx));
    let statuses = statuses(&explorer, cx);
    let root = active_root("root", "/repo", "%1");
    let begun = update(&explorer, cx, |explorer, cx| {
        explorer.begin_download(&root, DownloadOrigin::FileSurface, cx)
    });
    assert!(begun.is_none());
    assert_eq!(
        *statuses.borrow(),
        ["Downloads require the active live file workspace."]
    );
    assert!(host.asked.borrow().is_empty());
}

#[gpui_kit::test]
fn supersedes_a_read_of_the_same_kind_and_leaves_a_page_alone(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    host.answer(
        "watchDirectory",
        "/repo/src",
        Ok(page("/repo/src", &[("/repo/src/a", false)], Some("p2"))),
    );
    cx.run_until_parked();
    // The prefetched page is in flight; two refreshes follow it.
    update(&explorer, cx, |explorer, cx| {
        explorer.refresh(Some("/repo/src".into()), cx)
    });
    update(&explorer, cx, |explorer, cx| {
        explorer.refresh(Some("/repo/src".into()), cx)
    });
    assert_eq!(host.stopped(), ["listDirectory:/repo/src"]);
    let owed: Vec<Value> = host
        .owed
        .borrow()
        .iter()
        .filter(|(command, reply)| {
            operation(command) == "listDirectory"
                && reply.as_ref().is_some_and(|reply| !reply.is_canceled())
        })
        .map(|(command, _)| command["pageToken"].clone())
        .collect();
    assert_eq!(owed, [json!("p2"), json!("")]);
    assert_eq!(
        read(&explorer, cx, |explorer| explorer.requested_reads()),
        1
    );
}

#[gpui_kit::test]
fn downloads_a_file_tab_under_its_own_captured_root(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    // A tab opened before a `cd` keeps the root it was opened under.
    let captured = active_root("earlier", "/repo/old", "%1");
    let begun = captured.clone();
    let explorer_origin = update(&explorer, cx, move |explorer, cx| {
        explorer.begin_download(&begun, DownloadOrigin::Explorer, cx)
    });
    assert!(
        explorer_origin.is_none(),
        "the Explorer acts only on the live root"
    );
    let begun = captured.clone();
    let scope = update(&explorer, cx, move |explorer, cx| {
        explorer.begin_download(&begun, DownloadOrigin::TabMenu, cx)
    })
    .unwrap();
    update(&explorer, cx, move |explorer, cx| {
        explorer.finish_picking(
            scope,
            captured,
            file_intent(),
            DownloadOrigin::TabMenu,
            Ok(Some("/downloads/image.png".into())),
            cx,
        )
    });
    let started = host.started.borrow();
    assert_eq!(started[0].0.root.token, "earlier");
}

// Restores and recoveries.

fn rescan(explorer: &Entity<Explorer>, cx: &mut TestAppContext) {
    let listing = wire::decode_listing(page("/repo", &[("/repo/a", false)], Some("p2")), "root");
    update(explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::DirectorySnapshot {
                root_token: "root".into(),
                listing: listing.unwrap(),
            },
            cx,
        )
    });
}

#[gpui_kit::test]
fn lets_a_waiting_gap_outrank_a_restore_and_keeps_it(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::FileChanged {
                root_token: "root".into(),
                path: "/repo/racing".into(),
                entry: None,
                operation_id: String::new(),
                generation: String::new(),
            },
            cx,
        )
    });
    rescan(&explorer, cx);
    assert!(
        host.asked("listDirectory").is_empty(),
        "the gap's list comes first, and puts the first page back itself"
    );
    assert_eq!(rows(&explorer, cx), ["/repo/src", "/repo/a.txt"]);
    cx.executor().advance_clock(RECOVERY_COALESCE);
    cx.run_until_parked();
    assert_eq!(
        host.asked("listDirectory"),
        ["/repo"],
        "the listing that arrived meanwhile did not cancel the recovery"
    );
}

#[gpui_kit::test]
fn keeps_the_rows_it_had_when_a_restore_fails(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    rescan(&explorer, cx);
    host.answer("listDirectory", "/repo", Err("connection lost".into()));
    cx.run_until_parked();
    assert_eq!(rows(&explorer, cx), ["/repo/src", "/repo/a.txt"]);
    read(&explorer, cx, |explorer| {
        assert!(!explorer.tree.loading.contains("/repo"));
        assert_eq!(explorer.requested_reads(), 0);
    });
    assert_eq!(
        host.asked("listDirectory").len(),
        1,
        "nothing retries on its own"
    );
}

#[gpui_kit::test]
fn never_reads_a_path_of_the_root_it_was_raised_under_against_its_successor(
    cx: &mut TestAppContext,
) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::FileChanged {
                root_token: "root".into(),
                path: "/repo/racing.txt".into(),
                entry: None,
                operation_id: String::new(),
                generation: String::new(),
            },
            cx,
        );
        explorer.rearm(cx);
    });
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("next", "/other", "%1")),
    );
    cx.run_until_parked();
    assert_eq!(
        read(&explorer, cx, |explorer| explorer
            .tree
            .root
            .clone()
            .unwrap()
            .token),
        "next"
    );
    cx.executor().advance_clock(RECOVERY_COALESCE * 2);
    cx.run_until_parked();
    assert!(!host.asked("listDirectory").contains(&"/repo".to_owned()));
}

#[gpui_kit::test]
fn rearms_its_watches_when_the_same_path_becomes_a_different_root(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    cx.executor().advance_clock(ROOT_BACKSTOP);
    cx.run_until_parked();
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("second", "/repo", "%1")),
    );
    cx.run_until_parked();
    assert_eq!(unwatches(&host), 1, "the old root's watch is given back");
    assert_eq!(host.owed("watchDirectory"), ["/repo"]);
    assert_eq!(host.last("watchDirectory")["rootToken"], "second");
    host.answer(
        "watchDirectory",
        "/repo",
        Ok(directory("/repo", &[("/repo/second", false)])),
    );
    cx.run_until_parked();
    assert_eq!(rows(&explorer, cx), ["/repo/second"]);
}

// The paint gate.

fn arm(pane: &str, cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.default_global::<PanePaintGate>()
            .arm(&format!("local\0{pane}"))
    });
}

fn painted(pane: &str, cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.default_global::<PanePaintGate>()
            .note_painted(&format!("local\0{pane}"))
    });
    cx.run_until_parked();
}

fn probes(host: &Host) -> usize {
    host.asked("resolveActiveRoot").len()
}

fn unopened(cx: &mut TestAppContext) -> (Rc<Host>, Entity<Explorer>) {
    let host = Rc::new(Host::default());
    let explorer = cx.new(|cx| Explorer::new(Rc::new(Transport(host.clone())), cx));
    (host, explorer)
}

#[gpui_kit::test]
fn holds_the_root_probe_behind_the_revealed_panes_first_paint(cx: &mut TestAppContext) {
    let (host, explorer) = unopened(cx);
    arm("%1", cx);
    set(&explorer, inputs("%1", "/repo", "client"), cx);
    assert_eq!(probes(&host), 0, "asked ahead of the pane's own screen");
    painted("%1", cx);
    assert_eq!(probes(&host), 1);
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("root", "/repo", "%1")),
    );
    cx.run_until_parked();
    assert!(read(&explorer, cx, |explorer| explorer.paints_selection()));
}

/// Port of `panePaintOrdering.test.tsx`: on a switch the Explorer and the
/// terminal surface observe the same change, and the surface may arm the pane
/// after the Explorer has asked about it. The probe must still wait.
#[gpui_kit::test]
fn holds_the_probe_when_the_pane_arms_later_in_the_same_switch(cx: &mut TestAppContext) {
    let (host, explorer) = unopened(cx);
    cx.update(|cx| {
        explorer.update(cx, |explorer, cx| {
            explorer.set_inputs(inputs("%1", "/repo", "client"), cx)
        });
        cx.default_global::<PanePaintGate>().arm("local\0%1");
    });
    cx.run_until_parked();
    assert_eq!(probes(&host), 0, "asked ahead of the pane's own screen");
    painted("%1", cx);
    assert_eq!(probes(&host), 1);
}

#[gpui_kit::test]
fn holds_the_probe_behind_the_paint_when_the_switch_also_moved_the_cwd(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    assert_eq!(probes(&host), 1);
    arm("%2", cx);
    set(&explorer, inputs("%2", "/elsewhere", "client"), cx);
    assert_eq!(probes(&host), 1, "the cwd change probed ahead of the paint");
    painted("%2", cx);
    assert_eq!(probes(&host), 2);
}

#[gpui_kit::test]
fn answers_refresh_in_front_of_the_paint_gate(cx: &mut TestAppContext) {
    let (host, explorer) = unopened(cx);
    arm("%1", cx);
    set(&explorer, inputs("%1", "/repo", "client"), cx);
    assert_eq!(probes(&host), 0);
    update(&explorer, cx, |explorer, cx| explorer.refresh(None, cx));
    assert_eq!(probes(&host), 1, "a person's Refresh waited out the paint");
    painted("%1", cx);
    assert_eq!(probes(&host), 1, "one probe at a time");
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("root", "/repo", "%1")),
    );
    cx.run_until_parked();
    assert!(read(&explorer, cx, |explorer| explorer.paints_selection()));
    arm("%1", cx);
    update(&explorer, cx, |explorer, cx| explorer.refresh(None, cx));
    assert_eq!(probes(&host), 2);
}

#[gpui_kit::test]
fn resolves_the_root_anyway_when_the_pane_never_paints(cx: &mut TestAppContext) {
    let (host, explorer) = unopened(cx);
    arm("%1", cx);
    set(&explorer, inputs("%1", "/repo", "client"), cx);
    cx.executor()
        .advance_clock(PANE_PAINT_TIMEOUT - Duration::from_millis(1));
    cx.run_until_parked();
    assert_eq!(probes(&host), 0);
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert_eq!(probes(&host), 1);
}

#[gpui_kit::test]
fn refuses_a_late_root_from_the_previously_active_pane(cx: &mut TestAppContext) {
    let (host, explorer) = unopened(cx);
    set(&explorer, inputs("%1", "/one", "client"), cx);
    set(&explorer, inputs("%2", "/two", "client"), cx);
    // The first pane's probe went with its selection; its answer has nowhere
    // to land.
    assert_eq!(host.stopped(), ["resolveActiveRoot:"]);
    assert_eq!(host.owed("resolveActiveRoot").len(), 1);
    assert_eq!(host.last("resolveActiveRoot")["paneId"], "%2");
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("two", "/two", "%2")),
    );
    cx.run_until_parked();
    let root = read(&explorer, cx, |explorer| {
        explorer.tree.root.clone().unwrap()
    });
    assert_eq!((root.token.as_str(), root.pane_id.as_str()), ("two", "%2"));
}

#[gpui_kit::test]
fn probes_only_while_the_window_can_be_seen_and_checks_on_return(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.set_foreground(false, cx)
    });
    for _ in 0..3 {
        cx.executor().advance_clock(ROOT_BACKSTOP);
        cx.run_until_parked();
    }
    update(&explorer, cx, |explorer, cx| explorer.rearm(cx));
    assert_eq!(probes(&host), 1, "a hidden window issues nothing");
    update(&explorer, cx, |explorer, cx| {
        explorer.set_foreground(true, cx)
    });
    assert_eq!(probes(&host), 2, "coming back is a check");
    update(&explorer, cx, |explorer, cx| {
        explorer.set_foreground(true, cx)
    });
    assert_eq!(probes(&host), 2, "only the return is");
}

// Downloads across a reconnect.

#[gpui_kit::test]
fn tells_the_status_line_when_a_reconnect_fails_its_downloads(cx: &mut TestAppContext) {
    let (_host, explorer) = opened(cx);
    let changes = Rc::new(RefCell::new(0));
    cx.update(|cx| {
        let changes = changes.clone();
        cx.subscribe(&explorer, move |_, event: &ExplorerEvent, _| {
            if matches!(event, ExplorerEvent::DownloadChanged) {
                *changes.borrow_mut() += 1;
            }
        })
        .detach();
    });
    update(&explorer, cx, |explorer, _| {
        let key = explorer.inputs.scope.as_ref().unwrap().transfer_key();
        explorer.downloads.record(DownloadStatus {
            scope_key: key,
            ..download("1", TransferState::Running, None)
        });
    });
    set(&explorer, inputs("%2", "/repo", "client"), cx);
    assert_eq!(*changes.borrow(), 0, "a pane switch keeps them");
    set(&explorer, inputs("%2", "/repo", "reconnected"), cx);
    assert_eq!(*changes.borrow(), 1);
    assert_eq!(
        read(&explorer, cx, |explorer| explorer
            .downloads
            .get("1")
            .unwrap()
            .state),
        TransferState::Failed
    );
}

#[test]
fn never_downgrades_a_quarantine_cleanup_failure_after_a_later_no_op_event() {
    let mut list = DownloadList::default();
    list.set_connection("scope");
    list.record(DownloadStatus {
        failure_kind: Some(TransferFailureKind::Transfer),
        cleanup_status: Some(TransferCleanupStatus::Retained),
        cleanup_error: Some("partial quarantined".into()),
        ..download(
            "1",
            TransferState::Failed,
            Some(TransferOutcome::NotPublished),
        )
    });
    list.record(DownloadStatus {
        cleanup_status: Some(TransferCleanupStatus::Removed),
        error: Some("cancel was already terminal".into()),
        ..download(
            "1",
            TransferState::Cancelled,
            Some(TransferOutcome::NotPublished),
        )
    });
    let held = list.get("1").unwrap();
    assert_eq!(held.state, TransferState::Failed);
    assert_eq!(held.failure_kind, Some(TransferFailureKind::Transfer));
    assert_eq!(held.cleanup_status, Some(TransferCleanupStatus::Retained));
    assert_eq!(held.cleanup_error.as_deref(), Some("partial quarantined"));
    assert_ne!(held.error.as_deref(), Some("cancel was already terminal"));
}

// Open documents.

fn notices(
    explorer: &Entity<Explorer>,
    cx: &mut TestAppContext,
) -> Rc<RefCell<Vec<(String, FileNotice)>>> {
    let notices = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        let notices = notices.clone();
        cx.subscribe(explorer, move |_, event: &ExplorerEvent, _| {
            if let ExplorerEvent::Notice { path, notice, .. } = event {
                notices.borrow_mut().push((path.clone(), notice.clone()));
            }
        })
        .detach();
    });
    notices
}

fn document(watched: &str, resource: &str) -> WatchTarget {
    WatchTarget {
        host: "local".into(),
        server: "server".into(),
        root: Root {
            path: "/repo".into(),
            token: "root".into(),
        },
        watched: watched.into(),
        resource: resource.into(),
    }
}

fn listed(generation: Option<&str>, bootstrap: bool, fresh: bool) -> FileNotice {
    FileNotice::Listed {
        generation: generation.map(str::to_owned),
        bootstrap,
        fresh,
    }
}

#[gpui_kit::test]
fn shares_the_trees_watch_with_a_document_in_the_same_directory(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let notices = notices(&explorer, cx);
    let lease = update(&explorer, cx, |explorer, cx| {
        explorer.watch_document(&document("/repo", "/repo/a.txt"), cx)
    });
    assert_eq!(host.asked("watchDirectory"), ["/repo"], "one host watch");
    assert_eq!(
        *notices.borrow(),
        [("/repo/a.txt".to_owned(), listed(Some("1"), true, false))],
        "another holder's bootstrap, of unknown age"
    );
    drop(lease);
    cx.run_until_parked();
    assert_eq!(unwatches(&host), 0, "the tree still holds it");
    // Precise events and later listings reach the document.
    notices.borrow_mut().clear();
    let lease = update(&explorer, cx, |explorer, cx| {
        explorer.watch_document(&document("/repo", "/repo/a.txt"), cx)
    });
    notices.borrow_mut().clear();
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::FileChanged {
                root_token: "root".into(),
                path: "/repo/a.txt".into(),
                entry: Some(entry("/repo/a.txt", false)),
                operation_id: "save-1".into(),
                generation: entry("/repo/a.txt", false).generation,
            },
            cx,
        );
        explorer.apply_event(
            WorkspaceEvent::DirectorySnapshot {
                root_token: "root".into(),
                listing: listing("/repo", vec![entry("/repo/a.txt", false)]),
            },
            cx,
        );
        explorer.apply_event(
            WorkspaceEvent::FileDeleted {
                root_token: "root".into(),
                path: "/repo/a.txt".into(),
            },
            cx,
        );
        // Another root's file of the same name is not this document's.
        explorer.apply_event(
            WorkspaceEvent::FileDeleted {
                root_token: "other".into(),
                path: "/repo/a.txt".into(),
            },
            cx,
        );
    });
    let generation = entry("/repo/a.txt", false).generation;
    assert_eq!(
        *notices.borrow(),
        [
            (
                "/repo/a.txt".to_owned(),
                FileNotice::Changed {
                    generation: generation.clone(),
                    operation_id: Some("save-1".into()),
                }
            ),
            (
                "/repo/a.txt".to_owned(),
                listed(Some(&generation), false, true)
            ),
            ("/repo/a.txt".to_owned(), FileNotice::Deleted),
        ]
    );
    drop(lease);
}

#[gpui_kit::test]
fn revalidates_a_bootstrap_the_tree_joined_after_a_document_armed_it(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let notices = notices(&explorer, cx);
    let lease = update(&explorer, cx, |explorer, cx| {
        explorer.watch_document(&document("/repo/src", "/repo/src/main.rs"), cx)
    });
    assert_eq!(host.owed("watchDirectory"), ["/repo/src"]);
    host.answer(
        "watchDirectory",
        "/repo/src",
        Ok(directory("/repo/src", &[("/repo/src/main.rs", false)])),
    );
    cx.run_until_parked();
    assert_eq!(
        *notices.borrow(),
        [(
            "/repo/src/main.rs".to_owned(),
            listed(Some("1"), true, true)
        )],
        "its own watch's answer"
    );
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    assert_eq!(
        host.asked("watchDirectory"),
        ["/repo", "/repo/src"],
        "the tree joins the document's watch"
    );
    assert_eq!(
        rows(&explorer, cx),
        ["/repo/src", "/repo/src/main.rs", "/repo/a.txt"],
        "painted at once"
    );
    assert_eq!(
        host.owed("listDirectory"),
        ["/repo/src"],
        "and read again behind the paint"
    );
    // The last holder out gives it back.
    drop(lease);
    cx.run_until_parked();
    assert_eq!(unwatches(&host), 0);
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    assert_eq!(unwatches(&host), 1);
}

#[gpui_kit::test]
fn the_holder_that_armed_a_watch_leaving_mid_bootstrap_keeps_it_for_the_other(
    cx: &mut TestAppContext,
) {
    let (host, explorer) = opened(cx);
    let lease = update(&explorer, cx, |explorer, cx| {
        explorer.watch_document(&document("/repo/src", "/repo/src/main.rs"), cx)
    });
    update(&explorer, cx, |explorer, cx| {
        explorer.toggle_directory("/repo/src", cx)
    });
    assert_eq!(host.asked("watchDirectory"), ["/repo", "/repo/src"]);
    drop(lease);
    cx.run_until_parked();
    assert!(host.stopped().is_empty(), "the tree still waits on it");
    assert_eq!(unwatches(&host), 0);
    host.answer(
        "watchDirectory",
        "/repo/src",
        Ok(directory("/repo/src", &[("/repo/src/main.rs", false)])),
    );
    cx.run_until_parked();
    assert_eq!(
        rows(&explorer, cx),
        ["/repo/src", "/repo/src/main.rs", "/repo/a.txt"]
    );
    assert_eq!(unwatches(&host), 0);
}

#[gpui_kit::test]
fn rearms_a_documents_watch_on_a_new_connection_with_a_fresh_bootstrap(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let lease = update(&explorer, cx, |explorer, cx| {
        explorer.watch_document(&document("/repo/src", "/repo/src/main.rs"), cx)
    });
    host.answer(
        "watchDirectory",
        "/repo/src",
        Ok(directory("/repo/src", &[("/repo/src/main.rs", false)])),
    );
    cx.run_until_parked();
    let notices = notices(&explorer, cx);
    set(&explorer, inputs("%1", "/repo", "reconnected"), cx);
    assert!(
        host.owed("watchDirectory")
            .contains(&"/repo/src".to_owned()),
        "the document's watch is armed again on the new connection"
    );
    host.answer(
        "watchDirectory",
        "/repo/src",
        Ok(directory("/repo/src", &[("/repo/src/main.rs", false)])),
    );
    cx.run_until_parked();
    assert_eq!(
        *notices.borrow(),
        [(
            "/repo/src/main.rs".to_owned(),
            listed(Some("1"), true, true)
        )]
    );
    drop(lease);
}

#[gpui_kit::test]
fn watches_nothing_for_a_document_on_another_host(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let mut elsewhere = document("/repo/src", "/repo/src/main.rs");
    elsewhere.host = "remote".into();
    let notices = notices(&explorer, cx);
    let lease = update(&explorer, cx, |explorer, cx| {
        explorer.watch_document(&elsewhere, cx)
    });
    assert_eq!(host.asked("watchDirectory"), ["/repo"]);
    // Nor does this host's event reach it.
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::FileDeleted {
                root_token: "root".into(),
                path: "/repo/src/main.rs".into(),
            },
            cx,
        )
    });
    assert!(notices.borrow().is_empty());
    drop(lease);
}

#[gpui_kit::test]
fn reads_a_single_file_capabilitys_rescans_from_its_parent(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let notices = notices(&explorer, cx);
    let lease = update(&explorer, cx, |explorer, cx| {
        explorer.watch_document(&document("/repo/src/main.rs", "/repo/src/main.rs"), cx)
    });
    assert_eq!(host.owed("watchDirectory"), ["/repo/src/main.rs"]);
    notices.borrow_mut().clear();
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::DirectorySnapshot {
                root_token: "root".into(),
                listing: listing("/repo/src", vec![entry("/repo/src/main.rs", false)]),
            },
            cx,
        )
    });
    let generation = entry("/repo/src/main.rs", false).generation;
    assert_eq!(
        *notices.borrow(),
        [(
            "/repo/src/main.rs".to_owned(),
            listed(Some(&generation), false, true)
        )]
    );
    drop(lease);
}

// Row commands and the cursor.

#[gpui_kit::test]
fn offers_row_commands_only_while_shown_and_none_that_change_files_read_only(
    cx: &mut TestAppContext,
) {
    let (_host, explorer) = opened(cx);
    assert!(
        read(&explorer, cx, |explorer| explorer.row_command_ids()).is_empty(),
        "a hidden segment answers nothing"
    );
    update(&explorer, cx, |explorer, cx| explorer.set_shown(true, cx));
    update(&explorer, cx, |explorer, _| {
        explorer.view.focus_key = Some("/repo/a.txt".into())
    });
    assert_eq!(
        read(&explorer, cx, |explorer| explorer.row_command_ids()),
        [
            "files.open",
            "files.download",
            "files.rename",
            "files.move",
            "files.duplicate",
            "files.delete",
            "files.newFile",
            "files.newFolder",
            "files.refresh"
        ]
    );
    let mut frozen = inputs("%1", "/repo", "client");
    frozen.can_mutate = false;
    set(&explorer, frozen, cx);
    assert_eq!(
        read(&explorer, cx, |explorer| explorer.row_command_ids()),
        ["files.open", "files.download", "files.refresh"]
    );
}

#[gpui_kit::test]
fn keeps_the_row_cursor_where_it_was_when_the_focused_row_is_hidden(cx: &mut TestAppContext) {
    let (_host, explorer) = opened(cx);
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::FileChanged {
                root_token: "root".into(),
                path: "/repo/b.txt".into(),
                entry: Some(entry("/repo/b.txt", false)),
                operation_id: String::new(),
                generation: String::new(),
            },
            cx,
        );
        explorer.view.focus_key = Some("/repo/a.txt".into());
        explorer.view.last_focus_index = 1;
    });
    cx.update(|cx| super::set_ignored_paths(Some(set_of(&["/repo/a.txt"])), cx));
    update(&explorer, cx, |explorer, cx| explorer.refresh_rows(cx));
    assert_eq!(rows(&explorer, cx), ["/repo/src", "/repo/b.txt"]);
    assert_eq!(
        read(&explorer, cx, |explorer| explorer
            .focused_entry()
            .map(|entry| entry.path)),
        Some("/repo/b.txt".to_owned())
    );
}

#[gpui_kit::test]
fn tells_a_document_the_generation_of_a_change_it_cannot_draw(cx: &mut TestAppContext) {
    let (_host, explorer) = opened(cx);
    let notices = notices(&explorer, cx);
    let lease = update(&explorer, cx, |explorer, cx| {
        explorer.watch_document(&document("/repo", "/repo/a.txt"), cx)
    });
    notices.borrow_mut().clear();
    update(&explorer, cx, |explorer, cx| {
        explorer.apply_event(
            WorkspaceEvent::FileChanged {
                root_token: "root".into(),
                path: "/repo/a.txt".into(),
                entry: None,
                operation_id: String::new(),
                generation: "9".into(),
            },
            cx,
        )
    });
    assert_eq!(
        *notices.borrow(),
        [(
            "/repo/a.txt".to_owned(),
            FileNotice::Changed {
                generation: "9".into(),
                operation_id: None,
            }
        )]
    );
    drop(lease);
}

// Quick Open.

fn search_answer(matches: &[&str]) -> Value {
    json!({"search": {
        "complete": true,
        "matches": matches.iter().map(|relative| json!({
            "path": format!("/repo/{relative}"), "relativePath": relative, "score": 1,
        })).collect::<Vec<_>>(),
    }})
}

#[gpui_kit::test]
fn searches_the_panes_directory_resolving_its_root_once(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let first = cx.update(|cx| super::search(explorer.clone(), "main".into(), cx));
    cx.run_until_parked();
    assert_eq!(probes(&host), 2, "the search resolves its own root");
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("root", "/repo", "%1")),
    );
    cx.run_until_parked();
    let asked = host.last("searchFiles");
    assert_eq!(asked["searchQuery"], "main");
    assert_eq!(asked["expectedCwd"], "/repo");
    host.answer("searchFiles", "", Ok(search_answer(&["src/main.rs"])));
    cx.run_until_parked();
    let found = cx.foreground_executor().block_test(first).unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.matches[0].path, "/repo/src/main.rs");
    let _second = cx.update(|cx| super::search(explorer.clone(), "lib".into(), cx));
    cx.run_until_parked();
    assert_eq!(probes(&host), 2, "the root is kept for the same pane");
    assert_eq!(host.owed("searchFiles").len(), 1);
}

#[gpui_kit::test]
fn stops_the_hosts_search_when_the_search_is_dropped(cx: &mut TestAppContext) {
    let (host, explorer) = opened(cx);
    let search = cx.update(|cx| super::search(explorer.clone(), "main".into(), cx));
    cx.run_until_parked();
    host.answer(
        "resolveActiveRoot",
        "",
        Ok(root_answer("root", "/repo", "%1")),
    );
    cx.run_until_parked();
    assert_eq!(host.owed("searchFiles").len(), 1);
    drop(search);
    cx.run_until_parked();
    assert_eq!(host.stopped(), ["searchFiles:"]);
    assert!(host.owed("searchFiles").is_empty());
}

#[test]
fn decodes_a_terminal_file_resolution_only_for_its_own_pane() {
    let answer = json!({
        "activeRoot": {
            "paneId": "%1", "root": "/repo", "rootToken": "t", "serverIdentity": "server-a",
            "topologyGeneration": "7", "rootGeneration": "1", "gitWorktree": false,
        },
        "metadata": { "path": "/repo/src/main.rs" },
    });
    let resolved = wire::decode_terminal_file(answer.clone(), "server-a", "%1").unwrap();
    assert_eq!(resolved.path, "/repo/src/main.rs");
    assert_eq!(
        (resolved.root.path.as_str(), resolved.root.token.as_str()),
        ("/repo", "t")
    );
    assert_eq!(resolved.topology_generation, 7);
    let stale = "Host resolved the terminal file for a stale pane scope.";
    assert_eq!(
        wire::decode_terminal_file(answer.clone(), "server-a", "%2").unwrap_err(),
        stale
    );
    assert_eq!(
        wire::decode_terminal_file(answer.clone(), "server-b", "%1").unwrap_err(),
        stale
    );
    assert_eq!(
        wire::decode_terminal_file(
            json!({ "activeRoot": answer["activeRoot"] }),
            "server-a",
            "%1"
        )
        .unwrap_err(),
        "Host omitted the resolved terminal file."
    );
    let mut malformed = answer;
    malformed["activeRoot"]["topologyGeneration"] = json!("07");
    assert_eq!(
        wire::decode_terminal_file(malformed, "server-a", "%1").unwrap_err(),
        "Host returned a malformed terminal file topology generation."
    );
}

#[test]
fn refuses_more_search_matches_than_the_host_may_send() {
    let many: Vec<String> = (0..76).map(|index| format!("f{index}")).collect();
    let names: Vec<&str> = many.iter().map(String::as_str).collect();
    assert!(wire::decode_search(search_answer(&names[..75]), "/repo").is_ok());
    assert!(wire::decode_search(search_answer(&names), "/repo").is_err());
}

mod view {
    use std::path::Path;

    use gpui_kit::{Role, VisualTestContext, test::TestQueryExt};

    use super::*;
    use crate::app::{self, Core};

    /// The Explorer drawn in a window of its own, its root listed.
    fn shown(cx: &mut TestAppContext) -> (Rc<Host>, Entity<Explorer>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            app::init(Core::at(Path::new("/nonexistent/app-state.json")), cx);
        });
        let host = Rc::new(Host::default());
        let transport = Rc::new(Transport(host.clone()));
        let (explorer, visual) = cx.add_window_view(|_, cx| Explorer::new(transport, cx));
        explorer.update(visual, |explorer, cx| {
            explorer.set_inputs(inputs("%1", "/repo", "client"), cx)
        });
        visual.run_until_parked();
        host.answer(
            "resolveActiveRoot",
            "",
            Ok(root_answer("root", "/repo", "%1")),
        );
        visual.run_until_parked();
        host.answer(
            "watchDirectory",
            "/repo",
            Ok(directory(
                "/repo",
                &[("/repo/src", true), ("/repo/a.txt", false)],
            )),
        );
        visual.run_until_parked();
        (host, explorer, visual)
    }

    #[gpui_kit::test]
    fn names_the_tree_and_each_row_with_its_place_in_its_level(cx: &mut TestAppContext) {
        let (_host, _explorer, visual) = shown(cx);
        visual.update(|window, _| {
            assert_eq!(window.find_by_label("Files").role(), Some(Role::Tree));
            let rows = window.find_all_by_role(Role::TreeItem);
            assert_eq!(rows.len(), 2);
            let src = window.find_by_label("src");
            assert_eq!(src.role(), Some(Role::TreeItem));
            assert_eq!(src.expanded(), Some(false));
            assert_eq!(window.find_by_label("a.txt").role(), Some(Role::TreeItem));
        });
    }

    #[gpui_kit::test]
    fn walks_and_opens_the_tree_from_the_keyboard(cx: &mut TestAppContext) {
        let (host, explorer, visual) = shown(cx);
        visual.update(|window, cx| {
            let focus = explorer.read(cx).view.focus.clone();
            window.focus(&focus, cx);
        });
        visual.simulate_keystrokes("end");
        let focused = |visual: &mut VisualTestContext| {
            explorer.read_with(visual, |explorer, _| {
                explorer.focused_entry().map(|entry| entry.path.clone())
            })
        };
        assert_eq!(focused(visual).as_deref(), Some("/repo/a.txt"));
        visual.simulate_keystrokes("home right");
        visual.run_until_parked();
        assert_eq!(host.owed("watchDirectory"), ["/repo/src"]);
        host.answer(
            "watchDirectory",
            "/repo/src",
            Ok(directory("/repo/src", &[("/repo/src/main.rs", false)])),
        );
        visual.run_until_parked();
        visual.simulate_keystrokes("right");
        assert_eq!(focused(visual).as_deref(), Some("/repo/src/main.rs"));
        visual.simulate_keystrokes("left");
        assert_eq!(focused(visual).as_deref(), Some("/repo/src"));
        visual.simulate_keystrokes("left");
        assert!(!explorer.read_with(visual, |explorer, _| {
            explorer.tree.expanded.contains("/repo/src")
        }));
        assert_eq!(focused(visual).as_deref(), Some("/repo/src"));
    }
    #[gpui_kit::test]
    fn pages_a_directory_from_the_load_more_row_by_keyboard(cx: &mut TestAppContext) {
        let (host, explorer, visual) = shown(cx);
        visual.update(|window, cx| {
            let focus = explorer.read(cx).view.focus.clone();
            window.focus(&focus, cx);
        });
        visual.simulate_keystrokes("home right");
        visual.run_until_parked();
        host.answer(
            "watchDirectory",
            "/repo/src",
            Ok(page("/repo/src", &[("/repo/src/a", false)], Some("p2"))),
        );
        visual.run_until_parked();
        host.answer(
            "listDirectory",
            "/repo/src",
            Ok(page("/repo/src", &[("/repo/src/b", false)], Some("p3"))),
        );
        visual.run_until_parked();
        visual.simulate_keystrokes("down down down");
        let focused = explorer.read_with(visual, |explorer, _| {
            explorer.view.rows[explorer.view.focus_index()].key()
        });
        assert_eq!(focused, "/repo/src\0more");
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(host.owed("listDirectory"), ["/repo/src"]);
        assert_eq!(host.last("listDirectory")["pageToken"], "p3");
    }

    /// The Explorer under a window root that can host dialogs.
    fn rooted(cx: &mut TestAppContext) -> (Rc<Host>, Entity<Explorer>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            app::init(Core::at(Path::new("/nonexistent/app-state.json")), cx);
        });
        let host = Rc::new(Host::default());
        let transport = Rc::new(Transport(host.clone()));
        let explorer = cx.new(|cx| Explorer::new(transport, cx));
        let (_, visual) = cx.add_window_view({
            let explorer = explorer.clone();
            move |window, cx| gpui_kit::base::Root::new(explorer, window, cx)
        });
        explorer.update(visual, |explorer, cx| {
            explorer.set_inputs(inputs("%1", "/repo", "client"), cx)
        });
        visual.run_until_parked();
        host.answer(
            "resolveActiveRoot",
            "",
            Ok(root_answer("root", "/repo", "%1")),
        );
        visual.run_until_parked();
        host.answer("watchDirectory", "/repo", Ok(directory("/repo", &[])));
        visual.run_until_parked();
        (host, explorer, visual)
    }

    /// Opens New File, names it and submits it; the overlay keeps the dialog.
    fn create(
        explorer: &Entity<Explorer>,
        name: &str,
        visual: &mut VisualTestContext,
    ) -> gpui_kit::WeakEntity<MutationDialog> {
        let dialog = visual.update(|window, cx| {
            let root = explorer.read(cx).tree.root.clone().unwrap();
            let scope = explorer.read(cx).scope_identity();
            super::super::dialog::open(
                explorer.clone(),
                MutationAction::NewFile,
                None,
                root,
                scope,
                window,
                cx,
            )
            .downgrade()
        });
        visual.run_until_parked();
        let name = name.to_owned();
        visual.update(|window, cx| {
            let dialog = dialog.upgrade().unwrap();
            let input = dialog.read(cx).input.clone();
            input.update(cx, |input, cx| input.set_value(name, window, cx));
            dialog.update(cx, |dialog, cx| dialog.submit(window, cx));
        });
        visual.run_until_parked();
        dialog
    }

    fn dialog_open(visual: &mut VisualTestContext) -> bool {
        visual.update(|window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.has_active_dialog(cx)
        })
    }

    #[gpui_kit::test]
    fn shows_a_failed_change_in_its_dialog(cx: &mut TestAppContext) {
        let (host, explorer, visual) = rooted(cx);
        let statuses = statuses(&explorer, visual);
        let dialog = create(&explorer, "a.txt", visual);
        host.answer("mutate", "/repo/a.txt", Err("already exists".into()));
        visual.run_until_parked();
        assert!(dialog_open(visual), "the dialog stays to show it");
        let error = visual.update(|_, cx| dialog.upgrade().unwrap().read(cx).error.clone());
        assert_eq!(error.as_deref(), Some("already exists"));
        assert!(statuses.borrow().is_empty());
    }

    #[gpui_kit::test]
    fn reports_the_failure_of_a_dismissed_dialog_on_the_status_line(cx: &mut TestAppContext) {
        let (host, explorer, visual) = rooted(cx);
        let statuses = statuses(&explorer, visual);
        let _dialog = create(&explorer, "a.txt", visual);
        visual.update(|window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.close_dialog(cx)
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        assert!(!dialog_open(visual));
        host.answer("mutate", "/repo/a.txt", Err("already exists".into()));
        visual.run_until_parked();
        assert_eq!(*statuses.borrow(), ["already exists"]);
    }

    #[gpui_kit::test]
    fn closes_a_pending_action_and_forgets_show_ignored_when_the_root_changes(
        cx: &mut TestAppContext,
    ) {
        let (host, explorer, visual) = rooted(cx);
        let dialog = visual.update(|window, cx| {
            let root = explorer.read(cx).tree.root.clone().unwrap();
            let scope = explorer.read(cx).scope_identity();
            super::super::dialog::open(
                explorer.clone(),
                MutationAction::NewFile,
                None,
                root,
                scope,
                window,
                cx,
            )
            .downgrade()
        });
        visual.run_until_parked();
        assert!(dialog_open(visual));
        explorer.update(visual, |explorer, cx| {
            explorer.view.show_ignored = true;
            explorer.rearm(cx);
        });
        visual.run_until_parked();
        host.answer(
            "resolveActiveRoot",
            "",
            Ok(root_answer("moved", "/moved", "%1")),
        );
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        assert!(!dialog_open(visual), "a different root closes the form");
        assert!(!explorer.read_with(visual, |explorer, _| explorer.view.show_ignored));
        drop(dialog);
    }

    #[gpui_kit::test]
    fn downloads_the_keyboards_row_from_the_palette_without_reentering(cx: &mut TestAppContext) {
        let (_host, explorer, visual) = shown(cx);
        explorer.update_in(visual, |explorer, window, cx| {
            explorer.view.focus_key = Some("/repo/a.txt".into());
            explorer.run_row_command("files.download", window, cx);
        });
        visual.run_until_parked();
        assert!(explorer.read_with(visual, |explorer, _| explorer.picker_open));
    }
}
