//! Model layout and readiness (docs/mobile/voice-mode-plan.md §4.4).
//!
//! The sidecar downloads and verifies the GGUF; this side owns its exact
//! identity and what "complete" means. The marker is written only after the
//! expected byte count and SHA-256 have both matched.

use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tmux_agent_protocol::v1;

pub(crate) const MODEL_NAME: &str = "parakeet-unified-en-0.6b-q8_0";
pub(crate) const MODEL_FILENAME: &str = "parakeet-unified-en-0.6b-Q8_0.gguf";
pub(crate) const MODEL_REVISION: &str = "7e948f21b7bdbac698d3318db9d350f1096f3b6c";
pub(crate) const MODEL_URL: &str = "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/7e948f21b7bdbac698d3318db9d350f1096f3b6c/parakeet-unified-en-0.6b-Q8_0.gguf";
pub(crate) const MODEL_DOWNLOAD_BYTES: u64 = 731_357_568;
pub(crate) const MODEL_SHA256: &str =
    "4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795";
const COMPLETE_MARKER: &str = ".complete";
const OBSOLETE_MODEL_NAME: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8";
#[cfg(test)]
const FAKE_INTEGRITY_MARKER: &str = ".fake-integrity";

pub(crate) const PHASE_INSTALLING_RUNTIME: &str = "installing_runtime";
pub(crate) const PHASE_DOWNLOADING: &str = "downloading";
pub(crate) const PHASE_VERIFYING: &str = "verifying";
pub(crate) const PHASE_READY: &str = "ready";
pub(crate) const PHASE_FAILED: &str = "failed";

#[derive(Debug, Clone)]
pub(crate) struct ModelLayout {
    dir: PathBuf,
}

#[derive(Debug)]
pub(crate) struct ProvisionLock(fs::File);

impl Drop for ProvisionLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned by this guard and remains open for
        // the entire advisory-lock lifetime.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl ModelLayout {
    pub(crate) fn in_cache(cache_dir: &Path) -> Self {
        Self {
            dir: cache_dir.join("models").join(MODEL_NAME),
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn model(&self) -> PathBuf {
        self.dir.join(MODEL_FILENAME)
    }

    /// Status avoids re-hashing 697 MiB on every probe. Only the provisioner
    /// can create this exact marker, after hashing the completed download; the
    /// size check catches later truncation and a native load failure removes
    /// the whole layout.
    pub(crate) fn complete(&self) -> bool {
        fs::read_to_string(self.dir.join(COMPLETE_MARKER))
            .is_ok_and(|marker| marker == marker_contents())
            && fs::metadata(self.model())
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == MODEL_DOWNLOAD_BYTES)
    }

    /// Re-hashes before each cold native load. STATUS stays cheap, while a
    /// same-length file changed after provisioning cannot silently run.
    pub(crate) fn lock_and_verify(&self) -> io::Result<(ProvisionLock, bool)> {
        let lock = self.try_lock()?;
        let verified = self.verify_integrity_unlocked();
        Ok((lock, verified))
    }

    fn verify_integrity_unlocked(&self) -> bool {
        if !self.complete() {
            return false;
        }
        #[cfg(test)]
        if self.dir.join(FAKE_INTEGRITY_MARKER).is_file() {
            return true;
        }
        file_matches_sha256(&self.model(), MODEL_DOWNLOAD_BYTES, MODEL_SHA256)
    }

    /// Drops a model that failed verification so the next provision starts
    /// clean rather than loading the same corrupt files again.
    pub(crate) fn remove_locked(&self, _lock: &ProvisionLock) {
        let _ = fs::remove_dir_all(&self.dir);
    }

    #[cfg(test)]
    pub(crate) fn remove(&self) {
        let _ = fs::remove_dir_all(&self.dir);
    }

    /// Reclaims files left by a force-killed sidecar, but only when no other
    /// process owns the shared provisioning lock.
    pub(crate) fn cleanup_partials(&self) -> io::Result<()> {
        let _lock = self.try_lock()?;
        let Some(parent) = self.dir.parent() else {
            return Ok(());
        };
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(".partial-") && !name.starts_with(".extract-") {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    /// Removes the one cache layout made unreachable by this model switch.
    /// This is cleanup only; no code can load or provision the old model.
    pub(crate) fn remove_obsolete_model(&self) {
        if let Some(models) = self.dir.parent() {
            let _ = fs::remove_dir_all(models.join(OBSOLETE_MODEL_NAME));
        }
    }

    fn try_lock(&self) -> io::Result<ProvisionLock> {
        let parent = self
            .dir
            .parent()
            .ok_or_else(|| io::Error::other("model directory has no parent"))?;
        fs::create_dir_all(parent)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(parent.join(".provision.lock"))?;
        // SAFETY: flock only observes the valid descriptor owned by `file`.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ProvisionLock(file))
    }

    /// The header the sidecar's `load` needs.
    pub(crate) fn load_header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut header = serde_json::Map::new();
        header.insert("op".into(), "load".into());
        header.insert(
            "model".into(),
            self.model().to_string_lossy().into_owned().into(),
        );
        header
    }

    pub(crate) fn provision_header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut header = serde_json::Map::new();
        header.insert("op".into(), "provision".into());
        header.insert(
            "model_dir".into(),
            self.dir.to_string_lossy().into_owned().into(),
        );
        header.insert("url".into(), MODEL_URL.into());
        header.insert("revision".into(), MODEL_REVISION.into());
        header.insert("filename".into(), MODEL_FILENAME.into());
        header.insert("size".into(), MODEL_DOWNLOAD_BYTES.into());
        header.insert("sha256".into(), MODEL_SHA256.into());
        header.insert("marker".into(), marker_contents().into());
        header
    }

    /// Writes a complete-looking model for tests.
    #[cfg(test)]
    pub(crate) fn write_fake_complete(&self) {
        fs::create_dir_all(&self.dir).unwrap();
        let model = fs::File::create(self.model()).unwrap();
        model.set_len(MODEL_DOWNLOAD_BYTES).unwrap();
        fs::write(self.dir.join(COMPLETE_MARKER), marker_contents()).unwrap();
        fs::write(self.dir.join(FAKE_INTEGRITY_MARKER), b"").unwrap();
    }
}

