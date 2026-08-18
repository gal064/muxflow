//! Watch registration, precise native events, and the per-target polling
//! fallback that only failed targets ever pay for.

use super::*;

#[test]
fn native_watch_routing_is_independent_of_directory_entry_count() {
    let root_path = std::env::temp_dir().join(format!("ade-watch-route-{}", Uuid::new_v4()));
    fs::create_dir_all(root_path.join("huge")).unwrap();
    let root = Arc::new(RootCapability::capture(root_path.to_str().unwrap()).unwrap());
    let logical_root = root.logical_root().to_owned();
    let target_directory = root
        .anchor(&logical_root.join("huge"))
        .unwrap()
        .open_directory()
        .unwrap();
    let watch = Watch {
        root_token: root.token().to_owned(),
        root,
        path: logical_root.join("huge").to_string_lossy().into_owned(),
        target: logical_root.join("huge"),
        target_directory: Arc::new(target_directory),
        fallback: Arc::new(Mutex::new(FallbackTarget::native(0))),
    };
    // Routing is one grouping of the whole dirty batch, so a directory with a
    // quarter of a million entries costs the same lookup as an empty one.
    let deep = watch.target.join("entry-249999");
    let grouped = changes_by_parent([deep.clone(), PathBuf::from("/elsewhere/file")]);
    assert_eq!(grouped.get(&watch.target), Some(&vec![deep]));
    assert!(changes_by_parent([]).is_empty());
    // A path that is the watched directory belongs to its parent, never to
    // itself: the whole reason a self-event stopped becoming a row inside it.
    let self_event = changes_by_parent([watch.target.clone()]);
    assert!(!self_event.contains_key(&watch.target));
    fs::remove_dir_all(root_path).unwrap();
}

