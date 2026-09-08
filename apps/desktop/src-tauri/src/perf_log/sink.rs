use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PerfLogContract {
    max_lines_per_append: usize,
    max_line_bytes: usize,
    max_file_bytes: u64,
}

/// One source of truth is compiled into native and imported by the renderer.
fn contract() -> &'static PerfLogContract {
    static CONTRACT: OnceLock<PerfLogContract> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        serde_json::from_str(include_str!("../../../perf-log-contract.json"))
            .expect("perf-log-contract.json must remain valid")
    })
}

pub(super) fn configured_path() -> Option<&'static PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let value = std::env::var_os("ADE_PERF_LOG")?;
        let path = PathBuf::from(value);
        path.is_absolute().then_some(path)
    })
    .as_ref()
}

/// True only when the native process owns an absolute opt-in log destination.
#[tauri::command]
pub fn perf_log_enabled() -> bool {
    let enabled = configured_path().is_some();
    if enabled {
        super::operations::start_native_measurement_writer();
    }
    enabled
}

#[tauri::command]
pub async fn append_perf_log(lines: Vec<String>) -> Result<(), String> {
    let Some(path) = configured_path() else {
        return Err("performance logging is not enabled for this process".into());
    };
    if lines.len() > contract().max_lines_per_append {
        return Err("performance log append exceeds its per-call line bound".into());
    }
    if lines.is_empty() {
        return Ok(());
    }
    if lines
        .iter()
        .any(|line| line.len() > contract().max_line_bytes || line.contains(['\r', '\n']))
    {
        return Err("performance log lines must be single-line and bounded".into());
    }
    let body = lines.join("\n");
    tauri::async_runtime::spawn_blocking(move || append_body(path, &body))
        .await
        .map_err(|error| format!("performance log worker failed: {error}"))?
}

pub(super) fn append_body(path: &PathBuf, body: &str) -> Result<(), String> {
    static WRITER: OnceLock<Mutex<()>> = OnceLock::new();
    let _lock = WRITER
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "performance log lock poisoned")?;
    append_body_bounded(path, body, contract().max_file_bytes)
}

fn append_body_bounded(path: &PathBuf, body: &str, max_file_bytes: u64) -> Result<(), String> {
    // A process killed during rotation may leave the unpublished predecessor.
    // Clear it on every later append, not only when the active file fills
    // again, so the on-disk retention bound repairs itself immediately.
    let temporary = suffixed_path(path, ".1.tmp");
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("remove stale performance log rotation: {error}")),
    }
    // One trailing newline is always written. Reject a pathological batch
    // instead of allowing one command to defeat the on-disk bound.
    let maximum_append = u64::try_from(body.len())
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if maximum_append > max_file_bytes {
        return Err("performance log append exceeds the file-size limit".into());
    }
    let length = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if length.saturating_add(maximum_append) > max_file_bytes {
        rotate_recent_complete_lines(path, max_file_bytes)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("open performance log: {error}"))?;
    // A retry after an interrupted write removes the incomplete record. Merely
    // adding a newline would preserve malformed JSON and make the file cease
    // to be valid JSONL until its next rotation.
    let length = file
        .metadata()
        .map_err(|error| format!("stat performance log: {error}"))?
        .len();
    if length > 0 {
        file.seek(SeekFrom::End(-1))
            .map_err(|error| format!("seek performance log: {error}"))?;
        let mut last = [0_u8; 1];
        file.read_exact(&mut last)
            .map_err(|error| format!("read performance log tail: {error}"))?;
        if last[0] != b'\n' {
            file.seek(SeekFrom::Start(0))
                .map_err(|error| format!("seek performance log for repair: {error}"))?;
            let mut contents = Vec::new();
            file.read_to_end(&mut contents)
                .map_err(|error| format!("read performance log for repair: {error}"))?;
            let complete_length = contents
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |boundary| boundary + 1);
            file.set_len(u64::try_from(complete_length).unwrap_or(0))
                .map_err(|error| format!("truncate partial performance log record: {error}"))?;
        }
    }
    writeln!(file, "{body}").map_err(|error| format!("write performance log: {error}"))
}

fn suffixed_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

