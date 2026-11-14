//! The create, rename, move, duplicate and delete dialog. Port of
//! `ExplorerMutationDialog.tsx`.

use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, Role, StatefulInteractiveElement, Styled, WeakEntity, Window,
    component::{
        WindowExt,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    div,
    prelude::FluentBuilder,
};

use crate::{
    commands::dialogs::{escape_ends_composition, is_composing, open_overlay},
    theme::{
        self,
        metrics::{TYPE_META, ui},
    },
    ui::surface_error::SurfaceError,
};

use super::{
    explorer::{Explorer, ExplorerEvent},
    view::MutationAction,
    wire::{ActiveRoot, FileEntry, FileKind, FileMutation, Relocation},
};

const WIDTH: f32 = 460.;
const CANCELLED: &str = "This file action was cancelled because its host or active root changed.";

pub struct MutationDialog {
    explorer: WeakEntity<Explorer>,
    action: MutationAction,
    entry: Option<FileEntry>,
    /// The root and connection the action was started under; a change while
    /// somebody types must not apply it to a different tree.
    root_token: String,
    scope_identity: String,
    pub(super) input: Entity<InputState>,
    overwrite: bool,
    non_empty: bool,
    pub(super) error: Option<String>,
    /// Where the keyboard starts: the field, or for Delete, which has none,
    /// a handle the form itself tracks.
    focus: gpui_kit::FocusHandle,
    _changes: gpui_kit::Subscription,
    _tree: gpui_kit::Subscription,
}

impl MutationDialog {
    fn replaceable(&self) -> bool {
        matches!(
            self.action,
            MutationAction::Rename | MutationAction::Move | MutationAction::Duplicate
        )
    }

    /// The mutation the form describes, or `None` when there is nothing to do.
    pub fn mutation(
        action: MutationAction,
        entry: Option<&FileEntry>,
        root: &ActiveRoot,
        value: &str,
        overwrite: bool,
        non_empty: bool,
    ) -> Option<FileMutation> {
        let named = value.trim().to_owned();
        match action {
            MutationAction::NewFile | MutationAction::NewDirectory => {
                if named.is_empty() {
                    return None;
                }
                let parent = entry
                    .filter(|entry| entry.kind == FileKind::Directory)
                    .map_or_else(|| root.path.clone(), |entry| entry.path.clone());
                Some(if action == MutationAction::NewFile {
                    FileMutation::CreateFile {
                        parent,
                        name: named,
                    }
                } else {
                    FileMutation::CreateDirectory {
                        parent,
                        name: named,
                    }
                })
            }
            MutationAction::Delete => entry.map(|entry| FileMutation::Delete {
                path: entry.path.clone(),
                confirmed_non_empty: entry.kind == FileKind::Directory,
            }),
            MutationAction::Rename | MutationAction::Move | MutationAction::Duplicate => {
                let entry = entry.filter(|_| !named.is_empty())?;
                let relocation = Relocation {
                    path: entry.path.clone(),
                    destination: named,
                    overwrite,
                    confirmed_non_empty: non_empty,
                };
                Some(match action {
                    MutationAction::Rename => FileMutation::Rename(relocation),
                    MutationAction::Move => FileMutation::Move(relocation),
                    _ => FileMutation::Duplicate(relocation),
                })
            }
        }
    }

    pub(super) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if is_composing(&self.input, window, cx) {
            return;
        }
        let Some(explorer) = self.explorer.upgrade() else {
            return;
        };
        let (root, scope_identity, disabled) = {
            let explorer = explorer.read(cx);
            (
                explorer.tree.root.clone(),
                explorer.scope_identity(),
                !explorer.inputs.can_mutate,
            )
        };
        let Some(root) = root else {
            return;
        };
        if self.root_token != root.token || self.scope_identity != scope_identity || disabled {
            self.error = Some(CANCELLED.into());
            cx.notify();
            return;
        }
        let value = self.input.read(cx).value().to_string();
        let Some(mutation) = Self::mutation(
            self.action,
            self.entry.as_ref(),
            &root,
            &value,
            self.overwrite,
            self.non_empty,
        ) else {
            window.close_dialog(cx);
            return;
        };
        let task = explorer.update(cx, |explorer, cx| explorer.mutate(mutation, cx));
        let weak_explorer = self.explorer.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let shown = this
                .update_in(cx, |this, window, cx| match &result {
                    Ok(()) => window.close_dialog(cx),
                    Err(error) => {
                        this.error = Some(error.clone());
                        cx.notify();
                    }
                })
                .is_ok();
            // A dismissed dialog can no longer show the failure.
            if !shown && let Err(error) = result {
                weak_explorer
                    .update(cx, |_, cx| cx.emit(ExplorerEvent::Status(error)))
                    .ok();
            }
        })
        .detach();
    }
}

