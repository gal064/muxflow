use anyhow::{Context, bail};
use tmux_agent_protocol::v1;

use super::snapshot::{
    discover_consistent, reorder_session, server_identity, set_pinned, tmux_command,
};
use super::terminal::validate_tmux_id;
use command::{
    configure_new_session, configure_new_window, configure_split, escaped_format_literal, run,
    run_for_id, run_for_ids, validate_name,
};

mod command;

pub(super) struct ActionOutcome {
    pub result: v1::TmuxActionResult,
    pub snapshot: tmux_control::TmuxSnapshot,
    pub server_identity: String,
    pub covered_dirty_epoch: Option<u64>,
}

pub(super) fn execute(
    action: v1::TmuxAction,
    known_generation: u64,
    expected_snapshot: tmux_control::TmuxSnapshot,
    expected_identity: String,
    before_post_discovery: impl FnOnce(
        v1::TmuxActionKind,
        &v1::TmuxActionResult,
    ) -> anyhow::Result<Option<u64>>,
) -> anyhow::Result<ActionOutcome> {
    let kind = v1::TmuxActionKind::try_from(action.kind).unwrap_or_default();
    // The dispatcher discovered this snapshot under the same topology lock
    // immediately before calling in, and proved it equals the cached baseline.
    // Re-discovering here would repeat six tmux forks to learn nothing, and the
    // forks were most of every action's latency. The snapshot the dispatcher
    // resolved is the pre-action topology by construction.
    let bootstrapping = bootstraps_server(kind, &expected_identity);
    if bootstrapping && server_identity() != "tmux:none" {
        // The bootstrap arm is the one path where the dispatcher deliberately
        // skipped discovery, so it is the only place a server can have appeared
        // since. Probing identity costs one fork on the first-ever create and
        // preserves the previous refusal: a session is never created on a
        // server the caller never saw.
        bail!("stale topology: a tmux server started before the bootstrap action executed");
    }
    // The input barrier now precedes the dispatcher's final authoritative
    // discovery. Reuse that snapshot here: a second discovery used to repeat
    // the same tmux fork after the barrier and did not close the unavoidable
    // external-client race between the last precheck and the mutation.
    let mut before = expected_snapshot;
    let identity = expected_identity;
    if let Some(refusal) = staleness_refusal(kind, &action, known_generation, &identity) {
        bail!("{refusal}");
    }

    require_confirmation(kind, action.confirmed)?;
    validate_targets(kind, &action, &before)?;
    let postcondition_action = action.clone();
    let postcondition_before = before.clone();

    let mut result = v1::TmuxActionResult::default();
    let mut command = tmux_command()?;
    match kind {
        v1::TmuxActionKind::CreateSession => {
            configure_new_session(&mut command, &action)?;
            let ids = run_for_ids(command, &['$', '@', '%'])?;
            result.session_id = ids[0].clone();
            result.window_id = ids[1].clone();
            result.pane_id = ids[2].clone();
        }
        v1::TmuxActionKind::RenameSession => {
            validate_name(&action.name)?;
            // Literal, for the reason `escaped_format_literal` documents: tmux
            // expands a rename's name as a format too.
            command.args([
                "rename-session",
                "-t",
                &action.session_id,
                &escaped_format_literal(&action.name),
            ]);
            run(command)?;
            result.session_id = action.session_id;
        }
        v1::TmuxActionKind::ReorderSession => {
            // tmux has no session index. Persist app presentation order in a
            // private host sidecar keyed by server identity and stable IDs;
            // never mutate tmux user options, names, keys, prefix, or status.
            reorder_session(&identity, &mut before, &action.session_id, action.index)?;
            result.session_id = action.session_id;
        }
        v1::TmuxActionKind::SetPinned => {
            // Presentation, like the session order above: tmux is sent nothing
            // and the flag lives in a private host sidecar, which is what lets
            // every client of this server agree about what is pinned.
            set_pinned(
                &identity,
                &mut before,
                &action.session_id,
                &action.window_id,
                action.pinned,
            )?;
            result.session_id = action.session_id;
            result.window_id = action.window_id;
        }
        v1::TmuxActionKind::SelectSession => {
            // Session selection belongs to this app's control client, not a
            // normal user's simultaneously attached client. The client manager
            // applies visibility and sizing after this validated no-op.
            result.session_id = action.session_id;
        }
        v1::TmuxActionKind::CloseSession => {
            command.args(["kill-session", "-t", &action.session_id]);
            run(command)?;
            result.session_id = action.session_id;
        }
        v1::TmuxActionKind::CreateWindow => {
            configure_new_window(&mut command, &action)?;
            result.session_id = action.session_id;
            result.window_id = run_for_id(command, '@')?;
        }
        v1::TmuxActionKind::RenameWindow => {
            validate_name(&action.name)?;
            command.args([
                "rename-window",
                "-t",
                &action.window_id,
                &escaped_format_literal(&action.name),
            ]);
            run(command)?;
            result.window_id = action.window_id;
        }
        v1::TmuxActionKind::ReorderWindow => {
            reorder_window(
                &before,
                &action.session_id,
                &action.window_id,
                &action.target_window_id,
                v1::WindowRelativePosition::try_from(action.relative_position).unwrap_or_default(),
            )?;
            result.session_id = action.session_id;
            result.window_id = action.window_id;
        }
        v1::TmuxActionKind::SelectWindow => {
            command.args(["select-window", "-t", &action.window_id]);
            run(command)?;
            result.window_id = action.window_id;
        }
        v1::TmuxActionKind::CloseWindow => {
            command.args(["kill-window", "-t", &action.window_id]);
            run(command)?;
            result.window_id = action.window_id;
        }
        v1::TmuxActionKind::SplitPaneRight | v1::TmuxActionKind::SplitPaneDown => {
            configure_split(&mut command, kind, &action)?;
            result.pane_id = run_for_id(command, '%')?;
        }
        v1::TmuxActionKind::FocusPane => {
            command.args(["select-pane", "-t", &action.pane_id]);
            run(command)?;
            result.pane_id = action.pane_id;
        }
        v1::TmuxActionKind::ResizePaneLeft
        | v1::TmuxActionKind::ResizePaneRight
        | v1::TmuxActionKind::ResizePaneUp
        | v1::TmuxActionKind::ResizePaneDown => {
            let flag = match kind {
                v1::TmuxActionKind::ResizePaneLeft => "-L",
                v1::TmuxActionKind::ResizePaneRight => "-R",
                v1::TmuxActionKind::ResizePaneUp => "-U",
                v1::TmuxActionKind::ResizePaneDown => "-D",
                _ => unreachable!(),
            };
            let cells = action.resize_cells.max(1);
            if cells > 1000 {
                bail!("pane resize must be at most 1000 cells");
            }
            command.args([
                "resize-pane",
                flag,
                "-t",
                &action.pane_id,
                &cells.to_string(),
            ]);
            run(command)?;
            result.pane_id = action.pane_id;
        }
        v1::TmuxActionKind::ZoomPane => {
            let window = before
                .windows
                .iter()
                .find(|window| {
                    before
                        .panes
                        .iter()
                        .any(|pane| pane.id == action.pane_id && pane.window_id == window.id)
                })
                .context("pane window disappeared")?;
            if window.zoomed != action.zoomed {
                command.args(["resize-pane", "-Z", "-t", &action.pane_id]);
                run(command)?;
            }
            result.pane_id = action.pane_id;
        }
        v1::TmuxActionKind::ClosePane => {
            command.args(["kill-pane", "-t", &action.pane_id]);
            run(command)?;
            result.pane_id = action.pane_id;
        }
        v1::TmuxActionKind::Unspecified => bail!("tmux action kind is required"),
    }

    // Drain the connection writer through all dirty notifications already
    // emitted by this command. The authoritative post-discovery that follows
    // therefore closes exactly those epochs; later dirtiness remains pending.
    let covered_dirty_epoch = before_post_discovery(kind, &result)?;
    let (mut snapshot, server_identity) =
        finalize_action(kind, discover_consistent, server_identity)?;
    if kind == v1::TmuxActionKind::CreateSession && action.pinned {
        // A create issued while the app lists pinned workspaces only is born
        // pinned, or it would drop out of the list on the first switch away.
        // Written after the post-discovery on purpose: the pin names a session
        // only the post-action snapshot contains, and on the bootstrap create
        // the pre-action identity is "tmux:none" — the sidecar must be keyed
        // by the identity of the server the create just started.
        // A refusal-shaped error here would be a lie: the session exists.
        set_pinned(&server_identity, &mut snapshot, &result.session_id, "", true).map_err(
            |error| {
                anyhow::anyhow!(
                    "outcome unknown: tmux created the session but its pin could not be written: {error}"
                )
            },
        )?;
    }
    if result.pane_id.is_empty() {
        result.pane_id = interaction_pane_id(kind, &postcondition_action, &result, &snapshot)
            .unwrap_or_default();
    }
    let identity_preserved =
        identity_transition_allowed(kind, bootstrapping, &identity, &server_identity);
    if !identity_preserved
        || !action_postcondition(
            kind,
            &postcondition_action,
            &result,
            &postcondition_before,
            &snapshot,
        )
    {
        bail!(
            "outcome unknown: tmux accepted the action but its identity-relative authoritative postcondition failed"
        );
    }
    Ok(ActionOutcome {
        result,
        snapshot,
        server_identity,
        covered_dirty_epoch,
    })
}

