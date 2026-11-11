//! The file explorer: the active pane's root as a tree, kept current by host
//! watches, with the file actions and downloads it offers, as the right
//! panel's "files" segment. Port of `features/files` (less the editor) and the
//! file half of `useWorkspaceDomainController`.
//!
//! Seams for the rest of the app:
//! - Files open through [`crate::editor::open_file`]; the editor's watches and
//!   Download button are served from here (`start`).
//! - [`scope`]: the live file scope.
//! - [`follow_git_status`], [`set_ignored_paths`] and [`ignored_paths`]: what
//!   git says the tree may hide.
//! - [`search_key`] and [`search_files`]: Quick Open's file search.
//! - [`download`] and [`download_app_tab`]: Download… from anywhere.
//! - The [`crate::agents::view::ActiveRoot`] global follows the Explorer root.

pub mod client;
pub mod dialog;
pub mod downloads;
pub mod explorer;
pub mod icons;
pub mod listing;
pub mod view;
mod watches;
pub mod wire;

#[cfg(test)]
mod tests;

use std::{collections::HashSet, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use desktop_core::app_state::{AppTabKind, DockSide, PersistedAppState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, Global, IntoElement, ParentElement, Styled, Task,
    WeakEntity, Window,
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        notification::{Notification, NotificationType},
        v_flex,
    },
    div,
};

use crate::{
    agents::view::ActiveRoot as AgentRoot,
    commands::quick_open::FileSearchResults,
    connection_ui::{
        connection_reducer::ConnectionPhase,
        controller::{ConnectionController, ConnectionEvent},
    },
    git::model::StatusSnapshot,
    terminal::integration::transfers::InternalPathDrag,
    theme::metrics::ui,
    ui::{chrome::PanelBodies, shell::Shell},
};

use client::{FileTransport, operation_id};
use downloads::{DownloadIntent, DownloadKind, reconcile_status, reveal_label, suggested_name};
use explorer::{DownloadOrigin, Explorer, ExplorerEvent, Inputs};
use wire::{ActiveRoot, FileScope, FileSelection, PaneRoute, command};

/// How often the save panel's answer is checked for. Polled, not woken: the
/// portal answers on another thread.
const PICKER_POLL: Duration = Duration::from_millis(50);

pub struct Files {
    pub explorer: Entity<Explorer>,
    controller: WeakEntity<ConnectionController>,
}

impl Global for Files {}

/// What git says the tree may hide, or `None` for "show everything", and the
/// status it was derived from.
#[derive(Default)]
struct IgnoredPaths {
    paths: Option<Rc<HashSet<String>>>,
    status: Option<Rc<StatusSnapshot>>,
}

impl Global for IgnoredPaths {}

/// The finished download the status notice is announcing.
struct Completion {
    message: String,
    destination: String,
}

impl Global for Completion {}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    PanelBodies::register("files", body, cx);
    // The root backstop probes only while the window can be seen.
    cx.observe_new::<Shell>(|_, window, cx| {
        let Some(window) = window else {
            return;
        };
        cx.observe_window_activation(window, |_, window, cx| follow_window(window, cx))
            .detach();
        cx.observe_window_visibility(window, |_, _, window, cx| follow_window(window, cx))
            .detach();
        follow_window(window, cx);
    })
    .detach();
}

fn follow_window(window: &Window, cx: &mut App) {
    let foreground = window.is_visible() || window.is_window_active();
    if let Some(explorer) = cx.try_global::<Files>().map(|files| files.explorer.clone()) {
        explorer.update(cx, |explorer, cx| explorer.set_foreground(foreground, cx));
    }
}

fn body(window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(explorer) = cx.try_global::<Files>().map(|files| files.explorer.clone()) else {
        return div().into_any_element();
    };
    // Lives exactly as long as the segment is drawn every frame.
    window.use_keyed_state("files-panel-shown", cx, {
        let explorer = explorer.clone();
        move |_, cx| Shown::new(explorer, cx)
    });
    explorer.into_any_element()
}

