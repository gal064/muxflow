//! Source Control: the right panel's Git segment and the Git diff tab. Port of
//! `features/git` and the Git half of `useWorkspaceDomainController`.
//!
//! One [`store::GitRepositories`] holds a shared watch per repository scope,
//! fed by the bridge's Git frames. The panel follows the active pane's root;
//! each diff tab joins the same observation through [`diff_surface`], the
//! seam the document area routes a Git diff tab through. A diff tab's view
//! lives as long as the tab.

pub mod client;
pub mod diff_view;
pub mod model;
pub mod panel;
pub mod store;

#[cfg(test)]
mod tests;

use std::{rc::Rc, sync::Arc};

use desktop_core::app_state::{AppTabKind, AppTabRecord};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, Global, IntoElement, WeakEntity, Window, div,
};

use crate::{
    agents::view::ActiveRoot,
    connection_ui::{
        ConnectionUi,
        backend::TerminalBackend,
        connection_reducer::ConnectionPhase,
        controller::{ConnectionController, ConnectionEvent},
    },
    ui::chrome::PanelBodies,
};

use diff_view::GitDiffView;
use model::{DiffTabPlace, Live, RootInfo, StatusEntry, StatusSnapshot, Target};
use panel::{GitPanel, PanelContext, PanelEvent};
use store::GitRepositories;

/// The running panel. Absent until [`start`], so a test window observes no
/// repository.
struct Git {
    panel: Entity<GitPanel>,
    /// The diff tabs drawn most recently, oldest first. A covered tab keeps
    /// its observation and its diff, as Tauri kept its last few documents
    /// mounted; past that, or once closed, it goes.
    views: Vec<(String, Entity<GitDiffView>)>,
}

impl Global for Git {}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    PanelBodies::register("git", body, cx);
}

fn body(window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(panel) = cx.try_global::<Git>().map(|git| git.panel.clone()) else {
        return div().into_any_element();
    };
    // Lives exactly as long as the segment is drawn every frame.
    window.use_keyed_state("git-panel-shown", cx, {
        let panel = panel.clone();
        move |_, cx| Shown::new(panel, cx)
    });
    panel.into_any_element()
}

/// The panel's rows answer the palette only while the panel is on screen.
struct Shown(WeakEntity<GitPanel>);

impl Shown {
    fn new(panel: Entity<GitPanel>, cx: &mut gpui_kit::Context<Self>) -> Self {
        panel.update(cx, |panel, cx| panel.set_shown(true, cx));
        cx.on_release(|this, cx| {
            this.0
                .update(cx, |panel, cx| panel.set_shown(false, cx))
                .ok();
        })
        .detach();
        Self(panel.downgrade())
    }
}

/// Called once the connection controller exists, before the first window.
pub fn start(
    controller: Entity<ConnectionController>,
    backend: Arc<dyn TerminalBackend>,
    cx: &mut App,
) {
    let transport: client::Transport = Rc::new(client::BackendTransport(backend));
    let store = cx.new(|_| GitRepositories::new(transport));
    let panel = cx.new(|cx| GitPanel::new(store.clone(), cx));
    cx.set_global(Git {
        panel: panel.clone(),
        views: Vec::new(),
    });
    cx.subscribe(&controller, move |_, event, cx| {
        if let ConnectionEvent::GitService { payload } = event {
            for event in model::watch_events(payload) {
                store.update(cx, |store, cx| store.handle_event(event, cx));
            }
        }
    })
    .detach();
    cx.subscribe(&panel, |_, event, cx| match event {
        PanelEvent::Message(message) => report(message.clone(), cx),
        PanelEvent::OpenDiff {
            entry,
            target,
            status,
            preview,
        } => open_diff(entry, *target, status, *preview, cx),
    })
    .detach();
    cx.observe(&controller, {
        let panel = panel.clone();
        move |controller, cx| sync(&controller, &panel, cx)
    })
    .detach();
    cx.observe_global::<ActiveRoot>({
        let (controller, panel) = (controller.clone(), panel.clone());
        move |cx| sync(&controller, &panel, cx)
    })
    .detach();
    sync(&controller, &panel, cx);
}

