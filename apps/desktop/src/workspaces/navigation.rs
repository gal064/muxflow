//! Moving the shell between workspaces, windows and panes: which writes a move
//! needs, and the coordinator that sends them one move at a time, keeping only
//! the latest the user asked for. Port of `app/shellNavigationCoordinator.ts`
//! and `app/windowSelection.ts`, with `adjacentPane` from
//! `features/terminal/layout.ts`.

use tmux_control::{Pane, Window};

use super::actions::{ActionKind, RelativePosition, TmuxAction};
use crate::connection_ui::host_links::{resolve_active_window_id, session_windows};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub session_id: String,
    pub window_id: Option<String>,
}

/// How a move ended. A partial one changed the workspace and then stopped
/// for a newer move; an unknown one may or may not have changed anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Reached { location: Location, generation: u64 },
    Partial { location: Location, generation: u64 },
    Unknown,
}

impl Outcome {
    pub fn generation(&self) -> Option<u64> {
        match self {
            Self::Reached { generation, .. } | Self::Partial { generation, .. } => {
                Some(*generation)
            }
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub select_session: bool,
    pub select_window: bool,
}

/// The fewest writes that take the host from `predecessor` to `target`.
/// `whole_workspace` is a workspace destination, which always selects its
/// workspace when nothing came before it.
pub fn transition_plan(
    predecessor: Option<&Outcome>,
    target: &Location,
    whole_workspace: bool,
    window_already_active: bool,
    session_already_active: bool,
) -> Plan {
    let previous = match predecessor {
        None => {
            return Plan {
                select_session: whole_workspace || !session_already_active,
                select_window: target.window_id.is_some() && !window_already_active,
            };
        }
        Some(Outcome::Unknown) => {
            return Plan {
                select_session: true,
                select_window: target.window_id.is_some(),
            };
        }
        Some(Outcome::Reached { location, .. } | Outcome::Partial { location, .. }) => location,
    };
    let changes_session = previous.session_id != target.session_id;
    Plan {
        select_session: changes_session,
        // The same window id in another workspace is a window that moved there,
        // and whether it is current is that workspace's to say.
        select_window: target.window_id.is_some()
            && (changes_session || previous.window_id != target.window_id)
            && !((changes_session || previous.window_id.is_none()) && window_already_active),
    }
}

/// Serializes moves and keeps only the latest intent: a move asked for while
/// one is in flight replaces any other waiting move, and starts once the
/// flight lands, planned from where that one ended.
pub struct Coordinator<T> {
    next_id: u64,
    flight: Option<(u64, String)>,
    desired: Option<(String, T)>,
}

impl<T> Default for Coordinator<T> {
    fn default() -> Self {
        Self {
            next_id: 0,
            flight: None,
            desired: None,
        }
    }
}

/// What a landed flight hands on.
#[derive(Debug, PartialEq, Eq)]
pub enum Finish<T> {
    /// It was the latest intent: its result stands.
    Latest,
    /// A newer intent for the same destination arrived: it takes the result.
    SameDestination(T),
    /// A newer intent for somewhere else: start it, planned from this result.
    Next(u64, T),
    /// The connection changed under it; nothing it did counts.
    Detached,
}

impl<T> Coordinator<T> {
    /// Starts `intent` now with its flight id, or queues it behind the flight
    /// (replacing whatever was queued) and returns `None`.
    pub fn navigate(&mut self, key: String, intent: T) -> Option<(u64, T)> {
        if self.flight.is_some() {
            self.desired = Some((key, intent));
            return None;
        }
        let id = self.begin(key);
        Some((id, intent))
    }

    fn begin(&mut self, key: String) -> u64 {
        self.next_id += 1;
        self.flight = Some((self.next_id, key));
        self.next_id
    }

    /// Takes the move waiting behind the flight, which a newer one supersedes.
    pub fn drop_queued(&mut self) -> Option<T> {
        self.desired.take().map(|(_, intent)| intent)
    }

    pub fn has_flight(&self) -> bool {
        self.flight.is_some()
    }

    /// Whether flight `id` is still the latest intent, so a two-step move may
    /// take its second step.
    pub fn is_current(&self, id: u64) -> bool {
        self.flight
            .as_ref()
            .is_some_and(|(flight, _)| *flight == id)
            && self.desired.is_none()
    }

    pub fn finish(&mut self, id: u64) -> Finish<T> {
        let Some((flight, key)) = self.flight.take_if(|(flight, _)| *flight == id) else {
            return Finish::Detached;
        };
        debug_assert_eq!(flight, id);
        match self.desired.take() {
            None => Finish::Latest,
            Some((desired, intent)) if desired == key => Finish::SameDestination(intent),
            Some((desired, intent)) => Finish::Next(self.begin(desired), intent),
        }
    }

    /// The connection changed: the flight and anything queued are dropped, and
    /// the next move starts at once.
    pub fn invalidate(&mut self) {
        self.flight = None;
        self.desired = None;
    }
}

/// Which window a workspace shows, from the whole snapshot, so a workspace
/// switch can name its window in the same commit as the workspace.
pub fn window_for_session(
    windows: &[Window],
    session_id: &str,
    previous: Option<&str>,
) -> Option<String> {
    let owned = session_windows(windows, session_id);
    resolve_active_window_id(&owned, previous, None).map(str::to_owned)
}

/// Moves a window one place within its workspace, against its neighbour's id.
pub fn relative_window_reorder_action(
    windows: &[&Window],
    window_id: &str,
    left: bool,
) -> Option<TmuxAction> {
    let mut ordered = windows.to_vec();
    ordered.sort_by_key(|window| window.index);
    let current = ordered.iter().position(|window| window.id == window_id)?;
    let target = if left {
        current.checked_sub(1)?
    } else {
        current + 1
    };
    let (active, target) = (ordered[current], *ordered.get(target)?);
    if target.session_id != active.session_id || target.id == active.id {
        return None;
    }
    Some(TmuxAction {
        target_window_id: target.id.clone(),
        relative_position: Some(if left {
            RelativePosition::Before
        } else {
            RelativePosition::After
        }),
        ..TmuxAction::window(ActionKind::ReorderWindow, &active.session_id, &active.id)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneDirection {
    Left,
    Right,
    Up,
    Down,
}

/// The nearest pane in `direction`, weighing distance across the direction
/// four times as much as along it.
pub fn adjacent_pane<'a>(
    panes: &[&'a Pane],
    active: &Pane,
    direction: PaneDirection,
) -> Option<&'a Pane> {
    let center = |pane: &Pane| {
        (
            f32::from(pane.left) + f32::from(pane.width) / 2.,
            f32::from(pane.top) + f32::from(pane.height) / 2.,
        )
    };
    let (x, y) = center(active);
    panes
        .iter()
        .filter(|pane| pane.id != active.id)
        .filter_map(|pane| {
            let (cx, cy) = center(pane);
            let (primary, secondary) = match direction {
                PaneDirection::Left => (x - cx, (cy - y).abs()),
                PaneDirection::Right => (cx - x, (cy - y).abs()),
                PaneDirection::Up => (y - cy, (cx - x).abs()),
                PaneDirection::Down => (cy - y, (cx - x).abs()),
            };
            (primary > 0.).then_some((*pane, primary * primary + secondary * secondary * 4.))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(pane, _)| pane)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::model::fixtures::{pane, window};

    fn windows(active: &str) -> Vec<Window> {
        vec![
            window("@1", "$1", 0, "one", active == "@1"),
            window("@2", "$1", 1, "two", active == "@2"),
        ]
    }

    fn resolve(
        windows: &[Window],
        current: Option<&str>,
        optimistic: Option<&str>,
    ) -> Option<String> {
        let refs: Vec<&Window> = windows.iter().collect();
        resolve_active_window_id(&refs, current, optimistic).map(str::to_owned)
    }

    fn at(session: &str, window: Option<&str>) -> Location {
        Location {
            session_id: session.into(),
            window_id: window.map(str::to_owned),
        }
    }

    fn reached(session: &str, window: Option<&str>) -> Outcome {
        Outcome::Reached {
            location: at(session, window),
            generation: 1,
        }
    }

    #[test]
    fn follows_the_host_unless_a_switch_is_outstanding() {
        assert_eq!(
            resolve(&windows("@2"), Some("@1"), None).as_deref(),
            Some("@2")
        );
        assert_eq!(
            resolve(&windows("@1"), Some("@1"), Some("@2")).as_deref(),
            Some("@2")
        );
        assert_eq!(
            resolve(&windows("@1"), Some("@1"), Some("@9")).as_deref(),
            Some("@1")
        );
    }

    #[test]
    fn resolves_a_workspaces_window_from_the_whole_snapshot() {
        let mut across = windows("@1");
        across.push(window("@5", "$2", 1, "five", false));
        across.push(window("@4", "$2", 0, "four", true));
        assert_eq!(
            window_for_session(&across, "$2", Some("@1")).as_deref(),
            Some("@4")
        );
        across[3].active = false;
        assert_eq!(
            window_for_session(&across, "$2", None).as_deref(),
            Some("@4")
        );
        assert_eq!(window_for_session(&across, "$9", None), None);
    }

    #[test]
    fn reorders_against_an_adjacent_window_of_the_same_workspace() {
        let all = windows("@1");
        let unordered = vec![&all[1], &all[0]];
        let left = relative_window_reorder_action(&unordered, "@2", true).unwrap();
        assert_eq!(
            (
                left.window_id.as_str(),
                left.target_window_id.as_str(),
                left.relative_position
            ),
            ("@2", "@1", Some(RelativePosition::Before))
        );
        let right = relative_window_reorder_action(&unordered, "@1", false).unwrap();
        assert_eq!(right.relative_position, Some(RelativePosition::After));
        assert_eq!(relative_window_reorder_action(&unordered, "@1", true), None);
        assert_eq!(
            relative_window_reorder_action(&unordered, "@2", false),
            None
        );
        let mut cross = windows("@1");
        cross[1].session_id = "$2".into();
        let refs: Vec<&Window> = cross.iter().collect();
        assert_eq!(relative_window_reorder_action(&refs, "@1", false), None);
    }

    #[test]
    fn plans_only_the_writes_a_move_needs() {
        let plan = |predecessor: Outcome, target: Location, already_active: bool| {
            let plan = transition_plan(Some(&predecessor), &target, false, already_active, true);
            (plan.select_session, plan.select_window)
        };
        assert_eq!(
            plan(reached("$1", Some("@0")), at("$1", Some("@1")), false),
            (false, true)
        );
        assert_eq!(
            plan(reached("$1", Some("@0")), at("$1", Some("@0")), false),
            (false, false)
        );
        assert_eq!(
            plan(reached("$2", Some("@9")), at("$1", Some("@0")), false),
            (true, true)
        );
        assert_eq!(
            plan(Outcome::Unknown, at("$1", Some("@0")), false),
            (true, true)
        );
        assert_eq!(
            plan(reached("$1", Some("@0")), at("$2", Some("@9")), true),
            (true, false)
        );
        assert_eq!(
            plan(reached("$2", None), at("$2", Some("@9")), true),
            (false, false)
        );
        assert_eq!(
            plan(reached("$2", Some("@8")), at("$2", Some("@9")), true),
            (false, true)
        );
        // From nothing: a workspace move always selects its workspace.
        let first = transition_plan(None, &at("$1", None), true, false, true);
        assert_eq!((first.select_session, first.select_window), (true, false));
        let window = transition_plan(None, &at("$1", Some("@2")), false, false, true);
        assert_eq!((window.select_session, window.select_window), (false, true));
    }

    #[test]
    fn sends_the_first_move_then_only_the_latest() {
        let mut coordinator = Coordinator::default();
        let (a, _) = coordinator.navigate("A".into(), "a").unwrap();
        assert!(coordinator.navigate("B".into(), "b").is_none());
        assert!(coordinator.navigate("C".into(), "c").is_none());
        assert!(!coordinator.is_current(a));
        let Finish::Next(c, "c") = coordinator.finish(a) else {
            panic!("C starts once A lands");
        };
        assert!(coordinator.is_current(c));
        assert_eq!(coordinator.finish(c), Finish::Latest);
        assert!(!coordinator.has_flight());
    }

    #[test]
    fn coalesces_a_duplicate_of_the_flight_into_its_result() {
        let mut coordinator = Coordinator::default();
        let (first, _) = coordinator.navigate("same".into(), 1).unwrap();
        assert!(coordinator.navigate("same".into(), 2).is_none());
        assert_eq!(coordinator.finish(first), Finish::SameDestination(2));
    }

    #[test]
    fn detaches_old_work_when_the_connection_changes() {
        let mut coordinator = Coordinator::default();
        let (old, _) = coordinator.navigate("old".into(), "old").unwrap();
        coordinator.invalidate();
        let (replacement, _) = coordinator.navigate("new".into(), "new").unwrap();
        assert_eq!(coordinator.finish(old), Finish::Detached);
        assert_eq!(coordinator.finish(replacement), Finish::Latest);
    }

    #[test]
    fn finds_the_nearest_pane_in_a_direction() {
        let panes = [
            pane("%1", "@1", "$1", (0, 0, 40, 24)),
            pane("%2", "@1", "$1", (41, 0, 39, 11)),
            pane("%3", "@1", "$1", (41, 12, 39, 12)),
        ];
        let refs: Vec<&Pane> = panes.iter().collect();
        let id = |from: usize, direction| {
            adjacent_pane(&refs, &panes[from], direction).map(|pane| pane.id.as_str())
        };
        // The lower pane is nearer the centre line of the taller one.
        assert_eq!(id(0, PaneDirection::Right), Some("%3"));
        assert_eq!(id(2, PaneDirection::Left), Some("%1"));
        assert_eq!(id(2, PaneDirection::Up), Some("%2"));
        assert_eq!(id(1, PaneDirection::Down), Some("%3"));
        assert_eq!(id(0, PaneDirection::Left), None);
    }
}