/// Keeps one bounded predecessor while ensuring both files begin and end on a
/// complete JSONL record. Copying only happens at rotation, never on an append
/// below the cap, and also brings an oversized legacy log under the new bound.
fn rotate_recent_complete_lines(path: &PathBuf, max_file_bytes: u64) -> Result<(), String> {
    let mut source = match OpenOptions::new().read(true).open(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("open performance log for rotation: {error}")),
    };
    let length = source
        .metadata()
        .map_err(|error| format!("stat performance log for rotation: {error}"))?
        .len();
    let start = length.saturating_sub(max_file_bytes);
    source
        .seek(SeekFrom::Start(start))
        .map_err(|error| format!("seek performance log for rotation: {error}"))?;
    let mut recent = Vec::new();
    source
        .read_to_end(&mut recent)
        .map_err(|error| format!("read performance log for rotation: {error}"))?;
    if start > 0 {
        recent = recent
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or_else(Vec::new, |boundary| recent.split_off(boundary + 1));
    }
    if recent.last().is_some_and(|byte| *byte != b'\n') {
        recent.truncate(
            recent
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |boundary| boundary + 1),
        );
    }

    let rotated = suffixed_path(path, ".1");
    let temporary = suffixed_path(path, ".1.tmp");
    // Retention is a total bound, including the unpublished replacement. Drop
    // the older predecessor before materializing its successor so rotation
    // never transiently holds active + predecessor + temporary (48 MiB).
    match fs::remove_file(&rotated) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("remove previous performance log rotation: {error}")),
    }
    let mut backup = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| format!("open rotated performance log: {error}"))?;
    if let Err(error) = backup.write_all(&recent).and_then(|()| backup.sync_all()) {
        drop(backup);
        let _ = fs::remove_file(&temporary);
        return Err(format!("write rotated performance log: {error}"));
    }
    fs::rename(&temporary, &rotated)
        .map_err(|error| format!("publish rotated performance log: {error}"))?;
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| format!("reset performance log after rotation: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{append_body, append_body_bounded, contract, suffixed_path};
    use std::{fs, io::Write};
    use uuid::Uuid;

    #[test]
    fn shared_contract_stays_at_the_reviewed_native_boundary() {
        assert_eq!(contract().max_lines_per_append, 4_096);
        assert_eq!(contract().max_line_bytes, 4_096);
        assert_eq!(contract().max_file_bytes, 16 * 1024 * 1024);
    }

    #[test]
    fn retry_after_a_partial_prefix_starts_at_a_recoverable_record_boundary() {
        let path = std::env::temp_dir().join(format!("ade-perf-partial-{}.jsonl", Uuid::new_v4()));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(br#"{"recordId":"run:1"#).unwrap();
        drop(file);
        let complete = r#"{"recordId":"run:1","name":"paint","ms":1}"#;
        append_body(&path, complete).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, format!("{complete}\n"));
        for line in contents.lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rotation_keeps_recent_complete_lines_and_bounds_both_files() {
        let path = std::env::temp_dir().join(format!("ade-perf-rotate-{}.jsonl", Uuid::new_v4()));
        fs::write(&path, b"discarded-prefix\n{\"id\":1}\n{\"id\":2}\npartial").unwrap();
        append_body_bounded(&path, r#"{"id":3}"#, 34).unwrap();

        let rotated = fs::read_to_string(suffixed_path(&path, ".1")).unwrap();
        assert_eq!(rotated, "{\"id\":1}\n{\"id\":2}\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"id\":3}\n");
        assert!(fs::metadata(&path).unwrap().len() <= 34);
        assert!(fs::metadata(suffixed_path(&path, ".1")).unwrap().len() <= 34);

        fs::remove_file(suffixed_path(&path, ".1")).unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn one_append_cannot_exceed_the_file_cap() {
        let path = std::env::temp_dir().join(format!("ade-perf-oversize-{}.jsonl", Uuid::new_v4()));
        let error = append_body_bounded(&path, "12345678", 8).unwrap_err();
        assert_eq!(error, "performance log append exceeds the file-size limit");
        assert!(!path.exists());
    }

    #[test]
    fn a_second_rotation_replaces_the_predecessor() {
        let path = std::env::temp_dir().join(format!("ade-perf-replace-{}.jsonl", Uuid::new_v4()));
        fs::write(&path, b"{\"a\":1}\n").unwrap();
        append_body_bounded(&path, r#"{"b":2}"#, 12).unwrap();
        assert_eq!(
            fs::read_to_string(suffixed_path(&path, ".1")).unwrap(),
            "{\"a\":1}\n",
        );

        append_body_bounded(&path, r#"{"c":3}"#, 12).unwrap();
        assert_eq!(
            fs::read_to_string(suffixed_path(&path, ".1")).unwrap(),
            "{\"b\":2}\n",
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"c\":3}\n");

        fs::remove_file(suffixed_path(&path, ".1")).unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_stale_rotation_temporary_is_removed_before_a_small_append() {
        let path = std::env::temp_dir().join(format!("ade-perf-stale-{}.jsonl", Uuid::new_v4()));
        let temporary = suffixed_path(&path, ".1.tmp");
        fs::write(&temporary, vec![b'x'; 16]).unwrap();

        append_body_bounded(&path, r#"{"id":1}"#, 32).unwrap();

        assert!(!temporary.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"id\":1}\n");
        fs::remove_file(path).unwrap();
    }
}