#[test]
fn fallback_watch_registration_and_fingerprint_cover_all_change_shapes() {
    let (root, service) = fixture();
    fs::write(root.join("file"), "one").unwrap();
    let initial = watch_fingerprint(&root).unwrap();
    let snapshot = service
        .watch_directory(root.to_str().unwrap(), "", "fallback")
        .unwrap();
    assert!(snapshot.authoritative);
    // No native watcher was ever installed for this fixture, so this exact
    // target — and only this one — is on the polling fallback.
    assert!(
        !service
            .watches
            .lock()
            .unwrap()
            .get("fallback")
            .expect("the watch is registered")
            .fallback
            .lock()
            .unwrap()
            .is_native()
    );
    fs::write(root.join("file"), "longer").unwrap();
    let edited = watch_fingerprint(&root).unwrap();
    assert_ne!(initial, edited);
    fs::write(root.join("created"), "x").unwrap();
    let created = watch_fingerprint(&root).unwrap();
    assert_ne!(edited, created);
    fs::remove_file(root.join("created")).unwrap();
    assert_ne!(created, watch_fingerprint(&root).unwrap());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn fallback_fingerprint_covers_in_place_edits_beyond_4096_entries() {
    let root = std::env::temp_dir().join(format!("ade-fallback-large-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    for index in 0..=4096 {
        fs::write(root.join(format!("entry-{index:04}")), b"a").unwrap();
    }
    let before = watch_fingerprint(&root).unwrap();
    let scan = Mutex::new(FallbackScan::new(before));
    fs::write(root.join("entry-4096"), b"changed beyond old bound").unwrap();
    assert_ne!(before, watch_fingerprint(&root).unwrap());
    let mut shards = 0;
    loop {
        let shard =
            scan_fallback_shard_with_limits(&root, &scan, false, 64, Duration::from_secs(1))
                .unwrap();
        assert!(shard.processed <= 64);
        shards += 1;
        if shard.completed {
            assert!(shard.changed);
            break;
        }
    }
    assert!(shards > 64, "the scanner must resume across bounded shards");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn fallback_fingerprint_fold_covers_250k_records_with_a_hard_shard_cap() {
    let mut records = (0_u64..250_000).map(|value| value.wrapping_mul(31));
    let mut covered = 0;
    let mut shards = 0;
    loop {
        let (_, processed, completed) =
            fold_fingerprint_records(FALLBACK_SCAN_ENTRY_BUDGET, Duration::from_secs(1), || {
                Ok(records.next())
            })
            .unwrap();
        assert!(processed <= FALLBACK_SCAN_ENTRY_BUDGET);
        covered += processed;
        shards += 1;
        if completed {
            break;
        }
    }
    assert_eq!(covered, 250_000);
    assert!(shards > 100);
}

#[tokio::test(flavor = "current_thread")]
async fn fallback_filesystem_scan_never_blocks_the_async_control_worker() {
    let root = std::env::temp_dir().join(format!("ade-fallback-latency-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let scan = Arc::new(Mutex::new(FallbackTarget::polling(
        watch_fingerprint(&root).unwrap(),
    )));
    let held_scan = Arc::clone(&scan);
    let ready = Arc::new(std::sync::Barrier::new(2));
    let held_ready = Arc::clone(&ready);
    let holder = std::thread::spawn(move || {
        let _guard = held_scan.lock().unwrap();
        held_ready.wait();
        std::thread::sleep(Duration::from_millis(100));
    });
    ready.wait();

    let scan_task = tokio::spawn(super::watch_fallback::advance_target_async(
        root.clone(),
        scan,
        Instant::now(),
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), async {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        })
        .await
        .is_ok(),
        "the current-thread runtime must stay responsive while filesystem work waits"
    );
    assert!(scan_task.await.unwrap().is_ok());
    holder.join().unwrap();
    fs::remove_dir(root).unwrap();
}

/// The lock a scan in progress holds is not the lock the async poller takes.
///
/// The test above proves the *scan* runs off the runtime thread, which is a
/// weaker claim than its name: it exercises none of the async-side locks. A
/// scan is `metadata`, `read_dir`, and up to 2,048 `symlink_metadata` calls,
/// and while it ran it used to hold the same mutex `scan_due`,
/// `native_retry_due`, `fallback_watches`, and `degrade_all_to_polling` take
/// from the async watcher and poller tasks. Every one of those is asked here
/// while a scan holds its lock.
#[tokio::test(flavor = "current_thread")]
async fn the_async_poller_never_waits_on_a_scan_in_progress() {
    let root = std::env::temp_dir().join(format!("ade-fallback-locks-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let target = Arc::new(Mutex::new(FallbackTarget::polling(
        watch_fingerprint(&root).unwrap(),
    )));
    let scan = target.lock().unwrap().scan_lock();
    let ready = Arc::new(std::sync::Barrier::new(2));
    let held_ready = Arc::clone(&ready);
    let scanning = std::thread::spawn(move || {
        // Exactly what the blocking worker holds, for exactly as long as a slow
        // filesystem would make it.
        let _guard = scan.lock().unwrap();
        held_ready.wait();
        std::thread::sleep(Duration::from_millis(200));
    });
    ready.wait();

    let now = Instant::now();
    let answered = tokio::time::timeout(Duration::from_millis(30), async {
        let due = super::watch_fallback::scan_due(&target, now);
        let retry = super::watch_fallback::native_retry_due(&target, now);
        let native = target.lock().unwrap().is_native();
        target.lock().unwrap().degrade_to_polling();
        super::watch_fallback::record_native_retry(&target, false);
        (due, retry, native)
    })
    .await;
    let (due, _retry, native) =
        answered.expect("an async control task waited on a filesystem scan");
    assert!(due, "a polling target past its backoff is due");
    assert!(!native, "a polling target is not native");

    scanning.join().unwrap();
    fs::remove_dir(root).unwrap();
}