/// The active host's link and the active pane's root, as the panel needs them.
fn sync(controller: &Entity<ConnectionController>, panel: &Entity<GitPanel>, cx: &mut App) {
    let context = {
        let controller = controller.read(cx);
        let link = controller
            .active_link()
            .map_or_else(Live::default, |link| Live {
                connected: link.host_state.phase == ConnectionPhase::Connected,
                client_id: link.client_id.clone(),
                terminal_epoch: link.terminal_epoch,
                server_identity: link.host_state.server_identity.clone(),
                host_profile_id: link.profile_id.clone(),
            });
        let can_mutate = controller
            .active_link()
            .is_some_and(|link| link.host_state.can_mutate);
        let root = cx.try_global::<ActiveRoot>().map(|root| RootInfo {
            path: root.path.clone(),
            token: root.token.clone(),
            git_worktree: root.git_worktree,
        });
        PanelContext::new(root, link, can_mutate)
    };
    panel.update(cx, |panel, cx| panel.set_context(context, cx));
    // A closed tab's view goes, and its observation with it.
    let Ok(state) = controller.read(cx).app_state() else {
        return;
    };
    cx.global_mut::<Git>()
        .views
        .retain(|(id, _)| state.app_tabs.iter().any(|tab| tab.id == *id));
}

/// The app's status line.
pub(crate) fn report(message: String, cx: &mut App) {
    if let Some(controller) = cx.try_global::<ConnectionUi>().map(|ui| ui.0.clone()) {
        controller.update(cx, |controller, cx| controller.report_status(message, cx));
    }
}

/// The active repository's status: the Explorer's ignored paths and the
/// workspace's branch follow it.
pub fn active_status(cx: &App) -> Option<Rc<StatusSnapshot>> {
    cx.try_global::<Git>()?.panel.read(cx).status(cx)
}

/// Opens the row's diff in the active workspace, or re-selects the one already
/// open for that repository, path and side.
fn open_diff(
    entry: &StatusEntry,
    target: Target,
    status: &Rc<StatusSnapshot>,
    preview: bool,
    cx: &mut App,
) {
    let Some(workspaces) = crate::workspaces::current(cx) else {
        return;
    };
    let Some(root) = cx
        .try_global::<Git>()
        .and_then(|git| git.panel.read(cx).context().root.clone())
    else {
        return;
    };
    let view = workspaces.read(cx).view(cx);
    let (Some(session), Some(server)) = (view.session.clone(), view.server.clone()) else {
        return;
    };
    let host = view.host.clone();
    let (entry, status) = (entry.clone(), status.clone());
    let opened = Rc::new(std::cell::RefCell::new(None));
    {
        let opened = opened.clone();
        workspaces.update(cx, |workspaces, cx| {
            workspaces.update_app_state(
                move |state| {
                    let place = DiffTabPlace {
                        host: &host,
                        server: &server,
                        session_id: &session.id,
                        session_name: &session.name,
                        root_path: &root.path,
                        root_token: &root.token,
                    };
                    let id = model::open_git_diff_tab(
                        state,
                        &place,
                        &entry,
                        target,
                        &status,
                        preview,
                        || uuid::Uuid::new_v4().to_string(),
                    );
                    *opened.borrow_mut() = Some(id);
                    true
                },
                cx,
            )
        });
    }
    let Some(id) = opened.borrow_mut().take() else {
        return;
    };
    let session_id = view.session.map(|session| session.id).unwrap_or_default();
    workspaces.update(cx, |workspaces, cx| {
        workspaces.select_app_tab(&session_id, Some(&id), cx)
    });
}

/// Tauri's `MOUNTED_APP_TAB_LIMIT`.
const KEPT_DIFF_VIEWS: usize = 4;

/// What the document area draws for a Git diff tab. `None` for any other
/// kind of tab.
pub fn diff_surface(tab: &AppTabRecord, _: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if tab.kind != AppTabKind::GitDiff {
        return None;
    }
    let git = cx.try_global::<Git>()?;
    let kept = git.views.iter().position(|(id, _)| *id == tab.id);
    let panel = git.panel.clone();
    let view = match kept {
        Some(index) => cx.global_mut::<Git>().views.remove(index).1,
        None => cx.new(|cx| GitDiffView::new(panel, tab.clone(), cx)),
    };
    let views = &mut cx.global_mut::<Git>().views;
    views.push((tab.id.clone(), view.clone()));
    if views.len() > KEPT_DIFF_VIEWS {
        views.remove(0);
    }
    view.update(cx, |view, cx| view.set_tab(tab, cx));
    Some(view.into_any_element())
}