/// Selection actions: idempotent, carrying no state of their own, and validated
/// entirely by the target id `validate_targets` looks up in the pre-action
/// snapshot. Selecting `@7` means the same thing whatever the pane geometry did
/// since the caller last saw the topology, and re-running it changes nothing.
///
/// Everything else is excluded on purpose. Create/close/kill/rename/reorder act
/// on a topology the caller reasoned about (an index, a neighbour, the one they
/// meant), split and resize change geometry, zoom flips a window's layout, and
/// a pin writes host state — for those a generation guard is the caller's
/// consent stamp and must keep refusing.
pub(super) fn selection_only(kind: v1::TmuxActionKind) -> bool {
    matches!(
        kind,
        v1::TmuxActionKind::SelectSession
            | v1::TmuxActionKind::SelectWindow
            | v1::TmuxActionKind::FocusPane
    )
}

/// The staleness gate every action passes before its tmux command runs.
///
/// A different tmux server is always real staleness: nothing the caller named
/// exists on it. A newer *generation* is not, for a selection: on a slow link
/// every switch resizes the visible session, which bumps the generation, and
/// the snapshot carrying it is still crossing the wire when the user's next
/// switch is sent — so the switch that would fix the screen was the one being
/// refused. `validate_targets` still runs, so a selection naming something that
/// has gone away fails as cleanly as before.
fn staleness_refusal(
    kind: v1::TmuxActionKind,
    action: &v1::TmuxAction,
    known_generation: u64,
    identity: &str,
) -> Option<&'static str> {
    if !action.expected_server_identity.is_empty() && action.expected_server_identity != identity {
        return Some("stale topology: tmux server identity changed");
    }
    if selection_only(kind) {
        return None;
    }
    if action.expected_generation != 0 && action.expected_generation != known_generation {
        return Some("stale topology: generation changed");
    }
    None
}

