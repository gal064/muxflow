use std::{
    ffi::{CString, OsStr},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    os::unix::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawFd, FromRawFd},
    },
    path::Path,
};

use serde_json::Value;

const MAX_TRANSCRIPT_TAIL_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApprovalReviewer {
    AutoReview,
    User,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnTerminal {
    Completed,
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChildTransition {
    pub child_id: String,
    pub active: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ChildPoll {
    pub readable: bool,
    pub transitions: Vec<ChildTransition>,
}

/// In-memory observation of one exact Codex turn.
///
/// The validated file descriptor is retained instead of the supplied path, so
/// a later pathname replacement cannot redirect maintenance outside the
/// sessions root. Polling first compares the file length and only reads the
/// bounded tail after Codex appended another transcript record.
pub(crate) struct TurnMonitor {
    transcript: File,
    turn_id: String,
    /// File length at the last poll, used to avoid rereading an unchanged
    /// partial append every maintenance pass.
    observed_len: u64,
    /// Absolute byte immediately after the last complete JSONL newline.
    completed_len: u64,
}

impl TurnMonitor {
    pub(crate) fn open(payload: &Value, home: &Path) -> Option<Self> {
        let turn_id = string_field(payload, &["turn_id", "turnId"]).unwrap_or_default();
        let supplied_path = Path::new(string_field(
            payload,
            &["transcript_path", "transcriptPath"],
        )?);
        let transcript = TranscriptTarget::resolve(home, supplied_path)?.open()?;
        Some(Self {
            transcript,
            turn_id: turn_id.to_owned(),
            observed_len: 0,
            completed_len: 0,
        })
    }

    pub(crate) fn approval_reviewer(&mut self) -> Option<ApprovalReviewer> {
        let (tail, offset) = self.read_tail(false)?;
        for record in complete_records(&tail, offset) {
            if record.get("type").and_then(Value::as_str) != Some("turn_context")
                || record.pointer("/payload/turn_id").and_then(Value::as_str)
                    != Some(self.turn_id.as_str())
            {
                continue;
            }
            return Some(
                match record
                    .pointer("/payload/approvals_reviewer")
                    .and_then(Value::as_str)
                {
                    Some("auto_review") => ApprovalReviewer::AutoReview,
                    Some("user") => ApprovalReviewer::User,
                    _ => ApprovalReviewer::Unknown,
                },
            );
        }
        None
    }

    pub(crate) fn poll_terminal(&mut self) -> Option<TurnTerminal> {
        let (tail, offset) = self.read_tail(true)?;
        for record in complete_records(&tail, offset) {
            if record.get("type").and_then(Value::as_str) != Some("event_msg")
                || record.pointer("/payload/turn_id").and_then(Value::as_str)
                    != Some(self.turn_id.as_str())
            {
                continue;
            }
            match record.pointer("/payload/type").and_then(Value::as_str) {
                Some("turn_aborted") => return Some(TurnTerminal::Aborted),
                Some("task_complete") => return Some(TurnTerminal::Completed),
                _ => {}
            }
        }
        None
    }

    pub(crate) fn is_readable(&self) -> bool {
        self.transcript
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.nlink() > 0)
    }

    /// Read exact Codex child activity records in append order.
    ///
    /// `SubAgentActivity` is an internal transcript item rather than hook
    /// input. Keep its parser intentionally narrow: accepting a similarly
    /// named object elsewhere would let arbitrary tool output drive lifecycle
    /// state. The returned values contain only the opaque child ID and its
    /// active/inactive edge.
    pub(crate) fn poll_children(&mut self) -> ChildPoll {
        let Some((tail, offset)) = self.read_tail(true) else {
            return ChildPoll {
                readable: self
                    .transcript
                    .metadata()
                    .is_ok_and(|metadata| metadata.is_file() && metadata.nlink() > 0),
                transitions: Vec::new(),
            };
        };
        let transitions = complete_records_chronological(&tail, offset)
            .filter_map(|record| {
                if record.get("type").and_then(Value::as_str) != Some("event_msg")
                    || record.pointer("/payload/type").and_then(Value::as_str)
                        != Some("item_completed")
                    || record.pointer("/payload/item/type").and_then(Value::as_str)
                        != Some("SubAgentActivity")
                {
                    return None;
                }
                let child_id = record
                    .pointer("/payload/item/agent_thread_id")
                    .and_then(Value::as_str)
                    .filter(|id| {
                        !id.is_empty()
                            && id.len()
                                <= crate::service::agents::adapters::MAX_CODEX_SUBAGENT_ID_BYTES
                    })?;
                let active = match record.pointer("/payload/item/kind").and_then(Value::as_str) {
                    Some("started" | "interacted") => true,
                    Some("completed" | "interrupted") => false,
                    _ => return None,
                };
                Some(ChildTransition {
                    child_id: child_id.to_owned(),
                    active,
                })
            })
            .collect();
        ChildPoll {
            readable: true,
            transitions,
        }
    }

    fn read_tail(&mut self, only_if_grown: bool) -> Option<(Vec<u8>, u64)> {
        let metadata = self.transcript.metadata().ok()?;
        if !metadata.is_file() || metadata.nlink() == 0 {
            return None;
        }
        if only_if_grown && metadata.len() <= self.observed_len {
            return None;
        }
        let tail_floor = metadata.len().saturating_sub(MAX_TRANSCRIPT_TAIL_BYTES);
        // For a monitor poll, start one byte before the previous EOF. A
        // well-formed JSONL file leaves a newline there, so the existing
        // leading-fragment rule begins at the first newly appended record.
        // If the previous write was partial, that same rule safely discards
        // the remainder when it eventually receives its newline.
        let offset = if only_if_grown && self.completed_len > 0 {
            self.completed_len.saturating_sub(1).max(tail_floor)
        } else {
            tail_floor
        };
        self.transcript.seek(SeekFrom::Start(offset)).ok()?;
        let mut tail = Vec::with_capacity(
            metadata
                .len()
                .saturating_sub(offset)
                .try_into()
                .unwrap_or(0),
        );
        self.transcript
            .by_ref()
            .take(MAX_TRANSCRIPT_TAIL_BYTES)
            .read_to_end(&mut tail)
            .ok()?;
        // Advance only through the last complete JSONL record. If maintenance
        // catches Codex mid-write, the next growth poll rereads that bounded
        // suffix and can parse the record after its terminating newline lands.
        self.observed_len = metadata.len();
        self.completed_len = tail
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(offset, |newline| {
                offset.saturating_add(newline as u64).saturating_add(1)
            });
        Some((tail, offset))
    }
}

impl ApprovalReviewer {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AutoReview => "auto_review",
            Self::User => "user",
            Self::Unknown => "unknown",
        }
    }
}

