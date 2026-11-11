//! Where the user has been, for Back and Forward. Port of
//! `features/shell/focusHistory.ts`: a bounded, linear history with a cursor,
//! the browser model rather than a stack, because "back then forward" has to
//! land where it started.

/// A terminal (workspace and window) or a document tab (workspace, the window
/// it was opened over, and the tab).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusPoint {
    pub session_id: String,
    pub window_id: Option<String>,
    pub app_tab_id: Option<String>,
}

/// Deep enough to be useful, shallow enough that Back stays predictable.
pub const FOCUS_HISTORY_LIMIT: usize = 50;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FocusHistory {
    pub entries: Vec<FocusPoint>,
    /// The entry showing; `None` while empty.
    pub cursor: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Back,
    Forward,
}

impl FocusHistory {
    fn current(&self) -> Option<&FocusPoint> {
        self.cursor.and_then(|cursor| self.entries.get(cursor))
    }

    /// Records arriving somewhere. Arriving where you are is not a move, and
    /// arriving anywhere after going back drops the forward branch.
    pub fn visit(&mut self, point: FocusPoint) {
        if self.current() == Some(&point) {
            return;
        }
        self.entries
            .truncate(self.cursor.map_or(0, |cursor| cursor + 1));
        self.entries.push(point);
        let excess = self.entries.len().saturating_sub(FOCUS_HISTORY_LIMIT);
        self.entries.drain(..excess);
        self.cursor = Some(self.entries.len() - 1);
    }

    /// The nearest entry in `direction` that still exists, stepping over
    /// points whose tab or workspace has gone. Moves the cursor there.
    pub fn step(
        &mut self,
        direction: Direction,
        exists: impl Fn(&FocusPoint) -> bool,
    ) -> Option<FocusPoint> {
        let found = self.find_step(direction, exists)?;
        self.cursor = Some(found);
        Some(self.entries[found].clone())
    }

    fn find_step(
        &self,
        direction: Direction,
        exists: impl Fn(&FocusPoint) -> bool,
    ) -> Option<usize> {
        let cursor = self.cursor?;
        match direction {
            Direction::Back => (0..cursor)
                .rev()
                .find(|index| exists(&self.entries[*index])),
            Direction::Forward => {
                (cursor + 1..self.entries.len()).find(|index| exists(&self.entries[*index]))
            }
        }
    }

    pub fn has_step(&self, direction: Direction, exists: impl Fn(&FocusPoint) -> bool) -> bool {
        self.find_step(direction, exists).is_some()
    }

    /// Where a closing document hands focus: the nearest earlier entry that
    /// exists and is not that document, which is still in the state when this
    /// is asked. Moves the cursor there.
    pub fn back_from_closing(
        &mut self,
        app_tab_id: &str,
        exists: impl Fn(&FocusPoint) -> bool,
    ) -> Option<FocusPoint> {
        let cursor = self.cursor?;
        let found = (0..cursor).rev().find(|index| {
            let point = &self.entries[*index];
            point.app_tab_id.as_deref() != Some(app_tab_id) && exists(point)
        })?;
        self.cursor = Some(found);
        Some(self.entries[found].clone())
    }

