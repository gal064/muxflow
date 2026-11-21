//! The tmux actions the shell asks the host for, and the retry that reconciles
//! one whose topology went stale. Port of `features/tmux/actions.ts` and
//! `features/tmux/actionReconciliation.ts`. The wire is `desktop-core`'s
//! `tmux_action`: snake_case fields beside a camelCase `kind`.

use std::future::Future;

use serde_json::{Value, json};

use crate::connection_ui::host_scope::{HostScopeToken, same_host_connection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    CreateSession,
    RenameSession,
    ReorderSession,
    SelectSession,
    CloseSession,
    CreateWindow,
    RenameWindow,
    ReorderWindow,
    SelectWindow,
    CloseWindow,
    SplitPaneRight,
    SplitPaneDown,
    FocusPane,
    ResizePaneLeft,
    ResizePaneRight,
    ResizePaneUp,
    ResizePaneDown,
    ZoomPane,
    ClosePane,
    SetPinned,
    MoveWindow,
    JoinPane,
    BreakPane,
}

impl ActionKind {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::CreateSession => "createSession",
            Self::RenameSession => "renameSession",
            Self::ReorderSession => "reorderSession",
            Self::SelectSession => "selectSession",
            Self::CloseSession => "closeSession",
            Self::CreateWindow => "createWindow",
            Self::RenameWindow => "renameWindow",
            Self::ReorderWindow => "reorderWindow",
            Self::SelectWindow => "selectWindow",
            Self::CloseWindow => "closeWindow",
            Self::SplitPaneRight => "splitPaneRight",
            Self::SplitPaneDown => "splitPaneDown",
            Self::FocusPane => "focusPane",
            Self::ResizePaneLeft => "resizePaneLeft",
            Self::ResizePaneRight => "resizePaneRight",
            Self::ResizePaneUp => "resizePaneUp",
            Self::ResizePaneDown => "resizePaneDown",
            Self::ZoomPane => "zoomPane",
            Self::ClosePane => "closePane",
            Self::SetPinned => "setPinned",
            Self::MoveWindow => "moveWindow",
            Self::JoinPane => "joinPane",
            Self::BreakPane => "breakPane",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativePosition {
    Before,
    After,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneEdge {
    Left,
    Right,
    Top,
    Bottom,
}

/// One action. Unset ids and names are empty, as the wire reads them.
#[derive(Clone, Debug, PartialEq)]
pub struct TmuxAction {
    pub kind: ActionKind,
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
    pub name: String,
    pub index: u32,
    pub target_window_id: String,
    pub relative_position: Option<RelativePosition>,
    pub split_size: u32,
    pub resize_cells: u32,
    pub zoomed: bool,
    /// What `setPinned` writes; `createSession` reads it too, to create the
    /// workspace already pinned.
    pub pinned: bool,
    pub confirmed: bool,
    /// Where a created workspace's first pane starts, resolved on the host.
    pub directory: String,
    pub target_pane_id: String,
    pub target_session_id: String,
    pub pane_edge: Option<PaneEdge>,
}

impl TmuxAction {
    pub fn new(kind: ActionKind) -> Self {
        Self {
            kind,
            session_id: String::new(),
            window_id: String::new(),
            pane_id: String::new(),
            name: String::new(),
            index: 0,
            target_window_id: String::new(),
            relative_position: None,
            split_size: 0,
            resize_cells: 0,
            zoomed: false,
            pinned: false,
            confirmed: false,
            directory: String::new(),
            target_pane_id: String::new(),
            target_session_id: String::new(),
            pane_edge: None,
        }
    }

    pub fn session(kind: ActionKind, session_id: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            ..Self::new(kind)
        }
    }

    pub fn window(kind: ActionKind, session_id: &str, window_id: &str) -> Self {
        Self {
            window_id: window_id.to_owned(),
            ..Self::session(kind, session_id)
        }
    }

    pub fn pane(kind: ActionKind, session_id: &str, window_id: &str, pane_id: &str) -> Self {
        Self {
            pane_id: pane_id.to_owned(),
            ..Self::window(kind, session_id, window_id)
        }
    }

    pub fn is_destructive(&self) -> bool {
        matches!(
            self.kind,
            ActionKind::CloseSession | ActionKind::CloseWindow | ActionKind::ClosePane
        )
    }

    /// The `tmux_action` body under `precondition`.
    pub fn to_wire(&self, precondition: &Precondition) -> Result<Value, String> {
        if self.kind == ActionKind::ReorderWindow
            && (self.target_window_id.is_empty()
                || self.target_window_id == self.window_id
                || self.relative_position.is_none())
        {
            return Err(
                "window reorder requires a distinct target window ID and before/after position"
                    .into(),
            );
        }
        if self.kind == ActionKind::JoinPane
            && (self.pane_id.is_empty()
                || self.target_pane_id.is_empty()
                || self.pane_id == self.target_pane_id
                || self.pane_edge.is_none())
        {
            return Err("pane join requires a distinct target pane ID and an edge".into());
        }
        if matches!(self.kind, ActionKind::MoveWindow | ActionKind::BreakPane)
            && self.target_session_id.is_empty()
        {
            return Err("a move requires a target workspace".into());
        }
        Ok(json!({
            "kind": self.kind.wire_name(),
            "session_id": self.session_id,
            "window_id": self.window_id,
            "pane_id": self.pane_id,
            "name": self.name,
            "index": self.index,
            "target_window_id": self.target_window_id,
            "relative_position": match self.relative_position {
                None => "unspecified",
                Some(RelativePosition::Before) => "before",
                Some(RelativePosition::After) => "after",
            },
            "split_size": self.split_size,
            "resize_cells": self.resize_cells,
            "zoomed": self.zoomed,
            "pinned": self.pinned,
            "expected_server_identity": precondition.server_identity,
            "expected_generation": precondition.generation,
            "confirmed": self.confirmed,
            "directory": self.directory,
            "target_pane_id": self.target_pane_id,
            "target_session_id": self.target_session_id,
            "pane_edge": match self.pane_edge {
                None => "unspecified",
                Some(PaneEdge::Left) => "left",
                Some(PaneEdge::Right) => "right",
                Some(PaneEdge::Top) => "top",
                Some(PaneEdge::Bottom) => "bottom",
            },
        }))
    }
}

/// The server and topology generation an action is aimed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Precondition {
    pub server_identity: String,
    /// 0 is the host's "no generation guard".
    pub generation: u64,
}

