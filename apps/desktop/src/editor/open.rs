//! How a file gets into a tab: from the Explorer under its root, from a path
//! a terminal printed (resolved by the host for that pane), and from Quick
//! Open's file search (the search itself is `files::search_files`). Port of
//! `app/useTerminalFileOpen.ts` and `openExplorerEntry` in `App.tsx`; the
//! route check is `files::wire::current_terminal_file_pane`.

use std::time::Duration;

use desktop_core::app_state::DockSide;
use futures::{FutureExt, future::BoxFuture};
use gpui_kit::{App, AsyncApp};
use serde_json::{Value, json};
use tmux_control::Pane;

use super::{
    Root, report,
    tabs::{OpenOptions, kind_for_path, open_file_tab},
};
use crate::{
    connection_ui::{
        ConnectionUi, connection_reducer::ConnectionPhase, host_scope::same_host_connection,
    },
    files::wire::{current_terminal_file_pane, decode_terminal_file},
    workspaces::{self, model::dock_app_tab},
};

/// How often a host answer is looked for.
const POLL: Duration = Duration::from_millis(20);

/// A file to open in the workspace on screen.
#[derive(Clone, Debug)]
pub struct OpenFile {
    pub session_id: String,
    pub path: String,
    pub root: Root,
    /// A single click: the workspace's one disposable tab.
    pub preview: bool,
    /// The root came with this open and replaces an open tab's.
    pub refresh_root: bool,
    /// Beside the terminal, on this edge, instead of over it.
    pub dock: Option<DockSide>,
}

/// Opens a file in a tab of the active host's workspace and shows it.
pub fn open_file(request: OpenFile, cx: &mut App) {
    let Some(workspaces) = workspaces::current(cx) else {
        return;
    };
    let view_mode = crate::settings::get(cx).default_markdown_view;
    workspaces.update(cx, |this, cx| {
        let view = this.view(cx);
        let Some(server) = view.server.clone() else {
            return;
        };
        let Some(session) = view
            .snapshot
            .sessions
            .iter()
            .find(|session| session.id == request.session_id)
            .cloned()
        else {
            report(
                format!(
                    "Could not open {}: its session is no longer attached.",
                    request.path
                ),
                cx,
            );
            return;
        };
        let host = view.host.clone();
        let mut opened = None;
        this.update_app_state(
            |state| {
                let id = open_file_tab(
                    state,
                    &host,
                    &server,
                    &session,
                    &request.path,
                    kind_for_path(&request.path),
                    (&request.root.path, &request.root.token),
                    OpenOptions {
                        preview: request.preview,
                        refresh_root: request.refresh_root,
                        view_mode: Some(view_mode),
                    },
                );
                if let Some(side) = request.dock {
                    dock_app_tab(state, &host, &server, &session, &id, side);
                }
                opened = Some(id);
                true
            },
            cx,
        );
        // Through the shell, so a window move still in flight cannot take
        // the document back off the screen when it lands.
        if let (Some(id), None) = (opened, request.dock) {
            this.select_app_tab(&session.id, Some(&id), cx);
        }
    });
}

/// The live connection of the host on screen, and the pane a request names.
struct Captured {
    client_id: String,
    scope: crate::connection_ui::host_scope::HostScopeToken,
    server: String,
    pane: Pane,
}

/// The live link of the host on screen.
fn live_link(
    cx: &App,
) -> Option<(
    crate::workspaces::ShellView,
    crate::connection_ui::host_links::HostLink,
)> {
    let view = workspaces::current(cx)?.read(cx).view(cx);
    let link = cx
        .try_global::<ConnectionUi>()?
        .0
        .read(cx)
        .link(&view.host)?
        .clone();
    let state = &link.host_state;
    (state.phase == ConnectionPhase::Connected && link.terminal_epoch != 0).then_some((view, link))
}

fn capture(pane_id: Option<&str>, cx: &App) -> Option<Captured> {
    let (view, link) = live_link(cx)?;
    let state = &link.host_state;
    let pane = match pane_id {
        Some(id) => view
            .snapshot
            .panes
            .iter()
            .find(|pane| pane.id == id)?
            .clone(),
        None => view.active_pane.clone()?,
    };
    Some(Captured {
        client_id: link.client_id.clone()?,
        scope: link.scope(),
        server: state.server_identity.clone()?,
        pane,
    })
}