/// Best-effort classification of a Codex turn's approval reviewer.
///
/// Hook input is untrusted. The transcript must resolve to a regular JSONL file
/// below this user's Codex session root, and only a bounded tail is inspected.
/// Every failure returns `None`; ingest leaves that turn unclassified and
/// keeps it Working. Only an exact `user` result may raise Blocked, avoiding a
/// false notification when Codex writes the context just after its hook. A
/// matched context with an unknown reviewer remains explicit so it clears an
/// earlier cached result.
pub(crate) fn approval_reviewer(payload: &Value, home: &Path) -> Option<ApprovalReviewer> {
    TurnMonitor::open(payload, home)?.approval_reviewer()
}

fn complete_lines(tail: &[u8], offset: u64) -> &[u8] {
    let after_partial_prefix = if offset > 0 {
        tail.iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |first_newline| &tail[first_newline + 1..])
    } else {
        tail
    };
    after_partial_prefix
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(&[][..], |last_newline| {
            &after_partial_prefix[..last_newline]
        })
}

fn complete_records(tail: &[u8], offset: u64) -> impl Iterator<Item = Value> + '_ {
    complete_lines(tail, offset)
        .split(|byte| *byte == b'\n')
        .rev()
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
}

fn complete_records_chronological(tail: &[u8], offset: u64) -> impl Iterator<Item = Value> + '_ {
    complete_lines(tail, offset)
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
}

