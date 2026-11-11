//! The document editor that replaces Monaco: file tabs that read, edit and
//! autosave a host file, find and replace, go to line, and grammar
//! highlighting on the Kit code editor. Also the routing of every document
//! tab's surface (`AppTabSurface.tsx`), which the diff view and the Markdown
//! preview join through [`set_diff_surface`] and [`set_markdown_preview`].
//!
//! A file tab's buffer lives in a [`DocumentPane`] kept for the most recently
//! shown tabs, as Tauri kept the last four mounted. A pane that leaves saves
//! what it owes first, and [`flush_all`] saves every buffer before something
//! that would lose one: closing tabs or the window.
//!
//! Seams for the file service:
//! - [`open_file`] opens a path under a root capability, for the Explorer.
//! - [`open_terminal_path`] resolves a path a pane printed, then opens it.
//! - [`set_watcher`] arms the directory watch a document needs, and
//!   [`deliver`] hands it what the host said about the document's file.
//! - [`set_download`] puts a Download button on file tabs.

pub mod autosave;
pub mod document;
pub mod find;
pub mod io;
pub mod language;
mod open;
mod pane;
pub mod save_status;
pub mod tabs;

use std::{any::Any, rc::Rc, sync::Arc};

use desktop_core::app_state::{AppTabKind, AppTabRecord};
use gpui_kit::{
    AnyElement, App, AppContext, Entity, Focusable, Global, InteractiveElement, IntoElement,
    ParentElement, Role, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    Window, div,
};

pub use document::{Document, FileNotice};
pub use io::Root;
pub use open::{
    OpenFile, open_file, open_search_match, open_terminal_path, search_files, search_key,
};
pub use pane::DocumentPane;

use crate::{
    connection_ui::{ConnectionUi, backend::TerminalBackend, connection_reducer::ConnectionPhase},
    ui::shell::Shell,
};
use document::Live;
use io::{BackendIo, FileScope};

/// How many file tabs keep their buffer, undo and scroll while not shown.
const MOUNTED_LIMIT: usize = 4;

/// Draws a Git diff tab.
pub type DiffSurface = Rc<dyn Fn(&AppTabRecord, &mut Window, &mut App) -> AnyElement>;
/// Draws a Markdown tab's rendered view from its document's live buffer.
pub type MarkdownPreview =
    Rc<dyn Fn(&AppTabRecord, &Entity<Document>, &mut Window, &mut App) -> AnyElement>;
/// Starts the download of a file tab's file.
pub type Download = Rc<dyn Fn(&AppTabRecord, &Root, &mut Window, &mut App)>;
/// What a document's watch needs: the directory (or, for a terminal file
/// capability, the file) to watch under the root, on the tab's host.
pub struct WatchTarget {
    pub host: String,
    pub server: String,
    pub root: Root,
    pub watched: String,
    pub resource: String,
}
/// Arms a watch and returns its lease, held by the document while it lives.
/// The watch re-arms on every new live connection and delivers that
/// connection's bootstrap listing (`FileNotice::Listed { bootstrap: true, .. }`):
/// changes made while disconnected raise no event, and a document revalidates
/// what it shows only from that listing.
pub type Watcher = Rc<dyn Fn(&WatchTarget, &mut App) -> Box<dyn Any>>;

#[derive(Default)]
struct Editors {
    /// Most recently shown last.
    panes: Vec<(String, Entity<DocumentPane>)>,
    /// Documents saving what they owe on their way out.
    retiring: Vec<Entity<Document>>,
    diff: Option<DiffSurface>,
    preview: Option<MarkdownPreview>,
    download: Option<Download>,
    watcher: Option<Watcher>,
    observing: Option<Subscription>,
    closing: bool,
    /// The app tabs the workspace showed last, in its main area and its dock.
    selected: Option<String>,
    /// The docked tab and its workspace.
    docked: Option<(String, String)>,
    /// A shown tab asked to take the keyboard.
    focus_request: Option<String>,
    /// The keyboard was in a document that went away.
    focus_lost: bool,
}

impl Global for Editors {}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    language::register();
    cx.set_global(Editors::default());
    pane::init(cx);
    cx.observe_new::<Shell>(|_, window, cx| {
        if let Some(window) = window {
            window.on_window_should_close(cx, should_close);
        }
    })
    .detach();
    // A quit that bypasses the Quit action, from the platform, still gets a
    // best-effort save in the time the app gives it.
    cx.on_app_quit(|cx| {
        let flush = flush_all(cx);
        async move {
            let _ = flush.await;
        }
    })
    .detach();
}

