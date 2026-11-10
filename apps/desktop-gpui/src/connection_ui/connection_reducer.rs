//! One host's connection and topology state. Port of
//! `state/connectionReducer.ts`.
//!
//! `orderedSnapshot`, `orderedEvent` and `resyncRequested` are not carried over:
//! nothing in the desktop dispatched them. The native link is the sequence
//! authority, so a topology answer applies directly.

use std::sync::Arc;

use tmux_control::{Session, TmuxSnapshot};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectionPhase {
    #[default]
    Disconnected,
    Connecting,
    Reconnecting,
    Resyncing,
    Connected,
}

impl ConnectionPhase {
    pub fn from_wire(label: &str) -> Option<Self> {
        match label {
            "disconnected" => Some(Self::Disconnected),
            "connecting" => Some(Self::Connecting),
            "reconnecting" => Some(Self::Reconnecting),
            "resyncing" => Some(Self::Resyncing),
            "connected" => Some(Self::Connected),
            _ => None,
        }
    }

    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Reconnecting => "reconnecting",
            Self::Resyncing => "resyncing",
            Self::Connected => "connected",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NormalizedHostState {
    pub phase: ConnectionPhase,
    pub can_mutate: bool,
    pub server_identity: Option<String>,
    pub generation: u64,
    /// Whether `generation` was stamped by the connection that is live now.
    ///
    /// The host counts topology generations from zero, per process, so the
    /// number only means anything inside one connection. Until a snapshot has
    /// set that baseline, a lower generation cannot be told from a restarted
    /// one, so none is refused.
    pub generation_baselined: bool,
    pub last_sequence: u64,
    pub snapshot: Arc<TmuxSnapshot>,
}

impl NormalizedHostState {
    pub fn session(&self, id: &str) -> Option<&Session> {
        self.snapshot.sessions.iter().find(|item| item.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotAction {
    /// `None` is the host's reconciliation acknowledgement: a notified pass
    /// that found the world unchanged, with nothing to apply.
    pub snapshot: Option<Arc<TmuxSnapshot>>,
    pub sequence: u64,
    pub server_identity: String,
    pub generation: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostAction {
    Connection { phase: ConnectionPhase },
    Snapshot(SnapshotAction),
    Reset,
}

/// A snapshot the transport delivered late, after a newer one for the same tmux
/// server already landed. Applying it would walk `generation` backwards, and
/// every action stamped against a generation the host has left is refused.
fn precedes_live_generation(state: &NormalizedHostState, action: &SnapshotAction) -> bool {
    state.generation_baselined
        && state.server_identity.as_deref() == Some(action.server_identity.as_str())
        && action
            .generation
            .is_some_and(|value| value < state.generation)
}

fn replace_snapshot(state: &mut NormalizedHostState, action: SnapshotAction) {
    let Some(snapshot) = action.snapshot else {
        // The acknowledgement spends its sequence and changes nothing else,
        // except a forward generation: every action this side sends is stamped
        // against it. A frame with no world in it is never a new baseline.
        if let Some(generation) = action.generation.filter(|value| *value > state.generation) {
            state.generation = generation;
        }
        state.last_sequence = action.sequence;
        return;
    };
    state.server_identity = Some(action.server_identity);
    state.generation = action.generation.unwrap_or(state.generation + 1);
    state.generation_baselined = true;
    state.last_sequence = action.sequence;
    state.snapshot = snapshot;
}

pub fn connection_reducer(state: &mut NormalizedHostState, action: HostAction) {
    match action {
        HostAction::Reset => *state = NormalizedHostState::default(),
        HostAction::Connection { phase } => {
            state.phase = phase;
            state.can_mutate = phase == ConnectionPhase::Connected;
            // A restarted host process behind the same tmux server counts from
            // zero again, so a transition *away* from connected drops the
            // baseline. The bridge announces a new link as epoch, authoritative
            // snapshot, then connected, so the way in must keep it.
            state.generation_baselined =
                phase == ConnectionPhase::Connected && state.generation_baselined;
        }
        HostAction::Snapshot(action) => {
            if !precedes_live_generation(state, &action) {
                replace_snapshot(state, action);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tmux_control::{Pane, Window};

    use super::*;

    fn populated() -> Arc<TmuxSnapshot> {
        Arc::new(TmuxSnapshot {
            sessions: vec![Session {
                id: "$1".into(),
                name: "work".into(),
                window_count: 1,
                attached_clients: 0,
                order: 0,
                pinned: false,
            }],
            windows: vec![Window {
                id: "@2".into(),
                session_id: "$1".into(),
                index: 0,
                name: "shell".into(),
                active: true,
                layout: String::new(),
                zoomed: false,
                pinned: false,
            }],
            panes: vec![Pane {
                id: "%3".into(),
                session_id: "$1".into(),
                window_id: "@2".into(),
                index: 0,
                active: true,
                width: 80,
                height: 24,
                left: 0,
                top: 0,
                current_path: "/tmp".into(),
                current_command: "bash".into(),
                pane_pid: 0,
                start_command: String::new(),
                title: String::new(),
            }],
        })
    }

    fn empty() -> Arc<TmuxSnapshot> {
        Arc::new(TmuxSnapshot::default())
    }

    fn snapshot(
        world: Option<Arc<TmuxSnapshot>>,
        sequence: u64,
        generation: Option<u64>,
        server: &str,
    ) -> HostAction {
        HostAction::Snapshot(SnapshotAction {
            snapshot: world,
            sequence,
            server_identity: server.into(),
            generation,
        })
    }

    fn connection(phase: ConnectionPhase) -> HostAction {
        HostAction::Connection { phase }
    }

    fn reduced(mut state: NormalizedHostState, action: HostAction) -> NormalizedHostState {
        connection_reducer(&mut state, action);
        state
    }

    #[test]
    fn replaces_every_entity_on_a_snapshot_and_mutates_only_when_connected() {
        let snapshotted = reduced(
            NormalizedHostState::default(),
            snapshot(Some(populated()), 4, None, "server-a"),
        );
        assert_eq!(snapshotted.snapshot.panes[0].id, "%3");
        assert!(!snapshotted.can_mutate);
        let connected = reduced(snapshotted, connection(ConnectionPhase::Connected));
        assert!(connected.can_mutate);
    }

    #[test]
    fn a_snapshot_whose_generation_went_backwards_is_ignored_and_a_reconnect_rebaselines() {
        let live = reduced(
            reduced(
                NormalizedHostState::default(),
                connection(ConnectionPhase::Connected),
            ),
            snapshot(Some(populated()), 7, Some(12), "server-a"),
        );
        let late = reduced(
            live.clone(),
            snapshot(Some(empty()), 8, Some(11), "server-a"),
        );
        assert_eq!(late, live);
        // Another tmux server and a reconnected link both still land.
        let other = reduced(
            live.clone(),
            snapshot(Some(empty()), 1, Some(1), "server-b"),
        );
        assert_eq!(other.generation, 1);
        let reconnected = reduced(
            reduced(
                reduced(live, connection(ConnectionPhase::Reconnecting)),
                connection(ConnectionPhase::Connected),
            ),
            snapshot(Some(empty()), 0, Some(1), "server-a"),
        );
        assert_eq!(reconnected.generation, 1);
        assert!(reconnected.snapshot.panes.is_empty());
    }

    #[test]
    fn keeps_the_baseline_when_the_link_then_reports_connected() {
        let baselined = reduced(
            NormalizedHostState::default(),
            snapshot(Some(populated()), 7, Some(12), "server-a"),
        );
        let connected = reduced(baselined, connection(ConnectionPhase::Connected));
        assert!(connected.can_mutate);
        let late = reduced(
            connected.clone(),
            snapshot(Some(empty()), 8, Some(11), "server-a"),
        );
        assert_eq!(late, connected);
        assert_eq!(late.generation, 12);
    }

    #[test]
    fn keeps_the_world_a_reconciliation_acknowledgement_did_not_describe() {
        let live = reduced(
            NormalizedHostState::default(),
            snapshot(Some(populated()), 7, Some(4), "server-a"),
        );
        let acknowledged = reduced(live.clone(), snapshot(None, 8, Some(4), "server-a"));
        assert_eq!(acknowledged.snapshot, live.snapshot);
        assert_eq!(acknowledged.generation, 4);
        assert_eq!(acknowledged.server_identity.as_deref(), Some("server-a"));
        assert_eq!(acknowledged.last_sequence, 8);
    }

    #[test]
    fn takes_the_generation_an_acknowledgement_carries_forwards_only() {
        let live = reduced(
            NormalizedHostState::default(),
            snapshot(Some(populated()), 7, Some(4), "server-a"),
        );
        let advanced = reduced(live.clone(), snapshot(None, 8, Some(9), "server-a"));
        assert_eq!(advanced.generation, 9);
        assert_eq!(advanced.snapshot, live.snapshot);

        // Never backwards: after a reconnect the guard stands down, but a frame
        // carrying no world is not the baseline that may walk the number back.
        let reconnected = reduced(
            reduced(advanced, connection(ConnectionPhase::Reconnecting)),
            connection(ConnectionPhase::Connected),
        );
        let late = reduced(reconnected, snapshot(None, 10, Some(5), "server-a"));
        assert_eq!(late.generation, 9);
        assert_eq!(late.last_sequence, 10);
    }

    #[test]
    fn accepts_a_lower_sequence_authoritative_reconnect_snapshot_for_the_same_server() {
        let state = reduced(
            NormalizedHostState::default(),
            snapshot(Some(populated()), 42, None, "same-server"),
        );
        let reconciled = reduced(state, snapshot(Some(empty()), 0, None, "same-server"));
        assert_eq!(reconciled.last_sequence, 0);
        assert!(reconciled.snapshot.panes.is_empty());
    }
}