fn interaction_pane_id(
    kind: v1::TmuxActionKind,
    action: &v1::TmuxAction,
    result: &v1::TmuxActionResult,
    snapshot: &tmux_control::TmuxSnapshot,
) -> Option<String> {
    let window_id = match kind {
        v1::TmuxActionKind::CreateSession => {
            return snapshot
                .panes
                .iter()
                .find(|pane| pane.session_id == result.session_id && pane.active)
                .map(|pane| pane.id.clone());
        }
        v1::TmuxActionKind::CreateWindow => &result.window_id,
        v1::TmuxActionKind::SelectWindow => &action.window_id,
        v1::TmuxActionKind::SplitPaneRight | v1::TmuxActionKind::SplitPaneDown => {
            return (!result.pane_id.is_empty()).then(|| result.pane_id.clone());
        }
        _ => return None,
    };
    snapshot
        .panes
        .iter()
        .find(|pane| pane.window_id == *window_id && pane.active)
        .map(|pane| pane.id.clone())
}

fn finalize_action(
    kind: v1::TmuxActionKind,
    discover: impl FnOnce() -> anyhow::Result<(tmux_control::TmuxSnapshot, String)>,
    current_identity: impl FnOnce() -> String,
) -> anyhow::Result<(tmux_control::TmuxSnapshot, String)> {
    // Epoch capture only suppresses a duplicate reconciliation. A full or
    // stalled writer must not prevent the authoritative postcheck after tmux
    // has already accepted the mutation; simply leave the dirty notification
    // unacknowledged and let the actor reconcile it normally.
    let (snapshot, identity) = normalize_post_action(kind, discover(), current_identity).map_err(
        |error| {
            anyhow::anyhow!(
                "outcome unknown: tmux accepted the action but post-action discovery failed: {error}"
            )
        },
    )?;
    Ok((snapshot, identity))
}