/// A transcript path resolved relative to an inode-bound sessions directory.
/// Opening walks each component with `O_NOFOLLOW`, so replacing a validated
/// parent pathname cannot redirect the later read outside the captured root.
struct TranscriptTarget {
    sessions: File,
    relative: std::path::PathBuf,
}

impl TranscriptTarget {
    fn resolve(home: &Path, supplied_path: &Path) -> Option<Self> {
        if !supplied_path.is_absolute() {
            return None;
        }
        if supplied_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return None;
        }
        let sessions_path = fs::canonicalize(home.join(".codex").join("sessions")).ok()?;
        let expected = fs::metadata(&sessions_path).ok()?;
        let sessions = open_at(
            libc::AT_FDCWD,
            sessions_path.as_os_str(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )?;
        let captured = sessions.metadata().ok()?;
        if expected.dev() != captured.dev() || expected.ino() != captured.ino() {
            return None;
        }

        let transcript_path = fs::canonicalize(supplied_path).ok()?;
        if transcript_path.extension() != Some(OsStr::new("jsonl")) {
            return None;
        }
        let relative = transcript_path.strip_prefix(&sessions_path).ok()?;
        if relative.components().count() == 0
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return None;
        }
        Some(Self {
            sessions,
            relative: relative.to_owned(),
        })
    }

    fn open(self) -> Option<File> {
        let mut directory = self.sessions;
        let mut components = self.relative.components().peekable();
        while let Some(component) = components.next() {
            let std::path::Component::Normal(name) = component else {
                return None;
            };
            if components.peek().is_some() {
                directory = open_at(
                    directory.as_raw_fd(),
                    name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )?;
            } else {
                let transcript = open_at(
                    directory.as_raw_fd(),
                    name,
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )?;
                return transcript.metadata().ok()?.is_file().then_some(transcript);
            }
        }
        None
    }
}

fn open_at(parent: i32, path: &OsStr, flags: i32) -> Option<File> {
    let path = CString::new(path.as_bytes()).ok()?;
    // SAFETY: `path` is a live C string and a successful `openat` returns one
    // fresh descriptor whose ownership is transferred immediately to `File`.
    let descriptor = unsafe { libc::openat(parent, path.as_ptr(), flags) };
    if descriptor < 0 {
        return None;
    }
    // SAFETY: the successful call above returned a uniquely owned descriptor.
    Some(unsafe { File::from_raw_fd(descriptor) })
}

