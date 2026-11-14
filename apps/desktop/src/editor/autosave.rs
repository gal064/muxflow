//! Debounced saves for one buffer, and newer external generations admitted
//! over it. Port of `features/files/autosave.ts`.
//!
//! The machine owns no timer and no I/O, so it can be driven step by step:
//! [`Autosave::edit`] names the revision a timer should fire for after
//! [`SAVE_DELAY`], [`Autosave::due`] is that timer, and [`Autosave::finished`]
//! is a write's answer. Saves run one at a time: one asked for while another
//! is on the wire waits, and runs only if nothing newer replaced it.

use std::{collections::VecDeque, time::Duration};

use gpui_kit::component::Rope;

/// How long an edit waits for the next one before it is saved.
pub const SAVE_DELAY: Duration = Duration::from_millis(150);

/// Our own operation ids remembered, so their change events are not mistaken
/// for someone else's write.
const SELF_OPERATIONS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveState {
    Saved,
    Dirty,
    Saving,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
    Mixed,
    None,
}

impl LineEnding {
    /// What a file uses: `Mixed` when it has both kinds, `None` when it has no
    /// line break at all.
    pub fn detect(text: &str) -> Self {
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count() - crlf;
        match (crlf > 0, lf > 0) {
            (true, true) => Self::Mixed,
            (true, false) => Self::Crlf,
            (false, true) => Self::Lf,
            (false, false) => Self::None,
        }
    }

    /// The text as it is written back: a CRLF file keeps CRLF for the lines
    /// typed into it, and every other file is written as it stands.
    pub fn apply(self, text: &str) -> String {
        if self != Self::Crlf {
            return text.to_owned();
        }
        text.replace("\r\n", "\n").replace('\n', "\r\n")
    }
}

/// What a read or an external change put on screen.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub content: Rope,
    pub generation: String,
    pub line_ending: LineEnding,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutosaveView {
    pub content: Rope,
    pub generation: String,
    pub line_ending: LineEnding,
    pub state: SaveState,
    pub error: Option<String>,
}

/// One write to make, as the host is asked for it.
#[derive(Clone, Debug, PartialEq)]
pub struct SaveRequest {
    pub operation_id: String,
    pub content: Rope,
    /// The generation the write is based on, which the host checks.
    pub generation: String,
    pub line_ending: LineEnding,
    revision: u64,
    external_epoch: u64,
}

pub struct Autosave {
    view: AutosaveView,
    revision: u64,
    external_epoch: u64,
    self_operations: VecDeque<String>,
    in_flight: bool,
    /// A save asked for while another was on the wire. Only the newest is kept:
    /// an older one would find itself superseded when its turn came.
    queued: Option<SaveRequest>,
}

impl Autosave {
    pub fn new(initial: Snapshot) -> Self {
        Self {
            view: AutosaveView {
                content: initial.content,
                generation: initial.generation,
                line_ending: initial.line_ending,
                state: SaveState::Saved,
                error: None,
            },
            revision: 0,
            external_epoch: 0,
            self_operations: VecDeque::new(),
            in_flight: false,
            queued: None,
        }
    }

    pub fn current(&self) -> &AutosaveView {
        &self.view
    }

    /// A write is on the wire.
    pub fn saving(&self) -> bool {
        self.in_flight
    }

    /// A local edit. Returns the revision to save once [`SAVE_DELAY`] passes
    /// with no newer edit.
    pub fn edit(&mut self, content: Rope) -> u64 {
        self.revision += 1;
        self.view.content = content;
        self.view.state = SaveState::Dirty;
        self.view.error = None;
        self.revision
    }