/// The tree's rows answer the palette only while the segment is on screen,
/// as the Git panel's do.
struct Shown(WeakEntity<Explorer>);

impl Shown {
    fn new(explorer: Entity<Explorer>, cx: &mut gpui_kit::Context<Self>) -> Self {
        explorer.update(cx, |explorer, cx| explorer.set_shown(true, cx));
        cx.on_release(|this, cx| {
            this.0
                .update(cx, |explorer, cx| explorer.set_shown(false, cx))
                .ok();
        })
        .detach();
        Self(explorer.downgrade())
    }
}

/// Called once the connection controller exists, before the first window.
pub fn start(
    controller: Entity<ConnectionController>,
    transport: Rc<dyn FileTransport>,
    cx: &mut App,
) {
    let explorer = cx.new(|cx| Explorer::new(transport, cx));
    cx.set_global(Files {
        explorer: explorer.clone(),
        controller: controller.downgrade(),
    });
    cx.subscribe(&controller, {
        let explorer = explorer.clone();
        move |_, event, cx| {
            let ConnectionEvent::FileService { payload } = event else {
                return;
            };
            let Ok(events) = wire::decode_events(payload) else {
                return;
            };
            explorer.update(cx, |explorer, cx| {
                for event in events {
                    explorer.apply_event(event, cx);
                }
            });
        }
    })
    .detach();
    cx.observe(&controller, {
        let explorer = explorer.clone();
        move |controller, cx| {
            let inputs = inputs(controller.read(cx));
            explorer.update(cx, |explorer, cx| explorer.set_inputs(inputs, cx));
        }
    })
    .detach();
    cx.subscribe(&explorer, {
        let controller = controller.clone();
        move |explorer, event, cx| match event {
            ExplorerEvent::Status(message) => {
                let message = message.clone();
                controller.update(cx, |controller, cx| controller.report_status(message, cx));
            }
            ExplorerEvent::Relocated {
                host_profile_id,
                server_identity,
                root_path,
                source,
                destination,
            } => controller.update(cx, |controller, cx| {
                controller.update_app_state(
                    |state| {
                        relocate_file_tabs(
                            state,
                            host_profile_id,
                            server_identity,
                            root_path,
                            source,
                            destination,
                        )
                    },
                    cx,
                )
            }),
            ExplorerEvent::DownloadChanged => reconcile(&controller, &explorer, cx),
            ExplorerEvent::Notice {
                host,
                root_token,
                path,
                notice,
            } => crate::editor::deliver(host, root_token, path, notice.clone(), cx),
        }
    })
    .detach();
    // Open documents watch through the tree's registry, so a directory both
    // hold costs the host one watch.
    crate::editor::set_watcher(
        Rc::new({
            let explorer = explorer.downgrade();
            move |target, cx| match explorer.upgrade() {
                Some(explorer) => Box::new(
                    explorer.update(cx, |explorer, cx| explorer.watch_document(target, cx)),
                ),
                None => Box::new(()),
            }
        }),
        cx,
    );
    crate::editor::set_download(
        Rc::new({
            let explorer = explorer.downgrade();
            move |tab, root, window, cx| {
                let Some(explorer) = explorer.upgrade() else {
                    return;
                };
                let root = tab_root(&explorer, &root.path, &root.token, cx);
                let intent = DownloadIntent {
                    path: tab.resource.clone(),
                    kind: DownloadKind::File,
                };
                download(
                    explorer,
                    intent,
                    root,
                    DownloadOrigin::FileSurface,
                    window,
                    cx,
                );
            }
        }),
        cx,
    );
    cx.observe(&explorer, |explorer, cx| publish_root(&explorer, cx))
        .detach();
    let initial = inputs(controller.read(cx));
    explorer.update(cx, |explorer, cx| explorer.set_inputs(initial, cx));
}