/// Only the exact target whose native registration failed ever polls, and a
/// completed scan that found nothing publishes nothing.
#[test]
fn the_polling_fallback_is_per_target_backed_off_and_silent_while_unchanged() {
    let root = std::env::temp_dir().join(format!("ade-fallback-target-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "one").unwrap();
    let target = Arc::new(Mutex::new(FallbackTarget::polling(
        watch_fingerprint(&root).unwrap(),
    )));
    let mut now = Instant::now();

    // The first completed scan matches the registration fingerprint: nothing
    // changed, so nothing is published and the next scan is deferred.
    let mut turn = advance_target(&target, &root, now).unwrap();
    while turn == FallbackTurn::Scanning {
        turn = advance_target(&target, &root, now).unwrap();
    }
    assert_eq!(turn, FallbackTurn::Unchanged);
    assert_eq!(
        advance_target(&target, &root, now).unwrap(),
        FallbackTurn::Idle,
        "an unchanged target must back off rather than rescan immediately"
    );

    // A real change is reported once the backoff elapses.
    fs::write(root.join("file"), "changed").unwrap();
    now += Duration::from_secs(10);
    let mut turn = advance_target(&target, &root, now).unwrap();
    while turn == FallbackTurn::Scanning {
        turn = advance_target(&target, &root, now).unwrap();
    }
    assert_eq!(turn, FallbackTurn::Changed);

    // A healthy native target is never scanned at all.
    let native = Arc::new(Mutex::new(FallbackTarget::native(0)));
    assert_eq!(
        advance_target(&native, &root, now).unwrap(),
        FallbackTurn::Idle
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn deleted_file_events_use_absolute_root_and_nested_logical_paths() {
    let root_path = std::env::temp_dir().join(format!("ade-delete-path-{}", Uuid::new_v4()));
    fs::create_dir_all(root_path.join("nested")).unwrap();
    fs::write(root_path.join("removed"), "root").unwrap();
    fs::write(root_path.join("nested/removed"), "nested").unwrap();
    let root = Arc::new(RootCapability::capture(root_path.to_str().unwrap()).unwrap());
    let logical_root = root.logical_root().to_owned();
    let root_directory = root.open_root_directory().unwrap();
    let nested_directory = root
        .anchor(&logical_root.join("nested"))
        .unwrap()
        .open_directory()
        .unwrap();
    let root_watch = Watch {
        root_token: root.token().to_owned(),
        root: Arc::clone(&root),
        path: logical_root.to_string_lossy().into_owned(),
        target: logical_root.clone(),
        target_directory: Arc::new(root_directory),
        fallback: Arc::new(Mutex::new(FallbackTarget::native(0))),
    };
    let nested_watch = Watch {
        root_token: root.token().to_owned(),
        root,
        path: logical_root.join("nested").to_string_lossy().into_owned(),
        target: logical_root.join("nested"),
        target_directory: Arc::new(nested_directory),
        fallback: Arc::new(Mutex::new(FallbackTarget::native(0))),
    };
    let root_removed = root_path.join("removed");
    let nested_removed = root_path.join("nested/removed");
    fs::remove_file(&root_removed).unwrap();
    fs::remove_file(&nested_removed).unwrap();

    for (watch_id, watch, removed) in [
        ("root", &root_watch, &root_removed),
        ("nested", &nested_watch, &nested_removed),
    ] {
        let logical_removed = watch.target.join(removed.file_name().unwrap());
        let events = precise_file_events(watch_id, watch, std::slice::from_ref(&logical_removed));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].scope, logical_removed.to_string_lossy());
        let file = events[0].file.as_ref().unwrap();
        assert!(file.deleted);
        assert_eq!(
            file.metadata.as_ref().unwrap().path,
            logical_removed.to_string_lossy()
        );
    }
    fs::remove_dir_all(root_path).unwrap();
}

#[test]
fn independent_watch_ids_are_reference_counted_per_directory() {
    let (root, service) = fixture();
    service
        .watch_directory(root.to_str().unwrap(), "", "explorer")
        .unwrap();
    service
        .watch_directory(root.to_str().unwrap(), "", "editor-parent")
        .unwrap();
    assert_eq!(service.watches.lock().unwrap().len(), 2);
    service.unwatch_directory("explorer").unwrap();
    assert!(
        service
            .watches
            .lock()
            .unwrap()
            .contains_key("editor-parent")
    );
    service.unwatch_directory("editor-parent").unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Releasing one of two watches on the same directory leaves the *native
/// registration* alone — not merely the other map entry.
///
/// Map membership was all this was ever asserted about, and map membership is
/// not the invariant: a surviving watch whose registration was torn down by
/// its neighbour's release believes it is native, is therefore excluded from
/// the polling fallback, and silently stops reporting anything at all. The
/// only proof is a real event arriving after the release, which is what this
/// asks for.
#[tokio::test]
async fn releasing_one_of_two_watches_on_a_directory_keeps_the_survivor_reporting() {
    let root = std::env::temp_dir().join(format!("ade-watch-shared-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let service = Arc::new(FileService::new());
    let closed = Arc::new(AtomicBool::new(false));
    let overflowed = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::channel(8);
    let _registration = crate::service::register_control_event_sink(sender.clone());
    service.spawn_watcher(Arc::clone(&closed), sender, overflowed);
    service
        .watch_directory(root.to_str().unwrap(), "", "explorer")
        .unwrap();
    service
        .watch_directory(root.to_str().unwrap(), "", "editor-parent")
        .unwrap();
    service.unwatch_directory("explorer").unwrap();

    fs::write(root.join("after-release"), "event").unwrap();
    let expected = fs::canonicalize(&root)
        .unwrap()
        .join("after-release")
        .to_string_lossy()
        .into_owned();
    let observed = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(message) = receiver.recv().await {
            if matches!(message, SequencerControl::OrderedEvent(v1::HostEvent { kind, scope, .. })
                if kind == v1::EventKind::FileChanged as i32 && scope == expected)
            {
                return true;
            }
        }
        false
    })
    .await;
    closed.store(true, Ordering::Release);
    assert_eq!(
        observed,
        Ok(true),
        "releasing one watch tore down the native registration the other still held"
    );
    service.unwatch_directory("editor-parent").unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn native_watch_emits_precise_file_change_without_polling() {
    let root = std::env::temp_dir().join(format!("ade-watch-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let service = Arc::new(FileService::new());
    let closed = Arc::new(AtomicBool::new(false));
    let overflowed = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::channel(8);
    let _registration = crate::service::register_control_event_sink(sender.clone());
    service.spawn_watcher(Arc::clone(&closed), sender, overflowed);
    service
        .watch_directory(root.to_str().unwrap(), "", "watch-native")
        .unwrap();
    fs::write(root.join("created"), "event").unwrap();
    let expected = fs::canonicalize(&root)
        .unwrap()
        .join("created")
        .to_string_lossy()
        .into_owned();
    let observed = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(message) = receiver.recv().await {
            if matches!(message, SequencerControl::OrderedEvent(v1::HostEvent {
                kind,
                scope,
                ..
            }) if kind == v1::EventKind::FileChanged as i32 && scope == expected)
            {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    closed.store(true, Ordering::Release);
    assert!(observed);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn native_watch_delete_events_keep_absolute_metadata_for_root_and_nested_paths() {
    let root = std::env::temp_dir().join(format!("ade-watch-delete-{}", Uuid::new_v4()));
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join("removed"), "root").unwrap();
    fs::write(root.join("nested/removed"), "nested").unwrap();
    let service = Arc::new(FileService::new());
    let closed = Arc::new(AtomicBool::new(false));
    let overflowed = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::channel(16);
    let _registration = crate::service::register_control_event_sink(sender.clone());
    service.spawn_watcher(Arc::clone(&closed), sender, overflowed);
    service
        .watch_directory(root.to_str().unwrap(), "", "watch-delete-root")
        .unwrap();
    service
        .watch_directory(root.to_str().unwrap(), "nested", "watch-delete-nested")
        .unwrap();

    let canonical_root = fs::canonicalize(&root).unwrap();
    let expected_root = canonical_root
        .join("removed")
        .to_string_lossy()
        .into_owned();
    let expected_nested = canonical_root
        .join("nested/removed")
        .to_string_lossy()
        .into_owned();
    fs::remove_file(&expected_root).unwrap();
    fs::remove_file(&expected_nested).unwrap();
    let mut expected = BTreeSet::from([expected_root, expected_nested]);
    let observed = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(message) = receiver.recv().await {
            let SequencerControl::OrderedEvent(event) = message else {
                continue;
            };
            if event.kind != v1::EventKind::FileChanged as i32 {
                continue;
            }
            let Some(file) = event.file else { continue };
            let Some(metadata) = file.metadata else {
                continue;
            };
            if file.deleted && metadata.path == event.scope {
                expected.remove(&metadata.path);
            }
            if expected.is_empty() {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    closed.store(true, Ordering::Release);
    assert!(observed);
    fs::remove_dir_all(root).unwrap();
}

/// The bootstrap a watch returns *is* the directory's authoritative listing.
///
/// This is what lets one expansion cost one round trip: a caller that also
/// issued a list would be paying a second remote enumeration for an answer it
/// was already being handed.
#[test]
fn a_watch_bootstrap_is_the_directorys_full_authoritative_listing() {
    let (root, service) = fixture();
    fs::create_dir(root.join("src")).unwrap();
    for name in ["a.txt", "b.txt"] {
        fs::write(root.join(name), name).unwrap();
    }
    let listed = service
        .list_directory(root.to_str().unwrap(), "", "list")
        .unwrap();
    let bootstrap = service
        .watch_directory(root.to_str().unwrap(), "", "watch")
        .unwrap();
    assert!(bootstrap.authoritative);
    assert!(bootstrap.complete);
    assert_eq!(bootstrap.next_page_token, "");
    assert_eq!(
        bootstrap
            .entries
            .iter()
            .map(|entry| (entry.name.clone(), entry.kind, entry.generation))
            .collect::<Vec<_>>(),
        listed
            .entries
            .iter()
            .map(|entry| (entry.name.clone(), entry.kind, entry.generation))
            .collect::<Vec<_>>(),
        "a bootstrap that is not the listing would force a second read"
    );
    service.unwatch_directory("watch").unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Collapsing one directory releases one watch and leaves every other one
/// registered, so a tree with many open folders does not rebuild its watches.
#[test]
fn releasing_one_watch_leaves_every_other_registration_untouched() {
    let (root, service) = fixture();
    fs::create_dir(root.join("src")).unwrap();
    fs::create_dir(root.join("docs")).unwrap();
    for (path, id) in [("", "root"), ("src", "src"), ("docs", "docs")] {
        service
            .watch_directory(root.to_str().unwrap(), path, id)
            .unwrap();
    }
    assert_eq!(service.watches.lock().unwrap().len(), 3);
    service.unwatch_directory("src").unwrap();
    let remaining = service.watches.lock().unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(remaining.contains_key("root"));
    assert!(remaining.contains_key("docs"));
    drop(remaining);
    // Releasing something that was never held is not an error, so a duplicate
    // teardown cannot take a live watch with it.
    service.unwatch_directory("src").unwrap();
    assert_eq!(service.watches.lock().unwrap().len(), 2);
    fs::remove_dir_all(root).unwrap();
}

/// An event about the watched directory itself is not an entry in it.
///
/// Mapped as one it produced a row for the directory inside its own listing,
/// under a second path spelling with a trailing separator — two names for one
/// directory crossing the layer boundary, which every downstream key treats as
/// two different directories.
#[test]
fn an_event_about_the_watched_directory_itself_is_never_a_row_inside_it() {
    let root_path = std::env::temp_dir().join(format!("ade-watch-self-{}", Uuid::new_v4()));
    fs::create_dir_all(&root_path).unwrap();
    fs::write(root_path.join("child"), "x").unwrap();
    let root = Arc::new(RootCapability::capture(root_path.to_str().unwrap()).unwrap());
    let logical_root = root.logical_root().to_owned();
    let directory = root.open_root_directory().unwrap();
    let watch = Watch {
        root_token: root.token().to_owned(),
        root: Arc::clone(&root),
        path: logical_root.to_string_lossy().into_owned(),
        target: logical_root.clone(),
        target_directory: Arc::new(directory),
        fallback: Arc::new(Mutex::new(FallbackTarget::native(0))),
    };

    // Never routed to itself, and never an entry inside itself even if it were.
    let itself = std::slice::from_ref(&logical_root);
    assert!(!changes_by_parent([logical_root.clone()]).contains_key(&logical_root));
    assert!(precise_file_events("self", &watch, itself).is_empty());

    // A child of the same directory is still reported, exactly once.
    let child = logical_root.join("child");
    let events = precise_file_events("self", &watch, std::slice::from_ref(&child));
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].scope,
        logical_root.join("child").to_string_lossy()
    );
    fs::remove_dir_all(root_path).unwrap();
}

/// `recovered_from_overflow` means events were *lost*, not merely replaced.
///
/// The desktop drops its whole cached subtree when it sees this flag, so a
/// listing that simply stands in for a burst of precise events — where nothing
/// was lost — must not set it. Every published snapshot claiming recovery made
/// one ordinary file change invalidate the Explorer's entire cache.
#[tokio::test]
async fn an_authoritative_listing_claims_recovery_only_when_events_were_lost() {
    let root_path = std::env::temp_dir().join(format!("ade-watch-recovery-{}", Uuid::new_v4()));
    fs::create_dir_all(&root_path).unwrap();
    fs::write(root_path.join("kept"), "x").unwrap();
    let service = Arc::new(FileService::new());
    let overflowed = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::channel(8);
    let bootstrap = service
        .watch_directory(root_path.to_str().unwrap(), "", "watch-recovery")
        .unwrap();
    assert_eq!(bootstrap.entries.len(), 1);
    let watch = service
        .watches
        .lock()
        .unwrap()
        .get("watch-recovery")
        .cloned()
        .unwrap();

    for recovery in [false, true] {
        assert!(
            service
                .publish_authoritative_listing(
                    "watch-recovery",
                    &watch,
                    &sender,
                    &overflowed,
                    recovery
                )
                .await
        );
        let Some(SequencerControl::OrderedEvent(event)) = receiver.recv().await else {
            panic!("the authoritative listing was not published");
        };
        let snapshot = event.file.unwrap().directory.unwrap();
        assert!(snapshot.authoritative);
        assert_eq!(snapshot.recovered_from_overflow, recovery);
        assert_eq!(
            snapshot.entries.len(),
            1,
            "the rescan reported an empty directory"
        );
    }
    fs::remove_dir_all(root_path).unwrap();
}

/// A watcher-level failure moves every target onto polling.
///
/// A native registration that has silently stopped delivering reports nothing
/// at all: `is_native()` stays true, so the target is excluded from the polling
/// fallback and is watched by nobody, forever. The ordinary native-retry
/// backoff puts the healthy ones back.
#[tokio::test]
async fn a_failed_native_watcher_puts_every_target_back_on_polling() {
    let root_path = std::env::temp_dir().join(format!("ade-watch-degrade-{}", Uuid::new_v4()));
    fs::create_dir_all(&root_path).unwrap();
    let service = Arc::new(FileService::new());
    let closed = Arc::new(AtomicBool::new(false));
    let (sender, _receiver) = mpsc::channel(8);
    // A real native watcher, or the degrade has nothing to degrade *from* and
    // every assertion below holds against a service that never implemented it.
    service.spawn_watcher(
        Arc::clone(&closed),
        sender,
        Arc::new(AtomicBool::new(false)),
    );
    service
        .watch_directory(root_path.to_str().unwrap(), "", "watch-degrade")
        .unwrap();
    assert!(
        service.fallback_watches().is_empty(),
        "this host has no native watcher, so the degrade cannot be exercised here"
    );

    service.degrade_all_to_polling();

    assert_eq!(
        service.fallback_watches().len(),
        1,
        "a target the native watcher stopped covering is polled by nobody"
    );
    closed.store(true, Ordering::Release);
    fs::remove_dir_all(root_path).unwrap();
}

#[test]
fn reads_never_mark_a_watch_dirty_but_every_change_kind_does() {
    use notify::event::{AccessKind, AccessMode, CreateKind, EventKind, ModifyKind, RemoveKind};
    // The kernel echoes this connection's own reads (open, access,
    // close-no-write) into the watched parent; treating them as dirty made
    // every read broadcast a FileChanged for the file being read, and a read
    // that outlived one event round trip was restarted forever.
    for access in [
        EventKind::Access(AccessKind::Open(AccessMode::Read)),
        EventKind::Access(AccessKind::Read),
        EventKind::Access(AccessKind::Close(AccessMode::Read)),
        EventKind::Access(AccessKind::Any),
    ] {
        assert!(!super::watch_service::marks_watch_dirty(&access), "{access:?}");
    }
    for change in [
        EventKind::Any,
        EventKind::Create(CreateKind::File),
        EventKind::Modify(ModifyKind::Any),
        EventKind::Modify(ModifyKind::Metadata(notify::event::MetadataKind::Any)),
        EventKind::Remove(RemoveKind::File),
        EventKind::Other,
    ] {
        assert!(super::watch_service::marks_watch_dirty(&change), "{change:?}");
    }
}
