//! A terminal-link single-file capability may watch its own file, and every
//! output of that watch is narrowed to the one leaf: no sibling name or
//! generation leaves the host, and the parent is never listed.

use super::*;

struct Bound {
    directory: PathBuf,
    leaf: PathBuf,
    root: String,
    token: String,
}

fn bound(label: &str) -> Bound {
    let directory =
        std::env::temp_dir().join(format!("ade-single-watch-{label}-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let directory = fs::canonicalize(&directory).unwrap();
    let leaf = directory.join("note.md");
    fs::write(&leaf, "aaa\n").unwrap();
    fs::write(directory.join("sibling.md"), "secret\n").unwrap();
    let (root, token) = single_file_root(&leaf).unwrap();
    Bound {
        directory,
        leaf,
        root,
        token,
    }
}

fn leaf_generation(path: &Path) -> u64 {
    metadata_generation(&fs::symlink_metadata(path).unwrap())
}

#[test]
fn bootstrap_lists_only_the_bound_file() {
    let bound = bound("bootstrap");
    let service = FileService::new();
    let snapshot = service
        .watch_directory_authorized(
            &bound.root,
            &bound.token,
            bound.leaf.to_str().unwrap(),
            "single-bootstrap",
        )
        .unwrap();
    assert_eq!(snapshot.path, bound.directory.to_string_lossy());
    assert!(snapshot.authoritative && snapshot.complete);
    let paths: Vec<_> = snapshot
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, [bound.leaf.to_str().unwrap()]);
    assert_eq!(snapshot.entries[0].generation, leaf_generation(&bound.leaf));
    fs::remove_dir_all(bound.directory).unwrap();
}

#[test]
fn a_single_file_token_cannot_watch_its_parent_or_a_sibling_or_list() {
    let bound = bound("refused");
    let service = FileService::new();
    for path in [
        bound.directory.to_str().unwrap().to_owned(),
        bound
            .directory
            .join("sibling.md")
            .to_string_lossy()
            .into_owned(),
        bound
            .directory
            .join("missing.md")
            .to_string_lossy()
            .into_owned(),
    ] {
        assert!(
            service
                .watch_directory_authorized(&bound.root, &bound.token, &path, "single-refused")
                .is_err(),
            "a single-file token watched {path}"
        );
    }
    assert!(service.watches.lock().unwrap().is_empty());
    assert!(
        service
            .list_directory_page_authorized(
                &bound.root,
                &bound.token,
                bound.directory.to_str().unwrap(),
                "single-list",
                "",
                0,
                &NEVER_CANCELLED,
            )
            .is_err()
    );
    fs::remove_dir_all(bound.directory).unwrap();
}

#[tokio::test]
async fn native_events_report_the_leaf_and_never_a_sibling() {
    let bound = bound("native");
    let watch_id = format!("single-native-{}", Uuid::new_v4());
    let service = Arc::new(FileService::new());
    let closed = Arc::new(AtomicBool::new(false));
    let overflowed = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::channel(256);
    let _registration = crate::service::register_control_event_sink(sender.clone());
    service.spawn_watcher(Arc::clone(&closed), sender, overflowed);
    service
        .watch_directory_authorized(
            &bound.root,
            &bound.token,
            bound.leaf.to_str().unwrap(),
            &watch_id,
        )
        .unwrap();

    let leaf = bound.leaf.to_string_lossy().into_owned();
    let prefix = format!("{}/", bound.directory.to_string_lossy());
    // Fails the test on any event that names something in this directory other
    // than the leaf, and on any rescan — sibling churn must not cost one.
    // Returns the leaf event carrying the expected generation.
    let mut next_leaf_change = async |expected: u64| {
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(message) = receiver.recv().await {
                let SequencerControl::OrderedEvent(event) = message else {
                    continue;
                };
                assert!(
                    !(event.kind == v1::EventKind::DirectorySnapshot as i32
                        && event.scope == watch_id),
                    "churn beside a single-file watch cost a rescan"
                );
                if event.kind != v1::EventKind::FileChanged as i32
                    || !event.scope.starts_with(&prefix)
                {
                    continue;
                }
                assert_eq!(
                    event.scope, leaf,
                    "a sibling of a single-file watch was reported"
                );
                let file = event.file.unwrap();
                if file.metadata.as_ref().unwrap().generation == expected {
                    return file;
                }
            }
            panic!("the event stream closed");
        })
        .await
        .expect("the leaf change was not reported")
    };

    // A sibling written in place, and a burst past the precise-event cap, each
    // in a batch of its own, then the leaf itself appended in place.
    let batch = Duration::from_millis(300);
    fs::write(bound.directory.join("sibling.md"), "changed\n").unwrap();
    tokio::time::sleep(batch).await;
    for index in 0..(MAX_PRECISE_EVENTS_PER_BATCH + 8) {
        fs::write(bound.directory.join(format!("churn-{index}")), "x").unwrap();
    }
    tokio::time::sleep(batch).await;
    {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&bound.leaf)
            .unwrap();
        file.write_all(b"in place\n").unwrap();
    }
    next_leaf_change(leaf_generation(&bound.leaf)).await;

    // An editor's atomic save: a temporary sibling renamed over the leaf.
    let temporary = bound.directory.join(".note.md.tmp");
    fs::write(&temporary, "replaced\n").unwrap();
    fs::rename(&temporary, &bound.leaf).unwrap();
    next_leaf_change(leaf_generation(&bound.leaf)).await;

    // A symlink swapped in for the leaf is reported as gone, never described:
    // its target text and followed metadata would name a sibling.
    let link = bound.directory.join(".note.md.link");
    std::os::unix::fs::symlink("sibling.md", &link).unwrap();
    fs::rename(&link, &bound.leaf).unwrap();
    let replaced = next_leaf_change(0).await;
    assert!(replaced.deleted);
    assert_eq!(
        replaced.metadata.unwrap(),
        v1::FileMetadata {
            path: leaf.clone(),
            ..Default::default()
        }
    );

    closed.store(true, Ordering::Release);
    service.unwatch_directory(&watch_id).unwrap();
    fs::remove_dir_all(bound.directory).unwrap();
}