/// What the Explorer follows: the active host's selected session, window and
/// pane. The bridge publishes a snapshot before it says it is connected, and a
/// request scope never exists in that gap.
pub fn inputs(controller: &ConnectionController) -> Inputs {
    let Some(link) = controller.active_link() else {
        return Inputs::default();
    };
    let state = &link.host_state;
    let snapshot = &state.snapshot;
    let session = link
        .active_session_id
        .as_deref()
        .and_then(|id| snapshot.sessions.iter().find(|session| session.id == id));
    let pane = link.active_window_id.as_deref().and_then(|window| {
        let mut panes = snapshot
            .panes
            .iter()
            .filter(|pane| pane.window_id == window);
        let first = panes.clone().next();
        panes.find(|pane| pane.active).or(first)
    });
    let selection = match (&state.server_identity, session, pane) {
        (Some(server_identity), Some(session), Some(pane)) => Some(FileSelection {
            host_profile_id: link.profile_id.clone(),
            server_identity: server_identity.clone(),
            session_id: session.id.clone(),
            pane_id: pane.id.clone(),
        }),
        _ => None,
    };
    let scope = wire::live_file_scope(
        state.phase == ConnectionPhase::Connected,
        link.client_id.as_deref(),
        link.terminal_epoch,
        state.generation,
        selection.as_ref(),
    );
    Inputs {
        route: pane.map(|pane| PaneRoute {
            session_id: pane.session_id.clone(),
            window_id: pane.window_id.clone(),
            cwd: pane.current_path.clone(),
        }),
        selection,
        scope,
        can_mutate: state.can_mutate,
    }
}

/// Agent launch and resume open under the Explorer's root.
fn publish_root(explorer: &Entity<Explorer>, cx: &mut App) {
    let root = explorer.read(cx).tree.root.clone();
    let held = cx
        .try_global::<AgentRoot>()
        .map(|held| (held.path.clone(), held.token.clone(), held.git_worktree));
    match root {
        Some(root) if held != Some((root.path.clone(), root.token.clone(), root.git_worktree)) => {
            cx.set_global(AgentRoot {
                path: root.path,
                token: root.token,
                git_worktree: root.git_worktree,
            });
        }
        None if held.is_some() => {
            cx.remove_global::<AgentRoot>();
        }
        _ => {}
    }
}

/// Ends the status a download announced once the download ends. Port of the
/// App effect around `reconcileDownloadStatus`.
fn reconcile(controller: &Entity<ConnectionController>, explorer: &Entity<Explorer>, cx: &mut App) {
    let status = controller.read(cx).status().to_owned();
    let reconciled = {
        let explorer = explorer.read(cx);
        reconcile_status(
            &status,
            explorer.active_download.as_ref(),
            &explorer.downloads.items,
        )
    };
    explorer.update(cx, |explorer, _| {
        explorer.active_download = reconciled.active
    });
    if let Some(destination) = reconciled.completion {
        cx.set_global(Completion {
            message: reconciled.status.clone(),
            destination,
        });
    }
    if reconciled.status != status {
        controller.update(cx, |controller, cx| {
            controller.report_status(reconciled.status, cx)
        });
    }
}

/// The completion notice, with Open and the reveal action, when `message` is
/// the one announcing the finished download.
pub fn download_notice(message: &str, cx: &App) -> Option<Notification> {
    let completion = cx
        .try_global::<Completion>()
        .filter(|completion| completion.message == message)?;
    let (message, destination) = (completion.message.clone(), completion.destination.clone());
    Some(
        Notification::new()
            .with_type(NotificationType::Info)
            .content(move |_, _, _| {
                let act = |reveal: bool| {
                    let destination = destination.clone();
                    move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                        let Some(files) = cx.try_global::<Files>() else {
                            return;
                        };
                        let transport = files.explorer.read(cx).transport();
                        let result = if reveal {
                            transport.reveal_download(destination.clone())
                        } else {
                            transport.open_download(destination.clone())
                        };
                        if let (Err(error), Some(controller)) = (result, files.controller.upgrade())
                        {
                            controller
                                .update(cx, |controller, cx| controller.report_status(error, cx));
                        }
                    }
                };
                let reveal = reveal_label();
                v_flex()
                    .gap(ui(6.))
                    .child(div().text_sm().child(message.clone()))
                    .child(
                        h_flex()
                            .gap(ui(6.))
                            .child(
                                Button::new("download-notice-open")
                                    .ghost()
                                    .small()
                                    .label("Open")
                                    .accessibility_label(format!("Open {destination}"))
                                    .on_click(act(false)),
                            )
                            .child(
                                Button::new("download-notice-reveal")
                                    .ghost()
                                    .small()
                                    .label(reveal)
                                    .accessibility_label(format!("{reveal}: {destination}"))
                                    .on_click(act(true)),
                            ),
                    )
                    .into_any_element()
            }),
    )
}

