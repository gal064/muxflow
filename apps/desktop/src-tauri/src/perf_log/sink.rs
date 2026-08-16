use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PerfLogContract {
    max_lines_per_append: usize,
    max_line_bytes: usize,
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
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("open performance log: {error}"))?;
    // A retry after a partial write starts on a fresh line, while healthy
    // appends remain strict JSONL with no synthetic blank records.
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
            file.write_all(b"\n")
                .map_err(|error| format!("repair performance log boundary: {error}"))?;
        }
    }
    writeln!(file, "{body}").map_err(|error| format!("write performance log: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{append_body, contract};
    use std::{fs, io::Write};
    use uuid::Uuid;

    #[test]
    fn shared_contract_stays_at_the_reviewed_native_boundary() {
        assert_eq!(contract().max_lines_per_append, 4_096);
        assert_eq!(contract().max_line_bytes, 4_096);
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
        let records = contents
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect::<Vec<_>>();
        assert_eq!(
            records,
            vec![serde_json::from_str::<serde_json::Value>(complete).unwrap()]
        );
        fs::remove_file(path).unwrap();
    }
}