/// `discover_consistent` refuses when no tmux server is running, and every
/// action used to pass through it first — including `new-session`, the one
/// command that *starts* a server. On a machine that has never run tmux the app
/// was therefore unusable: creating the first session was gated behind a server
/// already existing, and the only thing a user saw was
/// `tmux_action_rejected: tmux server is unavailable`, with no way forward and
/// nothing in the message suggesting one (M10-E060). Creating a session is the
/// single legitimate bootstrap, so it — and only it — may start from no server,
/// evaluated against an empty snapshot.
fn bootstraps_server(kind: v1::TmuxActionKind, current_identity: &str) -> bool {
    kind == v1::TmuxActionKind::CreateSession && current_identity == "tmux:none"
}

/// A server identity change normally means the action's outcome is unknowable,
/// so it is refused. Two transitions are legitimate: closing the last object
/// ends the server, and the bootstrap creation starts one. The bootstrap arm
/// still demands a real resulting identity, so a `new-session` that failed to
/// bring a server up cannot be mistaken for success.
fn identity_transition_allowed(
    kind: v1::TmuxActionKind,
    bootstrapping: bool,
    before_identity: &str,
    after_identity: &str,
) -> bool {
    // The bootstrap arm is tested first on purpose. It starts from "tmux:none",
    // so an unchanged identity here means `new-session` did not bring a server
    // up — the one case where "identity preserved" would wrongly read as success.
    if bootstrapping {
        return after_identity != "tmux:none";
    }
    if after_identity == before_identity {
        return true;
    }
    matches!(
        kind,
        v1::TmuxActionKind::CloseSession
            | v1::TmuxActionKind::CloseWindow
            | v1::TmuxActionKind::ClosePane
    ) && after_identity == "tmux:none"
}