    /// Drops points that no longer exist: a window can close from another
    /// client at any moment, and Back must not go somewhere gone.
    pub fn prune(&mut self, exists: impl Fn(&FocusPoint) -> bool) {
        let before = self.entries.len();
        // The cursor follows its own entry, not the first equal one: the same
        // window can be in the history more than once.
        let mut cursor = None;
        let mut kept = 0;
        for (index, point) in std::mem::take(&mut self.entries).into_iter().enumerate() {
            if !exists(&point) {
                continue;
            }
            if self.cursor == Some(index) {
                cursor = Some(kept);
            }
            self.entries.push(point);
            kept += 1;
        }
        if self.entries.len() == before {
            return;
        }
        self.cursor = cursor.or_else(|| self.entries.len().checked_sub(1));
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(session: &str, window: &str) -> FocusPoint {
        FocusPoint {
            session_id: session.into(),
            window_id: Some(window.into()),
            app_tab_id: None,
        }
    }

    fn doc(session: &str, window: &str, tab: &str) -> FocusPoint {
        FocusPoint {
            app_tab_id: Some(tab.into()),
            ..at(session, window)
        }
    }

    fn all(_: &FocusPoint) -> bool {
        true
    }

    #[test]
    fn visits_ignore_staying_put_and_drop_the_forward_branch() {
        let mut history = FocusHistory::default();
        history.visit(at("$1", "@1"));
        history.visit(at("$1", "@1"));
        history.visit(at("$1", "@2"));
        history.visit(at("$2", "@3"));
        assert_eq!(history.entries.len(), 3);
        assert_eq!(history.step(Direction::Back, all), Some(at("$1", "@2")));
        history.visit(doc("$1", "@2", "file"));
        assert_eq!(
            history.entries,
            [at("$1", "@1"), at("$1", "@2"), doc("$1", "@2", "file")]
        );
        assert!(!history.has_step(Direction::Forward, all));
    }

    #[test]
    fn back_then_forward_lands_where_it_started() {
        let mut history = FocusHistory::default();
        history.visit(at("$1", "@1"));
        history.visit(at("$1", "@2"));
        assert_eq!(history.step(Direction::Back, all), Some(at("$1", "@1")));
        assert_eq!(history.step(Direction::Back, all), None);
        assert_eq!(history.step(Direction::Forward, all), Some(at("$1", "@2")));
        assert_eq!(history.step(Direction::Forward, all), None);
    }

    #[test]
    fn keeps_the_newest_fifty() {
        let mut history = FocusHistory::default();
        for index in 0..60 {
            history.visit(at("$1", &format!("@{index}")));
        }
        assert_eq!(history.entries.len(), FOCUS_HISTORY_LIMIT);
        assert_eq!(history.entries[0], at("$1", "@10"));
        assert_eq!(history.cursor, Some(49));
    }

    #[test]
    fn steps_over_points_that_are_gone() {
        let mut history = FocusHistory::default();
        history.visit(at("$1", "@1"));
        history.visit(at("$1", "@2"));
        history.visit(at("$1", "@3"));
        let exists = |point: &FocusPoint| point.window_id.as_deref() != Some("@2");
        assert!(history.has_step(Direction::Back, exists));
        assert_eq!(history.step(Direction::Back, exists), Some(at("$1", "@1")));
        assert_eq!(history.cursor, Some(0));
        let none = |_: &FocusPoint| false;
        assert_eq!(history.step(Direction::Forward, none), None);
        assert_eq!(history.cursor, Some(0), "a failed step does not move");
    }

    #[test]
    fn a_closing_document_hands_focus_to_the_nearest_earlier_other_point() {
        let mut history = FocusHistory::default();
        history.visit(at("$1", "@1"));
        history.visit(doc("$1", "@1", "a"));
        history.visit(at("$1", "@2"));
        history.visit(doc("$1", "@2", "a"));
        assert_eq!(history.back_from_closing("a", all), Some(at("$1", "@2")));
        assert_eq!(history.cursor, Some(2));
        let mut lone = FocusHistory::default();
        lone.visit(doc("$1", "@1", "a"));
        assert_eq!(lone.back_from_closing("a", all), None);
    }

    #[test]
    fn prunes_what_is_gone_and_keeps_the_cursor_on_its_point() {
        let mut history = FocusHistory::default();
        history.visit(at("$1", "@1"));
        history.visit(at("$1", "@2"));
        history.visit(at("$1", "@3"));
        history.step(Direction::Back, all);
        history.prune(|point| point.window_id.as_deref() != Some("@1"));
        assert_eq!(history.entries, [at("$1", "@2"), at("$1", "@3")]);
        assert_eq!(history.cursor, Some(0));
        history.prune(|point| point.window_id.as_deref() != Some("@2"));
        assert_eq!(
            history.cursor,
            Some(0),
            "its point gone, the cursor goes to the last"
        );
        history.prune(|_| false);
        assert_eq!(history, FocusHistory::default());
    }

    #[test]
    fn a_point_visited_twice_keeps_the_cursor_on_the_later_visit() {
        let mut history = FocusHistory::default();
        for window in ["@1", "@2", "@3", "@1"] {
            history.visit(at("$1", window));
        }
        history.prune(|point| point.window_id.as_deref() != Some("@2"));
        assert_eq!(
            history.entries,
            [at("$1", "@1"), at("$1", "@3"), at("$1", "@1")]
        );
        assert_eq!(history.cursor, Some(2));
    }
}
