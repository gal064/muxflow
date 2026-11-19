//! The conversation as the timeline draws it: which items stand alone, which
//! runs of steps fold into one line, and what the working line says.
//!
//! Nothing here knows about a window. [`layout`] is one pass over the items
//! with no measuring, so the pane lays out again after any change that can
//! move a row and tells the list only about the entries that differ.

use std::time::Duration;

use desktop_core::connection::chat::{
    ChatItem, ChatItemKind, ChatTool, ChatToolKind, ChatToolStatus, ChatTurnState,
};

/// Runs of this many steps or more fold into one line (D7).
pub(super) const GROUP_FROM: usize = 3;

/// One row of the timeline between its header and its trailing rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Entry {
    /// One item, drawn alone. `reply_end`: the last text of a finished
    /// reply, which carries the reply's buttons (D5).
    Item { index: usize, reply_end: bool },
    /// Items `first..end`, a run of tool calls and thinking, of which
    /// `steps` are drawn. `live`: the run a turn still in progress is adding
    /// to. Known by its first item's id, which is also the key of its open
    /// state.
    Group {
        first: usize,
        end: usize,
        steps: usize,
        live: bool,
    },
}

impl Entry {
    /// The first item it holds.
    pub fn first(self) -> usize {
        match self {
            Self::Item { index, .. } => index,
            Self::Group { first, .. } => first,
        }
    }

    pub fn contains(self, index: usize) -> bool {
        match self {
            Self::Item { index: at, .. } => at == index,
            Self::Group { first, end, .. } => (first..end).contains(&index),
        }
    }

    /// The same entry with `by` items put before it.
    pub fn shifted(self, by: usize) -> Self {
        match self {
            Self::Item { index, reply_end } => Self::Item {
                index: index + by,
                reply_end,
            },
            Self::Group {
                first,
                end,
                steps,
                live,
            } => Self::Group {
                first: first + by,
                end: end + by,
                steps,
                live,
            },
        }
    }
}

pub(super) fn kind(item: &ChatItem) -> ChatItemKind {
    ChatItemKind::try_from(item.kind).unwrap_or_default()
}

fn tool_kind(tool: &ChatTool) -> ChatToolKind {
    ChatToolKind::try_from(tool.kind).unwrap_or_default()
}

fn status(tool: &ChatTool) -> ChatToolStatus {
    ChatToolStatus::try_from(tool.status).unwrap_or_default()
}

/// A turn is in progress: the agent works, or waits on the user.
pub(super) fn is_busy(turn: ChatTurnState) -> bool {
    matches!(turn, ChatTurnState::Working | ChatTurnState::Waiting)
}

fn is_step(item: &ChatItem) -> bool {
    matches!(kind(item), ChatItemKind::Tool | ChatItemKind::Reasoning)
}

/// A subagent that still runs: while the turn works, a row of its own under
/// the working line (D16) and none in the timeline.
pub(super) fn is_running_subagent(item: &ChatItem) -> bool {
    item.tool.as_ref().is_some_and(|tool| {
        tool_kind(tool) == ChatToolKind::Subagent && status(tool) == ChatToolStatus::Running
    })
}

/// The subagents that still run, in the order they started: the rows under
/// the working line.
pub(super) fn running_subagents(items: &[ChatItem]) -> impl Iterator<Item = &ChatItem> {
    items.iter().filter(|item| is_running_subagent(item))
}

/// Whether `item` is drawn in the timeline while the turn is `turn`.
/// Thinking that streams while the turn is in progress is said by the
/// working line instead, and thinking with no text has nothing to show. A
/// subagent that runs while the turn works is drawn under the working line.
pub(super) fn is_drawn(item: &ChatItem, turn: ChatTurnState) -> bool {
    match kind(item) {
        ChatItemKind::Reasoning => {
            !item.text.trim().is_empty() && !(is_busy(turn) && item.streaming)
        }
        ChatItemKind::Tool => !(turn == ChatTurnState::Working && is_running_subagent(item)),
        _ => true,
    }
}

