//! The always-on incident journal's record format and the Ctrl+C copy-then-
//! interrupt journal. Port of `diagnostics/incidents.ts` and
//! `diagnostics/copyChordJournal.ts`; the file itself is written by
//! `desktop-core`'s `incidents`.

use std::time::{Duration, Instant};

use gpui_kit::{App, Global, SharedString, Window};
use serde_json::{Map, Value, json};

use crate::{commands::keymap::ShortcutDisposition, terminal::reading::focused_pane};

/// The record's own frame. A detail that carries one of these, an event `kind`
/// for example, must not overwrite the label the journal is searched by.
const RESERVED_KEYS: [&str; 3] = ["t", "launch", "kind"];

/// One journal record: the frame first, then the detail without any key the
/// frame already owns.
pub fn incident_record(launch: &str, time: String, kind: &str, detail: Value) -> Value {
    let mut record = Map::new();
    record.insert("t".into(), Value::String(time));
    record.insert("launch".into(), Value::String(launch.to_owned()));
    record.insert("kind".into(), Value::String(kind.to_owned()));
    if let Value::Object(fields) = detail {
        record.extend(
            fields
                .into_iter()
                .filter(|(key, _)| !RESERVED_KEYS.contains(&key.as_str())),
        );
    }
    Value::Object(record)
}

/// How long after a Linux Ctrl+C copy a second Ctrl+C still reads as the user
/// having wanted the interrupt the first time.
pub const COPY_THEN_INTERRUPT: Duration = Duration::from_millis(1_500);

/// What Linux Ctrl+C did: one chord is both copy and interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chord {
    Copy,
    Interrupt,
}

/// Journals each copy, and a copy followed quickly by an interrupt in the same
/// pane: the signature of a leftover selection having swallowed an interrupt,
/// which is the failure the selection gate exists to prevent and so the one
/// worth counting.
#[derive(Default)]
pub struct CopyChordJournal {
    last_copy: Option<(SharedString, Instant)>,
}

impl Global for CopyChordJournal {}

impl CopyChordJournal {
    /// The incident this chord earns, as `(kind, detail)`.
    pub fn note(
        &mut self,
        chord: Chord,
        pane_id: &SharedString,
        now: Instant,
    ) -> Option<(&'static str, Value)> {
        match chord {
            Chord::Copy => {
                self.last_copy = Some((pane_id.clone(), now));
                Some(("terminal.copyChord", json!({ "paneId": pane_id })))
            }
            Chord::Interrupt => {
                let (pane, at) = self.last_copy.take()?;
                let since = now.saturating_duration_since(at);
                (pane == *pane_id && since <= COPY_THEN_INTERRUPT).then(|| {
                    (
                        "terminal.copyThenInterrupt",
                        json!({ "paneId": pane_id, "ms": since.as_millis() as u64 }),
                    )
                })
            }
        }
    }
}

/// Called for each key press on Linux with what the keymap decided.
pub fn note_copy_chord(disposition: &ShortcutDisposition, window: &Window, cx: &mut App) {
    let chord = match disposition {
        ShortcutDisposition::Run("terminal.copy") => Chord::Copy,
        ShortcutDisposition::Yield("terminal.copy") => Chord::Interrupt,
        _ => return,
    };
    let Some(pane) = focused_pane(window, cx) else {
        return;
    };
    let (pane_id, backend) = {
        let pane = pane.read(cx);
        (
            pane.pane_id().clone(),
            pane.link().read(cx).backend().clone(),
        )
    };
    let incident = cx
        .default_global::<CopyChordJournal>()
        .note(chord, &pane_id, Instant::now());
    if let Some((kind, detail)) = incident {
        backend.record_incident(kind, detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_carries_the_frame_and_the_detail() {
        let record = incident_record(
            "abcd1234",
            "2026-01-01T00:00:00.000Z".into(),
            "reconnect.flowStall",
            json!({ "paneId": "%7" }),
        );
        assert_eq!(record["kind"], "reconnect.flowStall");
        assert_eq!(record["paneId"], "%7");
        assert_eq!(record["launch"], "abcd1234");
        assert_eq!(record["t"], "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_detail_cannot_overwrite_the_frame() {
        // The journal is searched by `kind`: a tmux event's own `kind` used to
        // replace the label and leave the line unattributable.
        let record = incident_record(
            "abcd1234",
            "2026-01-01T00:00:00.000Z".into(),
            "link.bridgeDown",
            json!({ "kind": "error", "t": "not a time", "launch": "hijacked", "detail": "closed" }),
        );
        assert_eq!(record["kind"], "link.bridgeDown");
        assert_eq!(record["t"], "2026-01-01T00:00:00.000Z");
        assert_eq!(record["launch"], "abcd1234");
        assert_eq!(record["detail"], "closed");
    }

    #[test]
    fn a_detail_that_is_not_an_object_adds_nothing() {
        let record = incident_record("l", "t".into(), "k", Value::Null);
        assert_eq!(record.as_object().unwrap().len(), 3);
    }

    fn pane(id: &str) -> SharedString {
        id.to_owned().into()
    }

    #[test]
    fn records_each_copy_and_a_quick_interrupt_after_it_in_the_same_pane() {
        let mut journal = CopyChordJournal::default();
        let start = Instant::now();
        let first = journal.note(Chord::Copy, &pane("%1"), start);
        assert_eq!(
            first,
            Some(("terminal.copyChord", json!({ "paneId": "%1" })))
        );
        let second = journal.note(
            Chord::Interrupt,
            &pane("%1"),
            start + Duration::from_millis(400),
        );
        assert_eq!(
            second,
            Some((
                "terminal.copyThenInterrupt",
                json!({ "paneId": "%1", "ms": 400 })
            ))
        );
    }

    #[test]
    fn does_not_count_a_late_interrupt_another_pane_or_a_plain_interrupt() {
        let mut journal = CopyChordJournal::default();
        let start = Instant::now();
        assert_eq!(journal.note(Chord::Interrupt, &pane("%1"), start), None);
        journal.note(Chord::Copy, &pane("%1"), start);
        let late = start + COPY_THEN_INTERRUPT + Duration::from_millis(1);
        assert_eq!(journal.note(Chord::Interrupt, &pane("%1"), late), None);
        journal.note(Chord::Copy, &pane("%1"), start);
        assert_eq!(journal.note(Chord::Interrupt, &pane("%2"), start), None);
        // One copy pairs with at most one interrupt.
        journal.note(Chord::Copy, &pane("%1"), start);
        assert!(journal.note(Chord::Interrupt, &pane("%1"), start).is_some());
        assert_eq!(journal.note(Chord::Interrupt, &pane("%1"), start), None);
    }
}
