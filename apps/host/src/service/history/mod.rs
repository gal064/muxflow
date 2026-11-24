//! Session history and snooze (docs/design/chat-plan.md §1 and §5 E).
//!
//! The agent sessions this host has seen, kept in the state directory's
//! `sessions.json` after their agent is gone, so a closed session can be found
//! and resumed. The agents service feeds it (`agents::history_feed`) every
//! time its state changes and every time it reconciles a topology; nothing
//! here reads tmux or the agents' state itself.
//!
//! A snooze is a wake time on an entry. The daemon's two-second timer
//! ([`maintain`]) ends the snoozes that are due: the entry is marked `woke`
//! until it runs again, and a history event names it for a notification.
//!
//! What is written to disk is the entry list. Which sessions are running is
//! the agents service's to say and is held in memory only.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tmux_agent_protocol::v1;

/// The most sessions kept. Past it the least recently active go first.
pub(crate) const MAX_ENTRIES: usize = 1_000;
/// Titles and workspace names are cut to this, and a longer folder is not
/// kept, so that a full listing stays far under a frame's limit.
const MAX_FIELD_BYTES: usize = 1024;
/// Longer than any vendor's session id. A longer one is not recorded.
pub(crate) const MAX_SESSION_ID_BYTES: usize = 256;
const FILE_VERSION: u32 = 1;
/// Activity alone is written at most this often: it moves on every hook.
const ACTIVITY_WRITE_MILLIS: i64 = 60_000;
/// How long after start the wake check waits for the agents service's first
/// look, so it does not wake a session that runs already.
const STARTUP_GRACE_MILLIS: i64 = 10_000;
/// How long a placed resume counts as running before its agent reports in.
const RESUME_CLAIM_MILLIS: i64 = 30_000;

/// One recorded session, as it is stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Entry {
    adapter_id: String,
    native_session_id: String,
    title: String,
    workspace_name: String,
    cwd: String,
    last_active_at: i64,
    snoozed_until: i64,
    woke: bool,
    chat: bool,
    /// Not a session of its own: a `/btw` or `/side` copy, which opens in a
    /// split beside its source ([`History::hide_pane`]), or one the chat
    /// runtime put another in the place of ([`History::replaced`]). Never
    /// listed.
    hidden: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct StoredFile {
    version: u32,
    entries: Vec<Entry>,
}

/// One running agent with a native session, as the agents service sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Seen {
    pub adapter_id: String,
    pub native_session_id: String,
    pub agent_id: String,
    pub pane_id: String,
    /// The tab's title, or empty when it says nothing better than the
    /// adapter's name.
    pub title: String,
    /// What to call it when nothing better was ever seen.
    pub fallback_title: String,
    pub workspace_name: String,
    /// `None` when this observation had no topology to read them from.
    pub cwd: Option<String>,
    pub chat: Option<bool>,
    pub last_active_at: i64,
}

type Key = (String, String);

#[derive(Default)]
struct Inner {
    entries: BTreeMap<Key, Entry>,
    /// The sessions running now, each with its agent id.
    running: BTreeMap<Key, String>,
    /// Whether `running` has been set since this process started: the first
    /// observation is a baseline, not a run of new starts.
    primed: bool,
    generation: u64,
    /// Entries changed since they were last written.
    dirty: bool,
    /// Only activity times changed since the last write.
    activity: bool,
    written_at: i64,
    /// Panes a side fork is to run in; the first session seen in each that is
    /// not hidden already is kept out of the list.
    side_panes: BTreeSet<String>,
    /// Resumes placed and not yet seen running, until when they count.
    resuming: BTreeMap<Key, i64>,
    loaded_at: i64,
    /// A woke session ran again: clients drop its placeholder at the next
    /// [`History::maintain`]. The feed runs under the agents service's lock,
    /// so it does not publish itself.
    announce: bool,
}

pub(crate) struct History {
    path: PathBuf,
    inner: Mutex<Inner>,
    /// Held across a write of the store, which is made outside `inner`: the
    /// agents service feeds `inner` under its own lock, and a write may wait
    /// on a stalled disk. Taken before `inner`, so the snapshot a writer takes
    /// is never older than one already written.
    writer: Mutex<()>,
}

/// Why a history action was refused, as the wire's error code and message.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub code: &'static str,
    pub message: String,
}

pub(crate) fn refusal(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        code,
        message: message.into(),
    }
}

static GLOBAL: OnceLock<History> = OnceLock::new();