/// The timeline's entries for `items` while the turn is `turn`. A run of
/// tool calls and thinking between two other items is one group when it has
/// [`GROUP_FROM`] steps or more, and its steps one entry each otherwise.
pub(super) fn layout(items: &[ChatItem], turn: ChatTurnState) -> Vec<Entry> {
    let busy = is_busy(turn);
    let mut entries = Vec::with_capacity(items.len());
    // The entry of the newest text of the reply being read.
    let mut text: Option<usize> = None;
    // The first item of the run being read, and how many of it are drawn.
    let mut run: Option<usize> = None;
    let mut steps = 0;
    for (index, item) in items.iter().enumerate() {
        if is_step(item) {
            run.get_or_insert(index);
            steps += usize::from(is_drawn(item, turn));
            continue;
        }
        if let Some(first) = run.take() {
            close(&mut entries, items, first..index, steps, false, turn);
            steps = 0;
        }
        match kind(item) {
            // A message of the user's starts the next reply.
            ChatItemKind::User => end_reply(&mut entries, text.take()),
            ChatItemKind::Assistant => text = Some(entries.len()),
            _ => {}
        }
        entries.push(Entry::Item {
            index,
            reply_end: false,
        });
    }
    if let Some(first) = run {
        close(&mut entries, items, first..items.len(), steps, busy, turn);
    }
    // The reply a turn is still adding to has no buttons yet.
    if !busy {
        end_reply(&mut entries, text);
    }
    entries
}

fn close(
    entries: &mut Vec<Entry>,
    items: &[ChatItem],
    run: std::ops::Range<usize>,
    steps: usize,
    live: bool,
    turn: ChatTurnState,
) {
    if steps >= GROUP_FROM {
        entries.push(Entry::Group {
            first: run.start,
            end: run.end,
            steps,
            live,
        });
        return;
    }
    entries.extend(
        run.filter(|index| is_drawn(&items[*index], turn))
            .map(|index| Entry::Item {
                index,
                reply_end: false,
            }),
    );
}

fn end_reply(entries: &mut [Entry], text: Option<usize>) {
    if let Some(Entry::Item { reply_end, .. }) = text.and_then(|at| entries.get_mut(at)) {
        *reply_end = true;
    }
}

/// The index of the entry that holds item `index`, if any does: entries
/// are in item order, so this is a binary search.
pub(super) fn entry_of(entries: &[Entry], index: usize) -> Option<usize> {
    let after = entries.partition_point(|entry| entry.first() <= index);
    let at = after.checked_sub(1)?;
    entries[at].contains(index).then_some(at)
}

/// How a group's collapsed line describes its steps.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Summary {
    pub failed: usize,
    /// The counts by kind (D13): "read 5 · ran 4 · searched 1 · edited 2 files".
    pub counts: String,
    /// The files its edits name, each once, in the order first edited.
    pub files: Vec<String>,
    /// Lines added and removed by the edits that did not fail.
    pub added: u32,
    pub removed: u32,
}

/// The summary of `steps`. `thought_for` is how long a thought took, when
/// this pane watched it.
pub(super) fn summary<'a>(
    steps: impl IntoIterator<Item = &'a ChatItem>,
    thought_for: impl Fn(&ChatItem) -> Option<Duration>,
) -> Summary {
    let mut summary = Summary::default();
    let (mut thoughts, mut thinking, mut timed) = (0, Duration::ZERO, true);
    // Read, ran, searched, fetched, called, subagents, other; edits apart.
    let mut by_kind = [0usize; 7];
    let mut pathless_edits = 0;
    for item in steps {
        let Some(tool) = item.tool.as_ref() else {
            thoughts += 1;
            match thought_for(item) {
                Some(took) => thinking += took,
                None => timed = false,
            }
            continue;
        };
        let failed = status(tool) == ChatToolStatus::Failed;
        summary.failed += usize::from(failed);
        let slot = match tool_kind(tool) {
            ChatToolKind::Read => 0,
            ChatToolKind::Command => 1,
            ChatToolKind::Search => 2,
            ChatToolKind::Web => 3,
            ChatToolKind::Mcp => 4,
            ChatToolKind::Subagent => 5,
            ChatToolKind::Edit => {
                if tool.path.is_empty() {
                    pathless_edits += 1;
                } else if !summary.files.contains(&tool.path) {
                    summary.files.push(tool.path.clone());
                }
                if !failed {
                    summary.added += tool.added;
                    summary.removed += tool.removed;
                }
                continue;
            }
            ChatToolKind::Other | ChatToolKind::Unspecified => 6,
        };
        by_kind[slot] += 1;
    }
    let mut parts = Vec::new();
    match (thoughts, timed) {
        (0, _) => {}
        (_, true) => parts.push(format!("thought for {}", elapsed(thinking.max(SECOND)))),
        (1, false) => parts.push("thought".to_owned()),
        (count, false) => parts.push(format!("thought {count} times")),
    }
    for (count, verb) in by_kind[..5]
        .iter()
        .zip(["read", "ran", "searched", "fetched", "called"])
    {
        if *count > 0 {
            parts.push(format!("{verb} {count}"));
        }
    }
    // A subagent is counted as a noun: "1 subagent", "2 subagents" (S9).
    match by_kind[5] {
        0 => {}
        1 => parts.push("1 subagent".to_owned()),
        count => parts.push(format!("{count} subagents")),
    }
    match (summary.files.len(), pathless_edits) {
        (0, 0) => {}
        (0, edits) => parts.push(format!("edited {edits}")),
        (1, _) => parts.push("edited 1 file".to_owned()),
        (files, _) => parts.push(format!("edited {files} files")),
    }
    if by_kind[6] > 0 {
        parts.push(format!("{} other", by_kind[6]));
    }
    summary.counts = parts.join(" · ");
    summary
}

