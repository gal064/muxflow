//! Finding `uv`, the one hard dependency of voice mode
//! (docs/mobile/voice-mode-plan.md §4.1).

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use tmux_control::{ExecutableError, resolve_executable};

pub(crate) const OVERRIDE_ENV: &str = "MUXFLOW_UV_PATH";
static AUTOMATIC_UV: Mutex<Option<PathBuf>> = Mutex::new(None);

/// `MUXFLOW_UV_PATH`, then `PATH`, then where the astral installer, cargo and
/// Homebrew put it. Same resolver as tmux, so a daemon started over a
/// non-interactive ssh with a bare `PATH` still finds a user-installed uv.
pub(crate) fn uv_executable() -> Result<PathBuf, ExecutableError> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_executable(
        OVERRIDE_ENV,
        "uv",
        || installer_candidates(home.as_deref()),
        &AUTOMATIC_UV,
    )
}

fn installer_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = home {
        candidates.push(home.join(".local/bin/uv"));
        candidates.push(home.join(".cargo/bin/uv"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/uv"));
    candidates.push(PathBuf::from("/usr/local/bin/uv"));
    candidates
}

/// What `VoiceStatus.detail` says when uv is missing: how to get one.
pub(crate) fn install_hint(error: &ExecutableError) -> String {
    match error {
        ExecutableError::InvalidOverride { .. } => format!(
            "{OVERRIDE_ENV} is set but does not name an absolute executable file; fix or unset it"
        ),
        ExecutableError::NotFound { .. } => format!(
            "uv was not found on this host. Install it with `curl -LsSf https://astral.sh/uv/install.sh | sh` \
             (or `brew install uv`), or set {OVERRIDE_ENV} to its absolute path"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_candidates_cover_the_common_prefixes() {
        let candidates = installer_candidates(Some(Path::new("/home/example")));
        assert_eq!(candidates[0], PathBuf::from("/home/example/.local/bin/uv"));
        assert!(candidates.contains(&PathBuf::from("/home/example/.cargo/bin/uv")));
        assert!(candidates.contains(&PathBuf::from("/opt/homebrew/bin/uv")));
        assert!(candidates.contains(&PathBuf::from("/usr/local/bin/uv")));
    }

    #[test]
    fn the_install_hint_names_the_installer_and_the_override() {
        let hint = install_hint(&ExecutableError::NotFound {
            program: "uv",
            override_env: OVERRIDE_ENV,
        });
        assert!(hint.contains("astral.sh/uv"));
        assert!(hint.contains(OVERRIDE_ENV));
        let invalid = install_hint(&ExecutableError::InvalidOverride {
            override_env: OVERRIDE_ENV,
        });
        assert!(invalid.contains("does not name an absolute executable"));
    }
}
