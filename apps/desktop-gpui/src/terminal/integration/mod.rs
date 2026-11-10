//! T3.6: OSC 52, screenshot and file paste, drag and drop, the transfer
//! surface, pane routing, agent state, and the `TerminalColors` report to the
//! host. The frame gestures (divider drag and keys, pane grab, hover focus,
//! selecting a pane in tmux) live with the frames in `surface.rs`, and their
//! rules in [`frames`].
//!
//! Paste reads the native clipboard through `desktop-core`, as the Tauri app
//! did: it is what turns a copied file into a path and a screenshot into a
//! staged PNG, on Wayland, X11 and macOS alike.

pub mod agents;
pub mod clipboard;
pub mod colors;
pub mod frames;
pub mod routing;
pub mod transfers;
pub mod upload;

use std::rc::Rc;
use std::time::Duration;

use desktop_core::connection::files::native_clipboard::read_native_terminal_clipboard;
use desktop_core::platform;
use gpui_kit::{
    AnyElement, App, Div, Entity, ExternalPaths, InteractiveElement, IntoElement, ParentElement,
    Pixels, Point, Role, SharedString, Stateful, StatefulInteractiveElement, Styled, Window,
    actions,
    component::{
        Sizable,
        button::{Button, ButtonVariants},
    },
    div,
    prelude::FluentBuilder,
};

use crate::commands;
use crate::connection_ui::ConnectionUi;
use crate::notices;
use crate::theme::{
    self,
    metrics::{RADIUS_PANEL, TYPE_SECONDARY, ui},
};

use super::pane::TerminalPane;
use transfers::{InternalPathDrag, NativeClipboard, parse_file_uri_list};
use upload::{Source, Uploads};

actions!(terminal, [Paste]);

/// How often a clipboard read on the runtime is checked for its answer.
const READ_POLL: Duration = Duration::from_millis(20);

/// The group a pane's drop hint answers to.
const DROP_GROUP: &str = "terminal-pane-drop";

pub fn init(cx: &mut App) {
    // The command reaches the focused pane as an action, so it pastes where
    // the keyboard is.
    commands::on_command("terminal.paste", cx, |window, cx| {
        window.dispatch_action(Box::new(Paste), cx);
    });
}

/// This feature's state on a pane.
#[derive(Default)]
pub struct PaneIntegration {
    /// A transfer batch is running; the next waits for it.
    pub busy: bool,
}

/// Text into the pane as a paste. Every paste here, text, paths or a staged
/// image, leaves through this one call.
///
/// How text becomes bytes, bracketed paste included, belongs to the input
/// module (T3.4), and reaches the program through the pane's `send_input`.
/// Until the two branches meet, the text goes as typed, which shells and
/// agents accept for a path.
pub fn paste_text(
    pane: &mut TerminalPane,
    text: String,
    cx: &mut gpui_kit::Context<TerminalPane>,
) -> Result<(), String> {
    pane.send_input(text.into_bytes(), cx)
}

/// Whether a press with the link modifier is on a link, which that modifier
/// opens rather than grabbing the pane. Links are the reading module's
/// (T3.5): until the two branches meet, a terminal shows none.
pub fn link_at(_pane: &TerminalPane, _position: Point<Pixels>) -> bool {
    false
}

/// Adds drop and paste listeners to a pane's root.
pub fn attach(
    element: Stateful<Div>,
    pane: &Entity<TerminalPane>,
    _window: &mut Window,
    _cx: &mut App,
) -> Stateful<Div> {
    let pasted = pane.downgrade();
    let dropped = pane.downgrade();
    let dragged = pane.downgrade();
    element
        .group(DROP_GROUP)
        .on_action(move |_: &Paste, window, cx| {
            if let Some(pane) = pasted.upgrade() {
                paste(&pane, window, cx);
            }
        })
        .on_drop(move |drag: &InternalPathDrag, window, cx| {
            let Some(pane) = dragged.upgrade() else {
                return;
            };
            let scope = upload::scope_of(pane.read(cx), cx);
            let target = scope
                .as_ref()
                .map(|scope| (scope.profile_id.as_str(), scope.server_identity.as_str()));
            let pasted = drag
                .shell_text(target)
                .and_then(|text| pane.update(cx, |pane, cx| paste_text(pane, text, cx)));
            if let Err(error) = pasted {
                notices::notify(&error, window, cx);
            }
        })
        .on_drop(move |paths: &ExternalPaths, window, cx| {
            let Some(pane) = dropped.upgrade() else {
                return;
            };
            let paths: Vec<String> = paths
                .paths()
                .iter()
                .filter_map(|path| path.to_str().map(str::to_owned))
                .collect();
            if paths.is_empty() {
                notices::notify(
                    "The dropped files have no paths this app can read.",
                    window,
                    cx,
                );
                return;
            }
            upload::accept(&pane, Source::Paths(paths), live_scope(), window, cx);
        })
}

fn live_scope() -> upload::ScopeOf {
    Rc::new(upload::scope_of)
}

/// Drawn over the grid: the drop hint while files are dragged over the pane.
pub fn children(
    _pane: &Entity<TerminalPane>,
    _window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let palette = theme::palette(cx);
    vec![
        div()
            .id("terminal-drop-hint")
            .role(Role::Status)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .invisible()
            .bg(palette.accent_wash)
            .border_2()
            .border_color(palette.accent)
            .text_color(palette.accent_ink)
            .group_drag_over::<ExternalPaths>(DROP_GROUP, |style| style.visible())
            .group_drag_over::<InternalPathDrag>(DROP_GROUP, |style| style.visible())
            .child("Drop files to paste paths")
            .into_any_element(),
    ]
}