const SECOND: Duration = Duration::from_secs(1);

/// "8s", "2m 05s", "1h 02m".
pub(super) fn elapsed(time: Duration) -> String {
    let seconds = time.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// "N steps", or "N steps so far" while the turn adds to them.
pub(super) fn steps_label(steps: usize, live: bool) -> String {
    let noun = if steps == 1 { "step" } else { "steps" };
    if live {
        format!("{steps} {noun} so far")
    } else {
        format!("{steps} {noun}")
    }
}

/// The names the collapsed line's chips show for `files`: up to three, and
/// past three the first two and a count of the rest.
pub(super) fn chips(files: &[String]) -> (Vec<&str>, Option<usize>) {
    fn name(path: &str) -> &str {
        path.rsplit(['/', '\\']).next().unwrap_or(path)
    }
    if files.len() <= 3 {
        (files.iter().map(|path| name(path)).collect(), None)
    } else {
        (
            files[..2].iter().map(|path| name(path)).collect(),
            Some(files.len() - 2),
        )
    }
}

/// What the working line says the agent does now.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Doing {
    pub label: String,
    /// The latest sentence of the thinking that streams, shown under it.
    pub preview: Option<String>,
}

/// What the working line says, from the newest steps: thinking that
/// streams, else a tool that runs, else the subagents it waits on, else that
/// the agent works.
pub(super) fn doing(items: &[ChatItem]) -> Doing {
    let run = items
        .iter()
        .rposition(|item| !is_step(item))
        .map_or(0, |at| at + 1);
    let active = items[run..]
        .iter()
        .rev()
        .find(|item| match item.tool.as_ref() {
            // A subagent has its own row, which says what it does.
            Some(tool) => {
                status(tool) == ChatToolStatus::Running && tool_kind(tool) != ChatToolKind::Subagent
            }
            None => item.streaming,
        });
    let label = match active.map(|item| (item, item.tool.as_ref())) {
        Some((item, None)) => {
            return Doing {
                label: "Thinking…".to_owned(),
                preview: latest_sentence(&item.text),
            };
        }
        Some((_, Some(tool))) => running(tool),
        None => match running_subagents(items).count() {
            0 => "Working…".to_owned(),
            1 => "Waiting on 1 subagent…".to_owned(),
            count => format!("Waiting on {count} subagents…"),
        },
    };
    Doing {
        label,
        preview: None,
    }
}

/// What a subagent's row is named by: its type, and what it was asked to
/// do, on one line. Codex names no type: "Agent", as S9 draws it.
pub(super) fn subagent_name(tool: &ChatTool) -> (&str, &str) {
    let kind = if tool.agent_type.is_empty() {
        "Agent"
    } else {
        tool.agent_type.as_str()
    };
    let asked = tool.detail.lines().next().unwrap_or_default().trim();
    (kind, asked)
}