/// Whether the connection a request was captured on is still the live one.
/// The pane itself is checked after, so a pane that went away says so.
fn same_connection(captured: &Captured, cx: &App) -> bool {
    live_link(cx).is_some_and(|(_, link)| {
        link.client_id.as_deref() == Some(captured.client_id.as_str())
            && same_host_connection(&link.scope(), &captured.scope)
    })
}

fn request(
    captured: &Captured,
    command: Value,
    cx: &App,
) -> BoxFuture<'static, Result<Value, String>> {
    let Some(ui) = cx.try_global::<ConnectionUi>() else {
        return futures::future::ready(Err("The file host is disconnected.".to_owned())).boxed();
    };
    ui.0.read(cx)
        .backend()
        .file_request(captured.client_id.clone(), command)
}

/// The answer, looked for on a timer: the host's thread never wakes the app.
async fn settle(
    mut answer: BoxFuture<'static, Result<Value, String>>,
    cx: &mut AsyncApp,
) -> Result<Value, String> {
    loop {
        if let Some(result) = (&mut answer).now_or_never() {
            return result;
        }
        cx.background_executor().timer(POLL).await;
    }
}

fn pane_fields(captured: &Captured) -> Value {
    json!({
        "paneId": captured.pane.id,
        "expectedServerIdentity": captured.server,
        "expectedTopologyGeneration": captured.scope.generation.to_string(),
        "expectedSessionId": captured.pane.session_id,
        "expectedWindowId": captured.pane.window_id,
        "expectedCwd": captured.pane.current_path,
    })
}

fn with(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

/// Resolves a path a terminal pane printed and opens it, over the terminal or
/// docked at `dock`.
pub fn open_terminal_path(pane_id: &str, candidate: &str, dock: Option<DockSide>, cx: &mut App) {
    let Some(captured) = capture(Some(pane_id), cx) else {
        report(
            "Reconnect the terminal before opening a file path.".into(),
            cx,
        );
        return;
    };
    let command = with(
        pane_fields(&captured),
        json!({
            "operation": "resolveTerminalFile",
            "operationId": uuid::Uuid::new_v4().to_string(),
            "path": candidate,
        }),
    );
    let answer = request(&captured, command, cx);
    let candidate = candidate.to_owned();
    cx.spawn(async move |cx| {
        let result = settle(answer, cx).await;
        cx.update(|cx| {
            if !same_connection(&captured, cx) {
                return;
            }
            let resolved = match result
                .and_then(|value| decode_terminal_file(value, &captured.server, &captured.pane.id))
            {
                Ok(resolved) => resolved,
                Err(error) => {
                    report(format!("Could not open {candidate}: {error}"), cx);
                    return;
                }
            };
            let Some(view) = workspaces::current(cx).map(|w| w.read(cx).view(cx)) else {
                return;
            };
            let Some(pane) = current_terminal_file_pane(
                &captured.pane,
                &view.snapshot.panes,
                captured.scope.generation,
                resolved.topology_generation,
            ) else {
                report(
                    format!(
                        "Could not open {candidate}: that pane moved while the path was resolving."
                    ),
                    cx,
                );
                return;
            };
            open_file(
                OpenFile {
                    session_id: pane.session_id.clone(),
                    path: resolved.path,
                    root: Root {
                        path: resolved.root.path,
                        token: resolved.root.token,
                    },
                    preview: false,
                    refresh_root: true,
                    dock,
                },
                cx,
            );
        });
    })
    .detach();
}

/// Opens a Quick Open file match, resolved for the active pane as a terminal
/// path is.
pub fn open_search_match(path: &str, cx: &mut App) {
    match capture(None, cx) {
        Some(captured) => open_terminal_path(&captured.pane.id, path, None, cx),
        None => report(
            "Reconnect the terminal before opening a file path.".into(),
            cx,
        ),
    }
}
