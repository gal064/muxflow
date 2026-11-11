//! How a file gets into a tab: from the Explorer under its root, from a path
//! a terminal printed (resolved by the host for that pane), and from Quick
//! Open's file search. Port of `app/useTerminalFileOpen.ts`,
//! `app/terminalFileOpenRoute.ts`, `openExplorerEntry` in `App.tsx` and the
//! search half of `features/workspaces/useQuickOpenSearch.ts`.

use std::time::Duration;

use desktop_core::app_state::DockSide;
use futures::{FutureExt, future::BoxFuture};
use gpui_kit::{App, AsyncApp, Task};
use serde_json::{Value, json};
use tmux_control::Pane;

use super::{
    Root, report,
    tabs::{OpenOptions, kind_for_path, open_file_tab},
};
use crate::{
    commands::quick_open::{FileMatch, FileSearchResults},
    connection_ui::{
        ConnectionUi, connection_reducer::ConnectionPhase, host_scope::same_host_connection,
    },
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

/// A terminal path the host resolved for one pane.
#[derive(Debug, PartialEq)]
struct Resolved {
    path: String,
    root: Root,
    topology_generation: u64,
}

fn decode_resolved(value: &Value, server: &str, pane_id: &str) -> Result<Resolved, String> {
    let root = value.get("activeRoot").filter(|root| root.is_object());
    let path = value
        .pointer("/metadata/path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty());
    let (Some(root), Some(path)) = (root, path) else {
        return Err("Host omitted the resolved terminal file.".into());
    };
    let text = |key: &str| root.get(key).and_then(Value::as_str).unwrap_or_default();
    if text("serverIdentity") != server || text("paneId") != pane_id {
        return Err("Host resolved the terminal file for a stale pane scope.".into());
    }
    let generation = text("topologyGeneration");
    let topology_generation = generation
        .parse::<u64>()
        .ok()
        .filter(|_| generation == "0" || !generation.starts_with('0'))
        .ok_or("Host returned a malformed terminal file topology generation.")?;
    Ok(Resolved {
        path: path.to_owned(),
        root: Root {
            path: text("root").to_owned(),
            token: text("rootToken").to_owned(),
        },
        topology_generation,
    })
}

/// The live pane, only while the answer still describes the route that asked.
/// The generation is compared against the one the request carried, not the
/// latest: unrelated topology on a busy pane advances it without moving this
/// pane, and the route is what decides.
fn current_pane<'a>(
    captured: &Pane,
    live: &'a [Pane],
    requested: u64,
    resolved: u64,
) -> Option<&'a Pane> {
    if resolved < requested {
        return None;
    }
    live.iter().find(|pane| {
        pane.id == captured.id
            && pane.session_id == captured.session_id
            && pane.window_id == captured.window_id
            && pane.current_path == captured.current_path
    })
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
                .and_then(|value| decode_resolved(&value, &captured.server, &captured.pane.id))
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
            let Some(pane) = current_pane(
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
                    root: resolved.root,
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

/// The most results the host returns for one search.
const SEARCH_LIMIT: usize = 75;

fn decode_search(value: &Value, cwd: &str) -> Result<FileSearchResults, String> {
    let malformed = || "Host returned malformed file search results.".to_owned();
    let search = value.get("search").ok_or_else(malformed)?;
    let matches = search
        .get("matches")
        .and_then(Value::as_array)
        .filter(|matches| matches.len() <= SEARCH_LIMIT)
        .ok_or_else(malformed)?;
    let complete = search
        .get("complete")
        .and_then(Value::as_bool)
        .ok_or_else(malformed)?;
    let prefix = format!("{}/", cwd.trim_end_matches('/'));
    let outside = || "Host returned a file search match outside the captured directory.".to_owned();
    let matches = matches
        .iter()
        .map(|item| {
            let relative = item
                .get("relativePath")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let path = item.get("path").and_then(Value::as_str).unwrap_or_default();
            let score = item.get("score").and_then(Value::as_i64);
            let sane = !relative.is_empty()
                && !relative.starts_with('/')
                && relative
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")
                && path == format!("{prefix}{relative}");
            match (sane, score) {
                (true, Some(score)) => Ok(FileMatch {
                    path: path.to_owned(),
                    relative_path: relative.to_owned(),
                    score,
                }),
                _ => Err(outside()),
            }
        })
        .collect::<Result<_, _>>()?;
    Ok(FileSearchResults { matches, complete })
}

/// Quick Open's file search under the active pane's directory. The pane's
/// root is resolved first; the host then answers for that pane's route.
/// What Quick Open searches: the active pane's folder on the live
/// connection. `None` when there is nothing to search. Port of the key in
/// `useQuickOpenSearch.ts`.
pub fn search_key(cx: &App) -> Option<String> {
    let captured = capture(None, cx)?;
    let scope = &captured.scope;
    Some(
        [
            captured.client_id.as_str(),
            &scope.host_profile_id,
            &captured.server,
            &scope.connection_epoch.to_string(),
            &captured.pane.session_id,
            &captured.pane.id,
            &captured.pane.window_id,
            &captured.pane.current_path,
        ]
        .join("\0"),
    )
}

/// The root resolved for the last search key, so typing in Quick Open costs one
/// request per query rather than two.
#[derive(Default)]
struct SearchRoot(Option<(String, Root)>);

impl gpui_kit::Global for SearchRoot {}

pub fn search_files(query: String, cx: &mut App) -> Task<Result<FileSearchResults, String>> {
    let (Some(captured), Some(key)) = (capture(None, cx), search_key(cx)) else {
        return Task::ready(Ok(FileSearchResults {
            matches: Vec::new(),
            complete: true,
        }));
    };
    let known = cx
        .try_global::<SearchRoot>()
        .and_then(|cached| cached.0.clone())
        .filter(|(cached, _)| *cached == key)
        .map(|(_, root)| root);
    let resolve = known.is_none().then(|| {
        request(
            &captured,
            with(
                pane_fields(&captured),
                json!({
                    "operation": "resolveActiveRoot",
                    "operationId": uuid::Uuid::new_v4().to_string(),
                    "knownRootToken": "",
                }),
            ),
            cx,
        )
    });
    cx.spawn(async move |cx| {
        let root = match (known, resolve) {
            (Some(root), _) => root,
            (None, resolve) => {
                let answer = settle(resolve.expect("a resolve without a known root"), cx).await?;
                let root = answer
                    .get("activeRoot")
                    .filter(|root| root.is_object())
                    .ok_or("Host omitted the active root.")?;
                let text = |key: &str| {
                    root.get(key)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let root = Root {
                    path: text("root"),
                    token: text("rootToken"),
                };
                let cached = (key.clone(), root.clone());
                cx.update(|cx| cx.set_global(SearchRoot(Some(cached))));
                root
            }
        };
        let search = with(
            pane_fields(&captured),
            json!({
                "operation": "searchFiles",
                "operationId": uuid::Uuid::new_v4().to_string(),
                "searchQuery": query,
                "root": root.path,
                "rootToken": root.token,
            }),
        );
        let answer = cx.update(|cx| request(&captured, search, cx));
        let found = settle(answer, cx).await;
        let found = found.and_then(|found| decode_search(&found, &captured.pane.current_path));
        if found.is_err() {
            // A root that fails a search is resolved again next time.
            cx.update(|cx| cx.set_global(SearchRoot(None)));
        }
        found
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::model::fixtures;

    #[test]
    fn decodes_a_terminal_file_resolution_for_its_own_pane() {
        let answer = json!({
            "activeRoot": {
                "paneId": "%1", "root": "/repo", "rootToken": "t", "serverIdentity": "server-a",
                "topologyGeneration": "7", "rootGeneration": "1", "gitWorktree": false,
            },
            "metadata": { "path": "/repo/src/main.rs" },
        });
        assert_eq!(
            decode_resolved(&answer, "server-a", "%1"),
            Ok(Resolved {
                path: "/repo/src/main.rs".into(),
                root: Root {
                    path: "/repo".into(),
                    token: "t".into()
                },
                topology_generation: 7,
            })
        );
        assert_eq!(
            decode_resolved(&answer, "server-a", "%2").unwrap_err(),
            "Host resolved the terminal file for a stale pane scope."
        );
        assert_eq!(
            decode_resolved(
                &json!({ "activeRoot": answer["activeRoot"] }),
                "server-a",
                "%1"
            )
            .unwrap_err(),
            "Host omitted the resolved terminal file."
        );
        let mut malformed = answer.clone();
        malformed["activeRoot"]["topologyGeneration"] = json!("07");
        assert_eq!(
            decode_resolved(&malformed, "server-a", "%1").unwrap_err(),
            "Host returned a malformed terminal file topology generation."
        );
    }

    #[test]
    fn opens_only_while_the_pane_route_that_asked_still_holds() {
        let mut pane = fixtures::pane("%1", "@1", "$1", (0, 0, 80, 24));
        pane.current_path = "/repo".into();
        let panes = vec![pane.clone()];
        // A later unrelated generation still opens.
        assert!(current_pane(&pane, &panes, 5, 9).is_some());
        // An answer older than the request does not.
        assert!(current_pane(&pane, &panes, 5, 4).is_none());
        let mut moved = pane.clone();
        moved.window_id = "@2".into();
        assert!(current_pane(&pane, &[moved], 5, 5).is_none());
        let mut elsewhere = pane.clone();
        elsewhere.current_path = "/other".into();
        assert!(current_pane(&pane, &[elsewhere], 5, 5).is_none());
        // Closed while the path resolved.
        assert!(current_pane(&pane, &[], 5, 5).is_none());
    }

    #[test]
    fn keeps_search_matches_inside_the_captured_directory() {
        let found = json!({ "search": { "complete": true, "matches": [
            { "path": "/repo/src/a.rs", "relativePath": "src/a.rs", "score": 3 },
        ] } });
        let results = decode_search(&found, "/repo/").unwrap();
        assert!(results.complete);
        assert_eq!(results.matches[0].relative_path, "src/a.rs");
        let escaping = json!({ "search": { "complete": true, "matches": [
            { "path": "/etc/passwd", "relativePath": "../etc/passwd", "score": 3 },
        ] } });
        assert_eq!(
            decode_search(&escaping, "/repo").unwrap_err(),
            "Host returned a file search match outside the captured directory."
        );
        let too_many = json!({ "search": { "complete": false,
            "matches": vec![json!({}); SEARCH_LIMIT + 1] } });
        assert_eq!(
            decode_search(&too_many, "/repo").unwrap_err(),
            "Host returned malformed file search results."
        );
    }
}
