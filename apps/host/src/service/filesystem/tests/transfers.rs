//! Download streaming and transfer identity.

use super::*;

#[test]
fn download_stream_verifies_blake3_and_cleans_transfer() {
    let (root, service) = fixture();
    let content = vec![7_u8; 2 * 1024 * 1024 + 17];
    fs::write(root.join("large.bin"), &content).unwrap();
    let descriptor = service
        .start_download(root.to_str().unwrap(), "large.bin", false, "transfer-1", 0)
        .unwrap();
    assert_eq!(descriptor.total_bytes, content.len() as u64);
    let mut offset = 0;
    let mut received = Vec::new();
    let digest = loop {
        let chunk = service
            .read_download_chunk("transfer-1", offset, 128 * 1024)
            .unwrap();
        offset += chunk.data.len() as u64;
        received.extend_from_slice(&chunk.data);
        if chunk.eof {
            break chunk.blake3;
        }
    };
    assert_eq!(received, content);
    assert_eq!(digest, blake3::hash(&content).to_hex().to_string());
    assert!(
        service
            .read_download_chunk("transfer-1", offset, 1)
            .is_err()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn terminal_single_file_capability_downloads_only_its_leaf() {
    let (directory, service) = fixture();
    let allowed = directory.join("prompt.md");
    let sibling = directory.join("private.md");
    fs::write(&allowed, "prompt").unwrap();
    fs::write(&sibling, "private").unwrap();
    let (root, token) = single_file_root(&allowed).unwrap();

    let descriptor = service
        .start_download_authorized(
            &root,
            &token,
            allowed.to_str().unwrap(),
            false,
            "terminal-file",
            0,
        )
        .unwrap();
    assert_eq!(descriptor.total_bytes, 6);
    service.cancel_download("terminal-file").unwrap();

    assert!(
        service
            .start_download_authorized(
                &root,
                &token,
                allowed.to_str().unwrap(),
                true,
                "terminal-folder",
                0,
            )
            .is_err()
    );

    assert!(
        service
            .start_download_authorized(
                &root,
                &token,
                sibling.to_str().unwrap(),
                false,
                "terminal-sibling",
                0,
            )
            .is_err()
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn transfer_ids_cannot_be_used_as_paths_or_options() {
    let (root, service) = fixture();
    fs::write(root.join("file"), "data").unwrap();
    for invalid in [
        "../escape",
        "nested/id",
        "--checkpoint-action=exec=sh",
        ".",
        "x y",
    ] {
        assert!(
            service
                .start_download(root.to_str().unwrap(), "file", false, invalid, 0)
                .is_err()
        );
        assert!(
            service
                .begin_file_write(root.to_str().unwrap(), "file", invalid, "operation", 4, 0)
                .is_err()
        );
    }
    assert!(!root.parent().unwrap().join("escape.tar.partial").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exact_generation_rejects_same_size_replacement() {
    let (root, service) = fixture();
    fs::write(root.join("versioned"), "aaaa").unwrap();
    let generation = service
        .open_file_stream(root.to_str().unwrap(), "versioned")
        .unwrap()
        .header()
        .generation;
    fs::write(root.join("replacement"), "bbbb").unwrap();
    fs::rename(root.join("replacement"), root.join("versioned")).unwrap();
    let error = service
        .start_download(
            root.to_str().unwrap(),
            "versioned",
            false,
            "same-size",
            generation,
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("stale_file_generation"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_archive_is_streamed_and_preserves_explicit_contents() {
    let (root, service) = fixture();
    let folder = "--checkpoint=1";
    fs::create_dir(root.join(folder)).unwrap();
    fs::create_dir(root.join(folder).join(".git")).unwrap();
    fs::write(root.join(folder).join(".git/config"), "kept").unwrap();
    fs::write(root.join(folder).join("file"), "body").unwrap();
    std::os::unix::fs::symlink("file", root.join(folder).join("link")).unwrap();
    let descriptor = service
        .start_download(root.to_str().unwrap(), folder, true, "archive-safe", 0)
        .unwrap();
    assert!(descriptor.folder_archive);
    assert!(!descriptor.total_known);
    let mut archive = Vec::new();
    let mut offset = 0;
    loop {
        let chunk = service
            .read_download_chunk("archive-safe", offset, 64 * 1024)
            .unwrap();
        archive.extend_from_slice(&chunk.data);
        offset += chunk.data.len() as u64;
        if chunk.eof {
            assert!(chunk.total_known);
            assert_eq!(chunk.total_bytes, offset);
            break;
        }
        if chunk.data.is_empty() {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let archive_path = root.join("result.tar");
    fs::write(&archive_path, archive).unwrap();
    let output = Command::new("tar")
        .args(["-tf"])
        .arg(&archive_path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let listing = String::from_utf8(output.stdout).unwrap();
    assert!(listing.contains(".git/config"));
    assert!(listing.contains("link"));
    fs::remove_dir_all(root).unwrap();
}