pub fn set_diff_surface(surface: DiffSurface, cx: &mut App) {
    cx.global_mut::<Editors>().diff = Some(surface);
}

pub fn set_markdown_preview(preview: MarkdownPreview, cx: &mut App) {
    cx.global_mut::<Editors>().preview = Some(preview);
}

pub fn set_download(download: Download, cx: &mut App) {
    cx.global_mut::<Editors>().download = Some(download);
}

pub fn set_watcher(watcher: Watcher, cx: &mut App) {
    cx.global_mut::<Editors>().watcher = Some(watcher);
}

pub(crate) fn preview(cx: &App) -> Option<MarkdownPreview> {
    cx.try_global::<Editors>()?.preview.clone()
}

pub(crate) fn download(cx: &App) -> Option<Download> {
    cx.try_global::<Editors>()?.download.clone()
}

/// What the file service saw happen to `path` under the root `root_token` on
/// a host, for every document showing that file.
pub fn deliver(host: &str, root_token: &str, path: &str, notice: FileNotice, cx: &mut App) {
    let documents: Vec<Entity<Document>> = documents(cx)
        .into_iter()
        .filter(|document| {
            let document = document.read(cx);
            document.host == host && document.root.token == root_token && document.resource == path
        })
        .collect();
    for document in documents {
        document.update(cx, |document, cx| document.notice(notice.clone(), cx));
    }
}

/// The surface a document tab shows in the workspace.
pub fn surface(record: &AppTabRecord, window: &mut Window, cx: &mut App) -> AnyElement {
    if record.kind == AppTabKind::GitDiff {
        let diff = cx
            .try_global::<Editors>()
            .and_then(|editors| editors.diff.clone());
        return match diff {
            Some(diff) => diff(record, window, cx),
            None => panel(record, pane::empty_tab(record, None, None, cx)),
        };
    }
    let root = record
        .root_path
        .clone()
        .zip(record.root_token.clone())
        .map(|(path, token)| Root { path, token })
        .or_else(|| {
            cx.try_global::<crate::agents::view::ActiveRoot>()
                .map(|root| Root {
                    path: root.path.clone(),
                    token: root.token.clone(),
                })
        });
    match root.and_then(|root| mount(record, root, cx)) {
        Some(pane) => {
            pane.update(cx, |pane, _| pane.record = record.clone());
            pane.into_any_element()
        }
        None => panel(
            record,
            pane::empty_tab(
                record,
                Some("Reconnect and select a terminal pane to reopen this file."),
                None,
                cx,
            ),
        ),
    }
}

/// Called by the workspace on every draw with the app tabs it shows. A
/// document that comes on screen, in the main area or the dock, takes the
/// keyboard, as opening or switching to a file does in Tauri; so does the
/// main-area document when the keyboard was left in one that went away (a
/// closed or hidden tab, or nowhere after Quick Open), so keys never fall to
/// an element no longer drawn.
pub fn shown(
    selected: Option<&AppTabRecord>,
    docked: Option<&AppTabRecord>,
    window: &mut Window,
    cx: &mut App,
) {
    if !cx.has_global::<Editors>() {
        return;
    }
    let selected = selected.map(|tab| tab.id.clone());
    let docked_tab = docked.map(|tab| {
        let workspace = format!("{}\0{}", tab.host_profile_id, tab.session_id);
        (workspace, tab.id.clone())
    });
    let docked = docked_tab.as_ref().map(|(_, id)| id.clone());
    let editors = cx.global::<Editors>();
    // Another document taking the dock of the same workspace, not a dock met
    // on switching workspace or at start.
    let redocked = match (&editors.docked, &docked_tab) {
        (Some((was, old)), Some((now, new))) => was == now && old != new,
        _ => false,
    };
    let request = editors
        .focus_request
        .clone()
        .filter(|id| Some(id) == selected.as_ref() || Some(id) == docked.as_ref());
    let target = request
        .or_else(|| selected.clone().filter(|_| editors.selected != selected))
        .or_else(|| docked.clone().filter(|_| redocked))
        .or_else(|| {
            let hidden = editors
                .panes
                .iter()
                .filter(|(id, _)| Some(id) != selected.as_ref() && Some(id) != docked.as_ref())
                .any(|(_, pane)| {
                    pane.read(cx)
                        .editor(cx)
                        .is_some_and(|editor| editor.read(cx).focus_handle(cx).is_focused(window))
                });
            let lost = editors.focus_lost || hidden || window.focused(cx).is_none();
            selected.clone().filter(|_| lost)
        });
    let editors = cx.global_mut::<Editors>();
    (editors.selected, editors.docked) = (selected, docked_tab);
    editors.focus_request = None;
    editors.focus_lost = false;
    let Some(id) = target else {
        return;
    };
    // After this draw, which mounts the pane.
    window.defer(cx, move |window, cx| {
        let pane = cx
            .global::<Editors>()
            .panes
            .iter()
            .find(|(pane_id, _)| *pane_id == id)
            .map(|(_, pane)| pane.clone());
        if let Some(pane) = pane {
            let target = match pane.read(cx).editor(cx) {
                Some(editor) => editor.read(cx).focus_handle(cx),
                None => pane.read(cx).focus.clone(),
            };
            window.focus(&target, cx);
        }
    });
}