/// A subagent's figures: "12 tools · 61k tokens". Empty when the provider
/// counts none.
pub(super) fn figures(tool: &ChatTool) -> String {
    let mut parts = Vec::new();
    match tool.tool_uses {
        0 => {}
        1 => parts.push("1 tool".to_owned()),
        uses => parts.push(format!("{uses} tools")),
    }
    if tool.tokens > 0 {
        parts.push(format!("{} tokens", tokens(tool.tokens)));
    }
    parts.join(" · ")
}

/// The line under a running subagent: what it does now, its figures and how
/// long it has run (`elapsed`, when known). A subagent that has a type is
/// Claude Code's, which reports all of it; Codex reports only its state, and
/// that is the whole line, as S9 draws it.
pub(super) fn subagent_line(tool: &ChatTool, elapsed: Option<Duration>) -> String {
    if tool.agent_type.is_empty() {
        return if tool.activity.is_empty() {
            "running".to_owned()
        } else {
            tool.activity.clone()
        };
    }
    let figures = figures(tool);
    let time = elapsed.map(self::elapsed);
    [Some(tool.activity.clone()), Some(figures), time]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `24.1k`, `61k`, `182k`: a count of tokens as a subagent's line says it.
pub(super) fn tokens(count: u64) -> String {
    // One decimal, rounded half up, and none when it is zero.
    let tenths = |unit_size: u64, unit: &str| {
        let tenths = (count + unit_size / 20) / (unit_size / 10);
        match tenths % 10 {
            0 => format!("{}{unit}", tenths / 10),
            tenth => format!("{}.{tenth}{unit}", tenths / 10),
        }
    };
    match count {
        0..1_000 => count.to_string(),
        1_000..99_950 => tenths(1_000, "k"),
        99_950..999_500 => format!("{}k", (count + 500) / 1_000),
        _ => tenths(1_000_000, "M"),
    }
}

/// "2 background tasks": the chip on the composer's top edge.
pub(super) fn background_label(count: usize) -> String {
    if count == 1 {
        "1 background task".to_owned()
    } else {
        format!("{count} background tasks")
    }
}

/// "Running cargo test…": a running tool's verb and what it acts on.
fn running(tool: &ChatTool) -> String {
    let detail = if tool.detail.is_empty() {
        tool.path.as_str()
    } else {
        tool.detail.as_str()
    };
    let verb = match tool_kind(tool) {
        ChatToolKind::Command => "Running",
        ChatToolKind::Read => "Reading",
        ChatToolKind::Edit => "Editing",
        ChatToolKind::Search => "Searching",
        ChatToolKind::Web if tool.title == "search" => "Searching",
        ChatToolKind::Web => "Fetching",
        ChatToolKind::Mcp => "Calling",
        // A tool the chat has no verb for is said by what it does.
        _ if !detail.is_empty() => return format!("{detail}…"),
        _ => return "Working…".to_owned(),
    };
    if detail.is_empty() {
        format!("{verb}…")
    } else {
        format!("{verb} {detail}…")
    }
}

/// The last whole sentence of the last line of `text`, or that line as far
/// as it has got when it holds none yet. None for no text.
pub(super) fn latest_sentence(text: &str) -> Option<String> {
    let line = text.trim_end().rsplit('\n').next()?.trim();
    if line.is_empty() {
        return None;
    }
    let ends: Vec<usize> = line
        .char_indices()
        .filter(|(at, mark)| {
            matches!(mark, '.' | '!' | '?')
                && line[at + 1..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map(|(at, _)| at + 1)
        .collect();
    let Some(&end) = ends.last() else {
        return Some(line.to_owned());
    };
    let start = ends.iter().rev().nth(1).copied().unwrap_or(0);
    Some(line[start..end].trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, kind: ChatItemKind, text: &str) -> ChatItem {
        ChatItem {
            id: id.into(),
            kind: kind.into(),
            text: text.into(),
            ..Default::default()
        }
    }

    fn user(id: &str) -> ChatItem {
        item(id, ChatItemKind::User, "Do it.")
    }

    fn says(id: &str) -> ChatItem {
        item(id, ChatItemKind::Assistant, "Done.")
    }

    fn thought(id: &str) -> ChatItem {
        item(id, ChatItemKind::Reasoning, "Look at the view first.")
    }

    fn tool(id: &str, kind: ChatToolKind, detail: &str) -> ChatItem {
        ChatItem {
            id: id.into(),
            kind: ChatItemKind::Tool.into(),
            tool: Some(ChatTool {
                kind: kind.into(),
                detail: detail.into(),
                status: ChatToolStatus::Ok.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn read(id: &str) -> ChatItem {
        tool(id, ChatToolKind::Read, "src/view.rs")
    }

    fn edit(id: &str, path: &str, added: u32, removed: u32) -> ChatItem {
        let mut item = tool(id, ChatToolKind::Edit, path);
        let tool = item.tool.as_mut().unwrap();
        tool.path = path.into();
        tool.added = added;
        tool.removed = removed;
        item
    }

    fn failed(mut item: ChatItem) -> ChatItem {
        item.tool.as_mut().unwrap().status = ChatToolStatus::Failed.into();
        item
    }

    fn running(mut item: ChatItem) -> ChatItem {
        item.tool.as_mut().unwrap().status = ChatToolStatus::Running.into();
        item
    }

    fn plain(index: usize) -> Entry {
        Entry::Item {
            index,
            reply_end: false,
        }
    }

    fn end(index: usize) -> Entry {
        Entry::Item {
            index,
            reply_end: true,
        }
    }

    fn group(first: usize, end: usize, steps: usize, live: bool) -> Entry {
        Entry::Group {
            first,
            end,
            steps,
            live,
        }
    }

    const IDLE: ChatTurnState = ChatTurnState::Idle;
    const WORKING: ChatTurnState = ChatTurnState::Working;

    #[test]
    fn runs_of_one_or_two_steps_stay_rows_and_three_fold() {
        let one = [user("u"), read("r1"), says("a")];
        assert_eq!(layout(&one, IDLE), [plain(0), plain(1), end(2)]);
        let two = [user("u"), read("r1"), read("r2"), says("a")];
        assert_eq!(layout(&two, IDLE), [plain(0), plain(1), plain(2), end(3)]);
        let three = [user("u"), read("r1"), read("r2"), read("r3"), says("a")];
        assert_eq!(
            layout(&three, IDLE),
            [plain(0), group(1, 4, 3, false), end(4)]
        );
    }

    #[test]
    fn the_agents_text_splits_a_run() {
        let items = [
            user("u"),
            says("a1"),
            read("r1"),
            read("r2"),
            says("a2"),
            read("r3"),
            read("r4"),
            read("r5"),
            says("a3"),
        ];
        assert_eq!(
            layout(&items, IDLE),
            [
                plain(0),
                plain(1),
                plain(2),
                plain(3),
                plain(4),
                group(5, 8, 3, false),
                end(8)
            ]
        );
    }

    #[test]
    fn thinking_and_every_kind_of_tool_are_steps_of_one_run() {
        let items = [
            user("u"),
            thought("t"),
            tool("c", ChatToolKind::Command, "cargo test"),
            edit("e", "src/view.rs", 3, 1),
            tool("m", ChatToolKind::Mcp, "github.get_pull_request"),
            item("n", ChatItemKind::Notice, "Interrupted"),
            read("r"),
        ];
        assert_eq!(
            layout(&items, IDLE),
            [plain(0), group(1, 5, 4, false), plain(5), plain(6)]
        );
    }

    #[test]
    fn the_run_a_working_turn_adds_to_is_live_and_a_finished_one_is_not() {
        let items = [user("u"), read("r1"), read("r2"), running(read("r3"))];
        assert_eq!(layout(&items, WORKING), [plain(0), group(1, 4, 3, true)]);
        assert_eq!(
            layout(&items, ChatTurnState::Waiting),
            [plain(0), group(1, 4, 3, true)]
        );
        assert_eq!(layout(&items, IDLE), [plain(0), group(1, 4, 3, false)]);
        // Followed by text, it is over, though the turn goes on.
        let mut items = items.to_vec();
        items.push(says("a"));
        assert_eq!(
            layout(&items, WORKING),
            [plain(0), group(1, 4, 3, false), plain(4)]
        );
    }

    #[test]
    fn thinking_that_streams_while_the_turn_works_is_not_a_row_until_it_ends() {
        let mut thinking = thought("t");
        thinking.streaming = true;
        let items = [user("u"), read("r1"), read("r2"), thinking.clone()];
        assert_eq!(layout(&items, WORKING), [plain(0), plain(1), plain(2)]);
        thinking.streaming = false;
        let items = [user("u"), read("r1"), read("r2"), thinking];
        assert_eq!(layout(&items, WORKING), [plain(0), group(1, 4, 3, true)]);
        // Thinking with no text has nothing to show at all.
        let silent = item("s", ChatItemKind::Reasoning, "");
        assert_eq!(layout(&[user("u"), silent], IDLE), [plain(0)]);
    }

    #[test]
    fn the_last_text_of_each_finished_reply_carries_its_buttons() {
        let items = [
            user("u1"),
            says("a1"),
            read("r"),
            says("a2"),
            item("n", ChatItemKind::Notice, "Interrupted"),
            user("u2"),
            says("a3"),
        ];
        let entries = [
            plain(0),
            plain(1),
            plain(2),
            end(3),
            plain(4),
            plain(5),
            end(6),
        ];
        assert_eq!(layout(&items, IDLE), entries);
        // The reply still being written has none yet; the one before keeps its.
        let mut working = entries;
        working[6] = plain(6);
        assert_eq!(layout(&items, WORKING), working);
    }

    #[test]
    fn an_entry_is_found_for_each_item_it_holds() {
        let items = [user("u"), read("r1"), read("r2"), read("r3"), says("a")];
        let entries = layout(&items, IDLE);
        assert_eq!(entry_of(&entries, 0), Some(0));
        assert_eq!(entry_of(&entries, 2), Some(1));
        assert_eq!(entry_of(&entries, 4), Some(2));
        assert_eq!(entry_of(&entries, 5), None);
        let mut streaming = thought("t");
        streaming.streaming = true;
        let entries = layout(&[user("u"), streaming], WORKING);
        assert_eq!(entry_of(&entries, 1), None, "not drawn");
    }

    #[test]
    fn the_collapsed_line_counts_by_kind_in_the_owners_words() {
        let mut items = vec![
            read("r1"),
            read("r2"),
            tool("c1", ChatToolKind::Command, "cargo check"),
            tool("s1", ChatToolKind::Search, "fn render_row"),
            edit("e1", "src/chat/view.rs", 84, 9),
            edit("e2", "src/chat/pane.rs", 36, 5),
            edit("e3", "src/chat/view.rs", 0, 0),
        ];
        let said = summary(&items, |_| None);
        assert_eq!(said.counts, "read 2 · ran 1 · searched 1 · edited 2 files");
        assert_eq!(said.files, ["src/chat/view.rs", "src/chat/pane.rs"]);
        assert_eq!((said.added, said.removed), (120, 14));
        assert_eq!(said.failed, 0);

        // One of each: the singular.
        items.truncate(1);
        items.push(edit("e1", "a.rs", 1, 0));
        assert_eq!(summary(&items, |_| None).counts, "read 1 · edited 1 file");

        // Thinking leads, timed when every thought was watched.
        let thoughts = [thought("t1"), read("r"), thought("t2")];
        let watched = |_: &ChatItem| Some(Duration::from_secs(4));
        assert_eq!(
            summary(&thoughts, watched).counts,
            "thought for 8s · read 1"
        );
        assert_eq!(
            summary(&thoughts, |item| (item.id == "t1").then_some(SECOND)).counts,
            "thought 2 times · read 1"
        );
        assert_eq!(summary(&thoughts[..2], |_| None).counts, "thought · read 1");

        let rest = [
            tool("w", ChatToolKind::Web, "lucide.dev"),
            tool("m", ChatToolKind::Mcp, "github.get_pull_request"),
            tool("g", ChatToolKind::Subagent, "Explore the git module"),
            tool("o", ChatToolKind::Other, "Update the task list"),
        ];
        assert_eq!(
            summary(&rest, |_| None).counts,
            "fetched 1 · called 1 · 1 subagent · 1 other"
        );
        // S9: "read 2 · searched 1 · 1 subagent", and the plural.
        let delegated = [
            read("r1"),
            read("r2"),
            tool("s", ChatToolKind::Search, "upload"),
            tool("g", ChatToolKind::Subagent, "Explore the git module"),
        ];
        assert_eq!(
            summary(&delegated, |_| None).counts,
            "read 2 · searched 1 · 1 subagent"
        );
        let two = [
            tool("g1", ChatToolKind::Subagent, "Explore"),
            tool("g2", ChatToolKind::Subagent, "Audit"),
        ];
        assert_eq!(summary(&two, |_| None).counts, "2 subagents");
    }

    #[test]
    fn a_failed_step_is_counted_and_its_lines_are_not() {
        let items = [
            read("r"),
            failed(tool("c", ChatToolKind::Command, "cargo test")),
            failed(edit("e", "src/view.rs", 10, 2)),
            edit("f", "src/pane.rs", 3, 1),
        ];
        let said = summary(&items, |_| None);
        assert_eq!(said.failed, 2);
        assert_eq!(said.counts, "read 1 · ran 1 · edited 2 files");
        assert_eq!((said.added, said.removed), (3, 1));
    }

    #[test]
    fn steps_and_chips_read_as_drawn() {
        assert_eq!(steps_label(12, false), "12 steps");
        assert_eq!(steps_label(7, true), "7 steps so far");
        assert_eq!(steps_label(1, false), "1 step");
        let files = |names: &[&str]| {
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            chips(&files(&["src/view.rs", "src/pane.rs", "a/b.rs"])),
            (vec!["view.rs", "pane.rs", "b.rs"], None)
        );
        assert_eq!(
            chips(&files(&["src/view.rs", "src/pane.rs", "a/b.rs", "c.rs"])),
            (vec!["view.rs", "pane.rs"], Some(2))
        );
    }

    #[test]
    fn elapsed_time_reads_in_seconds_then_minutes_then_hours() {
        assert_eq!(elapsed(Duration::from_millis(400)), "0s");
        assert_eq!(elapsed(Duration::from_secs(12)), "12s");
        assert_eq!(elapsed(Duration::from_secs(125)), "2m 05s");
        assert_eq!(elapsed(Duration::from_secs(3720)), "1h 02m");
    }

    #[test]
    fn the_working_line_says_what_the_newest_step_does() {
        assert_eq!(doing(&[user("u")]).label, "Working…");
        let mut thinking = thought("t");
        thinking.text = "First the view. Then the pane, which".into();
        thinking.streaming = true;
        assert_eq!(
            doing(&[user("u"), thinking.clone()]),
            Doing {
                label: "Thinking…".into(),
                preview: Some("First the view.".into())
            }
        );
        // Thinking the model keeps to itself: no text to show under it.
        thinking.text.clear();
        assert_eq!(doing(&[user("u"), thinking]).preview, None);
        let test = running(tool(
            "c",
            ChatToolKind::Command,
            "cargo test -p muxflow-gpui chat",
        ));
        assert_eq!(
            doing(&[user("u"), read("r"), test.clone()]).label,
            "Running cargo test -p muxflow-gpui chat…"
        );
        // A step that ran beside a newer finished one still runs.
        assert_eq!(
            doing(&[user("u"), test.clone(), read("r")]).label,
            "Running cargo test -p muxflow-gpui chat…"
        );
        // Text after it: whatever ran before is not what happens now.
        assert_eq!(doing(&[test, says("a")]).label, "Working…");
        let agent = running(tool("g", ChatToolKind::Subagent, "Explore the git module"));
        assert_eq!(doing(std::slice::from_ref(&agent)).label, "Waiting on 1 subagent…");
        assert_eq!(
            doing(&[agent.clone(), agent]).label,
            "Waiting on 2 subagents…"
        );
        let reading = running(tool("r", ChatToolKind::Read, "src/view.rs"));
        assert_eq!(doing(&[reading]).label, "Reading src/view.rs…");
        let other = running(tool("o", ChatToolKind::Other, "Update the task list"));
        assert_eq!(doing(&[other]).label, "Update the task list…");
    }

    fn subagent(id: &str, kind: &str) -> ChatItem {
        let mut item = running(tool(
            id,
            ChatToolKind::Subagent,
            "Find where uploads are staged",
        ));
        item.tool.as_mut().unwrap().agent_type = kind.into();
        item
    }

    #[test]
    fn a_running_subagent_is_out_of_its_run_while_the_turn_works_and_folds_once_it_ends() {
        let items = [
            user("u"),
            read("r1"),
            subagent("g", "Explore"),
            read("r2"),
            read("r3"),
        ];
        // Three steps drawn, the subagent under the working line instead.
        assert_eq!(layout(&items, WORKING), [plain(0), group(1, 5, 3, true)]);
        let two = [user("u"), read("r1"), subagent("g", "Explore")];
        assert_eq!(layout(&two, WORKING), [plain(0), plain(1)]);
        // Waiting on the user there is no working line: it stays a step.
        assert_eq!(
            layout(&two, ChatTurnState::Waiting),
            [plain(0), plain(1), plain(2)]
        );
        // Ended, it is a step like any other.
        let mut done = items.to_vec();
        done[2].tool.as_mut().unwrap().status = ChatToolStatus::Ok.into();
        assert_eq!(layout(&done, WORKING), [plain(0), group(1, 5, 4, true)]);
        assert_eq!(
            running_subagents(&items)
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["g"]
        );
    }

    #[test]
    fn the_working_line_waits_on_subagents_only_while_nothing_else_runs() {
        let agents = [user("u"), subagent("g1", "Explore"), subagent("g2", "Plan")];
        assert_eq!(doing(&agents).label, "Waiting on 2 subagents…");
        // A command the turn runs meanwhile is what the line says.
        let mut busy = agents.to_vec();
        busy.push(running(tool("c", ChatToolKind::Command, "cargo check")));
        assert_eq!(doing(&busy).label, "Running cargo check…");
        // Subagents an earlier run started are still waited on.
        let later = [subagent("g1", "Explore"), says("a"), read("r")];
        assert_eq!(doing(&later).label, "Waiting on 1 subagent…");
    }

    #[test]
    fn a_subagents_line_reads_as_drawn() {
        let mut explore = subagent("g", "Explore");
        let tool = explore.tool.as_mut().unwrap();
        tool.activity = "Reading terminal_upload.rs".into();
        tool.tool_uses = 7;
        tool.tokens = 24_100;
        let tool = explore.tool.as_ref().unwrap();
        assert_eq!(
            subagent_name(tool),
            ("Explore", "Find where uploads are staged")
        );
        assert_eq!(
            subagent_line(tool, Some(Duration::from_secs(41))),
            "Reading terminal_upload.rs · 7 tools · 24.1k tokens · 41s"
        );
        assert_eq!(figures(tool), "7 tools · 24.1k tokens");
        // Before its first figures: only the time.
        let fresh = subagent("f", "Plan");
        let fresh = fresh.tool.as_ref().unwrap();
        assert_eq!(subagent_line(fresh, Some(Duration::from_secs(3))), "3s");
        assert_eq!(figures(fresh), "");
        // Codex: "Agent", the first line of the prompt, and its state.
        let mut codex = running(tool_item_with_prompt());
        codex.tool.as_mut().unwrap().activity = "running".into();
        let codex = codex.tool.as_ref().unwrap();
        assert_eq!(
            subagent_name(codex),
            ("Agent", "Find where uploads are staged")
        );
        assert_eq!(
            subagent_line(codex, Some(Duration::from_secs(48))),
            "running"
        );
    }

    fn tool_item_with_prompt() -> ChatItem {
        tool(
            "x",
            ChatToolKind::Subagent,
            "Find where uploads are staged\nand list each caller",
        )
    }

    #[test]
    fn tokens_and_the_chip_read_as_s9_says_them() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(19_714), "19.7k");
        assert_eq!(tokens(61_000), "61k");
        assert_eq!(tokens(182_000), "182k");
        assert_eq!(tokens(1_250_000), "1.3M");
        assert_eq!(background_label(1), "1 background task");
        assert_eq!(background_label(2), "2 background tasks");
    }

    #[test]
    fn the_latest_sentence_is_the_last_whole_one_of_the_last_line() {
        assert_eq!(latest_sentence(""), None);
        assert_eq!(latest_sentence("  \n "), None);
        assert_eq!(
            latest_sentence("A plan.\nOne. Two, with v1.2 in it! Three"),
            Some("Two, with v1.2 in it!".into())
        );
        assert_eq!(latest_sentence("Only a start"), Some("Only a start".into()));
        assert_eq!(latest_sentence("One. Two."), Some("Two.".into()));
    }
}