/// Opens the dialog for `action` on `entry`. The overlay owns it; a caller
/// keeps at most a weak handle.
pub fn open(
    explorer: Entity<Explorer>,
    action: MutationAction,
    entry: Option<FileEntry>,
    root: ActiveRoot,
    scope_identity: String,
    window: &mut Window,
    cx: &mut App,
) -> Entity<MutationDialog> {
    let initial = match action {
        MutationAction::Rename | MutationAction::Duplicate => entry
            .as_ref()
            .map(|entry| entry.path.clone())
            .unwrap_or_default(),
        _ => String::new(),
    };
    let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
    // Delete has nothing to type, and Enter must not delete on its own.
    let focus = if action == MutationAction::Delete {
        cx.focus_handle()
    } else {
        input.focus_handle(cx)
    };
    let dialog = cx.new(|cx: &mut Context<MutationDialog>| MutationDialog {
        explorer: explorer.downgrade(),
        action,
        entry,
        root_token: root.token,
        scope_identity,
        focus: focus.clone(),
        _changes: cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }),
        // A different root or connection closes the form, as the Tauri tree
        // drops its pending action; submit's own check covers the race.
        _tree: cx.observe_in(&explorer, window, |this, explorer, window, cx| {
            let explorer = explorer.read(cx);
            let token = explorer.tree.root.as_ref().map(|root| root.token.as_str());
            if token != Some(this.root_token.as_str())
                || explorer.scope_identity() != this.scope_identity
            {
                window.close_dialog(cx);
            }
        }),
        input,
        overwrite: false,
        non_empty: false,
        error: None,
    });
    let view = dialog.clone();
    open_overlay(
        dialog.clone(),
        WIDTH,
        focus,
        false,
        move |window, cx| {
            if action != MutationAction::Delete {
                view.update(cx, |dialog, cx| dialog.submit(window, cx));
            }
        },
        window,
        cx,
    );
    dialog
}

impl Render for MutationDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let destructive = self.action == MutationAction::Delete;
        let replaceable = self.replaceable();
        let field_label = match self.action {
            MutationAction::NewFile | MutationAction::NewDirectory => "Name",
            _ => "Destination path",
        };
        let body = if destructive {
            let path = self
                .entry
                .as_ref()
                .map(|entry| entry.path.clone())
                .unwrap_or_default();
            let directory = self
                .entry
                .as_ref()
                .is_some_and(|entry| entry.kind == FileKind::Directory);
            // One sentence that wraps as text does, so the path is never cut
            // off and the question mark stays with it.
            div()
                .w_full()
                .child(format!(
                    "Delete {path}?{}",
                    if directory {
                        " Non-empty directories require this confirmation."
                    } else {
                        ""
                    }
                ))
                .into_any_element()
        } else {
            v_flex()
                .gap(ui(8.))
                .child(
                    div()
                        .text_size(ui(TYPE_META))
                        .text_color(palette.chrome_dim)
                        .child(field_label),
                )
                .child(
                    Input::new(&self.input)
                        .id("explorer-mutation-field")
                        .aria_label(field_label),
                )
                .into_any_element()
        };
        v_flex()
            .id("explorer-mutation")
            .role(Role::Dialog)
            .aria_label(self.action.label())
            // Only Delete's own handle: a field's handle is tracked by its
            // input, and a second element claiming it breaks accessibility focus.
            .when(destructive, |this| this.track_focus(&self.focus))
            .capture_action(escape_ends_composition)
            .p(ui(20.))
            .gap(ui(12.))
            .child(
                div()
                    .text_size(ui(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.action.label()),
            )
            .child(body)
            .when(replaceable, |this| {
                this.child(
                    Checkbox::new("explorer-mutation-overwrite")
                        .label("Allow overwrite after confirmation")
                        .checked(self.overwrite)
                        .on_change(cx.listener(|this, checked: &bool, _, cx| {
                            this.overwrite = *checked;
                            if !this.overwrite {
                                this.non_empty = false;
                            }
                            cx.notify();
                        })),
                )
            })
            .when(replaceable && self.overwrite, |this| {
                this.child(
                    Checkbox::new("explorer-mutation-non-empty")
                        .label("Also replace a non-empty destination directory")
                        .checked(self.non_empty)
                        .on_change(cx.listener(|this, checked: &bool, _, cx| {
                            this.non_empty = *checked;
                            cx.notify();
                        })),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(SurfaceError::new("explorer-mutation-error", error))
            })
            .child(
                h_flex()
                    .mt(ui(8.))
                    .justify_end()
                    .gap(ui(8.))
                    .child(
                        Button::new("explorer-mutation-cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("explorer-mutation-apply")
                            .label(if destructive { "Delete" } else { "Apply" })
                            .map(|button| {
                                if destructive {
                                    button.danger()
                                } else {
                                    button.primary()
                                }
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}