/// Gives the keyboard to the document tab `id` the next time it is drawn,
/// as choosing the docked document's tab does.
pub fn request_focus(id: &str, cx: &mut App) {
    if let Some(editors) = cx
        .has_global::<Editors>()
        .then(|| cx.global_mut::<Editors>())
    {
        editors.focus_request = Some(id.to_owned());
    }
}

/// The tab panel around a surface that has no pane of its own.
fn panel(record: &AppTabRecord, child: impl IntoElement) -> AnyElement {
    div()
        .id(SharedString::from(format!("app-tab-panel-{}", record.id)))
        .role(Role::TabPanel)
        .aria_label(SharedString::from(record.title.clone()))
        .size_full()
        .child(child)
        .into_any_element()
}

/// The pane for a tab, made or kept, and marked as the most recently shown.
fn mount(record: &AppTabRecord, root: Root, cx: &mut App) -> Option<Entity<DocumentPane>> {
    let controller = cx.try_global::<ConnectionUi>()?.0.clone();
    if cx.global::<Editors>().observing.is_none() {
        let observing = cx.observe(&controller, |_, cx| follow(cx));
        cx.global_mut::<Editors>().observing = Some(observing);
    }
    // Only the workspace on screen keeps documents mounted, as Tauri's: one
    // left behind that cannot save would otherwise hold every close hostage.
    let workspace = |tab: &AppTabRecord| {
        (
            tab.host_profile_id.clone(),
            tab.server_identity.clone(),
            tab.session_id.clone(),
        )
    };
    let here = workspace(record);
    let panes = std::mem::take(&mut cx.global_mut::<Editors>().panes);
    let (kept, left): (Vec<_>, Vec<_>) = panes
        .into_iter()
        .partition(|(_, pane)| workspace(&pane.read(cx).record) == here);
    cx.global_mut::<Editors>().panes = kept;
    for (_, pane) in left {
        retire(pane, cx);
    }
    let index = cx
        .global::<Editors>()
        .panes
        .iter()
        .position(|(id, _)| *id == record.id);
    if let Some(index) = index {
        let (id, pane) = cx.global_mut::<Editors>().panes.remove(index);
        let document = pane.read(cx).document.read(cx);
        let same = document.host == record.host_profile_id
            && document.server == record.server_identity
            && document.resource == record.resource
            && document.root == root;
        if same {
            cx.global_mut::<Editors>().panes.push((id, pane.clone()));
            return Some(pane);
        }
        // The preview slot now names another file, or the tab another root.
        retire(pane, cx);
    }
    let io = Rc::new(BackendIo(backend(&controller, cx)));
    let live = Rc::new(live(record, cx));
    let watcher = cx.global::<Editors>().watcher.clone();
    let record = record.clone();
    let pane = cx.new(|cx| DocumentPane::new(record.clone(), root, io, live, watcher, cx));
    let editors = cx.global_mut::<Editors>();
    editors.panes.push((record.id.clone(), pane.clone()));
    let excess = editors.panes.len().saturating_sub(MOUNTED_LIMIT);
    let evicted: Vec<_> = editors
        .panes
        .drain(..excess)
        .map(|(_, pane)| pane)
        .collect();
    for pane in evicted {
        retire(pane, cx);
    }
    Some(pane)
}

