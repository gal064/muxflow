//! The per-user directories the native app stores into: exactly the ones Tauri
//! resolves, so profiles, app state and logs carry over. Pinned by
//! `tests/paths.rs` against the same golden values as
//! `apps/desktop/src-tauri/src/golden_paths.rs`.

use std::path::PathBuf;

use desktop_core::platform::AppPaths;

const IDENTIFIER: &str = "dev.muxflow.desktop";

pub struct NativePaths;

fn resolved(dir: Option<PathBuf>) -> Result<PathBuf, String> {
    dir.ok_or_else(|| "could not resolve a per-user directory".to_owned())
}

impl AppPaths for NativePaths {
    fn app_config_dir(&self) -> Result<PathBuf, String> {
        Ok(resolved(dirs::config_dir())?.join(IDENTIFIER))
    }

    fn app_data_dir(&self) -> Result<PathBuf, String> {
        Ok(resolved(dirs::data_dir())?.join(IDENTIFIER))
    }

    fn app_log_dir(&self) -> Result<PathBuf, String> {
        #[cfg(target_os = "macos")]
        return Ok(resolved(dirs::home_dir())?
            .join("Library/Logs")
            .join(IDENTIFIER));
        #[cfg(not(target_os = "macos"))]
        return Ok(resolved(dirs::data_local_dir())?
            .join(IDENTIFIER)
            .join("logs"));
    }

    fn download_dir(&self) -> Result<PathBuf, String> {
        resolved(dirs::download_dir())
    }
}