    /// The debounce for `revision` ran out. Returns the write to start now, or
    /// none: the revision was superseded, nothing is owed, or a write is on the
    /// wire and this one waits behind it.
    pub fn due(&mut self, revision: u64) -> Option<SaveRequest> {
        if revision != self.revision || self.view.state == SaveState::Saved {
            return None;
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        self.self_operations.push_back(operation_id.clone());
        if self.self_operations.len() > SELF_OPERATIONS {
            self.self_operations.pop_front();
        }
        self.view.state = SaveState::Saving;
        self.view.error = None;
        let request = SaveRequest {
            operation_id,
            content: self.view.content.clone(),
            generation: self.view.generation.clone(),
            line_ending: self.view.line_ending,
            revision,
            external_epoch: self.external_epoch,
        };
        if self.in_flight {
            self.queued = Some(request);
            return None;
        }
        self.in_flight = true;
        Some(request)
    }

    /// Saves whatever is owed now, without waiting for the debounce.
    pub fn flush(&mut self) -> Option<SaveRequest> {
        match self.view.state {
            SaveState::Dirty | SaveState::Error => self.due(self.revision),
            _ => None,
        }
    }

    /// A write answered with its new generation, or failed. Returns the write
    /// that waited behind it, if it is still the newest.
    pub fn finished(
        &mut self,
        request: &SaveRequest,
        result: Result<String, String>,
    ) -> Option<SaveRequest> {
        self.in_flight = false;
        // A newer external generation replaced the buffer while this was on
        // the wire: its answer describes content nobody is looking at.
        if request.external_epoch == self.external_epoch {
            match result {
                Ok(generation) if request.revision == self.revision => {
                    self.view = AutosaveView {
                        content: request.content.clone(),
                        generation,
                        line_ending: request.line_ending,
                        state: SaveState::Saved,
                        error: None,
                    };
                }
                // The buffer moved on while this was saving. The next write
                // must build on what this one produced.
                Ok(generation) => {
                    self.view.generation = generation;
                    self.view.state = SaveState::Dirty;
                }
                Err(error) if request.revision == self.revision => {
                    self.view.state = SaveState::Error;
                    self.view.error = Some(error);
                }
                Err(_) => {}
            }
        }
        let mut next = self.queued.take()?;
        if next.revision != self.revision || next.external_epoch != self.external_epoch {
            return None;
        }
        next.generation = self.view.generation.clone();
        self.in_flight = true;
        Some(next)
    }

    /// Content that changed on disk, last writer wins. Returns false for the
    /// echo of one of our own writes, which changes nothing.
    pub fn external(&mut self, snapshot: Snapshot, operation_id: Option<&str>) -> bool {
        if operation_id.is_some_and(|id| self.self_operations.iter().any(|own| own == id)) {
            return false;
        }
        // A genuinely external write invalidates the suppression of older
        // in-flight local writes: if one commits afterwards, its change event
        // is a real last-writer transition and must be reloaded.
        self.self_operations.clear();
        self.revision += 1;
        self.external_epoch += 1;
        self.queued = None;
        self.view = AutosaveView {
            content: snapshot.content,
            generation: snapshot.generation,
            line_ending: snapshot.line_ending,
            state: SaveState::Saved,
            error: None,
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(content: &str, generation: &str, line_ending: LineEnding) -> Snapshot {
        Snapshot {
            content: Rope::from(content),
            generation: generation.into(),
            line_ending,
        }
    }

    fn text(view: &AutosaveView) -> String {
        view.content.to_string()
    }

    #[test]
    fn saves_the_last_edit_once_on_the_generation_it_was_read_at() {
        let mut autosave = Autosave::new(snapshot("one", "1", LineEnding::Lf));
        let mut states = Vec::new();
        let first = autosave.edit(Rope::from("two"));
        states.push(autosave.current().state);
        let last = autosave.edit(Rope::from("last"));
        states.push(autosave.current().state);
        // The first edit's debounce was superseded by the second.
        assert_eq!(autosave.due(first), None);
        let request = autosave.due(last).expect("the last edit is saved");
        states.push(autosave.current().state);
        assert_eq!(request.content.to_string(), "last");
        assert_eq!(request.generation, "1");
        assert_eq!(request.line_ending, LineEnding::Lf);
        assert_eq!(autosave.finished(&request, Ok("2".into())), None);
        states.push(autosave.current().state);
        assert_eq!(text(autosave.current()), "last");
        assert_eq!(autosave.current().generation, "2");
        use SaveState::*;
        assert_eq!(states, [Dirty, Dirty, Saving, Saved]);
    }

    #[test]
    fn external_content_wins_at_once_and_cancels_a_pending_save() {
        let mut autosave = Autosave::new(snapshot("one", "1", LineEnding::Crlf));
        let pending = autosave.edit(Rope::from("local"));
        assert!(autosave.external(snapshot("agent", "9", LineEnding::Crlf), None));
        assert_eq!(autosave.due(pending), None);
        let view = autosave.current();
        assert_eq!(
            (
                text(view),
                view.generation.as_str(),
                view.line_ending,
                view.state
            ),
            ("agent".into(), "9", LineEnding::Crlf, SaveState::Saved)
        );
    }

    #[test]
    fn reports_a_failed_save_and_a_later_edit_recovers() {
        let mut autosave = Autosave::new(snapshot("a", "1", LineEnding::Lf));
        let revision = autosave.edit(Rope::from("b"));
        let request = autosave.due(revision).unwrap();
        autosave.finished(&request, Err("permission denied".into()));
        assert_eq!(autosave.current().state, SaveState::Error);
        assert_eq!(
            autosave.current().error.as_deref(),
            Some("permission denied")
        );
        let revision = autosave.edit(Rope::from("ccc"));
        let request = autosave.due(revision).unwrap();
        autosave.finished(&request, Ok("3".into()));
        let view = autosave.current();
        assert_eq!(
            (view.state, view.generation.as_str(), text(view)),
            (SaveState::Saved, "3", "ccc".into())
        );
    }

    #[test]
    fn an_edit_during_a_save_is_written_on_the_generation_that_save_returns() {
        let mut autosave = Autosave::new(snapshot("a", "1", LineEnding::Lf));
        let revision = autosave.edit(Rope::from("b"));
        let first = autosave.due(revision).unwrap();
        let revision = autosave.edit(Rope::from("ccc"));
        // Waits behind the write on the wire.
        assert_eq!(autosave.due(revision), None);
        let second = autosave
            .finished(&first, Ok("2".into()))
            .expect("the queued save runs");
        assert_eq!(second.content.to_string(), "ccc");
        assert_eq!(second.generation, "2");
        assert_eq!(autosave.finished(&second, Ok("3".into())), None);
        let view = autosave.current();
        assert_eq!(
            (text(view), view.generation.as_str(), view.state),
            ("ccc".into(), "3", SaveState::Saved)
        );
    }

    #[test]
    fn a_late_local_answer_never_overwrites_a_newer_external_snapshot() {
        let mut autosave = Autosave::new(snapshot("old", "1", LineEnding::Lf));
        let revision = autosave.edit(Rope::from("local"));
        let request = autosave.due(revision).unwrap();
        autosave.external(snapshot("agent", "9", LineEnding::Lf), Some("agent-op"));
        autosave.finished(&request, Ok("2".into()));
        assert_eq!(
            *autosave.current(),
            AutosaveView {
                content: Rope::from("agent"),
                generation: "9".into(),
                line_ending: LineEnding::Lf,
                state: SaveState::Saved,
                error: None,
            }
        );
        // The external write cleared the suppression, so our own commit's
        // event is now a real change.
        assert!(autosave.external(
            snapshot("local", "2", LineEnding::Lf),
            Some(&request.operation_id)
        ));
        assert_eq!(text(autosave.current()), "local");
        assert_eq!(autosave.current().generation, "2");
    }

    #[test]
    fn the_echo_of_our_own_save_changes_nothing() {
        let mut autosave = Autosave::new(snapshot("a", "1", LineEnding::Lf));
        let revision = autosave.edit(Rope::from("b"));
        let request = autosave.due(revision).unwrap();
        autosave.finished(&request, Ok("2".into()));
        assert!(!autosave.external(
            snapshot("stale", "2", LineEnding::Lf),
            Some(&request.operation_id)
        ));
        assert_eq!(text(autosave.current()), "b");
    }

    #[test]
    fn a_flush_awaits_the_save_on_the_wire_without_writing_its_content_twice() {
        let mut autosave = Autosave::new(snapshot("old", "1", LineEnding::Lf));
        let revision = autosave.edit(Rope::from("local"));
        let request = autosave.due(revision).unwrap();
        // Saving, not dirty: nothing more is owed.
        assert_eq!(autosave.flush(), None);
        autosave.external(snapshot("external", "9", LineEnding::Lf), Some("agent"));
        assert_eq!(autosave.finished(&request, Ok("2".into())), None);
        assert_eq!(autosave.flush(), None);
        let view = autosave.current();
        assert_eq!(
            (text(view), view.generation.as_str(), view.state),
            ("external".into(), "9", SaveState::Saved)
        );
    }

    #[test]
    fn a_flush_retries_a_failed_save() {
        let mut autosave = Autosave::new(snapshot("a", "1", LineEnding::Lf));
        let revision = autosave.edit(Rope::from("b"));
        let request = autosave.due(revision).unwrap();
        autosave.finished(&request, Err("offline".into()));
        let retry = autosave.flush().expect("an errored buffer is still owed");
        assert_eq!(retry.content.to_string(), "b");
    }

    #[test]
    fn remembers_only_the_newest_own_operations() {
        let mut autosave = Autosave::new(snapshot("a", "1", LineEnding::Lf));
        let mut first = None;
        for round in 0..=SELF_OPERATIONS {
            let revision = autosave.edit(Rope::from(round.to_string()));
            let request = autosave.due(revision).unwrap();
            first.get_or_insert_with(|| request.operation_id.clone());
            autosave.finished(&request, Ok(round.to_string()));
        }
        assert!(autosave.external(snapshot("x", "99", LineEnding::Lf), first.as_deref()));
    }

    #[test]
    fn line_endings_are_detected_and_kept_for_typed_lines() {
        assert_eq!(LineEnding::detect("a\r\nb\r\n"), LineEnding::Crlf);
        assert_eq!(LineEnding::detect("a\nb"), LineEnding::Lf);
        assert_eq!(LineEnding::detect("a\r\nb\n"), LineEnding::Mixed);
        assert_eq!(LineEnding::detect("a"), LineEnding::None);
        assert_eq!(LineEnding::Crlf.apply("a\r\nb\nc"), "a\r\nb\r\nc");
        assert_eq!(LineEnding::Mixed.apply("a\r\nb\nc"), "a\r\nb\nc");
        assert_eq!(LineEnding::Lf.apply("a\nb"), "a\nb");
    }
}