fn backend(
    controller: &Entity<crate::connection_ui::controller::ConnectionController>,
    cx: &App,
) -> Arc<dyn TerminalBackend> {
    controller.read(cx).backend()
}

/// The live file scope of the tab's host, while it serves the tab's server.
fn live(record: &AppTabRecord, _cx: &App) -> Live {
    let (host, server) = (
        record.host_profile_id.clone(),
        record.server_identity.clone(),
    );
    let link_for = move |cx: &App| -> Option<FileScope> {
        let controller = cx.try_global::<ConnectionUi>()?.0.read(cx);
        let link = controller.link(&host)?;
        let state = &link.host_state;
        let live = state.phase == ConnectionPhase::Connected
            && link.terminal_epoch != 0
            && state.server_identity.as_deref() == Some(server.as_str());
        Some(FileScope {
            client_id: link.client_id.clone().filter(|_| live)?,
            profile_id: link.profile_id.clone(),
            server_identity: server.clone(),
            terminal_epoch: link.terminal_epoch,
        })
    };
    let link_for = Rc::new(link_for);
    let scope = link_for.clone();
    let host = record.host_profile_id.clone();
    Live {
        scope: Box::new(move |cx| scope(cx)),
        can_write: Box::new(move |cx| {
            link_for(cx).is_some()
                && cx.try_global::<ConnectionUi>().is_some_and(|ui| {
                    ui.0.read(cx)
                        .link(&host)
                        .is_some_and(|link| link.host_state.can_mutate)
                })
        }),
    }
}

/// Follows the connection: documents read once it is live, and a tab that
/// is gone takes its document with it.
fn follow(cx: &mut App) {
    let Some(state) = cx
        .try_global::<ConnectionUi>()
        .and_then(|ui| ui.0.read(cx).app_state().ok())
    else {
        return;
    };
    let panes = cx.global::<Editors>().panes.clone();
    let (open, closed): (Vec<_>, Vec<_>) = panes.into_iter().partition(|(id, pane)| {
        let host = pane.read(cx).document.read(cx).host.clone();
        state
            .app_tabs
            .iter()
            .any(|tab| tab.id == *id && tab.host_profile_id == host)
    });
    cx.global_mut::<Editors>().panes = open.clone();
    for (_, pane) in closed {
        retire(pane, cx);
    }
    for (_, pane) in open {
        let document = pane.read(cx).document.clone();
        document.update(cx, |document, cx| document.follow_connection(cx));
    }
}

/// Lets a pane go once its document has saved what it owes.
fn retire(pane: Entity<DocumentPane>, cx: &mut App) {
    let document = pane.read(cx).document.clone();
    // The keyboard in a document that goes away comes back to the one on
    // screen; asked after this update, as retiring can happen mid-draw.
    let mut handles = vec![pane.read(cx).focus.clone()];
    handles.extend(
        pane.read(cx)
            .editor(cx)
            .map(|editor| editor.read(cx).focus_handle(cx)),
    );
    drop(pane);
    cx.defer(move |cx| {
        for window in cx.windows() {
            let _ = window.update(cx, |_, window, cx| {
                if handles.iter().any(|handle| handle.is_focused(window)) {
                    cx.global_mut::<Editors>().focus_lost = true;
                    window.refresh();
                }
            });
        }
    });
    cx.global_mut::<Editors>().retiring.push(document.clone());
    let flush = Document::flush(&document, cx);
    cx.spawn(async move |cx| {
        let result = flush.await;
        cx.update(|cx| {
            if let Err(error) = result {
                let resource = document.read(cx).resource.clone();
                report(format!("Could not save {resource}: {error}"), cx);
            }
            cx.global_mut::<Editors>()
                .retiring
                .retain(|retiring| *retiring != document);
        })
    })
    .detach();
}

fn documents(cx: &App) -> Vec<Entity<Document>> {
    let Some(editors) = cx.try_global::<Editors>() else {
        return Vec::new();
    };
    editors
        .panes
        .iter()
        .map(|(_, pane)| pane.read(cx).document.clone())
        .chain(editors.retiring.iter().cloned())
        .collect()
}

