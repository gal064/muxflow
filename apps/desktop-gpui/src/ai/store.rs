//! The conversations, their approvals and their round budgets. Port of
//! `features/ai/conversationStore.ts`.
//!
//! Conversations live here rather than in the panel's render state because the
//! segment is unmounted the moment another tab of the panel is clicked: a
//! generation that was mid-answer would otherwise lose its text, and the
//! answer would arrive to nobody. Keyed by host *and tmux window* — the
//! strip's tab — so every tab holds its own thread and switching tabs switches
//! conversations. A closed window's thread is pruned: window ids are reused,
//! and an old answer about a dead window in a new window's panel would be a
//! lie.
//!
//! The React store parked closures (`decide`, `resume`, `cancel`) on the
//! conversation; the Rust port parks one-shot channels and a cancel flag
//! instead. Retiring a conversation drops the senders, and the loop waiting on
//! the receiver wakes as if the user had denied — which is exactly what a
//! vanished card means.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::channel::oneshot;

/// Four round trips per tab, kept short so the history cannot outgrow the
/// model's window.
pub const MAX_TURNS: usize = 40;

pub fn conversation_key(profile_id: &str, window_id: &str) -> String {
    format!("{profile_id}\0{window_id}")
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// What the assistant did inside one answer, kept for the transcript and the
/// history: the model is told its own actions in words, not in raw tool wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiAction {
    pub name: String,
    /// One human line: `send_to_pane "git status" ⏎`.
    pub summary: String,
    pub outcome: ActionOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionOutcome {
    Done,
    Denied,
    Failed,
}

impl ActionOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Denied => "denied",
            Self::Failed => "failed",
        }
    }
}

/// One bubble: what was asked, and what came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiTurn {
    /// "user" or "assistant".
    pub role: String,
    pub text: String,
    pub actions: Vec<AiAction>,
    /// When the bubble happened, epoch ms: a question is stamped as it is
    /// sent, an answer as it finishes. `None` means still filling in, and the
    /// panel shows no stamp until the answer lands.
    pub created_at: Option<u64>,
    /// Tokens the answer cost, summed over the round(s) that produced it.
    /// Only when the server reported usage; absent never means zero.
    pub tokens: Option<u64>,
}

/// A tool call waiting for the user's decision. The loop parked the sender
/// when it asked; the panel's Allow/Deny moves it out and resolves it, and any
/// other path (Stop, Clear, a closed window) drops it, which reads as a denial.
pub struct Approval {
    pub id: String,
    pub name: String,
    pub summary: String,
    /// The command in full when the summary shortened it; the card's `…`
    /// reveals it, because approving a truncation is approving on faith.
    pub detail: Option<String>,
    respond: Option<oneshot::Sender<bool>>,
}

/// The round budget spent without an answer, waiting on the user. Continue
/// buys the run more rounds; anything else — Stop, Clear, a closed window —
/// ends it. Like an approval, it lives on the conversation so the panel can
/// raise it after a tab switch.
pub struct Continuation {
    pub rounds_used: u64,
    resume: Option<oneshot::Sender<bool>>,
}

/// What the panel renders for one conversation.
#[derive(Default)]
pub struct Conversation {
    pub turns: Vec<AiTurn>,
    /// A generation is in flight for this conversation.
    pub streaming: bool,
    /// The last failure; the next send clears it.
    pub error: Option<String>,
    /// What the assistant is doing between deltas, e.g. `calling list_panes`.
    pub activity: Option<String>,
    /// Tool calls waiting on the user's Allow/Deny, oldest first.
    pub pending: Vec<Approval>,
    /// The round budget is spent; the run waits here for Continue or Stop.
    pub continuation: Option<Continuation>,
    /// Identifies this question's run. Clearing, pruning or reopening a window
    /// moves the generation, and a loop still holding the old one stops
    /// touching the conversation — and stops its own network call.
    pub generation: u64,
    /// How to stop the in-flight generation. It lives here rather than in the
    /// panel because the panel unmounts when another tab is clicked, and the
    /// Stop button has to survive that.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Conversation {
    pub fn empty() -> Self {
        Self::default()
    }
}

/// The map, with the store's rules. Every method that changes something
/// returns whether it did, and the panel calls `cx.notify()` on `true`.
#[derive(Default)]
pub struct Conversations {
    entries: HashMap<String, Conversation>,
    next_generation: u64,
}

impl Conversations {
    pub fn read(&self, key: &str) -> Option<&Conversation> {
        self.entries.get(key)
    }