fn string_field<'a>(payload: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| payload.get(name).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write as _, os::unix::fs::symlink};

    fn sessions(home: &Path) -> std::path::PathBuf {
        let sessions = home.join(".codex/sessions/2026/08/22");
        fs::create_dir_all(&sessions).unwrap();
        sessions
    }

    fn payload(path: &Path, turn_id: &str) -> Value {
        serde_json::json!({
            "turn_id": turn_id,
            "transcript_path": path,
        })
    }

    fn context(turn_id: &str, reviewer: &str) -> String {
        serde_json::json!({
            "type": "turn_context",
            "payload": {
                "turn_id": turn_id,
                "approval_policy": "on-request",
                "approvals_reviewer": reviewer,
            }
        })
        .to_string()
    }

    fn terminal(turn_id: &str, kind: &str) -> String {
        serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": kind,
                "turn_id": turn_id,
            }
        })
        .to_string()
    }

    fn child(child_id: &str, kind: &str) -> String {
        serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": "item_completed",
                "item": {
                    "type": "SubAgentActivity",
                    "kind": kind,
                    "agent_thread_id": child_id,
                    "agent_path": "/private/not-retained",
                }
            }
        })
        .to_string()
    }

    #[test]
    fn matches_the_current_turn_and_ignores_an_incomplete_final_record() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("rollout.jsonl");
        fs::write(
            &transcript,
            format!(
                "{}\n{}\n{{\"type\":\"turn_context\"",
                context("wanted", "user"),
                context("other", "auto_review"),
            ),
        )
        .unwrap();

        assert_eq!(
            approval_reviewer(&payload(&transcript, "wanted"), home.path()),
            Some(ApprovalReviewer::User)
        );

        fs::write(
            &transcript,
            format!("{}\n", context("wanted", "auto_review")),
        )
        .unwrap();
        assert_eq!(
            approval_reviewer(&payload(&transcript, "wanted"), home.path()),
            Some(ApprovalReviewer::AutoReview)
        );
    }

    #[test]
    fn rejects_untrusted_paths_and_unknown_reviewer_values() {
        let home = tempfile::tempdir().unwrap();
        let session_dir = sessions(home.path());
        let outside = home.path().join("outside.jsonl");
        fs::write(&outside, format!("{}\n", context("turn", "auto_review"))).unwrap();
        assert_eq!(
            approval_reviewer(&payload(&outside, "turn"), home.path()),
            None
        );

        let linked = session_dir.join("linked.jsonl");
        symlink(&outside, &linked).unwrap();
        assert_eq!(
            approval_reviewer(&payload(&linked, "turn"), home.path()),
            None
        );

        let transcript = session_dir.join("unknown.jsonl");
        fs::write(
            &transcript,
            format!("{}\n", context("turn", "future_reviewer")),
        )
        .unwrap();
        assert_eq!(
            approval_reviewer(&payload(&transcript, "turn"), home.path()),
            Some(ApprovalReviewer::Unknown)
        );
        assert_eq!(
            approval_reviewer(
                &serde_json::json!({"turn_id": "turn", "transcript_path": "relative.jsonl"}),
                home.path(),
            ),
            None
        );

        let missing = session_dir.join("missing.jsonl");
        assert_eq!(
            approval_reviewer(&payload(&missing, "turn"), home.path()),
            None
        );
        let malformed = session_dir.join("malformed.jsonl");
        fs::write(&malformed, b"not json\n").unwrap();
        assert_eq!(
            approval_reviewer(&payload(&malformed, "turn"), home.path()),
            None
        );
        let directory = session_dir.join("directory.jsonl");
        fs::create_dir(&directory).unwrap();
        assert_eq!(
            approval_reviewer(&payload(&directory, "turn"), home.path()),
            None
        );
    }

    #[test]
    fn parent_symlink_swap_cannot_retarget_a_resolved_transcript() {
        let home = tempfile::tempdir().unwrap();
        let session_dir = sessions(home.path());
        let transcript = session_dir.join("rollout.jsonl");
        fs::write(&transcript, format!("{}\n", context("turn", "auto_review"))).unwrap();
        let target = TranscriptTarget::resolve(home.path(), &transcript).unwrap();

        let saved = session_dir.with_file_name("22-saved");
        fs::rename(&session_dir, &saved).unwrap();
        let outside = home.path().join("outside-directory");
        fs::create_dir(&outside).unwrap();
        fs::write(
            outside.join("rollout.jsonl"),
            format!("{}\n", context("turn", "auto_review")),
        )
        .unwrap();
        symlink(&outside, &session_dir).unwrap();

        assert!(target.open().is_none());
    }

    #[test]
    fn monitor_reads_only_growth_and_matches_the_exact_turn_terminal() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("monitor.jsonl");
        fs::write(&transcript, format!("{}\n", context("wanted", "user"))).unwrap();
        let mut monitor = TurnMonitor::open(&payload(&transcript, "wanted"), home.path()).unwrap();
        assert_eq!(monitor.approval_reviewer(), Some(ApprovalReviewer::User));
        assert_eq!(monitor.poll_terminal(), None);

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(file, "{}", terminal("other", "turn_aborted")).unwrap();
        file.sync_all().unwrap();
        assert_eq!(monitor.poll_terminal(), None);
        assert_eq!(monitor.poll_terminal(), None);

        writeln!(file, "{}", terminal("wanted", "turn_aborted")).unwrap();
        file.sync_all().unwrap();
        assert_eq!(monitor.poll_terminal(), Some(TurnTerminal::Aborted));
    }

    #[test]
    fn monitor_recognizes_normal_completion() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("completed.jsonl");
        fs::write(&transcript, format!("{}\n", context("turn", "user"))).unwrap();
        let mut monitor = TurnMonitor::open(&payload(&transcript, "turn"), home.path()).unwrap();
        assert_eq!(monitor.approval_reviewer(), Some(ApprovalReviewer::User));

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(file, "{}", terminal("turn", "task_complete")).unwrap();
        file.sync_all().unwrap();
        assert_eq!(monitor.poll_terminal(), Some(TurnTerminal::Completed));
    }

    #[test]
    fn an_unlinked_transcript_descriptor_is_no_longer_live_evidence() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("unlinked.jsonl");
        fs::write(&transcript, format!("{}\n", child("child", "started"))).unwrap();
        let mut monitor = TurnMonitor::open(&payload(&transcript, "turn"), home.path()).unwrap();
        assert!(monitor.poll_children().readable);

        fs::remove_file(transcript).unwrap();
        assert_eq!(
            monitor.poll_children(),
            ChildPoll {
                readable: false,
                transitions: Vec::new()
            }
        );
    }

    #[test]
    fn never_scans_past_the_bounded_tail() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("large.jsonl");
        let old_context = context("wanted", "auto_review");
        let padding = format!(
            "{}\n",
            serde_json::json!({"type": "tool_output", "payload": "x".repeat(MAX_TRANSCRIPT_TAIL_BYTES as usize)})
        );
        fs::write(&transcript, format!("{old_context}\n{padding}")).unwrap();

        assert_eq!(
            approval_reviewer(&payload(&transcript, "wanted"), home.path()),
            None
        );

        fs::write(
            &transcript,
            format!("{padding}{}\n", context("wanted", "auto_review")),
        )
        .unwrap();
        assert_eq!(
            approval_reviewer(&payload(&transcript, "wanted"), home.path()),
            Some(ApprovalReviewer::AutoReview)
        );
    }

    #[test]
    fn child_transitions_preserve_order_duplicates_and_resume_activity() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("children.jsonl");
        fs::write(
            &transcript,
            [
                child("child-a", "started"),
                child("child-a", "started"),
                child("child-b", "interacted"),
                child("child-a", "completed"),
                child("child-a", "interacted"),
                child("child-b", "interrupted"),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        let mut monitor = TurnMonitor::open(&payload(&transcript, "turn"), home.path()).unwrap();

        assert_eq!(
            monitor.poll_children(),
            ChildPoll {
                readable: true,
                transitions: vec![
                    ChildTransition {
                        child_id: "child-a".into(),
                        active: true
                    },
                    ChildTransition {
                        child_id: "child-a".into(),
                        active: true
                    },
                    ChildTransition {
                        child_id: "child-b".into(),
                        active: true
                    },
                    ChildTransition {
                        child_id: "child-a".into(),
                        active: false
                    },
                    ChildTransition {
                        child_id: "child-a".into(),
                        active: true
                    },
                    ChildTransition {
                        child_id: "child-b".into(),
                        active: false
                    },
                ],
            }
        );
        assert_eq!(
            monitor.poll_children(),
            ChildPoll {
                readable: true,
                transitions: Vec::new()
            }
        );
    }

    #[test]
    fn growth_poll_uses_the_previous_eof_as_its_causal_watermark() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("watermark.jsonl");
        fs::write(&transcript, format!("{}\n", child("child", "completed"))).unwrap();
        let mut monitor = TurnMonitor::open(&payload(&transcript, "turn"), home.path()).unwrap();
        assert_eq!(monitor.poll_children().transitions.len(), 1);

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(file, "{}", child("child", "interacted")).unwrap();
        file.sync_all().unwrap();
        assert_eq!(
            monitor.poll_children().transitions,
            vec![ChildTransition {
                child_id: "child".into(),
                active: true
            }]
        );
    }

    #[test]
    fn a_record_completed_after_a_partial_poll_is_not_lost() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("partial-growth.jsonl");
        let line = child("child", "interrupted");
        let split = line.len() / 2;
        fs::write(&transcript, &line.as_bytes()[..split]).unwrap();
        let mut monitor = TurnMonitor::open(&payload(&transcript, "turn"), home.path()).unwrap();
        assert!(monitor.poll_children().transitions.is_empty());

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        file.write_all(&line.as_bytes()[split..]).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();

        assert_eq!(
            monitor.poll_children().transitions,
            vec![ChildTransition {
                child_id: "child".into(),
                active: false
            }]
        );
    }

    #[test]
    fn child_parser_rejects_near_matches_malformed_and_partial_records() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("strict-children.jsonl");
        fs::write(
            &transcript,
            format!(
                "not-json\n{}\n{}\n{}\n{}",
                serde_json::json!({
                    "type": "tool_output",
                    "payload": {"type": "item_completed", "item": {
                        "type": "SubAgentActivity", "kind": "started",
                        "agent_thread_id": "from-tool-output"
                    }}
                }),
                child("unknown-kind", "paused"),
                child("accepted", "completed"),
                child("partial", "started"),
            ),
        )
        .unwrap();
        let mut monitor = TurnMonitor::open(&payload(&transcript, "turn"), home.path()).unwrap();

        assert_eq!(
            monitor.poll_children().transitions,
            vec![ChildTransition {
                child_id: "accepted".into(),
                active: false
            }]
        );
    }

    #[test]
    fn child_parser_rejects_an_oversized_opaque_id() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("oversized-child-id.jsonl");
        fs::write(
            &transcript,
            format!(
                "{}\n",
                child(
                    &"x".repeat(crate::service::agents::adapters::MAX_CODEX_SUBAGENT_ID_BYTES + 1),
                    "started"
                )
            ),
        )
        .unwrap();
        let mut monitor = TurnMonitor::open(&payload(&transcript, "turn"), home.path()).unwrap();

        assert!(monitor.poll_children().transitions.is_empty());
    }

    #[test]
    fn oversized_records_and_the_one_mib_tail_cannot_hide_a_later_child_event() {
        let home = tempfile::tempdir().unwrap();
        let transcript = sessions(home.path()).join("large-children.jsonl");
        let old = child("outside-tail", "started");
        let oversized = serde_json::json!({
            "type": "tool_output",
            "payload": "x".repeat(MAX_TRANSCRIPT_TAIL_BYTES as usize + 128),
        });
        let recent = child("inside-tail", "interrupted");
        fs::write(&transcript, format!("{old}\n{oversized}\n{recent}\n")).unwrap();
        let mut monitor = TurnMonitor::open(&payload(&transcript, "turn"), home.path()).unwrap();

        assert_eq!(
            monitor.poll_children().transitions,
            vec![ChildTransition {
                child_id: "inside-tail".into(),
                active: false
            }]
        );
    }

    #[test]
    fn rejects_a_supplied_parent_traversal_even_when_it_resolves_inside_sessions() {
        let home = tempfile::tempdir().unwrap();
        let session_dir = sessions(home.path());
        let transcript = session_dir.join("traversal.jsonl");
        fs::write(&transcript, format!("{}\n", child("child", "started"))).unwrap();
        let supplied = session_dir.join("nested/../traversal.jsonl");

        assert!(TurnMonitor::open(&payload(&supplied, "turn"), home.path()).is_none());
    }
}