fn action_postcondition(
    kind: v1::TmuxActionKind,
    action: &v1::TmuxAction,
    result: &v1::TmuxActionResult,
    before: &tmux_control::TmuxSnapshot,
    after: &tmux_control::TmuxSnapshot,
) -> bool {
    let session = |id: &str| after.sessions.iter().find(|item| item.id == id);
    let window = |id: &str| after.windows.iter().find(|item| item.id == id);
    let pane = |id: &str| after.panes.iter().find(|item| item.id == id);
    match kind {
        v1::TmuxActionKind::CreateSession => session(&result.session_id).is_some_and(|item| {
            (action.name.is_empty() || item.name == action.name) && (!action.pinned || item.pinned)
        }),
        v1::TmuxActionKind::RenameSession => {
            session(&action.session_id).is_some_and(|item| item.name == action.name)
        }
        v1::TmuxActionKind::ReorderSession => {
            let expected = action
                .index
                .min(u32::try_from(after.sessions.len().saturating_sub(1)).unwrap_or(u32::MAX));
            session(&action.session_id).is_some_and(|item| item.order == expected)
        }
        // Read back through the same overlay the app reads: the post-action
        // discovery re-applies the sidecar, so this checks the pin the way the
        // user will see it rather than the write that was just made.
        v1::TmuxActionKind::SetPinned => {
            if action.window_id.is_empty() {
                session(&action.session_id).is_some_and(|item| item.pinned == action.pinned)
            } else {
                window(&action.window_id).is_some_and(|item| {
                    item.session_id == action.session_id && item.pinned == action.pinned
                })
            }
        }
        v1::TmuxActionKind::SelectSession => session(&action.session_id).is_some(),
        v1::TmuxActionKind::CloseSession => session(&action.session_id).is_none(),
        v1::TmuxActionKind::CreateWindow => window(&result.window_id).is_some_and(|item| {
            item.session_id == action.session_id
                && item.active
                && (action.name.is_empty() || item.name == action.name)
        }),
        v1::TmuxActionKind::RenameWindow => {
            window(&action.window_id).is_some_and(|item| item.name == action.name)
        }
        v1::TmuxActionKind::ReorderWindow => window_reorder_postcondition(
            after,
            &action.session_id,
            &action.window_id,
            &action.target_window_id,
            v1::WindowRelativePosition::try_from(action.relative_position).unwrap_or_default(),
        ),
        v1::TmuxActionKind::SelectWindow => after
            .windows
            .iter()
            .any(|item| item.id == action.window_id && item.active),
        v1::TmuxActionKind::CloseWindow => window(&action.window_id).is_none(),
        v1::TmuxActionKind::SplitPaneRight | v1::TmuxActionKind::SplitPaneDown => {
            let Some(source_before) = before.panes.iter().find(|item| item.id == action.pane_id)
            else {
                return false;
            };
            let Some(source_after) = pane(&action.pane_id) else {
                return false;
            };
            let Some(created) = pane(&result.pane_id) else {
                return false;
            };
            source_before.window_id == source_after.window_id
                && created.window_id == source_before.window_id
                && created.active
                && if kind == v1::TmuxActionKind::SplitPaneRight {
                    created.left > source_after.left
                } else {
                    created.top > source_after.top
                }
        }
        v1::TmuxActionKind::FocusPane => pane(&action.pane_id).is_some_and(|item| item.active),
        v1::TmuxActionKind::ResizePaneLeft
        | v1::TmuxActionKind::ResizePaneRight
        | v1::TmuxActionKind::ResizePaneUp
        | v1::TmuxActionKind::ResizePaneDown => {
            let Some(before_pane) = before.panes.iter().find(|item| item.id == action.pane_id)
            else {
                return false;
            };
            pane(&action.pane_id).is_some_and(|item| {
                item.window_id == before_pane.window_id
                    && (item.left, item.top, item.width, item.height)
                        != (
                            before_pane.left,
                            before_pane.top,
                            before_pane.width,
                            before_pane.height,
                        )
            })
        }
        v1::TmuxActionKind::ZoomPane => {
            let Some(target) = pane(&action.pane_id) else {
                return false;
            };
            window(&target.window_id).is_some_and(|item| item.zoomed == action.zoomed)
        }
        v1::TmuxActionKind::ClosePane => pane(&action.pane_id).is_none(),
        v1::TmuxActionKind::Unspecified => false,
    }
}

/// `current_identity` is resolved lazily: it only matters when discovery
/// failed, and probing it eagerly would put an extra tmux fork on the hot path
/// of every successful action.
fn normalize_post_action(
    kind: v1::TmuxActionKind,
    discovered: anyhow::Result<(tmux_control::TmuxSnapshot, String)>,
    current_identity: impl FnOnce() -> String,
) -> anyhow::Result<(tmux_control::TmuxSnapshot, String)> {
    match discovered {
        Ok(value) => Ok(value),
        Err(error) => {
            if matches!(
                kind,
                v1::TmuxActionKind::CloseSession
                    | v1::TmuxActionKind::CloseWindow
                    | v1::TmuxActionKind::ClosePane
            ) && current_identity() == "tmux:none"
            {
                Ok((tmux_control::TmuxSnapshot::default(), "tmux:none".into()))
            } else {
                Err(error)
            }
        }
    }
}

pub(super) fn discover_before_action() -> anyhow::Result<(tmux_control::TmuxSnapshot, String)> {
    discover_consistent()
}

