//! Directory enumeration: hiding policy, snapshot-backed pagination, page
//! token binding, cancellation, and root-capability identity.

use super::*;

/// Three populations, and the difference between them is the whole rule.
///
/// Repository plumbing and platform scratch files are *hidden*: VS Code's
/// `files.exclude` defaults, which is the explorer this one is compared
/// against. `node_modules` is *shown and collapsed*, because VS Code shows
/// it too and people open it on purpose. Every other dotfile — `.env`,
/// `.gitignore` — is an ordinary file the user edits, and hiding those
/// would be a different product.
#[test]
fn listing_hides_repository_plumbing_keeps_dotfiles_and_collapses_heavy_directories() {
    let (root, service) = fixture();
    fs::write(root.join(".env"), "ok").unwrap();
    fs::write(root.join(".DS_Store"), "noise").unwrap();
    fs::write(root.join("Thumbs.db"), "noise").unwrap();
    for hidden in [".git", ".svn", ".hg", "CVS"] {
        fs::create_dir(root.join(hidden)).unwrap();
    }
    fs::create_dir(root.join("node_modules")).unwrap();
    fs::create_dir(root.join("real")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("linked")).unwrap();
    let snapshot = service
        .list_directory(root.to_str().unwrap(), "", "watch")
        .unwrap();
    let named = |name: &str| snapshot.entries.iter().find(|item| item.name == name);

    assert!(named(".env").is_some(), "ordinary dotfiles stay visible");
    for hidden in [".git", ".svn", ".hg", "CVS", ".DS_Store", "Thumbs.db"] {
        assert!(named(hidden).is_none(), "{hidden} must not be listed");
    }
    for name in ["node_modules", "linked"] {
        assert!(
            !named(name).expect("shown, just not expandable").expandable,
            "{name} stays visible and collapsed"
        );
    }
    // Hidden is not the same as merely absent from one view: the path is
    // refused too, so nothing lists or watches it by asking directly.
    for hidden in [".git", ".svn", ".hg", "CVS", "node_modules"] {
        assert!(
            service
                .list_directory(root.to_str().unwrap(), hidden, "watch")
                .is_err(),
            "{hidden} must not be enterable by path"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

/// A hidden entry that consumed a page slot would make a page shorter than
/// it claims, and one that became the page token would make the next page
/// resume from a name no client was ever told about.
#[test]
fn hidden_entries_do_not_consume_page_slots_or_become_page_tokens() {
    let (root, service) = fixture();
    for name in ["a", "b", "c", "d"] {
        fs::write(root.join(name), name).unwrap();
    }
    for hidden in [".DS_Store", "Thumbs.db"] {
        fs::write(root.join(hidden), "noise").unwrap();
    }
    fs::create_dir(root.join(".git")).unwrap();
    let mut token = String::new();
    let mut names = Vec::new();
    let mut pages = 0;
    loop {
        let page = service
            .list_directory_page(root.to_str().unwrap(), "", "page", &token, 2)
            .unwrap();
        pages += 1;
        // The load-bearing assertion, and the reason it is not merely
        // `len() <= 2`: filtering *after* the page window would fill the
        // window with hidden entries and hand back a short page that still
        // claims more to come. Every page but the last is full.
        assert!(
            page.complete || page.entries.len() == 2,
            "a non-final page must be full, got {} entries",
            page.entries.len()
        );
        names.extend(page.entries.into_iter().map(|entry| entry.name));
        if page.complete {
            break;
        }
        token = page.next_page_token;
    }
    assert_eq!(names, vec!["a", "b", "c", "d"]);
    assert_eq!(pages, 2, "four visible entries at two per page");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn directory_pages_are_bounded_stable_and_complete() {
    let (root, service) = fixture();
    for name in ["c", "a", "e", "b", "d"] {
        fs::write(root.join(name), name).unwrap();
    }
    let mut token = String::new();
    let mut names = Vec::new();
    loop {
        let page = service
            .list_directory_page(root.to_str().unwrap(), "", "page", &token, 2)
            .unwrap();
        assert!(page.entries.len() <= 2);
        names.extend(page.entries.into_iter().map(|entry| entry.name));
        if page.complete {
            break;
        }
        assert!(!page.next_page_token.is_empty());
        token = page.next_page_token;
    }
    assert_eq!(names, ["a", "b", "c", "d", "e"]);
    assert!(
        service
            .list_directory_page(root.to_str().unwrap(), "", "page", "bad", 2)
            .is_err()
    );
    fs::remove_dir_all(root).unwrap();
}

/// Later pages slice a retained ordered snapshot.
///
/// The proof is deliberately destructive: an entry removed from disk after the
/// first page still appears in the second. A page that re-enumerated and
/// re-stated the directory could not possibly still see it.
#[test]
fn later_pages_slice_the_retained_snapshot_instead_of_restating_the_directory() {
    let (root, service) = fixture();
    for name in ["a", "b", "c", "d"] {
        fs::write(root.join(name), name).unwrap();
    }
    let first = service
        .list_directory_page(root.to_str().unwrap(), "", "page", "", 2)
        .unwrap();
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(!first.complete);
    fs::remove_file(root.join("d")).unwrap();
    let second = service
        .list_directory_page(
            root.to_str().unwrap(),
            "",
            "page",
            &first.next_page_token,
            2,
        )
        .unwrap();
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>(),
        ["c", "d"],
        "the second page re-enumerated the directory instead of slicing its snapshot"
    );
    assert!(second.complete);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_page_token_is_bound_to_its_exact_server_root_and_directory() {
    let (root, service) = fixture();
    fs::create_dir(root.join("left")).unwrap();
    fs::create_dir(root.join("right")).unwrap();
    for name in ["a", "b", "c"] {
        fs::write(root.join("left").join(name), name).unwrap();
        fs::write(root.join("right").join(name), name).unwrap();
    }
    let left = service
        .list_directory_page(root.to_str().unwrap(), "left", "page", "", 2)
        .unwrap();
    assert!(!left.next_page_token.is_empty());
    let crossed = service
        .list_directory_page(
            root.to_str().unwrap(),
            "right",
            "page",
            &left.next_page_token,
            2,
        )
        .unwrap_err();
    // The classification, not the wording: the wire code is the error's
    // identity, so renaming the message cannot silently reclassify it.
    assert_eq!(FileFailure::of(&crossed), FileFailure::StalePageToken);
    assert_eq!(FileFailure::StalePageToken.code(), "stale_page_token");
    // Every field the token carries is covered by its digest, not just the
    // binding. An edited position addresses a window of a listing the client
    // was never handed, which is a page skipped or repeated in its tree.
    let fields: Vec<&str> = left.next_page_token.split(':').collect();
    for (index, replacement) in [(3, "0"), (4, "1"), (5, "0"), (6, "00")] {
        let mut edited = fields.clone();
        if edited[index] == replacement {
            continue;
        }
        edited[index] = replacement;
        let tampered = edited.join(":");
        let rejected = service
            .list_directory_page(root.to_str().unwrap(), "left", "page", &tampered, 2)
            .expect_err("an edited page token was answered");
        assert_eq!(FileFailure::of(&rejected), FileFailure::StalePageToken);
    }
    for malformed in ["bad", "p1:deadbeefdeadbeef", "p1::::"] {
        let rejected = service
            .list_directory_page(root.to_str().unwrap(), "left", "page", malformed, 2)
            .expect_err("{malformed} was accepted");
        assert_eq!(FileFailure::of(&rejected), FileFailure::StalePageToken);
    }
    fs::remove_dir_all(root).unwrap();
}

/// A snapshot the cache no longer holds still resumes exactly, because the
/// token carries its own resume key.
#[test]
fn an_expired_snapshot_resumes_from_the_token_rather_than_restarting() {
    let (root, service) = fixture();
    for name in ["a", "b", "c", "d"] {
        fs::write(root.join(name), name).unwrap();
    }
    let first = service
        .list_directory_page(root.to_str().unwrap(), "", "page", "", 2)
        .unwrap();
    // Evict every retained snapshot by establishing more than the cache holds.
    for index in 0..12 {
        let directory = format!("spill-{index}");
        fs::create_dir(root.join(&directory)).unwrap();
        for name in ["x", "y", "z"] {
            fs::write(root.join(&directory).join(name), name).unwrap();
        }
        service
            .list_directory_page(root.to_str().unwrap(), &directory, "page", "", 1)
            .unwrap();
    }
    let second = service
        .list_directory_page(
            root.to_str().unwrap(),
            "",
            "page",
            &first.next_page_token,
            2,
        )
        .unwrap();
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>(),
        ["c", "d"]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_cancelled_listing_stops_the_bounded_scan_instead_of_completing_it() {
    let (root, service) = fixture();
    for index in 0..64 {
        fs::write(root.join(format!("entry-{index}")), "x").unwrap();
    }
    let cancelled = AtomicBool::new(true);
    let error = service
        .list_directory_page_authorized(
            root.to_str().unwrap(),
            &root_token(root.to_str().unwrap()).unwrap(),
            "",
            "page",
            "",
            0,
            &cancelled,
        )
        .unwrap_err();
    assert_eq!(FileFailure::of(&error), FileFailure::Cancelled);
    assert_eq!(FileFailure::Cancelled.code(), "cancelled");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn root_token_changes_when_same_path_is_replaced() {
    let (root, _service) = fixture();
    let first = root_token(root.to_str().unwrap()).unwrap();
    fs::remove_dir(&root).unwrap();
    fs::create_dir(&root).unwrap();
    let second = root_token(root.to_str().unwrap()).unwrap();
    assert_ne!(first, second);
    fs::remove_dir(root).unwrap();
}

#[test]
fn stale_root_token_rejects_same_path_replacement_before_worker_open() {
    let (root, service) = fixture();
    fs::write(root.join("note"), "original").unwrap();
    let token = root_token(root.to_str().unwrap()).unwrap();
    let displaced = root.with_extension("displaced");
    fs::rename(&root, &displaced).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("note"), "replacement").unwrap();
    let error = service
        .read_file_authorized(root.to_str().unwrap(), &token, "note")
        .unwrap_err();
    assert!(error.to_string().contains("root snapshot token"));
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(displaced).unwrap();
}

#[test]
fn an_unchanged_root_capability_keeps_the_generation_its_client_already_holds() {
    let (root, service) = fixture();
    let token = root_token(root.to_str().unwrap()).unwrap();
    let first = root_generation(&token);
    assert_eq!(root_generation(&token), first);
    assert_ne!(root_generation("a-different-capability"), first);
    // Never the protobuf default, which a client reads as "unsaid".
    assert_ne!(first, 0);
    // Derived, so no number of distinct roots can make an existing one look
    // replaced — the map this used to keep evicted by clearing itself.
    for index in 0..512 {
        assert_ne!(root_generation(&format!("capability-{index}")), 0);
    }
    assert_eq!(root_generation(&token), first);
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

/// One capability lists the same directory the same way every time.
///
/// The enumeration hands its descriptor to `fdopendir`, which consumes it. A
/// `dup` of the capability shares its file offset, so the second listing began
/// where the first stopped — at the end — and every authoritative rescan of an
/// already-listed directory reported it as empty. The desktop installs an
/// authoritative listing as the directory's contents, so that is every row on
/// screen disappearing.
#[test]
fn a_capability_lists_the_same_entries_however_many_times_it_is_asked() {
    let root_path = std::env::temp_dir().join(format!("ade-relist-{}", Uuid::new_v4()));
    fs::create_dir_all(root_path.join("child")).unwrap();
    fs::write(root_path.join("a"), "a").unwrap();
    fs::write(root_path.join("child/b"), "b").unwrap();
    let root = RootCapability::capture(root_path.to_str().unwrap()).unwrap();

    for turn in 0..3 {
        let names = root.directory_entries().unwrap();
        assert_eq!(names.len(), 2, "root enumeration {turn} lost its entries");
        let child = root.anchor(&root.logical_root().join("child")).unwrap();
        assert_eq!(
            child.directory_entries().unwrap().len(),
            1,
            "child enumeration {turn} lost its entries"
        );
    }
    fs::remove_dir_all(root_path).unwrap();
}
