//! The confirmation and text-entry dialogs, on Kit's `Dialog`, plus the bits the
//! palette and Quick Open share. Kit supplies the focus trap, Escape and the
//! focus return.

use std::{cell::Cell, rc::Rc};

use gpui_kit::{
    AnyView, App, AppContext, Context, Entity, EntityInputHandler, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, Role, SharedString, Styled, Window,
    component::{
        Disableable, WindowExt,
        button::{Button, ButtonVariants},
        dialog::DialogEntrance,
        input::{Escape, Input, InputEvent, InputState},
        v_flex,
    },
    div,
    prelude::*,
};

use crate::theme::{self, metrics::ui};
use crate::ui::dialog::{self, Action};

const CONFIRMATION_WIDTH: f32 = 620.;

pub fn is_composing(input: &Entity<InputState>, window: &mut Window, cx: &mut App) -> bool {
    input.update(cx, |state, cx| {
        state.marked_text_range(window, cx).is_some()
    })
}

/// Escape during an IME composition ends the composition, not the dialog.
pub fn escape_ends_composition(_: &Escape, window: &mut Window, cx: &mut App) {
    let Some(input) = window
        .focused_input(cx)
        .and_then(|input| input.as_input().cloned())
    else {
        return;
    };
    if is_composing(&input, window, cx) {
        input.update(cx, |state, cx| state.unmark_text(window, cx));
        cx.stop_propagation();
    }
}

/// A view in a Kit dialog at palette height. Enter in the `field` runs
/// `on_confirm`; the dialog stays open unless the view closes it. Elsewhere Enter
/// carries on to whatever button holds focus.
pub fn open_overlay(
    view: impl Into<AnyView>,
    width: f32,
    field: gpui_kit::FocusHandle,
    backdrop_closes: bool,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let view = view.into();
    let focus_once = Cell::new(Some(field.clone()));
    let on_confirm = Rc::new(on_confirm);
    window.open_dialog(cx, move |dialog, window, cx| {
        if let Some(focus) = focus_once.take() {
            focus.focus(window, cx);
        }
        let palette = theme::palette(cx);
        let on_confirm = on_confirm.clone();
        let field = field.clone();
        dialog::place(dialog::card(dialog, &palette), window)
            .entrance(DialogEntrance::None)
            .w(ui(width).to_pixels(window.rem_size()))
            .close_button(false)
            .overlay_closable(backdrop_closes)
            .p_0()
            .on_ok(move |_, window, cx| {
                if field.is_focused(window) {
                    on_confirm(window, cx);
                } else {
                    cx.propagate();
                }
                false
            })
            .child(view.clone())
    });
}

type Submit = Rc<dyn Fn(&mut Window, &mut App)>;
type SubmitText = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// A modal yes-or-no. Cancel holds the initial focus, so Enter cancels unless
/// the user moves to the confirming button.
pub struct Confirmation {
    pub title: SharedString,
    pub detail: SharedString,
    pub confirm_label: SharedString,
    pub destructive: bool,
    pub on_confirm: Submit,
}

/// Title and detail for closing something in tmux.
pub fn tmux_confirmation(command_title: &str, target_label: &str) -> (String, String) {
    (
        command_title.replace('…', "?"),
        format!(
            "This will ask tmux to permanently close {target_label}. Running processes in it will be terminated."
        ),
    )
}

pub fn open_confirmation(confirmation: Confirmation, window: &mut Window, cx: &mut App) {
    let cancel_focus = cx.focus_handle();
    let focus_once = Cell::new(Some(cancel_focus.clone()));
    let confirmation = Rc::new(confirmation);
    window.open_alert_dialog(cx, move |alert, window, cx| {
        if let Some(focus) = focus_once.take() {
            focus.focus(window, cx);
        }
        let palette = theme::palette(cx);
        let confirmation = confirmation.clone();
        let confirm = confirmation.clone();
        let button = dialog::button(
            Button::new("confirmation-confirm")
                .label(confirmation.confirm_label.clone())
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    (confirm.on_confirm)(window, cx);
                }),
            if confirmation.destructive {
                Action::Danger
            } else {
                Action::Primary
            },
            false,
            &palette,
        );
        dialog::card(alert, &palette)
            .entrance(DialogEntrance::None)
            .w(ui(CONFIRMATION_WIDTH).to_pixels(window.rem_size()))
            .title(confirmation.title.clone())
            .description(confirmation.detail.clone())
            .on_ok(|_, _, cx| {
                cx.propagate();
                false
            })
            .footer(
                gpui_kit::component::h_flex()
                    .w_full()
                    .justify_end()
                    .gap(ui(8.))
                    .child(dialog::button(
                        Button::new("confirmation-cancel")
                            .label("Cancel")
                            .track_focus(&cancel_focus)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                        Action::Plain,
                        false,
                        &palette,
                    ))
                    .child(button),
            )
    });
}

/// What a [`TextPrompt`] asks.
pub struct TextPromptRequest {
    pub title: SharedString,
    pub label: SharedString,
    pub initial_value: String,
    pub submit: SubmitText,
}

/// One line of text and a Save, used for the rename commands.
pub struct TextPrompt {
    request: TextPromptRequest,
    input: Entity<InputState>,
    _changes: gpui_kit::Subscription,
}

impl TextPrompt {
    fn value(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if is_composing(&self.input, window, cx) {
            return;
        }
        let value = self.value(cx);
        if value.is_empty() {
            return;
        }
        window.close_dialog(cx);
        (self.request.submit)(value, window, cx);
    }
}

pub fn open_text_prompt(request: TextPromptRequest, window: &mut Window, cx: &mut App) {
    let initial_value = request.initial_value.clone();
    let input = cx.new(|cx| InputState::new(window, cx).default_value(initial_value));
    let focus = input.focus_handle(cx);
    let prompt = cx.new(|cx| TextPrompt {
        request,
        _changes: cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }),
        input,
    });
    let view = prompt.clone();
    open_overlay(
        prompt,
        CONFIRMATION_WIDTH,
        focus,
        false,
        move |window, cx| view.update(cx, |prompt, cx| prompt.submit(window, cx)),
        window,
        cx,
    );
}

impl Render for TextPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let empty = self.value(cx).is_empty();
        v_flex()
            .id("text-prompt")
            .role(Role::Group)
            .aria_label(self.request.title.clone())
            .capture_action(escape_ends_composition)
            .p(ui(20.))
            .gap(ui(8.))
            .child(
                div()
                    .text_size(ui(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.request.title.clone()),
            )
            .child(
                div()
                    .text_color(palette.chrome_dim)
                    .child(self.request.label.clone()),
            )
            .child(
                Input::new(&self.input)
                    .id("text-prompt-input")
                    .aria_label(self.request.label.clone()),
            )
            .child(
                gpui_kit::component::h_flex()
                    .justify_end()
                    .gap(ui(8.))
                    .child(
                        Button::new("text-prompt-cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("text-prompt-save")
                            .label("Save")
                            .primary()
                            .disabled(empty)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}