/// The dispatcher resolves a baseline before it reaches [`execute`], so the
/// bootstrap exemption has to be honoured here as well; otherwise the request
/// is rejected as `tmux_action_rejected: tmux server is unavailable` and the
/// exemption inside `execute` is unreachable. That is exactly how the first
/// attempt at M10-E060 failed: the rule was correct and never ran. Background
/// reconciliation keeps using [`discover_before_action`], which stays strict,
/// because it has no action to be a bootstrap for.
pub(super) fn discover_for_action(
    kind: v1::TmuxActionKind,
) -> anyhow::Result<(tmux_control::TmuxSnapshot, String)> {
    tmux_control::tmux_executable().context("locate tmux executable")?;
    normalize_pre_action_discovery(kind, discover_consistent(), server_identity)
}

fn normalize_pre_action_discovery(
    kind: v1::TmuxActionKind,
    discovered: anyhow::Result<(tmux_control::TmuxSnapshot, String)>,
    current_identity: impl FnOnce() -> String,
) -> anyhow::Result<(tmux_control::TmuxSnapshot, String)> {
    match discovered {
        Ok(discovered) => Ok(discovered),
        Err(_)
            if kind == v1::TmuxActionKind::CreateSession && current_identity() == "tmux:none" =>
        {
            Ok((
                tmux_control::TmuxSnapshot::default(),
                "tmux:none".to_owned(),
            ))
        }
        Err(error) => Err(error),
    }
}

fn require_confirmation(kind: v1::TmuxActionKind, confirmed: bool) -> anyhow::Result<()> {
    if matches!(
        kind,
        v1::TmuxActionKind::CloseSession
            | v1::TmuxActionKind::CloseWindow
            | v1::TmuxActionKind::ClosePane
    ) && !confirmed
    {
        bail!("destructive tmux action requires confirmation");
    }
    Ok(())
}

fn validate_targets(
    kind: v1::TmuxActionKind,
    action: &v1::TmuxAction,
    snapshot: &tmux_control::TmuxSnapshot,
) -> anyhow::Result<()> {
    let session_required = matches!(
        kind,
        v1::TmuxActionKind::RenameSession
            | v1::TmuxActionKind::ReorderSession
            | v1::TmuxActionKind::SelectSession
            | v1::TmuxActionKind::CloseSession
            | v1::TmuxActionKind::CreateWindow
            | v1::TmuxActionKind::ReorderWindow
            | v1::TmuxActionKind::SetPinned
    );
    if session_required {
        validate_tmux_id(&action.session_id, '$')?;
        if !snapshot
            .sessions
            .iter()
            .any(|item| item.id == action.session_id)
        {
            bail!("session no longer exists");
        }
    }
    let window_required = matches!(
        kind,
        v1::TmuxActionKind::RenameWindow
            | v1::TmuxActionKind::ReorderWindow
            | v1::TmuxActionKind::SelectWindow
            | v1::TmuxActionKind::CloseWindow
    );
    if window_required {
        validate_tmux_id(&action.window_id, '@')?;
        if !snapshot
            .windows
            .iter()
            .any(|item| item.id == action.window_id)
        {
            bail!("window no longer exists");
        }
    }
    // A tab pin names a window inside the session; a workspace pin names none.
    if kind == v1::TmuxActionKind::SetPinned && !action.window_id.is_empty() {
        validate_tmux_id(&action.window_id, '@')?;
        if !snapshot
            .windows
            .iter()
            .any(|item| item.id == action.window_id && item.session_id == action.session_id)
        {
            bail!("window is not linked to the requested session");
        }
    }
    if kind == v1::TmuxActionKind::ReorderWindow {
        validate_tmux_id(&action.target_window_id, '@')?;
        if action.target_window_id == action.window_id {
            bail!("window reorder target must be a different window");
        }
        if !snapshot
            .windows
            .iter()
            .any(|item| item.id == action.target_window_id && item.session_id == action.session_id)
        {
            bail!("window reorder target is not linked to the requested session");
        }
        if v1::WindowRelativePosition::try_from(action.relative_position).unwrap_or_default()
            == v1::WindowRelativePosition::Unspecified
        {
            bail!("window reorder requires an explicit before/after position");
        }
    }
    let pane_required = matches!(
        kind,
        v1::TmuxActionKind::SplitPaneRight
            | v1::TmuxActionKind::SplitPaneDown
            | v1::TmuxActionKind::FocusPane
            | v1::TmuxActionKind::ResizePaneLeft
            | v1::TmuxActionKind::ResizePaneRight
            | v1::TmuxActionKind::ResizePaneUp
            | v1::TmuxActionKind::ResizePaneDown
            | v1::TmuxActionKind::ZoomPane
            | v1::TmuxActionKind::ClosePane
    );
    if pane_required {
        validate_tmux_id(&action.pane_id, '%')?;
        if !snapshot.panes.iter().any(|item| item.id == action.pane_id) {
            bail!("pane no longer exists");
        }
    }
    Ok(())
}