/// Saves every buffer, before something that would lose one.
pub fn flush_all(cx: &mut App) -> Task<Result<(), String>> {
    let flushes: Vec<_> = documents(cx)
        .iter()
        .map(|document| Document::flush(document, cx))
        .collect();
    cx.spawn(async move |_| {
        let mut failed = false;
        for flush in flushes {
            failed |= flush.await.is_err();
        }
        if failed {
            Err("One or more editor files could not be saved.".into())
        } else {
            Ok(())
        }
    })
}

/// Whether any buffer holds work that is not on disk yet.
pub fn owes_save(cx: &App) -> bool {
    use autosave::SaveState;
    documents(cx).iter().any(|document| {
        matches!(
            document.read(cx).save_state(),
            Some(SaveState::Dirty | SaveState::Saving | SaveState::Error)
        )
    })
}

/// Closing the window waits for every buffer to save, and stays open when
/// one cannot.
fn should_close(window: &mut Window, cx: &mut App) -> bool {
    if !owes_save(cx) {
        return true;
    }
    if std::mem::replace(&mut cx.global_mut::<Editors>().closing, true) {
        return false;
    }
    let flush = flush_all(cx);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let result = flush.await;
        cx.update(|cx| {
            cx.global_mut::<Editors>().closing = false;
            match result {
                Ok(()) => {
                    handle
                        .update(cx, |_, window, _| window.remove_window())
                        .ok();
                }
                Err(error) => report(
                    format!("Could not save application state; close cancelled: {error}"),
                    cx,
                ),
            }
        })
    })
    .detach();
    false
}

/// Whether focus is inside the docked layer, as Tauri's `dockHoldsFocus`.
pub fn holds_focus(window: &Window) -> bool {
    window
        .context_stack()
        .iter()
        .any(|context| context.contains(DOCKED_CONTEXT))
}

/// The key context of the docked layer, whatever surface it shows.
pub const DOCKED_CONTEXT: &str = "DockedDocument";

/// Quitting waits for every buffer to save, and stays when one cannot.
pub fn quit(cx: &mut App) {
    if !owes_save(cx) {
        return cx.quit();
    }
    if std::mem::replace(&mut cx.global_mut::<Editors>().closing, true) {
        return;
    }
    let flush = flush_all(cx);
    cx.spawn(async move |cx| {
        let result = flush.await;
        cx.update(|cx| {
            cx.global_mut::<Editors>().closing = false;
            match result {
                Ok(()) => cx.quit(),
                Err(error) => report(
                    format!("Could not save application state; close cancelled: {error}"),
                    cx,
                ),
            }
        })
    })
    .detach();
}

/// The window's own Close control: the same guard as a platform close.
pub fn close_window(window: &mut Window, cx: &mut App) {
    if should_close(window, cx) {
        window.remove_window();
    }
}

/// A message for the status line.
pub(crate) fn report(message: String, cx: &mut App) {
    if let Some(ui) = cx.try_global::<ConnectionUi>() {
        let controller = ui.0.clone();
        controller.update(cx, |controller, cx| controller.report_status(message, cx));
    }
}

/// Whether a chord pressed in a file tab's editor is one the editor leaves
/// alone, so the app's shortcut for it runs, as Monaco let unhandled chords
/// through. Typing, and copy, cut and paste, stay native; the find and go to
/// line fields too, as Monaco's widgets sat inside its host.
pub fn yields_shortcut(event: &gpui_kit::KeystrokeEvent, cx: &App) -> bool {
    let within = |context: &str| event.context_stack.iter().any(|c| c.contains(context));
    if !within(pane::CONTEXT) {
        return false;
    }
    let keystroke = &event.keystroke;
    let modifiers = &keystroke.modifiers;
    if !modifiers.control && !modifiers.platform && keystroke.key.chars().count() == 1 {
        return false;
    }
    let secondary = if cfg!(target_os = "macos") {
        modifiers.platform && !modifiers.control
    } else {
        modifiers.control && !modifiers.platform
    };
    if secondary
        && !modifiers.alt
        && matches!(keystroke.key.to_ascii_lowercase().as_str(), "c" | "x" | "v")
    {
        return false;
    }
    cx.key_bindings()
        .borrow()
        .bindings_for_input(std::slice::from_ref(keystroke), &event.context_stack)
        .0
        .is_empty()
}