impl History {
    pub(crate) fn global() -> &'static Self {
        GLOBAL.get_or_init(|| {
            // Tests feed this one store through the agents service; they get a
            // state directory that is not the real one, as the chat runtime's do.
            let directory = if cfg!(test) {
                std::env::temp_dir().join(format!("muxflow-host-test-{}", std::process::id()))
            } else {
                crate::paths::state_dir()
            };
            Self::load(directory.join("sessions.json"))
        })
    }

    pub(crate) fn load(path: PathBuf) -> Self {
        let stored = match fs::read(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            read => read
                .ok()
                .and_then(|bytes| serde_json::from_slice::<StoredFile>(&bytes).ok())
                .filter(|file| file.version == FILE_VERSION)
                .or_else(|| {
                    // Kept aside rather than overwritten by the next write.
                    let _ = fs::rename(&path, path.with_extension("json.unreadable"));
                    None
                }),
        }
        .unwrap_or_default();
        let entries = stored
            .entries
            .into_iter()
            .filter(|entry| !entry.adapter_id.is_empty() && !entry.native_session_id.is_empty())
            .map(|entry| (key(&entry.adapter_id, &entry.native_session_id), entry))
            .collect();
        let mut inner = Inner {
            entries,
            loaded_at: now_millis(),
            ..Default::default()
        };
        trim(&mut inner);
        Self {
            path,
            inner: Mutex::new(inner),
            writer: Mutex::new(()),
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn writer(&self) -> MutexGuard<'_, ()> {
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Marks the pane a side fork (`/btw`, `/side`) opened in: the session
    /// it comes to run is a fork, and is not listed. Marked again when the
    /// fork's agent is replaced by one on another session, which is then
    /// the fork.
    pub(crate) fn hide_pane(&self, pane_id: &str) {
        self.inner().side_panes.insert(pane_id.to_owned());
    }

    /// The chat runtime put another session in this one's place, in its
    /// pane: a terminal agent's, replaced by its chat as it started, or a
    /// conversation a rewind started over. It is no session of its own and
    /// is never listed, also when it is reported after this.
    pub(crate) fn replaced(&self, adapter_id: &str, native_session_id: &str) {
        if native_session_id.is_empty() || native_session_id.len() > MAX_SESSION_ID_BYTES {
            return;
        }
        let mut inner = self.inner();
        let entry = inner
            .entries
            .entry(key(adapter_id, native_session_id))
            .or_insert_with(|| Entry {
                adapter_id: bounded(adapter_id),
                native_session_id: native_session_id.to_owned(),
                ..Entry::default()
            });
        if !std::mem::replace(&mut entry.hidden, true) {
            inner.generation += 1;
            inner.dirty = true;
        }
    }

    /// Takes in every running agent that has a native session. `seen` is the
    /// whole set: a session missing from it is not running.
    pub(crate) fn observe(&self, seen: Vec<Seen>) {
        let mut inner = self.inner();
        let inner = &mut *inner;
        let mut changed = false;
        let mut activity = false;
        let mut running = BTreeMap::new();
        let primed = inner.primed;
        for seen in seen {
            if seen.adapter_id.is_empty()
                || seen.native_session_id.is_empty()
                || seen.native_session_id.len() > MAX_SESSION_ID_BYTES
            {
                continue;
            }
            let key = key(&seen.adapter_id, &seen.native_session_id);
            let started = primed && !inner.running.contains_key(&key);
            inner.resuming.remove(&key);
            let entry = inner.entries.entry(key.clone()).or_insert_with(|| {
                changed = true;
                Entry {
                    adapter_id: bounded(&seen.adapter_id),
                    native_session_id: seen.native_session_id.clone(),
                    title: bounded(&seen.fallback_title),
                    ..Entry::default()
                }
            });
            let before = entry.clone();
            if !seen.title.is_empty() {
                entry.title = bounded(&seen.title);
            }
            // The first session in a side fork's pane is the fork; whatever
            // runs there later is not. One hidden already is what the fork
            // took the place of.
            if !entry.hidden && inner.side_panes.remove(&seen.pane_id) {
                entry.hidden = true;
            }
            if !seen.workspace_name.is_empty() {
                entry.workspace_name = bounded(&seen.workspace_name);
            }
            if let Some(cwd) = seen
                .cwd
                .filter(|cwd| !cwd.is_empty() && cwd.len() <= MAX_FIELD_BYTES)
            {
                entry.cwd = cwd;
            }
            if let Some(chat) = seen.chat {
                entry.chat = chat;
            }
            entry.last_active_at = entry.last_active_at.max(seen.last_active_at);
            // Running again, however it was started, ends a snooze and its
            // placeholder; running at the first look, a placeholder only.
            let ended =
                (started && (entry.woke || entry.snoozed_until != 0)) || (!primed && entry.woke);
            if ended {
                entry.woke = false;
                entry.snoozed_until = 0;
            }
            let moved = entry.last_active_at != before.last_active_at;
            let mut rest = entry.clone();
            rest.last_active_at = before.last_active_at;
            changed |= rest != before;
            activity |= moved;
            inner.announce |= ended;
            running.insert(key, seen.agent_id);
        }
        // Which sessions run is not written, only listed.
        let ran = running != inner.running;
        inner.running = running;
        inner.primed = true;
        if changed || activity || ran {
            inner.generation += 1;
        }
        inner.activity |= activity;
        if changed {
            inner.dirty = true;
        }
        if changed || ran {
            trim(inner);
        }
    }

    /// Every session that is one of its own, newest first.
    pub(crate) fn list(&self) -> v1::HistoryResponse {
        let inner = self.inner();
        let mut entries: Vec<v1::HistoryEntry> = inner
            .entries
            .iter()
            .filter(|(_, entry)| !entry.hidden)
            .map(|(key, entry)| wire_entry(entry, inner.running.get(key)))
            .collect();
        entries.sort_by(|left, right| {
            right
                .last_active_at_unix_millis
                .cmp(&left.last_active_at_unix_millis)
        });
        v1::HistoryResponse {
            generation: inner.generation,
            entries,
            ..Default::default()
        }
    }

    /// The session as recorded, and the agent running it if one is.
    pub(crate) fn lookup(
        &self,
        adapter_id: &str,
        native_session_id: &str,
    ) -> Option<v1::HistoryEntry> {
        let inner = self.inner();
        let key = key(adapter_id, native_session_id);
        inner
            .entries
            .get(&key)
            .map(|entry| wire_entry(entry, inner.running.get(&key)))
    }

    /// Records a snooze until `until`, written before it answers.
    pub(crate) fn snooze(
        &self,
        adapter_id: &str,
        native_session_id: &str,
        until: i64,
    ) -> Result<v1::HistoryEvent, Refusal> {
        self.change(adapter_id, native_session_id, |entry| {
            entry.snoozed_until = until;
            entry.woke = false;
        })
    }

    /// Ends a snooze now. The session becomes a woke placeholder, as at its
    /// wake time; no notification, since someone asked for it.
    pub(crate) fn wake(
        &self,
        adapter_id: &str,
        native_session_id: &str,
    ) -> Result<v1::HistoryEvent, Refusal> {
        self.change(adapter_id, native_session_id, |entry| {
            if entry.snoozed_until != 0 {
                entry.snoozed_until = 0;
                entry.woke = true;
            }
        })
    }

    /// Claims a resume of the session, so a second one is refused until its
    /// agent reports in. False when one is under way.
    pub(crate) fn claim_resume(&self, adapter_id: &str, native_session_id: &str, now: i64) -> bool {
        let mut inner = self.inner();
        inner.resuming.retain(|_, until| *until > now);
        let key = key(adapter_id, native_session_id);
        if inner.resuming.contains_key(&key) {
            return false;
        }
        inner.resuming.insert(key, now + RESUME_CLAIM_MILLIS);
        true
    }

    /// A claimed resume that was not placed.
    pub(crate) fn release_resume(&self, adapter_id: &str, native_session_id: &str) {
        self.inner()
            .resuming
            .remove(&key(adapter_id, native_session_id));
    }

    /// Puts back a snooze state taken before a snooze that did not happen.
    pub(crate) fn restore(
        &self,
        adapter_id: &str,
        native_session_id: &str,
        snoozed_until: i64,
        woke: bool,
    ) -> Result<v1::HistoryEvent, Refusal> {
        self.change(adapter_id, native_session_id, |entry| {
            entry.snoozed_until = snoozed_until;
            entry.woke = woke;
        })
    }

    /// A resume was placed: whatever snooze or placeholder it had is over.
    pub(crate) fn resumed(
        &self,
        adapter_id: &str,
        native_session_id: &str,
    ) -> Result<v1::HistoryEvent, Refusal> {
        self.change(adapter_id, native_session_id, |entry| {
            entry.snoozed_until = 0;
            entry.woke = false;
        })
    }

    /// Changes one entry and writes the store; a write that fails leaves the
    /// entry as it was.
    fn change(
        &self,
        adapter_id: &str,
        native_session_id: &str,
        edit: impl FnOnce(&mut Entry),
    ) -> Result<v1::HistoryEvent, Refusal> {
        let _writing = self.writer();
        let key = key(adapter_id, native_session_id);
        let (entries, before, unwritten) = {
            let mut inner = self.inner();
            let entry = inner.entries.get_mut(&key).ok_or_else(|| {
                refusal("history_unknown_session", "This session is not in History.")
            })?;
            let before = entry.clone();
            edit(entry);
            if *entry == before {
                return Ok(v1::HistoryEvent {
                    generation: inner.generation,
                    woke: Vec::new(),
                });
            }
            let unwritten = take_unwritten(&mut inner);
            (inner.entries.clone(), before, unwritten)
        };
        // Written outside `inner`, which the agents service waits on.
        let written = write(&self.path, &entries);
        let mut inner = self.inner();
        if let Err(error) = written {
            restore_unwritten(&mut inner, unwritten);
            // Only the snooze is changed here, so only it is put back: what
            // the agents service fed the entry meanwhile stays.
            if let Some(entry) = inner.entries.get_mut(&key) {
                entry.snoozed_until = before.snoozed_until;
                entry.woke = before.woke;
            }
            return Err(refusal(
                "history_not_saved",
                format!("History could not be saved: {error:#}"),
            ));
        }
        inner.generation += 1;
        inner.written_at = now_millis();
        Ok(v1::HistoryEvent {
            generation: inner.generation,
            woke: Vec::new(),
        })
    }

    /// Ends the snoozes due by `now` and writes what changed since the last
    /// write. Answers the event to publish when a snooze ended or a woke
    /// session ran again.
    pub(crate) fn maintain(&self, now: i64) -> Option<v1::HistoryEvent> {
        let _writing = self.writer();
        let (event, unwritten) = self.maintain_in_memory(now);
        // Written outside `inner`, which the agents service waits on.
        if let Some((entries, unwritten)) = unwritten {
            match write(&self.path, &entries) {
                Ok(()) => self.inner().written_at = now,
                // Tried again on the next tick; the snoozes it ended stay
                // ended in memory, so nobody is told twice.
                Err(_) => restore_unwritten(&mut self.inner(), unwritten),
            }
        }
        event
    }

    /// What [`History::maintain`] does under `inner`: answers its event and,
    /// when the store is due a write, what to write.
    fn maintain_in_memory(&self, now: i64) -> (Option<v1::HistoryEvent>, Option<ToWrite>) {
        let mut inner = self.inner();
        let inner = &mut *inner;
        let mut woke_keys = Vec::new();
        let mut ended = false;
        let ready = inner.primed || now >= inner.loaded_at + STARTUP_GRACE_MILLIS;
        for (key, entry) in &mut inner.entries {
            if ready && entry.snoozed_until != 0 && entry.snoozed_until <= now {
                entry.snoozed_until = 0;
                ended = true;
                // One that runs already, started while this helper was down,
                // has nothing to wake from.
                if !inner.running.contains_key(key) {
                    entry.woke = true;
                    woke_keys.push(key.clone());
                }
            }
        }
        if ended {
            inner.generation += 1;
            inner.dirty = true;
        }
        let activity_due = inner.activity && now - inner.written_at >= ACTIVITY_WRITE_MILLIS;
        let unwritten =
            (inner.dirty || activity_due).then(|| (inner.entries.clone(), take_unwritten(inner)));
        let announce = std::mem::take(&mut inner.announce);
        if woke_keys.is_empty() && !announce {
            return (None, unwritten);
        }
        let woke = woke_keys
            .iter()
            .filter_map(|key| {
                let entry = inner.entries.get(key)?;
                Some(wire_entry(entry, inner.running.get(key)))
            })
            .collect();
        let event = v1::HistoryEvent {
            generation: inner.generation,
            woke,
        };
        (Some(event), unwritten)
    }
}

/// What was unwritten when a snapshot of the store was taken to be written:
/// `dirty` and `activity`, cleared then, so a change made while the write
/// runs marks the store again.
type Unwritten = (bool, bool);
/// A snapshot of the store, taken to be written, and what it leaves written.
type ToWrite = (BTreeMap<Key, Entry>, Unwritten);

fn take_unwritten(inner: &mut Inner) -> Unwritten {
    (
        std::mem::take(&mut inner.dirty),
        std::mem::take(&mut inner.activity),
    )
}

/// After a failed write, the store is as unwritten as it was.
fn restore_unwritten(inner: &mut Inner, (dirty, activity): Unwritten) {
    inner.dirty |= dirty;
    inner.activity |= activity;
}

/// A tab's title as the desktop labels its agent: the pane's own title, else
/// the tab's name, without the agent's status ticker, and empty when both are
/// only an adapter's name or a machine identifier.
pub(crate) fn tab_title(pane_title: &str, window_name: &str) -> String {
    [pane_title, window_name]
        .into_iter()
        .map(|title| {
            super::topology::agent_title_suffix(title)
                .unwrap_or(title)
                .trim()
        })
        .find(|title| !title.is_empty() && !generic(title))
        .unwrap_or_default()
        .to_owned()
}

/// An adapter's own name, or an id, as the desktop's `agent_session_label`
/// skips them.
fn generic(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "agent"
            | "codex"
            | "claude"
            | "claude code"
            | "claude-code"
            | "opencode"
            | "pi"
            // What tmux names a tab after the shell a launch starts in.
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "dash"
    ) {
        return true;
    }
    let hex = |text: &str| text.bytes().all(|byte| byte.is_ascii_hexdigit());
    let uuid = lower.len() == 36
        && lower.split('-').map(str::len).eq([8, 4, 4, 4, 12])
        && hex(&lower.replace('-', ""));
    uuid || (lower.len() >= 24 && hex(&lower))
}