fn reorder_window(
    snapshot: &tmux_control::TmuxSnapshot,
    session_id: &str,
    window_id: &str,
    target_window_id: &str,
    relative_position: v1::WindowRelativePosition,
) -> anyhow::Result<()> {
    let mut windows: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == session_id)
        .collect();
    windows.sort_by_key(|window| window.index);
    let current = windows
        .iter()
        .position(|window| window.id == window_id)
        .context("window is not linked to the requested session")?;
    let target_before_removal = windows
        .iter()
        .position(|window| window.id == target_window_id)
        .context("target window is not linked to the requested session")?;
    let target = relative_reorder_position(
        current,
        target_before_removal,
        windows.len(),
        relative_position,
    )?;
    let adjacent_ids: Vec<_> = if current < target {
        windows[current + 1..=target]
            .iter()
            .map(|window| window.id.as_str())
            .collect()
    } else {
        windows[target..current]
            .iter()
            .rev()
            .map(|window| window.id.as_str())
            .collect()
    };
    if !adjacent_ids.is_empty() {
        run(window_reorder_command(window_id, &adjacent_ids)?)?;
    }
    Ok(())
}

fn window_reorder_command(
    window_id: &str,
    adjacent_ids: &[&str],
) -> anyhow::Result<std::process::Command> {
    let mut command = tmux_command()?;
    if let Some((first, rest)) = adjacent_ids.split_first() {
        command.args(["swap-window", "-d", "-s", window_id, "-t", first]);
        for adjacent in rest {
            command.arg(";");
            command.args(["swap-window", "-d", "-s", window_id, "-t", adjacent]);
        }
    }
    Ok(command)
}

fn window_reorder_postcondition(
    snapshot: &tmux_control::TmuxSnapshot,
    session_id: &str,
    window_id: &str,
    target_window_id: &str,
    position: v1::WindowRelativePosition,
) -> bool {
    let mut windows: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == session_id)
        .collect();
    windows.sort_by_key(|window| window.index);
    let Some(source) = windows.iter().position(|window| window.id == window_id) else {
        return false;
    };
    let Some(target) = windows
        .iter()
        .position(|window| window.id == target_window_id)
    else {
        return false;
    };
    match position {
        v1::WindowRelativePosition::Before => source.checked_add(1) == Some(target),
        v1::WindowRelativePosition::After => target.checked_add(1) == Some(source),
        v1::WindowRelativePosition::Unspecified => false,
    }
}

fn relative_reorder_position(
    current: usize,
    target_before_removal: usize,
    length: usize,
    relative_position: v1::WindowRelativePosition,
) -> anyhow::Result<usize> {
    let mut target = match relative_position {
        v1::WindowRelativePosition::Before => target_before_removal,
        v1::WindowRelativePosition::After => target_before_removal + 1,
        v1::WindowRelativePosition::Unspecified => {
            bail!("window reorder requires an explicit before/after position")
        }
    };
    if current < target {
        target -= 1;
    }
    Ok(target.min(length.saturating_sub(1)))
}

#[cfg(test)]
#[path = "tmux_actions_tests.rs"]
mod tests;
