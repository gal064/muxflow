//! Opening and saving one file: descriptor-bound classification, bounded
//! content policy, and atomic writes.

use super::*;

/// One descriptor, one classification, one body — for every classification the
/// editor can be handed.
#[test]
fn one_open_classifies_and_carries_exactly_the_content_policy_allows() {
    let (root, service) = fixture();
    fs::write(root.join("note.txt"), "hello").unwrap();
    fs::write(root.join("blob.bin"), [0_u8, 1, 2, 3]).unwrap();
    fs::write(root.join("empty.txt"), "").unwrap();
    fs::write(root.join("tiny.png"), [0x89, b'P', b'N', b'G']).unwrap();

    let text = service
        .open_file_stream(root.to_str().unwrap(), "note.txt")
        .unwrap();
    assert_eq!(text.header().content_kind, v1::FileContentKind::Text as i32);
    assert!(text.header().content_streaming);
    assert_eq!(text.header().total_bytes, 5);
    assert_eq!(text.digest(), blake3::hash(b"hello").to_hex().to_string());
    let header = text.header().clone();
    let metadata = header.metadata.as_ref().unwrap();
    assert_eq!(metadata.generation, header.generation);
    let body: Vec<u8> = text
        .chunks()
        .flat_map(|(_, chunk)| chunk.to_vec())
        .collect();
    assert_eq!(body, b"hello");

    // A binary file is classified and never streamed: the editor cannot show
    // it, so paying for its bytes is pure waste on the remote link.
    let binary = service
        .open_file_stream(root.to_str().unwrap(), "blob.bin")
        .unwrap();
    assert_eq!(
        binary.header().content_kind,
        v1::FileContentKind::Binary as i32
    );
    assert!(!binary.header().content_streaming);
    assert_eq!(binary.chunks().count(), 0);

    // An empty file still streams: zero bytes is content, not an absence.
    let empty = service
        .open_file_stream(root.to_str().unwrap(), "empty.txt")
        .unwrap();
    assert!(empty.header().content_streaming);
    assert_eq!(empty.header().total_bytes, 0);
    assert_eq!(empty.chunks().count(), 0);

    // An eligible image is streamed by the same one request that classified
    // it, so no second "was that an image?" probe is ever needed.
    let image = service
        .open_file_stream(root.to_str().unwrap(), "tiny.png")
        .unwrap();
    assert_eq!(
        image.header().content_kind,
        v1::FileContentKind::Image as i32
    );
    assert!(image.header().content_streaming);
    assert!(
        image
            .header()
            .metadata
            .as_ref()
            .unwrap()
            .image_preview_eligible
    );
    assert_eq!(image.header().total_bytes, 4);

    assert!(
        service
            .open_file_stream(root.to_str().unwrap(), "")
            .is_err()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_oversized_text_file_is_classified_without_being_read() {
    let (root, service) = fixture();
    let path = root.join("huge.txt");
    let file = File::create(&path).unwrap();
    file.set_len(MAX_TEXT_BYTES + 1).unwrap();
    drop(file);
    let opened = service
        .open_file_stream(root.to_str().unwrap(), "huge.txt")
        .unwrap();
    assert_eq!(
        opened.header().content_kind,
        v1::FileContentKind::TooLarge as i32
    );
    assert!(!opened.header().content_streaming);
    assert_eq!(opened.header().total_bytes, 0);
    assert_eq!(opened.chunks().count(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_open_follows_a_symlink_safely_and_refuses_one_that_escapes() {
    let (root, service) = fixture();
    let outside = std::env::temp_dir().join(format!("ade-open-outside-{}", Uuid::new_v4()));
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret"), "no").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    fs::write(root.join("target.txt"), "inside").unwrap();
    std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();

    let opened = service
        .open_file_stream(root.to_str().unwrap(), "link.txt")
        .unwrap();
    let metadata = opened.header().metadata.as_ref().unwrap();
    assert!(metadata.symlink);
    assert_eq!(metadata.symlink_target_kind, v1::FileKind::File as i32);
    assert_eq!(opened.header().total_bytes, 6);
    assert!(
        service
            .open_file_stream(root.to_str().unwrap(), "escape/secret")
            .is_err()
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside).unwrap();
}

#[test]
fn safe_file_symlink_open_and_save_preserve_the_link() {
    let (root, service) = fixture();
    fs::write(root.join("target.txt"), "old").unwrap();
    std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();
    let opened = service
        .open_file_stream(root.to_str().unwrap(), "link.txt")
        .unwrap();
    let metadata = opened.header().metadata.clone().unwrap();
    assert!(metadata.symlink);
    assert_eq!(metadata.symlink_target_kind, v1::FileKind::File as i32);
    service
        .begin_file_write(
            root.to_str().unwrap(),
            "link.txt",
            "symlink-save",
            "save",
            3,
            metadata.generation,
        )
        .unwrap();
    service.write_file_chunk("symlink-save", 0, b"new").unwrap();
    service
        .commit_file_write("symlink-save", blake3::hash(b"new").to_hex().as_ref())
        .unwrap();
    assert_eq!(
        fs::read_link(root.join("link.txt")).unwrap(),
        PathBuf::from("target.txt")
    );
    assert_eq!(fs::read_to_string(root.join("target.txt")).unwrap(), "new");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn text_write_is_atomic_permission_preserving_and_bounded() {
    let (root, service) = fixture();
    let path = root.join("note.md");
    fs::write(&path, "old").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    service
        .begin_file_write(root.to_str().unwrap(), "note.md", "write-1", "save-1", 3, 0)
        .unwrap();
    assert_eq!(service.write_file_chunk("write-1", 0, b"ne").unwrap(), 2);
    assert_eq!(service.write_file_chunk("write-1", 2, b"w").unwrap(), 3);
    let metadata = service
        .commit_file_write("write-1", blake3::hash(b"new").to_hex().as_ref())
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    assert_eq!(metadata.mode & 0o777, 0o640);
    service
        .begin_file_write(root.to_str().unwrap(), "note.md", "write-2", "save-2", 1, 0)
        .unwrap();
    service.write_file_chunk("write-2", 0, &[0]).unwrap();
    assert!(
        service
            .commit_file_write("write-2", blake3::hash(&[0]).to_hex().as_ref())
            .is_err()
    );
    assert!(!root.join(".tmux-ide-save-write-2.partial").exists());
    service
        .begin_file_write(
            root.to_str().unwrap(),
            "note.md",
            "write-max",
            "save-max",
            MAX_TEXT_BYTES,
            0,
        )
        .unwrap();
    service.cancel_file_write("write-max").unwrap();
    assert!(
        service
            .begin_file_write(
                root.to_str().unwrap(),
                "note.md",
                "write-over",
                "save-over",
                MAX_TEXT_BYTES + 1,
                0,
            )
            .is_err()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancelled_file_write_cleans_partial_and_preserves_original() {
    let (root, service) = fixture();
    fs::write(root.join("note.txt"), "original").unwrap();
    service
        .begin_file_write(
            root.to_str().unwrap(),
            "note.txt",
            "cancel-me",
            "save",
            8,
            0,
        )
        .unwrap();
    service
        .write_file_chunk("cancel-me", 0, b"partial")
        .unwrap();
    service.cancel_file_write("cancel-me").unwrap();
    assert_eq!(
        fs::read_to_string(root.join("note.txt")).unwrap(),
        "original"
    );
    assert!(!root.join(".tmux-ide-save-cancel-me.partial").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_commits_are_serialized_last_writer_wins() {
    let (root, service) = fixture();
    fs::write(root.join("note.txt"), "old").unwrap();
    let original = metadata_generation(&fs::metadata(root.join("note.txt")).unwrap());
    for (id, body) in [
        ("writer-a", b"one".as_slice()),
        ("writer-b", b"two".as_slice()),
    ] {
        service
            .begin_file_write(root.to_str().unwrap(), "note.txt", id, id, 3, original)
            .unwrap();
        service.write_file_chunk(id, 0, body).unwrap();
    }
    service
        .commit_file_write("writer-a", blake3::hash(b"one").to_hex().as_ref())
        .unwrap();
    service
        .commit_file_write("writer-b", blake3::hash(b"two").to_hex().as_ref())
        .unwrap();
    assert_eq!(fs::read_to_string(root.join("note.txt")).unwrap(), "two");
    fs::remove_dir_all(root).unwrap();
}

/// A save carrying the generation it opened at cannot clobber a newer file.
///
/// The parameter was taken and discarded, so an editor whose file had been
/// changed underneath it — by an agent in the same pane, by a rebase, by
/// anything — overwrote that change on the next save without a word. The read
/// side of this feature exists to make the claim available: `open_stream`
/// re-stats its own descriptor and refuses a file that moved while it was
/// being opened. This is the write side honouring it.
#[test]
fn a_save_whose_file_moved_since_it_was_opened_is_refused_rather_than_clobbering_it() {
    let (root, service) = fixture();
    fs::write(root.join("note.txt"), "opened").unwrap();
    // The generation the editor is actually handed, taken the way it takes it.
    let opened_at = service
        .open_file_stream(root.to_str().unwrap(), "note.txt")
        .unwrap()
        .header()
        .metadata
        .clone()
        .unwrap()
        .generation;

    // Somebody else writes the file while the editor holds it.
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(root.join("note.txt"), "changed underneath").unwrap();
    let moved = |service: &FileService| {
        service
            .open_file_stream(root.to_str().unwrap(), "note.txt")
            .unwrap()
            .header()
            .metadata
            .clone()
            .unwrap()
            .generation
    };
    assert_ne!(
        moved(&service),
        opened_at,
        "the fixture did not move the file"
    );

    let refused = service
        .begin_file_write(
            root.to_str().unwrap(),
            "note.txt",
            "stale",
            "save",
            4,
            opened_at,
        )
        .expect_err("a stale save was admitted");
    assert!(
        refused.to_string().contains("changed"),
        "the refusal did not name the reason: {refused}",
    );
    assert_eq!(
        fs::read_to_string(root.join("note.txt")).unwrap(),
        "changed underneath",
        "a stale save overwrote the newer file",
    );
    assert!(!root.join(".tmux-ide-save-stale.partial").exists());

    // The same save, re-read at the generation the file actually has, lands.
    let current = moved(&service);
    service
        .begin_file_write(
            root.to_str().unwrap(),
            "note.txt",
            "fresh",
            "save",
            4,
            current,
        )
        .unwrap();
    service.write_file_chunk("fresh", 0, b"mine").unwrap();
    service
        .commit_file_write("fresh", blake3::hash(b"mine").to_hex().as_ref())
        .unwrap();
    assert_eq!(fs::read_to_string(root.join("note.txt")).unwrap(), "mine");

    // And a caller that claims no generation is unaffected: the wire field is
    // optional, and a first write has nothing to compare against.
    service
        .begin_file_write(
            root.to_str().unwrap(),
            "note.txt",
            "unclaimed",
            "save",
            4,
            0,
        )
        .unwrap();
    service.cancel_file_write("unclaimed").unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// One path, one generation, whichever way it was observed.
///
/// The editor accepts its first read when the parent watch's listing agrees
/// with it. If a listing and an open described the same file with different
/// generations, that agreement could never happen and every open of that file
/// class would pay a second full remote read.
#[test]
fn a_listing_and_an_open_report_the_same_generation_for_the_same_path() {
    let (root, service) = fixture();
    fs::write(root.join("plain.txt"), "body").unwrap();
    fs::write(root.join("target.txt"), "body").unwrap();
    std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();

    let listed = service
        .list_directory(root.to_str().unwrap(), "", "list")
        .unwrap();
    let entry = |name: &str| {
        listed
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("{name} is listed"))
            .generation
    };
    for name in ["plain.txt", "link.txt"] {
        let opened = service
            .open_file_stream(root.to_str().unwrap(), name)
            .unwrap();
        let metadata = opened.header().metadata.as_ref().unwrap();
        assert_eq!(
            metadata.generation,
            entry(name),
            "{name} has two generations depending on who asked"
        );
    }

    // The *content's* identity is carried separately, so a save still binds to
    // the bytes rather than to the link that points at them.
    let link = service
        .open_file_stream(root.to_str().unwrap(), "link.txt")
        .unwrap();
    assert_ne!(link.header().generation, entry("link.txt"));
    let plain = service
        .open_file_stream(root.to_str().unwrap(), "plain.txt")
        .unwrap();
    assert_eq!(plain.header().generation, entry("plain.txt"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_cancelled_open_stops_reading_instead_of_finishing_the_file() {
    let (root, service) = fixture();
    fs::write(root.join("note.txt"), vec![b'a'; 4096]).unwrap();
    let cancelled = AtomicBool::new(true);
    let outcome = service.open_file_stream_authorized(
        root.to_str().unwrap(),
        &root_token(root.to_str().unwrap()).unwrap(),
        "note.txt",
        &cancelled,
    );
    let Err(error) = outcome else {
        panic!("a cancelled open still read the file");
    };
    assert_eq!(FileFailure::of(&error), FileFailure::Cancelled);
    fs::remove_dir_all(root).unwrap();
}