fn key(adapter_id: &str, native_session_id: &str) -> Key {
    (adapter_id.to_owned(), native_session_id.to_owned())
}

fn bounded(value: &str) -> String {
    if value.len() <= MAX_FIELD_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_FIELD_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn wire_entry(entry: &Entry, agent_id: Option<&String>) -> v1::HistoryEntry {
    v1::HistoryEntry {
        adapter_id: entry.adapter_id.clone(),
        native_session_id: entry.native_session_id.clone(),
        title: entry.title.clone(),
        workspace_name: entry.workspace_name.clone(),
        cwd: entry.cwd.clone(),
        last_active_at_unix_millis: entry.last_active_at,
        running: agent_id.is_some(),
        agent_id: agent_id.cloned().unwrap_or_default(),
        snoozed_until_unix_millis: entry.snoozed_until,
        woke: entry.woke,
        chat: entry.chat,
    }
}

/// Holds the store to [`MAX_ENTRIES`], dropping the least recently active
/// first. A running, snoozed or woke session is never dropped.
fn trim(inner: &mut Inner) {
    let excess = inner.entries.len().saturating_sub(MAX_ENTRIES);
    if excess == 0 {
        return;
    }
    let mut candidates: Vec<(i64, Key)> = inner
        .entries
        .iter()
        .filter(|(key, entry)| {
            !inner.running.contains_key(*key) && entry.snoozed_until == 0 && !entry.woke
        })
        .map(|(key, entry)| (entry.last_active_at, key.clone()))
        .collect();
    candidates.sort();
    for (_, key) in candidates.into_iter().take(excess) {
        inner.entries.remove(&key);
    }
    inner.dirty = true;
}

fn write(path: &Path, entries: &BTreeMap<Key, Entry>) -> anyhow::Result<()> {
    #[cfg(test)]
    tests::stall_if_asked(path);
    let parent = path.parent().context("history store has no parent")?;
    crate::paths::prepare_runtime_dir(parent)?;
    let file = StoredFile {
        version: FILE_VERSION,
        entries: entries.values().cloned().collect(),
    };
    let temporary = parent.join(format!(".sessions-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut handle = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        handle.write_all(&serde_json::to_vec(&file)?)?;
        handle.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// Tells every connection the history changed.
pub(crate) fn publish(event: v1::HistoryEvent) {
    super::broadcast_control_event(v1::HostEvent {
        kind: v1::EventKind::SessionHistory.into(),
        scope: "history".into(),
        history: Some(event),
        ..Default::default()
    });
}

/// The daemon's two-second pass: wakes what is due and saves the store.
pub(crate) fn maintain() {
    if let Some(event) = History::global().maintain(now_millis()) {
        publish(event);
    }
}

#[cfg(test)]
mod tests;