// Seams.

/// The live file scope, while there is one.
pub fn scope(cx: &App) -> Option<FileScope> {
    cx.try_global::<Files>()?
        .explorer
        .read(cx)
        .inputs
        .scope
        .clone()
}

/// Names whose visibility the host already decided: `node_modules` is shown
/// collapsed on purpose, and the rest the listing never reports.
const HOST_OWNED_NAMES: [&str; 7] = [
    "node_modules",
    ".git",
    ".svn",
    ".hg",
    "CVS",
    ".DS_Store",
    "Thumbs.db",
];

/// What the Explorer may hide, from a git status: `None` for anything short of
/// an authoritative answer. Paths are rebuilt from `display_path` against the
/// worktree root. Port of `ignoredPathsFromStatus`.
pub fn ignored_paths<'a>(
    authoritative: bool,
    oversized: bool,
    worktree_root: &str,
    entries: impl IntoIterator<Item = (&'a str, bool)>,
) -> Option<HashSet<String>> {
    if !authoritative || oversized {
        return None;
    }
    let root = worktree_root.trim_end_matches('/');
    Some(
        entries
            .into_iter()
            .filter(|(path, ignored)| {
                let name = path.rsplit('/').next().unwrap_or(path);
                *ignored && !HOST_OWNED_NAMES.contains(&name)
            })
            .map(|(path, _)| format!("{root}/{path}"))
            .collect(),
    )
}

