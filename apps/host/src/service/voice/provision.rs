//! Model layout and readiness (docs/mobile/voice-mode-plan.md §4.4).
//!
//! The sidecar downloads and extracts the archive; this side only knows where
//! the four files must end up and what "complete" means: a `.complete` marker
//! naming the URL it came from, next to four non-empty files.

use std::{
    fs,
    path::{Path, PathBuf},
};

use tmux_agent_protocol::v1;

pub(crate) const MODEL_NAME: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8";
pub(crate) const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2";
/// The archive's size on the release page, for the consent copy.
pub(crate) const MODEL_DOWNLOAD_BYTES: u64 = 487_170_055;
const COMPLETE_MARKER: &str = ".complete";

pub(crate) const PHASE_INSTALLING_RUNTIME: &str = "installing_runtime";
pub(crate) const PHASE_DOWNLOADING: &str = "downloading";
pub(crate) const PHASE_EXTRACTING: &str = "extracting";
pub(crate) const PHASE_VERIFYING: &str = "verifying";
pub(crate) const PHASE_READY: &str = "ready";
pub(crate) const PHASE_FAILED: &str = "failed";

#[derive(Debug, Clone)]
pub(crate) struct ModelLayout {
    dir: PathBuf,
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

    pub(crate) fn encoder(&self) -> PathBuf {
        self.dir.join("encoder.int8.onnx")
    }

    pub(crate) fn decoder(&self) -> PathBuf {
        self.dir.join("decoder.int8.onnx")
    }

    pub(crate) fn joiner(&self) -> PathBuf {
        self.dir.join("joiner.int8.onnx")
    }

    pub(crate) fn tokens(&self) -> PathBuf {
        self.dir.join("tokens.txt")
    }

    /// The marker names the URL and every model file is non-empty.
    pub(crate) fn complete(&self) -> bool {
        fs::read_to_string(self.dir.join(COMPLETE_MARKER)).is_ok_and(|url| url.trim() == MODEL_URL)
            && [self.encoder(), self.decoder(), self.joiner(), self.tokens()]
                .iter()
                .all(|path| {
                    fs::metadata(path)
                        .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
                })
    }

    /// Drops a model that failed verification so the next provision starts
    /// clean rather than loading the same corrupt files again.
    pub(crate) fn remove(&self) {
        let _ = fs::remove_dir_all(&self.dir);
    }

    /// Removes the sidecar's in-progress files (`.partial-*`, `.extract-*`)
    /// beside the model: what a provision the host had to kill leaves behind.
    pub(crate) fn sweep_partials(&self) {
        let Some(parent) = self.dir.parent() else {
            return;
        };
        let Ok(entries) = fs::read_dir(parent) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".partial-") || name.starts_with(".extract-") {
                let path = entry.path();
                if path.is_dir() {
                    let _ = fs::remove_dir_all(&path);
                } else {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }

    /// The header the sidecar's `load` needs.
    pub(crate) fn load_header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut header = serde_json::Map::new();
        header.insert("op".into(), "load".into());
        header.insert(
            "encoder".into(),
            self.encoder().to_string_lossy().into_owned().into(),
        );
        header.insert(
            "decoder".into(),
            self.decoder().to_string_lossy().into_owned().into(),
        );
        header.insert(
            "joiner".into(),
            self.joiner().to_string_lossy().into_owned().into(),
        );
        header.insert(
            "tokens".into(),
            self.tokens().to_string_lossy().into_owned().into(),
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
        header
    }

    /// Writes a complete-looking model for tests.
    #[cfg(test)]
    pub(crate) fn write_fake_complete(&self) {
        fs::create_dir_all(&self.dir).unwrap();
        for path in [self.encoder(), self.decoder(), self.joiner(), self.tokens()] {
            fs::write(path, b"x").unwrap();
        }
        fs::write(self.dir.join(COMPLETE_MARKER), MODEL_URL).unwrap();
    }
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
    fn a_model_is_complete_only_with_the_marker_and_four_non_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let layout = ModelLayout::in_cache(dir.path());
        assert!(!layout.complete());
        layout.write_fake_complete();
        assert!(layout.complete());
        fs::write(layout.tokens(), b"").unwrap();
        assert!(!layout.complete());
        fs::write(layout.tokens(), b"t").unwrap();
        fs::write(layout.dir().join(COMPLETE_MARKER), "https://elsewhere").unwrap();
        assert!(!layout.complete());
        layout.remove();
        assert!(!layout.dir().exists());

        let models = layout.dir().parent().unwrap();
        fs::write(models.join(".partial-77.tar.bz2"), b"half").unwrap();
        fs::create_dir_all(models.join(".extract-77.model")).unwrap();
        fs::write(models.join("keep.txt"), b"unrelated").unwrap();
        layout.sweep_partials();
        assert!(!models.join(".partial-77.tar.bz2").exists());
        assert!(!models.join(".extract-77.model").exists());
        assert!(models.join("keep.txt").exists());
    }
}
