//! The calm save status in a file tab's toolbar. Port of
//! `features/files/saveStatus.ts`.
//!
//! Typing runs autosave around a dirty, saving, saved loop several times a
//! second, and a status keyed on that loop flickers. So the loop is read as
//! one fact, a save is owed, and only an unbroken 1.2 seconds of owing one is
//! slow enough to report. Errors show at once, a lost write authority still
//! says so, and a successful save says nothing at all.

use std::time::{Duration, Instant};

use super::autosave::SaveState;

pub const SLOW_SAVE_DELAY: Duration = Duration::from_millis(1200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visible {
    Dirty,
    Saving,
    Error,
}

impl Visible {
    pub fn label(self) -> &'static str {
        match self {
            Self::Saving => "Saving…",
            Self::Dirty => "Unsaved",
            Self::Error => "Save failed",
        }
    }
}

pub fn project(state: Option<SaveState>, slow: bool, write_available: bool) -> Option<Visible> {
    match state? {
        SaveState::Error => Some(Visible::Error),
        SaveState::Saved => None,
        SaveState::Dirty if !write_available => Some(Visible::Dirty),
        SaveState::Dirty | SaveState::Saving => slow.then_some(Visible::Saving),
    }
}

/// Since when a save has been owed without a break.
#[derive(Debug, Default)]
pub struct SaveStatus {
    owed_since: Option<Instant>,
}

impl SaveStatus {
    /// What to show at `now`, and when to look again for the delay to run out.
    pub fn at(
        &mut self,
        state: Option<SaveState>,
        write_available: bool,
        now: Instant,
    ) -> (Option<Visible>, Option<Instant>) {
        let owed = matches!(state, Some(SaveState::Dirty | SaveState::Saving));
        if !owed {
            self.owed_since = None;
        } else if self.owed_since.is_none() {
            self.owed_since = Some(now);
        }
        let due = self.owed_since.map(|since| since + SLOW_SAVE_DELAY);
        let slow = due.is_some_and(|due| now >= due);
        (project(state, slow, write_available), due.filter(|_| !slow))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SaveState::*;

    #[test]
    fn shows_only_actionable_immediate_states() {
        assert_eq!(project(Some(Saved), true, true), None);
        assert_eq!(project(Some(Dirty), false, true), None);
        assert_eq!(project(Some(Saving), false, true), None);
        // A save owed long enough reads the same in the debounce or on the host.
        assert_eq!(project(Some(Dirty), true, true), Some(Visible::Saving));
        assert_eq!(project(Some(Saving), true, true), Some(Visible::Saving));
        assert_eq!(project(Some(Dirty), true, false), Some(Visible::Dirty));
        assert_eq!(project(Some(Error), true, true), Some(Visible::Error));
        assert_eq!(project(None, true, true), None);
    }

    #[test]
    fn keeps_a_fast_save_calm_and_never_publishes_a_saved_confirmation() {
        let start = Instant::now();
        let mut status = SaveStatus::default();
        assert_eq!(status.at(Some(Saved), true, start).0, None);
        assert_eq!(status.at(Some(Dirty), true, start).0, None);
        let almost = start + SLOW_SAVE_DELAY - Duration::from_millis(1);
        assert_eq!(status.at(Some(Saving), true, almost).0, None);
        assert_eq!(status.at(Some(Saved), true, almost), (None, None));
        let late = start + SLOW_SAVE_DELAY * 2;
        assert_eq!(status.at(Some(Saved), true, late).0, None);
    }

    #[test]
    fn measures_one_delay_across_the_whole_save_the_keystroke_owes() {
        let start = Instant::now();
        let mut status = SaveStatus::default();
        let (_, due) = status.at(Some(Dirty), true, start);
        assert_eq!(due, Some(start + SLOW_SAVE_DELAY));
        // The debounce handing over to the write does not restart the count.
        let half = start + SLOW_SAVE_DELAY / 2;
        assert_eq!(status.at(Some(Saving), true, half), (None, due));
        let almost = start + SLOW_SAVE_DELAY - Duration::from_millis(1);
        assert_eq!(status.at(Some(Dirty), true, almost).0, None);
        let at = start + SLOW_SAVE_DELAY;
        assert_eq!(
            status.at(Some(Saving), true, at),
            (Some(Visible::Saving), None)
        );
    }

    #[test]
    fn shows_dirty_when_the_editor_has_lost_write_authority() {
        let mut status = SaveStatus::default();
        assert_eq!(
            status.at(Some(Dirty), false, Instant::now()).0,
            Some(Visible::Dirty)
        );
    }

    #[test]
    fn shows_an_error_immediately_and_cancels_delayed_progress() {
        let start = Instant::now();
        let mut status = SaveStatus::default();
        status.at(Some(Saving), true, start);
        let almost = start + SLOW_SAVE_DELAY - Duration::from_millis(1);
        assert_eq!(
            status.at(Some(Error), true, almost),
            (Some(Visible::Error), None)
        );
        let later = start + SLOW_SAVE_DELAY * 2;
        assert_eq!(status.at(Some(Error), true, later).0, Some(Visible::Error));
    }

    #[test]
    fn a_stalled_save_says_it_once_and_holds_through_further_typing() {
        let start = Instant::now();
        let mut status = SaveStatus::default();
        let mut shown = Vec::new();
        for step in 0..40u32 {
            let state = if step % 2 == 0 { Dirty } else { Saving };
            let label = status
                .at(Some(state), true, start + Duration::from_millis(100) * step)
                .0;
            if shown.last() != Some(&label) {
                shown.push(label);
            }
        }
        assert_eq!(shown, [None, Some(Visible::Saving)]);
    }
}