/// Follows the Git panel's status, which it calls on every change; the set
/// is rebuilt only for a different snapshot.
pub fn follow_git_status(cx: &mut App) {
    let status = crate::git::active_status(cx);
    if let Some(held) = cx.try_global::<IgnoredPaths>() {
        let same = match (&held.status, &status) {
            (Some(held), Some(status)) => Rc::ptr_eq(held, status),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
    }
    let paths = status.as_ref().and_then(|status| {
        ignored_paths(
            status.authoritative,
            status.oversized,
            &status.repository.worktree_root,
            status
                .entries
                .iter()
                .map(|entry| (entry.display_path.as_str(), entry.ignored)),
        )
    });
    set_ignored_paths(paths, cx);
    cx.global_mut::<IgnoredPaths>().status = status;
}

/// The Git panel's half of the ignored-file filter.
pub fn set_ignored_paths(paths: Option<HashSet<String>>, cx: &mut App) {
    cx.set_global(IgnoredPaths {
        paths: paths.map(Rc::new),
        status: None,
    });
    if let Some(files) = cx.try_global::<Files>() {
        files.explorer.clone().update(cx, |explorer, cx| {
            explorer.refresh_rows(cx);
            cx.notify();
        });
    }
}

fn ignored(cx: &App) -> Option<Rc<HashSet<String>>> {
    cx.try_global::<IgnoredPaths>()?.paths.clone()
}

/// Rewrites open file tabs below a renamed or moved path. Port of
/// `relocateFileTabs`.
pub fn relocate_file_tabs(
    state: &mut PersistedAppState,
    host_profile_id: &str,
    server_identity: &str,
    root_path: &str,
    source: &str,
    destination: &str,
) -> bool {
    let absolute = if destination.starts_with('/') {
        destination.to_owned()
    } else {
        format!("{}/{destination}", root_path.trim_end_matches('/'))
    };
    let resolved = format!(
        "/{}",
        absolute
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect::<Vec<_>>()
            .join("/")
    );
    let prefix = format!("{}/", source.trim_end_matches('/'));
    let mut changed = false;
    for tab in &mut state.app_tabs {
        if tab.host_profile_id != host_profile_id
            || tab.server_identity != server_identity
            || !matches!(tab.kind, AppTabKind::File | AppTabKind::Markdown)
            || (tab.resource != source && !tab.resource.starts_with(&prefix))
        {
            continue;
        }
        let suffix = tab.resource.strip_prefix(&prefix).unwrap_or_default();
        tab.resource = if suffix.is_empty() {
            resolved.clone()
        } else {
            format!("{}/{suffix}", resolved.trim_end_matches('/'))
        };
        tab.title = tab
            .resource
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or(&tab.resource)
            .to_owned();
        changed = true;
    }
    changed
}

/// Download…: the save panel, then the transfer. The default name is made
/// unique before the panel opens, so downloading twice asks nothing.
pub fn download(
    explorer: Entity<Explorer>,
    intent: DownloadIntent,
    root: ActiveRoot,
    origin: DownloadOrigin,
    _window: &mut Window,
    cx: &mut App,
) {
    let Some(scope) = explorer.update(cx, |explorer, cx| {
        explorer.begin_download(&root, origin, cx)
    }) else {
        return;
    };
    let name = suggested_name(&intent);
    // A failure costs a nicer default, not the download.
    let suggested = explorer
        .read(cx)
        .transport()
        .suggest_destination(name.clone())
        .unwrap_or_else(|_| name.clone());
    let suggested = PathBuf::from(suggested);
    let directory = suggested
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(PathBuf::from)
        .or_else(dirs::download_dir)
        .or_else(dirs::home_dir)
        .unwrap_or_default();
    let file_name = suggested
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(name);
    let mut answer = cx.prompt_for_new_path(&directory, Some(&file_name));
    cx.spawn(async move |cx| {
        let chosen = loop {
            match answer.try_recv() {
                Ok(Some(chosen)) => {
                    break chosen
                        .map(|path| path.map(|path| path.to_string_lossy().into_owned()))
                        .map_err(|error| error.to_string());
                }
                Ok(None) => cx.background_executor().timer(PICKER_POLL).await,
                // Cancelling the panel is a complete answer.
                Err(_) => break Ok(None),
            }
        };
        explorer.update(cx, |explorer, cx| {
            explorer.finish_picking(scope, root, intent, origin, chosen, cx)
        })
    })
    .detach();
}

/// The tab strip's Download…: the tab's own root when it carries one, the
/// Explorer's otherwise.
pub fn download_app_tab(tab_id: &str, window: &mut Window, cx: &mut App) {
    let Some(files) = cx.try_global::<Files>() else {
        return;
    };
    let (explorer, controller) = (files.explorer.clone(), files.controller.upgrade());
    let Some(tab) = controller
        .and_then(|controller| controller.read(cx).app_state().ok())
        .and_then(|state| state.app_tabs.iter().find(|tab| tab.id == tab_id).cloned())
    else {
        return;
    };
    let root = match (&tab.root_path, &tab.root_token) {
        (Some(path), Some(token)) => Some(tab_root(&explorer, path, token, cx)),
        _ => explorer.read(cx).tree.root.clone(),
    };
    let Some(root) = root else {
        return;
    };
    let intent = DownloadIntent {
        path: tab.resource.clone(),
        kind: DownloadKind::File,
    };
    download(explorer, intent, root, DownloadOrigin::TabMenu, window, cx);
}

/// A path dropped on a workspace edge opens beside the terminal, resolved as
/// the pane would. Port of the dock zones' `onDockPath`.
pub fn dock_path(drag: &InternalPathDrag, side: DockSide, cx: &mut App) {
    // The drop is checked against the connected host first, as the zones are,
    // and only then needs the live file scope.
    let target = cx
        .try_global::<Files>()
        .and_then(|files| files.controller.upgrade())
        .and_then(|controller| {
            let link = controller.read(cx).active_link()?;
            let server = link.host_state.server_identity.clone()?;
            Some((link.profile_id.clone(), server))
        });
    let target = target
        .as_ref()
        .map(|(profile, server)| (profile.as_str(), server.as_str()));
    if let Err(reason) = drag.shell_text(target) {
        report(reason, cx);
        return;
    }
    let Some(scope) = scope(cx) else {
        report(
            "Reconnect before opening a file beside the terminal.".into(),
            cx,
        );
        return;
    };
    crate::editor::open_terminal_path(&scope.pane_id, &drag.path, Some(side), cx);
}

fn report(message: String, cx: &mut App) {
    if let Some(controller) = cx
        .try_global::<Files>()
        .and_then(|files| files.controller.upgrade())
    {
        controller.update(cx, |controller, cx| controller.report_status(message, cx));
    }
}

/// A file tab's own root snapshot, as its surface reconstructs it.
fn tab_root(explorer: &Entity<Explorer>, path: &str, token: &str, cx: &App) -> ActiveRoot {
    ActiveRoot {
        token: token.to_owned(),
        pane_id: explorer
            .read(cx)
            .inputs
            .scope
            .as_ref()
            .map(|scope| scope.pane_id.clone())
            .unwrap_or_default(),
        path: path.to_owned(),
        git_worktree: false,
        revision: "0".into(),
    }
}

/// The key a Quick Open search belongs to: one pane's directory on one live
/// connection.
pub fn search_key(cx: &App) -> Option<String> {
    cx.try_global::<Files>()?.explorer.read(cx).search_key()
}

/// Searches the active pane's directory. The root is resolved once per search
/// key; dropping the task stops the host's search.
pub fn search_files(query: String, cx: &mut App) -> Task<Result<FileSearchResults, String>> {
    match cx.try_global::<Files>().map(|files| files.explorer.clone()) {
        Some(explorer) => search(explorer, query, cx),
        None => Task::ready(Err("Files are unavailable.".into())),
    }
}

fn search(
    explorer: Entity<Explorer>,
    query: String,
    cx: &mut App,
) -> Task<Result<FileSearchResults, String>> {
    let (Some(key), Some(scope), Some(route)) = (
        explorer.read(cx).search_key(),
        explorer.read(cx).inputs.scope.clone(),
        explorer.read(cx).inputs.route.clone(),
    ) else {
        return Task::ready(Err("Files are unavailable.".into()));
    };
    let held = explorer
        .read(cx)
        .search_root
        .clone()
        .filter(|(held, _)| *held == key)
        .map(|(_, root)| root);
    let transport = explorer.read(cx).transport();
    cx.spawn(async move |cx| {
        let result = async {
            let root = match held {
                Some(root) => root,
                None => {
                    let operation = operation_id();
                    let value = client::request(
                        transport.clone(),
                        scope.client_id.clone(),
                        operation.clone(),
                        command::resolve_root(&scope, None, &operation),
                    )
                    .await?;
                    wire::decode_root(value)?
                }
            };
            explorer.update(cx, |explorer, _| {
                explorer.search_root = Some((key.clone(), root.clone()));
            });
            let operation = operation_id();
            let value = client::request(
                transport,
                scope.client_id.clone(),
                operation.clone(),
                command::search(&scope, &root, &route, &query, &operation),
            )
            .await?;
            wire::decode_search(value, &route.cwd)
        }
        .await;
        if result.is_err() {
            explorer.update(cx, |explorer, _| explorer.search_root = None);
        }
        result
    })
}

/// The file service as `desktop-core` serves it.
pub fn native_transport(
    clients: Arc<desktop_core::connection::TerminalClients>,
    profiles: Arc<desktop_core::connection::ProfileStore>,
) -> Rc<dyn FileTransport> {
    Rc::new(client::NativeFiles {
        clients,
        profiles,
        downloads: Default::default(),
    })
}
