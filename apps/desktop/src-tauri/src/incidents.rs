//! Always-on incident journal.
//!
//! Release builds have no devtools and the perf log is opt-in, so every past
//! investigation of "the amber bar keeps flashing" and "one pane froze" ran
//! blind: the code that decides to tear a connection down knows exactly why,
//! and the reason died with the render. This journal is the permanent record
//! of those decisions — connection rebuilds with their trigger, pane watchdog
//! episodes, link degradation and recovery — appended as one JSON line each.
//!
//! It is deliberately not the perf log: that one is a measurement campaign
//! (opt-in, high volume, compiled out of release). Incidents are rare by
//! definition — a healthy session writes a handful of lines — so this stays
//! on everywhere, and a build without it is the blindness this exists to end.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

/// Rotate once the journal passes this size. One rotated file is kept, so the
/// on-disk bound is twice this and history survives at least one rotation.
const MAX_JOURNAL_BYTES: u64 = 2 * 1024 * 1024;
/// A record is one screenful of context, not a payload dump.
const MAX_LINE_BYTES: usize = 8 * 1024;

pub struct IncidentJournal {
    path: Mutex<PathBuf>,
}

impl IncidentJournal {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            path: Mutex::new(directory.join("incidents.jsonl")),
        }
    }

    /// Native-side incidents share the renderer journal without exposing a
    /// second log location. Callers provide already-redacted structured data;
    /// this adds only process/time correlation and never affects app behavior.
    pub(crate) fn record_native(&self, kind: &str, detail: Value) -> Result<(), String> {
        let mut record = Map::new();
        if let Value::Object(detail) = detail {
            record.extend(detail);
        }
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        record.insert("tUnixMillis".into(), Value::String(at.to_string()));
        record.insert("nativePid".into(), Value::from(std::process::id()));
        record.insert("kind".into(), Value::String(kind.to_owned()));
        let line = serde_json::to_string(&record).map_err(|error| error.to_string())?;
        self.append(&line)
    }

    fn append(&self, line: &str) -> Result<(), String> {
        if line.len() > MAX_LINE_BYTES {
            return Err("incident record exceeds byte limit".into());
        }
        let line = line.replace(['\n', '\r'], " ");
        let path = self.path.lock().map_err(|_| "journal lock poisoned")?;
        append_line(&path, &line).map_err(|error| error.to_string())
    }
}

/// Appends one journal line. Failures are reported but must stay harmless:
/// the frontend fires and forgets, and a journal that cannot be written must
/// never become a reason the app misbehaves.
#[tauri::command]
pub fn record_incident(
    journal: tauri::State<'_, IncidentJournal>,
    line: String,
) -> Result<(), String> {
    journal.append(&line)
}

fn append_line(path: &PathBuf, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_JOURNAL_BYTES) {
        // Rename-then-recreate keeps rotation atomic enough for a journal:
        // the worst interleaving loses lines to the old file, never corrupts.
        let _ = fs::rename(path, path.with_extension("jsonl.1"));
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_lines_and_rotates_past_the_cap() {
        let dir = std::env::temp_dir().join(format!("incidents-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("incidents.jsonl");
        append_line(&path, r#"{"kind":"first"}"#).unwrap();
        append_line(&path, r#"{"kind":"second"}"#).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(written.lines().count(), 2);

        fs::write(&path, vec![b'x'; MAX_JOURNAL_BYTES as usize + 1]).unwrap();
        append_line(&path, r#"{"kind":"after-rotation"}"#).unwrap();
        let fresh = fs::read_to_string(&path).unwrap();
        assert_eq!(fresh.lines().count(), 1);
        assert!(path.with_extension("jsonl.1").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn native_records_add_correlation_without_flattening_private_payloads() {
        let dir =
            std::env::temp_dir().join(format!("native-incidents-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let journal = IncidentJournal::new(dir.clone());
        journal
            .record_native(
                "download.test",
                serde_json::json!({
                    "attemptId": "attempt-1", "parentClass": "homeDownloads", "kind": "spoofed",
                }),
            )
            .unwrap();
        let line = fs::read_to_string(dir.join("incidents.jsonl")).unwrap();
        let value: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["kind"], "download.test");
        assert_eq!(value["attemptId"], "attempt-1");
        assert!(value["nativePid"].is_number());
        assert!(value["tUnixMillis"].is_string());
        fs::remove_dir_all(&dir).unwrap();
    }
}