/// Port of `pasteClipboard`: files become paths, an image a staged PNG, and
/// text is pasted as it is.
fn paste(pane: &Entity<TerminalPane>, window: &mut Window, cx: &mut App) {
    // Read on the runtime and polled: GPUI's test scheduler refuses a wake from
    // another thread, and the read can wait on the app that owns the clipboard.
    let read = platform::spawn(read_native_terminal_clipboard());
    let pane = pane.downgrade();
    window
        .spawn(cx, async move |cx| {
            while !read.is_finished() {
                cx.background_executor().timer(READ_POLL).await;
            }
            let answer = match read.await {
                Ok(answer) => answer,
                Err(error) => Err(format!("native clipboard read failed: {error}")),
            };
            let _ = cx.update(|window, cx| {
                let Some(pane) = pane.upgrade() else {
                    return;
                };
                if let Err(error) = deliver(&pane, answer, window, cx) {
                    notices::notify(&error, window, cx);
                }
            });
        })
        .detach();
}

fn deliver(
    pane: &Entity<TerminalPane>,
    answer: Result<Option<serde_json::Value>, String>,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    let native = answer?.map(NativeClipboard::from_wire).transpose()?;
    let text = match native {
        Some(NativeClipboard::Files { uris }) => {
            let paths = parse_file_uri_list(&uris.join("\n"))?;
            upload::accept(pane, Source::Paths(paths), live_scope(), window, cx);
            return Ok(());
        }
        Some(NativeClipboard::Image { staged }) => {
            let source = Source::StagedImage {
                path: staged.path,
                name: staged.name,
            };
            upload::accept(pane, source, live_scope(), window, cx);
            return Ok(());
        }
        Some(NativeClipboard::Text { text }) if !text.is_empty() => text,
        Some(NativeClipboard::Text { .. }) => return Ok(()),
        // Nothing the native reader knows: what GPUI's own clipboard holds.
        None => match cx.read_from_clipboard().and_then(|item| item.text()) {
            Some(text) if !text.is_empty() => text,
            _ => return Ok(()),
        },
    };
    pane.update(cx, |pane, cx| paste_text(pane, text, cx))
}

/// The upload list, in the terminal's bottom-right corner. Port of
/// `TerminalTransferHistory`.
pub fn upload_list(cx: &mut App) -> Option<AnyElement> {
    let records = cx.try_global::<Uploads>()?.0.records.clone();
    if records.is_empty() {
        return None;
    }
    let palette = theme::palette(cx);
    let rows = records.into_iter().map(|record| {
        let progress = record.progress;
        let id = progress.transfer_id.clone();
        let name = if progress.name.is_empty() {
            transfers::basename(&progress.source_path)
        } else {
            progress.name.clone()
        };
        let state = progress.state;
        let label = SharedString::from(format!("Upload {name}: {}", state.label()));
        let action = if state.finished() {
            let id = id.clone();
            Button::new(SharedString::from(format!("upload-dismiss-{id}")))
                .ghost()
                .small()
                .label("Dismiss")
                .on_click(move |_, _, cx| {
                    cx.default_global::<Uploads>().0.dismiss(&id);
                })
        } else {
            let id = id.clone();
            Button::new(SharedString::from(format!("upload-cancel-{id}")))
                .ghost()
                .small()
                .label("Cancel")
                .on_click(move |_, window, cx| cancel(&id, window, cx))
        };
        div()
            .id(SharedString::from(format!("upload-{id}")))
            .role(Role::Group)
            .aria_label(label)
            .flex()
            .flex_col()
            .gap(ui(2.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap(ui(8.))
                    // A staged screenshot's long name must not push the
                    // state out of the row.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(palette.chrome_ink_strong)
                            .child(name),
                    )
                    .child(state.label()),
            )
            .child(progress.progress_text())
            .when_some(progress.error.clone(), |row, error| {
                row.child(div().text_color(palette.danger_ink).child(error))
            })
            .when_some(progress.cleanup_error.clone(), |row, error| {
                row.child(
                    div()
                        .text_color(palette.danger_ink)
                        .child(format!("Partial cleanup failed: {error}")),
                )
            })
            .child(div().flex().justify_end().child(action))
    });
    Some(
        div()
            .id("terminal-uploads")
            .role(Role::Region)
            .aria_label("Terminal uploads")
            // Over a pane, but its clicks are its own.
            .occlude()
            .absolute()
            .right(ui(12.))
            .bottom(ui(12.))
            .w(ui(300.))
            .max_h(ui(320.))
            .overflow_y_scroll()
            .p(ui(10.))
            .flex()
            .flex_col()
            .gap(ui(10.))
            .rounded(ui(RADIUS_PANEL))
            .border_1()
            .border_color(palette.chrome_border)
            .bg(palette.chrome_raised)
            .text_size(ui(TYPE_SECONDARY))
            .text_color(palette.chrome_ink)
            .children(rows)
            .into_any_element(),
    )
}

fn cancel(transfer_id: &str, window: &mut Window, cx: &mut App) {
    // Every host's link shares one backend, so the active one will do.
    let backend = cx.try_global::<ConnectionUi>().and_then(|ui| {
        let controller = ui.0.read(cx);
        let link = controller.terminal_link(&controller.active_profile_id())?;
        Some(link.read(cx).backend().clone())
    });
    let Some(backend) = backend else {
        return;
    };
    match backend.cancel_upload(transfer_id.to_owned()) {
        // Past the point of no return: the host's verdict is still to come.
        Ok(answer) if answer["disposition"] == "awaitingAuthoritativeOutcome" => {
            cx.default_global::<Uploads>().0.mark_verifying(transfer_id);
        }
        Ok(_) => {}
        Err(error) => {
            notices::notify(&format!("Could not cancel the upload: {error}"), window, cx);
        }
    }
}