    /// Whether `generation` still owns `key`'s in-flight run. Every await in
    /// the loop checks this.
    pub fn alive(&self, key: &str, generation: u64) -> bool {
        self.entries
            .get(key)
            .is_some_and(|entry| entry.generation == generation && entry.streaming)
    }

    /// Ends one entry for good: its generation moves, so any loop still holding
    /// it stops, and anything it was waiting on is refused rather than left hung.
    fn retire(&mut self, key: &str) {
        let Some(mut entry) = self.entries.remove(key) else {
            return;
        };
        for approval in &mut entry.pending {
            approval.respond.take();
        }
        if let Some(continuation) = &mut entry.continuation {
            continuation.resume.take();
        }
        if let Some(cancel) = &entry.cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Opens a question and the empty answer that fills in under it. Returns
    /// the turns that preceded it — the history the model gets — and the
    /// generation this question owns; the loop checks it after every await and
    /// stops when the conversation was cleared, pruned or reopened underneath
    /// it.
    pub fn begin(&mut self, key: &str, question: &str) -> (Vec<AiTurn>, u64) {
        let history = self
            .entries
            .get(key)
            .map_or_else(Vec::new, |entry| entry.turns.clone());
        self.retire(key);
        self.next_generation += 1;
        let generation = self.next_generation;
        let mut turns = history.clone();
        turns.push(AiTurn {
            role: "user".into(),
            text: question.to_owned(),
            actions: Vec::new(),
            created_at: Some(now_ms()),
            tokens: None,
        });
        // The answer is stamped when `end` closes it, not now: its time is
        // when it arrived, and until then the panel shows no stamp.
        turns.push(AiTurn {
            role: "assistant".into(),
            text: String::new(),
            actions: Vec::new(),
            created_at: None,
            tokens: None,
        });
        if turns.len() > MAX_TURNS {
            turns.drain(..turns.len() - MAX_TURNS);
        }
        self.entries.insert(
            key.to_owned(),
            Conversation {
                turns,
                streaming: true,
                error: None,
                activity: None,
                pending: Vec::new(),
                continuation: None,
                generation,
                cancel: None,
            },
        );
        (history, generation)
    }

    /// The last assistant bubble, if the answer is still open to fill.
    fn open_answer(&mut self, key: &str) -> Option<&mut AiTurn> {
        let entry = self.entries.get_mut(key)?;
        if !entry.streaming {
            return None;
        }
        let last = entry.turns.last_mut()?;
        (last.role == "assistant").then_some(last)
    }

    /// Appends to the open answer; ignored after `end`, e.g. by a late delta.
    pub fn delta(&mut self, key: &str, text: &str) -> bool {
        let Some(last) = self.open_answer(key) else {
            return false;
        };
        last.text.push_str(text);
        true
    }

    /// Records what the assistant did, under the answer it did it for.
    pub fn add_action(&mut self, key: &str, action: AiAction) -> bool {
        let Some(last) = self.open_answer(key) else {
            return false;
        };
        last.actions.push(action);
        true
    }

    /// Reports what one round's answer cost, under the answer it cost it;
    /// rounds of one answer add up. Ignored after `end`, like a late delta.
    pub fn add_tokens(&mut self, key: &str, tokens: u64) -> bool {
        if tokens == 0 {
            return false;
        }
        let Some(last) = self.open_answer(key) else {
            return false;
        };
        last.tokens = Some(last.tokens.unwrap_or(0) + tokens);
        true
    }

    /// What the assistant is doing between deltas; `None` clears it.
    pub fn set_activity(&mut self, key: &str, activity: Option<String>) -> bool {
        let Some(entry) = self.entries.get_mut(key) else {
            return false;
        };
        if !entry.streaming || entry.activity == activity {
            return false;
        }
        entry.activity = activity;
        true
    }

    /// Hands over the means to stop, once the current round's call exists.
    pub fn attach(&mut self, key: &str, cancel: Arc<AtomicBool>) {
        if let Some(entry) = self.entries.get_mut(key)
            && entry.streaming
        {
            entry.cancel = Some(cancel);
        }
    }

    /// Parks a tool call on the conversation until the user decides. The loop
    /// keeps the returned receiver; `None` means nobody could ever answer
    /// (the answer already ended), which the loop reads as a denial.
    pub fn park_approval(
        &mut self,
        key: &str,
        id: String,
        name: String,
        summary: String,
        detail: Option<String>,
    ) -> Option<oneshot::Receiver<bool>> {
        let entry = self.entries.get_mut(key)?;
        if !entry.streaming {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        entry.pending.push(Approval {
            id,
            name,
            summary,
            detail,
            respond: Some(sender),
        });
        Some(receiver)
    }

    /// The panel's Allow/Deny. Unknown or already-answered ids are ignored.
    /// Returns whether the card was there to answer.
    pub fn resolve_approval(&mut self, key: &str, id: &str, approved: bool) -> bool {
        let Some(entry) = self.entries.get_mut(key) else {
            return false;
        };
        let Some(index) = entry.pending.iter().position(|approval| approval.id == id) else {
            return false;
        };
        let approval = entry.pending.remove(index);
        if let Some(respond) = approval.respond {
            let _ = respond.send(approved);
        }
        true
    }

    /// Parks a spent round budget on the conversation until Continue or Stop.
    pub fn park_continuation(
        &mut self,
        key: &str,
        rounds_used: u64,
    ) -> Option<oneshot::Receiver<bool>> {
        let entry = self.entries.get_mut(key)?;
        if !entry.streaming {
            return None;
        }
        let (sender, resume) = oneshot::channel();
        entry.continuation = Some(Continuation {
            rounds_used,
            resume: Some(sender),
        });
        Some(resume)
    }

    /// The user chose Continue: the card goes away and the loop resumes. The
    /// store clears the card *first*: the loop must not find it still parked
    /// when it resumes a moment later.
    pub fn continue_run(&mut self, key: &str) -> bool {
        let Some(entry) = self.entries.get_mut(key) else {
            return false;
        };
        let Some(continuation) = entry.continuation.take() else {
            return false;
        };
        if let Some(resume) = continuation.resume {
            let _ = resume.send(true);
        }
        true
    }

    /// The card's text, for the view.
    pub fn continuation_rounds(&self, key: &str) -> Option<u64> {
        self.entries
            .get(key)?
            .continuation
            .as_ref()
            .map(|waiting| waiting.rounds_used)
    }

    /// Ends the generation — answered, cancelled or failed. Idempotent.
    pub fn end(&mut self, key: &str, error: Option<String>) -> bool {
        let Some(entry) = self.entries.get_mut(key) else {
            return false;
        };
        if !entry.streaming {
            return false;
        }
        for approval in &mut entry.pending {
            approval.respond.take();
        }
        if let Some(continuation) = &mut entry.continuation {
            continuation.resume.take();
        }
        if let Some(last) = entry.turns.last_mut()
            && last.role == "assistant"
            && last.created_at.is_none()
        {
            last.created_at = Some(now_ms());
        }
        entry.streaming = false;
        entry.pending.clear();
        entry.continuation = None;
        entry.cancel = None;
        entry.activity = None;
        entry.error = error;
        true
    }

    pub fn clear(&mut self, key: &str) -> bool {
        if !self.entries.contains_key(key) {
            return false;
        }
        self.retire(key);
        true
    }

    /// Drops this host's conversations whose window has closed. The live ids
    /// come from the host's current snapshot; an empty live set while the host
    /// is connected still means every one of its windows is gone.
    pub fn prune_host(&mut self, profile_id: &str, live_window_ids: &HashSet<String>) -> bool {
        let prefix = format!("{profile_id}\0");
        let dead: Vec<String> = self
            .entries
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .filter(|key| !live_window_ids.contains(&key[prefix.len()..]))
            .cloned()
            .collect();
        let changed = !dead.is_empty();
        for key in dead {
            self.retire(&key);
        }
        changed
    }

    /// Ends every conversation: for tests, and for when the panel goes away.
    pub fn clear_all(&mut self) {
        for key in self.entries.keys().cloned().collect::<Vec<_>>() {
            self.retire(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt as _;
    use std::collections::HashSet;

    #[test]
    fn begin_moves_the_generation_and_returns_the_history_the_model_gets() {
        let mut store = Conversations::default();
        let (history, first) = store.begin("k", "one");
        assert!(history.is_empty());
        let entry = store.read("k").unwrap();
        assert_eq!(entry.turns.len(), 2);
        assert_eq!(entry.turns[0].text, "one");
        assert!(entry.turns[0].created_at.is_some());
        // The open answer has no stamp until it lands.
        assert_eq!(entry.turns[1].created_at, None);
        assert!(entry.streaming);
        store.end("k", None);
        let (history, second) = store.begin("k", "two");
        assert_eq!(history.len(), 2);
        assert!(second > first);
        // The old loop is dead to the store now.
        assert!(!store.alive("k", first));
        assert!(store.alive("k", second));
    }

    #[test]
    fn filling_the_answer_appends_text_actions_and_token_totals() {
        let mut store = Conversations::default();
        store.begin("k", "q");
        assert!(store.delta("k", "hel"));
        assert!(store.delta("k", "lo"));
        assert!(store.add_tokens("k", 3));
        assert!(store.add_tokens("k", 4));
        assert!(!store.add_tokens("k", 0));
        assert!(store.add_action(
            "k",
            AiAction {
                name: "list_panes".into(),
                summary: "read the current tab's panes".into(),
                outcome: ActionOutcome::Done,
            }
        ));
        store.end("k", None);
        // A late delta after `end` is ignored, like the React guard.
        assert!(!store.delta("k", "late"));
        let entry = store.read("k").unwrap();
        assert_eq!(entry.turns[1].text, "hello");
        assert_eq!(entry.turns[1].tokens, Some(7));
        assert_eq!(entry.turns[1].actions.len(), 1);
        assert!(entry.turns[1].created_at.is_some());
    }

    #[test]
    fn retiring_refuses_what_the_run_was_waiting_on() {
        let mut store = Conversations::default();
        store.begin("k", "q");
        let approval = store.park_approval(
            "k",
            "c1".into(),
            "send_to_pane".into(),
            "type ls".into(),
            None,
        );
        let continuation = store.park_continuation("k", 6);
        store.end("k", Some("boom".into()));
        // Both waiters woke as denials; the error is what the panel shows.
        assert_eq!(
            approval.unwrap().now_or_never(),
            Some(Err(oneshot::Canceled))
        );
        assert_eq!(
            continuation.unwrap().now_or_never(),
            Some(Err(oneshot::Canceled))
        );
        assert_eq!(store.read("k").unwrap().error.as_deref(), Some("boom"));
        // The next send clears it.
        store.begin("k", "again");
        assert_eq!(store.read("k").unwrap().error, None);
    }

    #[test]
    fn answering_an_approval_moves_the_card_and_resumes_the_run() {
        let mut store = Conversations::default();
        store.begin("k", "q");
        let approval = store.park_approval(
            "k",
            "c1".into(),
            "send_to_pane".into(),
            "type ls".into(),
            None,
        );
        assert!(store.resolve_approval("k", "c1", true));
        // The card is gone; a second answer is ignored.
        assert!(store.read("k").unwrap().pending.is_empty());
        assert!(!store.resolve_approval("k", "c1", false));
        assert_eq!(approval.unwrap().now_or_never(), Some(Ok(true)));
    }

    #[test]
    fn continuing_clears_the_card_before_the_loop_wakes() {
        let mut store = Conversations::default();
        store.begin("k", "q");
        let resume = store.park_continuation("k", 6);
        assert_eq!(store.continuation_rounds("k"), Some(6));
        assert!(store.continue_run("k"));
        assert_eq!(store.continuation_rounds("k"), None);
        assert_eq!(resume.unwrap().now_or_never(), Some(Ok(true)));
    }

    #[test]
    fn pruning_drops_only_the_windows_that_closed() {
        let mut store = Conversations::default();
        store.begin(&conversation_key("h1", "@1"), "a");
        store.begin(&conversation_key("h1", "@2"), "b");
        store.begin(&conversation_key("h2", "@1"), "c");
        let mut live: HashSet<String> = HashSet::new();
        live.insert("@1".to_owned());
        assert!(store.prune_host("h1", &live));
        assert!(store.read(&conversation_key("h1", "@1")).is_some());
        assert!(store.read(&conversation_key("h1", "@2")).is_none());
        // The other host's ids are its own; an id that survives is untouched.
        assert!(store.read(&conversation_key("h2", "@1")).is_some());
        // Nothing to prune the second time.
        assert!(!store.prune_host("h1", &live));
        // An empty live set while connected means every window is gone.
        assert!(store.prune_host("h2", &HashSet::new()));
        assert!(store.read(&conversation_key("h2", "@1")).is_none());
    }

    #[test]
    fn the_history_keeps_the_newest_turns() {
        let mut store = Conversations::default();
        for index in 0..(MAX_TURNS + 5) {
            store.begin("k", &format!("q{index}"));
            store.end("k", None);
        }
        let (history, _) = store.begin("k", "last");
        assert_eq!(history.len(), MAX_TURNS);
        // The newest question survived; the oldest ones did not.
        let newest = history
            .iter()
            .rev()
            .find(|turn| turn.role == "user")
            .unwrap();
        assert_eq!(newest.text, format!("q{}", MAX_TURNS + 4));
        assert!(!history.iter().any(|turn| turn.text == "q0"));
    }
}
