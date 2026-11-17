//! The conversation a Claude Code session already holds, read from the file
//! the agent keeps of it: `projects/<folder>/<session>.jsonl` in the agent's
//! configuration directory.
//!
//! One JSON object per line. The entries of a conversation carry a `uuid`
//! and the `parentUuid` of the entry before them, so the file is a tree: a
//! session resumed part of the way grows a new branch. The conversation is
//! the chain from the newest entry back to the root.
//!
//! The format is the agent's own and not documented. Whatever cannot be read
//! is left out, and the chat says earlier messages are not available.

use std::{
    collections::BTreeMap,
    io::{BufRead, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use serde_json::Value;
use tmux_agent_protocol::v1::{self, ChatItemKind as Kind};

use super::{Reader, Said, said};
use crate::service::chat::items::{self, string};

/// How much of the end of a session's file is read first. A conversation
/// whose chain runs off it is read again from twice as far back, up to
/// [`MAX_TAIL_BYTES`]; one longer than that opens on its newest part.
pub(in crate::service::chat) const TAIL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TAIL_BYTES: u64 = 64 * 1024 * 1024;
/// The most entries one conversation is followed back through.
const MAX_CHAIN: usize = 20_000;
/// A window that holds this much of the conversation is not read again from
/// further back: it opens on its newest part. Fewer is a window a few large
/// messages filled, images mostly.
const ENOUGH_CHAIN: usize = 64;

pub(in crate::service::chat) struct History {
    /// Oldest first.
    pub(super) items: Vec<v1::ChatItem>,
    /// False when the conversation has more than could be read.
    pub(super) complete: bool,
    /// Where the conversation stands: the tools still running, and the
    /// message its last block belongs to.
    pub(super) reader: Reader,
}

impl History {
    fn unavailable() -> Self {
        Self {
            items: Vec::new(),
            complete: false,
            reader: Reader::default(),
        }
    }
}

/// The session's file, if it has one. `Err` for a session id that is not
/// one: an id is a UUID, so it names nothing but a file of that name.
fn session_file(configuration: &Path, session: &str) -> Result<Option<PathBuf>, ()> {
    let session = uuid::Uuid::parse_str(session).map_err(drop)?.to_string();
    let Ok(projects) = std::fs::read_dir(configuration.join("projects")) else {
        return Ok(None);
    };
    Ok(projects
        .flatten()
        .map(|project| project.path().join(format!("{session}.jsonl")))
        .find(|file| file.is_file()))
}

/// The session's own file holds a message. A copy's holds none until
/// something is said in it, though the agent may have named it there already.
pub(in crate::service::chat) fn has_record(configuration: &Path, session: &str) -> bool {
    last_message(configuration, session).is_some()
}

struct Entry {
    parent: Option<String>,
    value: Value,
}

type Entries = BTreeMap<String, Entry>;

/// The entries of the end of the file, read far enough back to hold the
/// chain from `leaf`, or from the newest entry when there is none: the
/// window grows while the chain runs off its start, up to `max_window`.
/// Answers the entries and the leaf, which is `None` when the file holds no
/// entry.
fn read_back(
    file: &Path,
    leaf: Option<&str>,
    max_window: u64,
) -> Option<(Entries, Option<String>)> {
    let mut window = TAIL_BYTES;
    loop {
        let (entries, newest, whole) = read_entries(file, window)?;
        let leaf = leaf.map(str::to_owned).or(newest);
        let short = !whole
            && window < max_window
            && leaf.as_ref().is_none_or(|leaf| runs_off(&entries, leaf));
        if !short {
            return Some((entries, leaf));
        }
        window *= 2;
    }
}

/// The chain from `leaf` ends at an entry whose parent was not read: it
/// started before what was read of the file.
fn runs_off(entries: &Entries, leaf: &str) -> bool {
    if !entries.contains_key(leaf) {
        return true;
    }
    let (ids, _) = chain(entries, leaf);
    ids.len() < ENOUGH_CHAIN
        && ids
            .last()
            .and_then(|id| entries[id].parent.as_ref())
            .is_some_and(|parent| !entries.contains_key(parent))
}

/// The entries of the last `window` bytes of the file by their id, the id of
/// the newest, and whether that was the whole file.
fn read_entries(file: &Path, window: u64) -> Option<(Entries, Option<String>, bool)> {
    let mut file = std::fs::File::open(file).ok()?;
    let length = file.metadata().ok()?.len();
    let cut = length > window;
    if cut {
        file.seek(SeekFrom::Start(length - window)).ok()?;
    }
    let mut reader = std::io::BufReader::new(file.take(window));
    let mut line = Vec::new();
    if cut {
        // What is left of the line the cut fell in.
        reader.read_until(b'\n', &mut line).ok()?;
    }
    let mut entries = BTreeMap::new();
    let mut newest = None;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            break;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        // A subagent's entries are a conversation of its own.
        let uuid = string(&value, "uuid");
        if uuid.is_empty() || value["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        newest = Some(uuid.to_owned());
        entries.insert(
            uuid.to_owned(),
            Entry {
                parent: value["parentUuid"].as_str().map(str::to_owned),
                value,
            },
        );
    }
    Some((entries, newest, !cut))
}

/// The ids from `leaf` back to the root, newest first, and whether the root
/// was reached.
fn chain(entries: &BTreeMap<String, Entry>, leaf: &str) -> (Vec<String>, bool) {
    let mut ids = Vec::new();
    let mut next = Some(leaf.to_owned());
    while let Some(id) = next {
        let Some(entry) = entries.get(&id) else {
            return (ids, false);
        };
        if ids.len() >= MAX_CHAIN {
            return (ids, false);
        }
        // A conversation that was compacted starts again at its summary;
        // what came before is not the agent's conversation any more.
        let compacted = !string(&entry.value, "logicalParentUuid").is_empty();
        next = entry.parent.clone();
        ids.push(id);
        if compacted && next.is_none() {
            return (ids, false);
        }
    }
    (ids, true)
}

/// Reads the conversation of `session`. `rewound` is the message a rewind
/// dropped. Until a message follows the rewind, the newest entry is of the
/// branch that was left behind, and the conversation ends before `rewound`;
/// once one does, the newest entry is of a branch `rewound` is not on.
pub(in crate::service::chat) fn load(
    configuration: &Path,
    session: &str,
    rewound: &str,
) -> History {
    load_within(configuration, session, rewound, MAX_TAIL_BYTES)
}

/// [`load`], reading no more than `max_window` of the end of the file.
pub(super) fn load_within(
    configuration: &Path,
    session: &str,
    rewound: &str,
    max_window: u64,
) -> History {
    let file = match session_file(configuration, session) {
        Ok(Some(file)) => file,
        // A session that has said nothing yet has no file.
        Ok(None) => {
            return History {
                complete: true,
                ..History::unavailable()
            };
        }
        Err(()) => return History::unavailable(),
    };
    let Some((entries, Some(newest))) = read_back(&file, None, max_window) else {
        return History::unavailable();
    };
    read_chain(&entries, &newest, rewound)
}

/// Reads the conversation of `session` as it was up to and including the
/// entry `through`: what a copy cut there holds.
pub(in crate::service::chat) fn load_through(
    configuration: &Path,
    session: &str,
    through: &str,
) -> History {
    let Ok(Some(file)) = session_file(configuration, session) else {
        return History::unavailable();
    };
    match read_back(&file, Some(through), MAX_TAIL_BYTES) {
        Some((entries, _)) if entries.contains_key(through) => read_chain(&entries, through, ""),
        _ => History::unavailable(),
    }
}

/// The newest message of `session`'s conversation, the user's or the
/// agent's: where a copy of all of it is cut. `None` when there is none.
pub(in crate::service::chat) fn last_message(
    configuration: &Path,
    session: &str,
) -> Option<String> {
    let file = session_file(configuration, session).ok()??;
    // Only the newest message is wanted: the first window has it.
    let (entries, newest, _) = read_entries(&file, TAIL_BYTES)?;
    let (ids, _) = chain(&entries, &newest?);
    ids.into_iter()
        .find(|id| matches!(string(&entries[id].value, "type"), "assistant" | "user"))
}

/// The conversation from `leaf` back to its root, as items, without
/// `rewound` and what came before it on that chain.
fn read_chain(entries: &BTreeMap<String, Entry>, leaf: &str, rewound: &str) -> History {
    let (mut ids, rooted) = chain(entries, leaf);
    if let Some(dropped) = ids.iter().position(|id| id == rewound) {
        ids.drain(..=dropped);
    }
    let mut reader = Reader::default();
    let mut items = Vec::new();
    for id in ids.iter().rev() {
        let entry = &entries[id].value;
        match string(entry, "type") {
            "assistant" => items.extend(reader.assistant(entry)),
            "user" => {
                for finished in reader.results(entry) {
                    // A tool's row stays where the tool was called.
                    match items.iter_mut().find(|item| item.id == finished.id) {
                        Some(item) => *item = finished,
                        None => items.push(finished),
                    }
                }
                let (kind, text, attachments) = match said(entry) {
                    Said::User(text, attachments) => (Kind::User, text, attachments),
                    Said::Interrupted => (Kind::Notice, "Interrupted".to_owned(), Vec::new()),
                    Said::Nothing => continue,
                };
                let mut item = items::text(id.as_str(), kind, text);
                item.attachments = attachments;
                item.rewindable = kind == Kind::User;
                item.at_unix_millis = items::unix_millis(string(entry, "timestamp"));
                items.push(item);
            }
            "system" if string(entry, "subtype") == "compact_boundary" => {
                // The session's record names its fields in camel case.
                let text = super::compacted(&entry["compactMetadata"]);
                items.push(items::text(id.as_str(), Kind::Notice, text));
            }
            _ => {}
        }
    }
    // Tools that ran side by side each record their result as an entry of
    // its own, and only one of those is on the chain. A tool the chain left
    // running may have its result in an entry beside it.
    if reader.has_open() {
        let on_chain: std::collections::BTreeSet<&String> = ids.iter().collect();
        for (id, entry) in entries {
            if on_chain.contains(id) || string(&entry.value, "type") != "user" {
                continue;
            }
            for finished in reader.results(&entry.value) {
                if let Some(item) = items.iter_mut().find(|item| item.id == finished.id) {
                    *item = finished;
                }
            }
        }
    }
    History {
        items,
        // A chain that ran off the part of the file that was read has more.
        complete: rooted,
        reader,
    }
}

/// The entry before `uuid` in its conversation: where a rewind of that
/// message goes back to. `Some(None)` when it is the conversation's first
/// entry, `None` when the session's file does not hold it.
pub(in crate::service::chat) fn parent_of(
    configuration: &Path,
    session: &str,
    uuid: &str,
) -> Option<Option<String>> {
    let file = std::fs::File::open(session_file(configuration, session).ok()??).ok()?;
    let mut found = None;
    for line in std::io::BufReader::new(file).split(b'\n') {
        let line = line.ok()?;
        // Most lines are not it, and a line can be megabytes of tool output.
        if !line
            .windows(uuid.len())
            .any(|window| window == uuid.as_bytes())
        {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if string(&value, "uuid") == uuid && value["isSidechain"].as_bool() != Some(true) {
            // The last record of it is the one that counts.
            found = Some(value["parentUuid"].as_str().map(str::to_owned));
        }
    }
    found
}