#[tokio::test]
async fn a_rescan_publishes_only_the_leaf() {
    let bound = bound("rescan");
    let service = Arc::new(FileService::new());
    let overflowed = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::channel(8);
    service
        .watch_directory_authorized(
            &bound.root,
            &bound.token,
            bound.leaf.to_str().unwrap(),
            "single-rescan",
        )
        .unwrap();
    let watch = service
        .watches
        .lock()
        .unwrap()
        .get("single-rescan")
        .cloned()
        .unwrap();

    // As read, then replaced by a symlink to a sibling, then gone. Only the
    // regular file is ever described.
    for (step, expected) in [("read", 1), ("symlinked", 0), ("removed", 0)] {
        match step {
            "symlinked" => {
                fs::remove_file(&bound.leaf).unwrap();
                std::os::unix::fs::symlink("sibling.md", &bound.leaf).unwrap();
            }
            "removed" => fs::remove_file(&bound.leaf).unwrap(),
            _ => {}
        }
        assert!(
            service
                .publish_authoritative_listing("single-rescan", &watch, &sender, &overflowed, true)
                .await
        );
        let Some(SequencerControl::OrderedEvent(event)) = receiver.recv().await else {
            panic!("the rescan was not published");
        };
        let snapshot = event.file.unwrap().directory.unwrap();
        assert_eq!(snapshot.path, bound.directory.to_string_lossy());
        assert_eq!(snapshot.entries.len(), expected);
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.path == bound.leaf.to_string_lossy())
        );
    }
    fs::remove_dir_all(bound.directory).unwrap();
}

#[test]
fn polling_fallback_follows_the_leaf_and_ignores_siblings() {
    let bound = bound("polling");
    let leaf = OsString::from("note.md");
    let target = Arc::new(Mutex::new(FallbackTarget::polling(
        watch_fingerprint(&bound.directory, Some(&leaf)).unwrap(),
    )));
    let mut now = Instant::now();
    let mut turn = || {
        now += Duration::from_secs(60);
        advance_target(&target, &bound.directory, Some(&leaf), now).unwrap()
    };

    fs::write(bound.directory.join("sibling.md"), "changed\n").unwrap();
    fs::write(bound.directory.join("new-sibling.md"), "new\n").unwrap();
    assert_eq!(turn(), FallbackTurn::Unchanged);

    std::thread::sleep(Duration::from_millis(10));
    fs::write(&bound.leaf, "bbb\n").unwrap();
    assert_eq!(turn(), FallbackTurn::Changed);
    assert_eq!(turn(), FallbackTurn::Unchanged);

    fs::remove_file(&bound.leaf).unwrap();
    assert_eq!(turn(), FallbackTurn::Changed);
    fs::remove_dir_all(bound.directory).unwrap();
}
