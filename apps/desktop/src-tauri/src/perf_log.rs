//! Phase 12 latency-instrumentation sink.
//!
//! The renderer owns several of the phase's budgets (keystroke to painted
//! glyph, action to interactive pane, tab switch, explorer expand), so the only
//! honest way to close them is to record spans inside the renderer and persist
//! them for the harness to read. This sink exists for exactly that.
//!
//! It is opt-in from the process environment, never from the page: without
//! `ADE_PERF_LOG` naming an absolute path, every append is refused. A
//! compromised or merely buggy frontend therefore cannot use this to write
//! anywhere, and a normal launch has no instrumentation surface at all.

use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

/// Bounds one append so a runaway renderer cannot fill the disk in one call.
const MAX_LINES_PER_APPEND: usize = 4_096;
const MAX_LINE_BYTES: usize = 4_096;

fn configured_path() -> Option<&'static PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let value = std::env::var_os("ADE_PERF_LOG")?;
        let path = PathBuf::from(value);
        // An absolute path keeps the destination independent of the working
        // directory the app happened to inherit.
        path.is_absolute().then_some(path)
    })
    .as_ref()
}

/// True when this process was started with instrumentation enabled. The
/// frontend asks first and stays completely inert when the answer is no.
#[tauri::command]
pub fn perf_log_enabled() -> bool {
    configured_path().is_some()
}

#[tauri::command]
pub async fn append_perf_log(lines: Vec<String>) -> Result<(), String> {
    let Some(path) = configured_path() else {
        return Err("performance logging is not enabled for this process".into());
    };
    if lines.len() > MAX_LINES_PER_APPEND {
        return Err("performance log append exceeds its per-call line bound".into());
    }
    if lines
        .iter()
        .any(|line| line.len() > MAX_LINE_BYTES || line.contains(['\r', '\n']))
    {
        return Err("performance log lines must be single-line and bounded".into());
    }
    // One writer at a time keeps concurrent flushes from interleaving records.
    static WRITER: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = WRITER.get_or_init(|| Mutex::new(()));
    let body = lines.join("\n");
    tauri::async_runtime::spawn_blocking(move || {
        let _lock = guard.lock().map_err(|_| "performance log lock poisoned")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| format!("open performance log: {error}"))?;
        writeln!(file, "{body}").map_err(|error| format!("write performance log: {error}"))
    })
    .await
    .map_err(|error| format!("performance log worker failed: {error}"))?
}
