//! Claude Code's own record of whether a session is waiting on a person.
//!
//! Every running Claude keeps `~/.claude/sessions/<pid>.json` current. It
//! answers two things hooks cannot: whether a prompt is still open while other
//! subagents fire tool hooks under the same session, and whether a prompt was
//! dismissed, which fires no hook at all.
//!
//! The file is undocumented, so any failure to read or understand it means "no
//! opinion" and the hooks decide alone.

use std::{fs::File, io::Read, os::unix::fs::OpenOptionsExt, path::Path};

use serde::Deserialize;

/// Far beyond any real record.
const MAX_SESSION_FILE_BYTES: u64 = 64 * 1024;
/// One file per running Claude; the bound only limits a directory gone wrong.
const MAX_SESSION_FILES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClaudeSessionStatus {
    Waiting,
    Busy,
    Idle,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionFile {
    session_id: String,
    status: String,
}

/// The status of the live Claude process running `session_id`, if one reports it.
pub(super) fn read_status(directory: &Path, session_id: &str) -> Option<ClaudeSessionStatus> {
    std::fs::read_dir(directory)
        .ok()?
        .flatten()
        .take(MAX_SESSION_FILES)
        .filter(|entry| {
            // A crashed Claude leaves its file behind, frozen at its last status.
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .and_then(|pid| pid.parse::<libc::pid_t>().ok())
                .is_some_and(process_exists)
        })
        .filter_map(|entry| read_session_file(&entry.path()))
        .find(|session| session.session_id == session_id)
        .and_then(|session| match session.status.as_str() {
            "waiting" => Some(ClaudeSessionStatus::Waiting),
            "busy" => Some(ClaudeSessionStatus::Busy),
            "idle" => Some(ClaudeSessionStatus::Idle),
            _ => None,
        })
}

fn read_session_file(path: &Path) -> Option<SessionFile> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SESSION_FILE_BYTES {
        return None;
    }
    let mut contents = Vec::new();
    file.take(MAX_SESSION_FILE_BYTES)
        .read_to_end(&mut contents)
        .ok()?;
    serde_json::from_slice(&contents).ok()
}

fn process_exists(pid: libc::pid_t) -> bool {
    // Signal 0 only probes. EPERM still proves the process exists.
    pid > 0
        && (unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(name: &str) -> std::path::PathBuf {
        let path = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("claude-session-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn own_file(directory: &Path) -> std::path::PathBuf {
        directory.join(format!("{}.json", std::process::id()))
    }

    #[test]
    fn reads_a_real_record() {
        let directory = directory("real");
        std::fs::write(
            own_file(&directory),
            r#"{"pid":632874,"sessionId":"s","cwd":"/x","startedAt":1,"version":"2.1.319","kind":"interactive","tmux":"muxflow:@109.%125","status":"waiting","updatedAt":5,"statusUpdatedAt":5,"waitingFor":"permission prompt"}"#,
        )
        .unwrap();
        assert_eq!(
            read_status(&directory, "s"),
            Some(ClaudeSessionStatus::Waiting)
        );
        assert_eq!(read_status(&directory, "another-session"), None);
    }

    #[test]
    fn every_status_maps_and_an_unknown_one_has_no_opinion() {
        let directory = directory("statuses");
        for (status, expected) in [
            ("waiting", Some(ClaudeSessionStatus::Waiting)),
            ("busy", Some(ClaudeSessionStatus::Busy)),
            ("idle", Some(ClaudeSessionStatus::Idle)),
            ("renamed", None),
        ] {
            std::fs::write(
                own_file(&directory),
                format!(r#"{{"sessionId":"s","status":"{status}"}}"#),
            )
            .unwrap();
            assert_eq!(read_status(&directory, "s"), expected, "{status}");
        }
    }

    #[test]
    fn a_dead_process_file_is_ignored() {
        let directory = directory("dead");
        // Above Linux's PID_MAX of 2^22, so this can never name a process.
        std::fs::write(
            directory.join("99999999.json"),
            r#"{"sessionId":"s","status":"waiting"}"#,
        )
        .unwrap();
        assert_eq!(read_status(&directory, "s"), None);
    }

    #[test]
    fn unreadable_records_have_no_opinion() {
        let directory = directory("unreadable");
        assert_eq!(read_status(&directory.join("missing"), "s"), None);

        std::fs::write(own_file(&directory), "{not json").unwrap();
        assert_eq!(read_status(&directory, "s"), None);

        std::fs::write(
            own_file(&directory),
            format!(
                r#"{{"sessionId":"s","status":"busy","padding":"{}"}}"#,
                "x".repeat(70_000)
            ),
        )
        .unwrap();
        assert_eq!(read_status(&directory, "s"), None, "oversized file");
    }

    #[test]
    fn a_symlinked_record_is_not_followed() {
        let directory = directory("symlink");
        let target = directory.join("target");
        std::fs::write(&target, r#"{"sessionId":"s","status":"busy"}"#).unwrap();
        std::os::unix::fs::symlink(&target, own_file(&directory)).unwrap();
        assert_eq!(read_status(&directory, "s"), None);
    }
}