fn marker_contents() -> String {
    format!("{MODEL_URL}\n{MODEL_REVISION}\n{MODEL_DOWNLOAD_BYTES}\n{MODEL_SHA256}\n")
}

fn file_matches_sha256(path: &Path, expected_size: u64, expected_sha256: &str) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.len() != expected_size {
        return false;
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let Ok(length) = file.read(&mut buffer) else {
            return false;
        };
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    format!("{:x}", digest.finalize()) == expected_sha256
}

pub(crate) fn progress(
    operation_id: &str,
    phase: &str,
    transferred_bytes: u64,
    total_bytes: u64,
    error: &str,
) -> v1::VoiceProvisionProgress {
    v1::VoiceProvisionProgress {
        operation_id: operation_id.to_owned(),
        phase: phase.to_owned(),
        transferred_bytes,
        total_bytes,
        error: error.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_is_complete_only_with_the_exact_marker_and_gguf_size() {
        let dir = tempfile::tempdir().unwrap();
        let layout = ModelLayout::in_cache(dir.path());
        assert!(!layout.complete());
        layout.write_fake_complete();
        assert!(layout.complete());
        fs::File::create(layout.model())
            .unwrap()
            .set_len(MODEL_DOWNLOAD_BYTES - 1)
            .unwrap();
        assert!(!layout.complete());
        fs::File::create(layout.model())
            .unwrap()
            .set_len(MODEL_DOWNLOAD_BYTES)
            .unwrap();
        fs::write(layout.dir().join(COMPLETE_MARKER), "https://elsewhere").unwrap();
        assert!(!layout.complete());
        layout.remove();
        assert!(!layout.dir().exists());
    }

    #[test]
    fn headers_pin_one_model_and_its_integrity_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let layout = ModelLayout::in_cache(dir.path());
        let load = layout.load_header();
        assert_eq!(load["model"], layout.model().to_string_lossy().as_ref());
        assert!(load.get("encoder").is_none());

        let provision = layout.provision_header();
        assert_eq!(provision["url"], MODEL_URL);
        assert_eq!(provision["revision"], MODEL_REVISION);
        assert_eq!(provision["filename"], MODEL_FILENAME);
        assert_eq!(provision["size"], MODEL_DOWNLOAD_BYTES);
        assert_eq!(provision["sha256"], MODEL_SHA256);
        assert_eq!(provision["marker"], marker_contents());
    }

    #[test]
    fn integrity_hashes_content_and_obsolete_cleanup_is_exact() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("model.gguf");
        fs::write(&file, b"abc").unwrap();
        assert!(file_matches_sha256(
            &file,
            3,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));
        fs::write(&file, b"abd").unwrap();
        assert!(!file_matches_sha256(
            &file,
            3,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));

        let layout = ModelLayout::in_cache(dir.path());
        let models = layout.dir().parent().unwrap();
        fs::create_dir_all(models.join(OBSOLETE_MODEL_NAME)).unwrap();
        fs::create_dir_all(models.join("keep-model")).unwrap();
        layout.remove_obsolete_model();
        assert!(!models.join(OBSOLETE_MODEL_NAME).exists());
        assert!(models.join("keep-model").exists());
    }

    #[test]
    fn provision_lock_serializes_verification_and_partial_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let layout = ModelLayout::in_cache(dir.path());
        layout.write_fake_complete();
        let (lock, verified) = layout.lock_and_verify().unwrap();
        assert!(verified);
        assert_eq!(
            layout.lock_and_verify().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        let models = layout.dir().parent().unwrap();
        fs::write(models.join(".partial-dead.gguf"), b"half").unwrap();
        assert_eq!(
            layout.cleanup_partials().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(lock);
        layout.cleanup_partials().unwrap();
        assert!(!models.join(".partial-dead.gguf").exists());
    }
}