/// What a close guards on: the server, and no generation. A close names its
/// target by id, which keeps its meaning while the topology moves; a pinned
/// generation would be gone by the time a dialog is answered, and would turn
/// off the retry so the second click failed like the first.
pub fn close_precondition(server_identity: &str) -> Precondition {
    Precondition {
        server_identity: server_identity.to_owned(),
        generation: 0,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionResult {
    pub session_id: Option<String>,
    pub window_id: Option<String>,
    pub pane_id: Option<String>,
    pub topology_generation: u64,
}

pub fn parse_result(value: &Value) -> Result<ActionResult, String> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let Some(generation) = value.get("topologyGeneration").and_then(Value::as_u64) else {
        return Err("Native tmux action returned invalid topology metadata.".into());
    };
    Ok(ActionResult {
        session_id: text("sessionId"),
        window_id: text("windowId"),
        pane_id: text("paneId"),
        topology_generation: generation,
    })
}

/// A refusal the host raised before running anything, so a retry cannot run
/// the action twice. A failure after a mutation is reported as an unknown
/// outcome, which this deliberately does not match.
pub fn is_stale_topology_error(error: &str) -> bool {
    let error = error.to_lowercase();
    error.contains("stale topology") || error.contains("generation changed")
}

pub const SCOPE_CHANGED: &str =
    "authoritative connection changed while the tmux action was in flight";

/// How long a retry waits for a newer topology, in polls: a second at 16 ms.
/// Returns as soon as one lands, so a fast link pays nothing for it; a slow
/// one (a 300 ms round trip) needs most of it.
pub const RECONCILE_POLLS: u32 = 63;
pub const RECONCILE_POLL_MS: u64 = 16;
const RECONCILE_RETRIES: u32 = 2;

/// Sends `action`, retrying a stale-topology refusal against the newer
/// generation, at most twice. A generation the caller pinned is consent to
/// that topology, so it is never retried; a stamp of 0 pins nothing.
///
/// `request` sends one attempt, `current` reads the host's scope now, and
/// `pause` waits one poll.
pub async fn request_reconciled<Req, ReqFut, Pause, PauseFut>(
    action: &TmuxAction,
    initial: &HostScopeToken,
    captured: Option<Precondition>,
    mut request: Req,
    current: impl Fn() -> Option<HostScopeToken>,
    pause: Pause,
) -> Result<ActionResult, String>
where
    Req: FnMut(Value) -> ReqFut,
    ReqFut: Future<Output = Result<Value, String>>,
    Pause: Fn() -> PauseFut,
    PauseFut: Future<Output = ()>,
{
    let Some(server) = initial.server_identity.clone() else {
        return Err("authoritative server identity is unavailable".into());
    };
    let pins_generation = captured.as_ref().is_some_and(|stamp| stamp.generation != 0);
    let mut precondition = captured.unwrap_or(Precondition {
        server_identity: server,
        generation: initial.generation,
    });
    let mut attempted = HostScopeToken {
        server_identity: Some(precondition.server_identity.clone()),
        generation: if precondition.generation == 0 {
            initial.generation
        } else {
            precondition.generation
        },
        ..initial.clone()
    };
    let mut retry = 0;
    loop {
        let answer = request(action.to_wire(&precondition)?).await;
        let error = match answer.and_then(|value| parse_result(&value)) {
            Ok(result) => {
                return if current().is_some_and(|now| same_host_connection(&attempted, &now)) {
                    Ok(result)
                } else {
                    Err(SCOPE_CHANGED.into())
                };
            }
            Err(error) => error,
        };
        if retry >= RECONCILE_RETRIES || pins_generation || !is_stale_topology_error(&error) {
            return Err(error);
        }
        retry += 1;
        let mut refreshed = None;
        for _ in 0..RECONCILE_POLLS {
            let Some(now) = current().filter(|now| same_host_connection(&attempted, now)) else {
                break;
            };
            if now.server_identity.is_some() && now.generation > attempted.generation {
                refreshed = Some(now);
                break;
            }
            pause().await;
        }
        let Some(refreshed) = refreshed else {
            return Err(error);
        };
        precondition = Precondition {
            server_identity: refreshed.server_identity.clone().unwrap_or_default(),
            generation: if precondition.generation == 0 {
                0
            } else {
                refreshed.generation
            },
        };
        attempted = refreshed;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
    };

    use futures::executor::block_on;

    use super::*;

    fn scope(generation: u64) -> HostScopeToken {
        HostScopeToken {
            host_profile_id: "local".into(),
            connection_key: "local".into(),
            connection_epoch: 1,
            server_identity: Some("server".into()),
            generation,
        }
    }

    fn ok(generation: u64) -> Result<Value, String> {
        Ok(json!({ "sessionId": "$1", "windowId": "@2", "topologyGeneration": generation }))
    }

    const STALE: &str = "stale topology: expected 4, current 5";

    /// Runs one request against scripted answers, with the host's scope
    /// read from `scopes` one poll at a time (the last one repeats).
    fn run(
        captured: Option<Precondition>,
        answers: Vec<Result<Value, String>>,
        scopes: Vec<HostScopeToken>,
    ) -> (Result<ActionResult, String>, Vec<(u64, String)>) {
        let answers = RefCell::new(VecDeque::from(answers));
        let sent = RefCell::new(Vec::new());
        let scopes = RefCell::new(VecDeque::from(scopes));
        let polls = Cell::new(0);
        let action = TmuxAction::session(ActionKind::RenameSession, "$1");
        let result = block_on(request_reconciled(
            &action,
            &scope(4),
            captured,
            |wire: Value| {
                sent.borrow_mut().push((
                    wire["expected_generation"].as_u64().unwrap(),
                    wire["expected_server_identity"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                ));
                let answer = answers.borrow_mut().pop_front().unwrap();
                async move { answer }
            },
            || {
                let mut scopes = scopes.borrow_mut();
                if scopes.len() > 1 {
                    scopes.pop_front()
                } else {
                    scopes.front().cloned()
                }
            },
            || {
                polls.set(polls.get() + 1);
                async {}
            },
        ));
        (result, sent.into_inner())
    }

    #[test]
    fn serializes_snake_case_fields_with_a_camel_case_kind() {
        let action = TmuxAction {
            name: "build".into(),
            ..TmuxAction::window(ActionKind::RenameWindow, "$1", "@2")
        };
        let wire = action.to_wire(&close_precondition("server")).unwrap();
        assert_eq!(wire["kind"], "renameWindow");
        assert_eq!(wire["session_id"], "$1");
        assert_eq!(wire["window_id"], "@2");
        assert_eq!(wire["name"], "build");
        assert_eq!(wire["relative_position"], "unspecified");
        assert_eq!(wire["pane_edge"], "unspecified");
        assert_eq!(wire["expected_generation"], 0);
        assert_eq!(wire["expected_server_identity"], "server");
    }

    #[test]
    fn refuses_a_malformed_reorder_join_or_move() {
        let stamp = close_precondition("server");
        assert!(
            TmuxAction::window(ActionKind::ReorderWindow, "$1", "@1")
                .to_wire(&stamp)
                .is_err()
        );
        let reorder = TmuxAction {
            target_window_id: "@2".into(),
            relative_position: Some(RelativePosition::After),
            ..TmuxAction::window(ActionKind::ReorderWindow, "$1", "@1")
        };
        assert_eq!(
            reorder.to_wire(&stamp).unwrap()["relative_position"],
            "after"
        );
        assert!(
            TmuxAction::pane(ActionKind::JoinPane, "$1", "@1", "%1")
                .to_wire(&stamp)
                .is_err()
        );
        assert!(
            TmuxAction::window(ActionKind::MoveWindow, "$1", "@1")
                .to_wire(&stamp)
                .is_err()
        );
    }

    #[test]
    fn closes_are_destructive_and_stamp_no_generation() {
        assert!(TmuxAction::session(ActionKind::CloseSession, "$1").is_destructive());
        assert!(!TmuxAction::session(ActionKind::RenameSession, "$1").is_destructive());
        assert_eq!(close_precondition("server").generation, 0);
    }

    #[test]
    fn recognizes_only_stale_topology_refusals() {
        assert!(is_stale_topology_error("Stale Topology"));
        assert!(is_stale_topology_error("topology generation changed"));
        assert!(!is_stale_topology_error("outcome unknown"));
    }

    #[test]
    fn parses_the_native_result_and_refuses_one_without_a_generation() {
        let result = parse_result(&ok(7).unwrap()).unwrap();
        assert_eq!(result.topology_generation, 7);
        assert_eq!(result.window_id.as_deref(), Some("@2"));
        assert_eq!(result.pane_id, None);
        assert!(parse_result(&json!({ "sessionId": "$1" })).is_err());
        assert!(parse_result(&json!({ "topologyGeneration": -1 })).is_err());
    }

    #[test]
    fn retries_a_stale_refusal_against_the_newer_generation() {
        let (result, sent) = run(
            None,
            vec![Err(STALE.into()), ok(6)],
            vec![scope(4), scope(5)],
        );
        assert_eq!(result.unwrap().topology_generation, 6);
        assert_eq!(sent, [(4, "server".to_owned()), (5, "server".to_owned())]);
    }

    #[test]
    fn a_zero_stamp_retries_and_stays_unguarded() {
        let (result, sent) = run(
            Some(close_precondition("server")),
            vec![Err(STALE.into()), ok(6)],
            vec![scope(4), scope(5)],
        );
        assert!(result.is_ok());
        assert_eq!(
            sent.iter()
                .map(|(generation, _)| *generation)
                .collect::<Vec<_>>(),
            [0, 0]
        );
    }

    #[test]
    fn never_retries_a_pinned_generation_or_another_error() {
        let pinned = Precondition {
            server_identity: "server".into(),
            generation: 4,
        };
        let (result, sent) = run(Some(pinned), vec![Err(STALE.into())], vec![scope(5)]);
        assert_eq!(result.unwrap_err(), STALE);
        assert_eq!(sent.len(), 1);
        let (result, sent) = run(None, vec![Err("outcome unknown".into())], vec![scope(5)]);
        assert_eq!(result.unwrap_err(), "outcome unknown");
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn gives_up_after_two_retries_or_without_a_newer_topology() {
        let (result, sent) = run(
            None,
            vec![Err(STALE.into()), Err(STALE.into()), Err(STALE.into())],
            vec![scope(4), scope(5), scope(6)],
        );
        assert_eq!(result.unwrap_err(), STALE);
        assert_eq!(sent.len(), 3);
        // The topology never moves: the original refusal stands.
        let (result, sent) = run(None, vec![Err(STALE.into())], vec![scope(4)]);
        assert_eq!(result.unwrap_err(), STALE);
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn a_connection_that_changed_under_the_request_is_not_success() {
        let restarted = HostScopeToken {
            server_identity: Some("other".into()),
            ..scope(4)
        };
        let (result, _) = run(None, vec![ok(5)], vec![restarted.clone()]);
        assert_eq!(result.unwrap_err(), SCOPE_CHANGED);
        // Nor is it somewhere to retry.
        let (result, sent) = run(None, vec![Err(STALE.into())], vec![restarted]);
        assert_eq!(result.unwrap_err(), STALE);
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn needs_a_server_identity() {
        let action = TmuxAction::session(ActionKind::SelectSession, "$1");
        let mut initial = scope(1);
        initial.server_identity = None;
        let result = block_on(request_reconciled(
            &action,
            &initial,
            None,
            |_| async { ok(1) },
            || None,
            || async {},
        ));
        assert!(result.is_err());
    }
}
