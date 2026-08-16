use super::watch_fallback::{
    FALLBACK_SCAN_ENTRY_BUDGET, FallbackTurn, advance_target, fold_fingerprint_records,
    scan_fallback_shard_with_limits,
};
use super::watch_service::{precise_file_events, watch_matches_events};
use super::*;

mod listing;
mod mutations;
mod open;
mod transfers;
mod watch;

fn fixture() -> (PathBuf, FileService) {
    #[cfg(target_os = "macos")]
    let temporary_root = Path::new("/private/tmp");
    #[cfg(not(target_os = "macos"))]
    let temporary_root = std::env::temp_dir();
    let root = temporary_root.join(format!("ade-files-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    (root, FileService::new())
}
