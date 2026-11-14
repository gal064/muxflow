//! The composer under the conversation: the field, the pills that choose the
//! model, effort and permissions, the context meter and Send. Send is Stop
//! while the agent works. A typed `/` opens the provider's commands above it.

use std::rc::Rc;

use desktop_core::connection::chat::{
    ChatCommand, ChatConnection, ChatOptions, ChatPermissionMode, ChatSpeed, ChatTurnState,
    ChatUsage,
};
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    Anchor, AnyElement, App, Context, EntityInputHandler as _, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Pixels, Role, SharedString, StatefulInteractiveElement, Styled,
    Window,
    accesskit::Toggled,
    component::{
        Disableable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Enter, Escape, IndentInline, MoveDown, MoveUp, OutdentInline, Textarea},
        menu::DropdownMenu,
        v_flex,
    },
    div,
    prelude::FluentBuilder,
    px,
};

use crate::{
    commands::dialogs::{Confirmation, open_confirmation},
    context_menu::{Entry, Item, fill},
    fonts,
    theme::{Palette, metrics::ui},
};

use super::{
    pane::ChatPane,
    slash::{self, Section},
};

/// The rows of the command list on show before it scrolls.
const SLASH_ROWS: f32 = 9.;
const SLASH_ROW_HEIGHT: f32 = 26.;

/// `209k / 1M`: a token count as the meter says it.
pub(super) fn tokens(count: u64) -> String {
    let thousands = (count + 500) / 1_000;
    if count < 1_000 {
        return count.to_string();
    }
    if thousands < 1_000 {
        return format!("{thousands}k");
    }
    let tenths = (count + 50_000) / 100_000;
    if tenths.is_multiple_of(10) {
        format!("{}M", tenths / 10)
    } else {
        format!("{}.{}M", tenths / 10, tenths % 10)
    }
}

/// `high` as the pill says it: `High`.
fn capitalized(word: &str) -> String {
    let mut letters = word.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// `.pill`: a choice in the composer's bar, opening its options above.
fn pill(
    id: &'static str,
    label: SharedString,
    name: SharedString,
    off: bool,
    p: &Palette,
) -> Button {
    let pill = Button::new(id)
        .label(format!("{label} \u{25be}"))
        .accessibility_label(name)
        .xsmall()
        .h_auto()
        .px(ui(9.))
        .py(ui(2.))
        .border_1()
        .border_color(p.chrome_border)
        .rounded_full()
        .bg(p.chrome_bg)
        .text_size(ui(11.5))
        .text_color(p.chrome_ink)
        .disabled(off);
    if off { pill.opacity(0.42) } else { pill }
}

impl ChatPane {
    fn composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.composer.update(cx, |composer, cx| {
            composer.marked_text_range(window, cx).is_some()
        })
    }

    /// What the empty field says: what typing there would do now.
    fn placeholder_now(&self) -> &'static str {
        if self.editing.is_some() {
            return "Edit your message";
        }
        match self.send_refusal() {
            None if self.turn() == ChatTurnState::Working => "Send a follow-up while it works",
            None => "Reply, or type / for commands and $ for skills",
            Some(reason) => reason,
        }
    }

    /// Sets one option, leaving the rest as they are.
    fn choose(&mut self, options: ChatOptions, cx: &mut Context<Self>) {
        self.set_options(options, cx);
    }

    fn model_pill(&self, off: bool, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let capabilities = self.capabilities();
        let current = self.options().model;
        if self.chat.catalog.models.is_empty() && current.is_empty() {
            return None;
        }
        let label: SharedString = self
            .model()
            .map(|model| model.label.clone())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| current.clone())
            .into();
        let models: Vec<(String, String)> = self
            .chat
            .catalog
            .models
            .iter()
            .map(|model| (model.id.clone(), model.label.clone()))
            .collect();
        let this = cx.entity().downgrade();
        let off = off || !capabilities.model || models.is_empty();
        Some(
            pill(
                "chat-model",
                label.clone(),
                format!("Model: {label}").into(),
                off,
                p,
            )
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, cx| {
                let entries: Vec<Entry> = models
                    .iter()
                    .map(|(id, label)| {
                        let checked = *id == current;
                        let (id, this) = (id.clone(), this.clone());
                        Item::new(label.clone(), move |_, cx| {
                            let model = id.clone();
                            this.update(cx, |pane, cx| {
                                pane.choose(
                                    ChatOptions {
                                        model,
                                        ..Default::default()
                                    },
                                    cx,
                                );
                            })
                            .ok();
                        })
                        .checked(checked)
                        .into()
                    })
                    .collect();
                fill(menu, &entries, window, cx)
            })
            .into_any_element(),
        )
    }

    fn effort_pill(&self, off: bool, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let efforts = self.model()?.efforts.clone();
        if efforts.is_empty() || !self.capabilities().effort {
            return None;
        }
        let current = self.options().effort;
        let label: SharedString = if current.is_empty() {
            "Effort".into()
        } else {
            capitalized(&current).into()
        };
        let this = cx.entity().downgrade();
        Some(
            pill(
                "chat-effort",
                label.clone(),
                format!("Effort: {label}").into(),
                off,
                p,
            )
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, cx| {
                let entries: Vec<Entry> = efforts
                    .iter()
                    .map(|effort| {
                        let checked = *effort == current;
                        let (effort, this) = (effort.clone(), this.clone());
                        Item::new(capitalized(&effort), move |_, cx| {
                            let effort = effort.clone();
                            this.update(cx, |pane, cx| {
                                pane.choose(
                                    ChatOptions {
                                        effort,
                                        ..Default::default()
                                    },
                                    cx,
                                );
                            })
                            .ok();
                        })
                        .checked(checked)
                        .into()
                    })
                    .collect();
                fill(menu, &entries, window, cx)
            })
            .into_any_element(),
        )
    }

    fn permission_pill(
        &self,
        off: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.capabilities().permission {
            return None;
        }
        let current = ChatPermissionMode::try_from(self.options().permission).unwrap_or_default();
        let label: SharedString = match current {
            ChatPermissionMode::Full => "Full",
            ChatPermissionMode::Auto => "Auto",
            ChatPermissionMode::Unspecified => "Permissions",
        }
        .into();
        let this = cx.entity().downgrade();
        let set = move |mode: ChatPermissionMode, cx: &mut App| {
            this.update(cx, |pane, cx| {
                pane.choose(
                    ChatOptions {
                        permission: mode.into(),
                        ..Default::default()
                    },
                    cx,
                );
            })
            .ok();
        };
        Some(
            pill(
                "chat-permission",
                label.clone(),
                format!("Permissions: {label}").into(),
                off,
                p,
            )
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, cx| {
                let auto = {
                    let set = set.clone();
                    Item::new(
                        "Auto: the provider approves actions it judges safe",
                        move |_, cx| set(ChatPermissionMode::Auto, cx),
                    )
                    .checked(current == ChatPermissionMode::Auto)
                };
                let full = {
                    let set = set.clone();
                    Item::new("Full: no checks and no sandbox", move |window, cx| {
                        if current == ChatPermissionMode::Full {
                            return;
                        }
                        // One slip in a menu must not take every check away.
                        let set = set.clone();
                        open_confirmation(
                            Confirmation {
                                title: "Run with no checks and no sandbox?".into(),
                                detail: "In Full mode the agent runs commands and changes \
                                         files without asking, from its next message on."
                                    .into(),
                                confirm_label: "Use Full".into(),
                                destructive: true,
                                on_confirm: Rc::new(move |_, cx| {
                                    set(ChatPermissionMode::Full, cx);
                                }),
                            },
                            window,
                            cx,
                        );
                    })
                    .checked(current == ChatPermissionMode::Full)
                };
                fill(menu, &[auto.into(), full.into()], window, cx)
            })
            .into_any_element(),
        )
    }

    /// `.seg`: Standard or Fast, for a model that has a fast mode.
    fn speed_toggle(&self, off: bool, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.capabilities().speed || !self.model()?.supports_fast {
            return None;
        }
        let fast = self.options().speed == i32::from(ChatSpeed::Fast);
        let (strong, hover) = (p.chrome_ink_strong, p.chrome_hover);
        let half = |id: &'static str, label: &'static str, speed: ChatSpeed, on: bool| {
            div()
                .id(id)
                .test_support()
                .role(Role::RadioButton)
                .aria_label(label)
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .px(ui(8.))
                .py(ui(2.))
                .text_color(if on { p.accent } else { p.chrome_dim })
                .when(on, |half| half.bg(p.accent_wash))
                .when(!off && !on, |half| {
                    half.cursor_pointer()
                        .tab_index(0)
                        .focus_visible(move |style| style.bg(hover).text_color(strong))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choose(
                                ChatOptions {
                                    speed: speed.into(),
                                    ..Default::default()
                                },
                                cx,
                            );
                        }))
                })
                .child(label)
        };
        Some(
            h_flex()
                .id("chat-speed")
                .test_support()
                .role(Role::RadioGroup)
                .aria_label("Speed")
                .flex_none()
                .border_1()
                .border_color(p.chrome_border)
                .rounded(ui(5.))
                .overflow_hidden()
                .text_size(ui(11.))
                .when(off, |toggle| toggle.opacity(0.42))
                .child(half(
                    "chat-speed-standard",
                    "Standard",
                    ChatSpeed::Standard,
                    !fast,
                ))
                .child(half("chat-speed-fast", "Fast", ChatSpeed::Fast, fast))
                .into_any_element(),
        )
    }

    /// `.ctx`: how much of the context window is used.
    fn meter(usage: &ChatUsage, p: &Palette) -> AnyElement {
        let share = if usage.window_tokens == 0 {
            0.
        } else {
            (usage.used_tokens as f32 / usage.window_tokens as f32).clamp(0., 1.)
        };
        let text = format!(
            "{} / {}",
            tokens(usage.used_tokens),
            tokens(usage.window_tokens)
        );
        h_flex()
            .id("chat-context")
            .test_support()
            .role(Role::Meter)
            .aria_label(SharedString::from(format!("Context used: {text}")))
            .flex_none()
            .gap(ui(6.))
            .text_size(ui(11.))
            .text_color(p.chrome_faint)
            .child(
                div()
                    .w(ui(54.))
                    .h(ui(4.))
                    .rounded(ui(2.))
                    .bg(p.chrome_hairline)
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .w(gpui_kit::relative(share))
                            .bg(if share >= 0.9 { p.warn } else { p.chrome_dim }),
                    ),
            )
            .child(text)
            .into_any_element()
    }

    /// `.send`, or `.send.stop` while the agent works.
    fn send_button(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let stop = self.turn() == ChatTurnState::Working && self.is_live();
        let off = !stop && self.send_refusal().is_some();
        let ring = p.chrome_ink_strong;
        let (label, glyph, ground) = if stop {
            ("Stop", "\u{25a0}", p.danger)
        } else {
            ("Send", "\u{2191}", p.accent)
        };
        div()
            .id("chat-send")
            .test_support()
            .role(Role::Button)
            .aria_label(label)
            .flex_none()
            .size(ui(26.))
            .rounded(ui(7.))
            .bg(ground)
            .text_color(p.chrome_bg)
            .text_size(ui(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .flex()
            .items_center()
            .justify_center()
            .child(glyph)
            .map(|button| {
                if off {
                    button.opacity(0.42)
                } else {
                    button
                        .cursor_pointer()
                        .tab_index(0)
                        .border_1()
                        .border_color(ground)
                        .focus_visible(move |style| style.border_color(ring))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if stop {
                                this.interrupt(cx);
                            } else {
                                this.submit(window, cx);
                            }
                            // The next thing typed goes on to the agent.
                            window.focus(&this.input_focus(cx), cx);
                        }))
                }
            })
            .into_any_element()
    }

    /// `.pop`: the commands a typed `/word` or `$word` matches, in their
    /// sections, named with the sigil they were asked for with.
    fn slash_list(
        &self,
        sigil: char,
        suggestions: &[ChatCommand],
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.slash_active.min(suggestions.len().saturating_sub(1));
        let mut section: Option<Section> = None;
        let rows = suggestions.iter().enumerate().map(|(index, command)| {
            let here = slash::section(command);
            let heading = (section != Some(here)).then(|| {
                div()
                    .px(ui(10.))
                    .pt(ui(7.))
                    .pb(ui(3.))
                    .text_size(ui(10.5))
                    .text_color(p.chrome_faint)
                    .child(here.title().to_uppercase())
            });
            section = Some(here);
            let on = index == active;
            v_flex().children(heading).child(
                h_flex()
                    .id(("chat-command", index))
                    .test_support()
                    .role(Role::ListBoxOption)
                    .aria_label(SharedString::from(format!("{sigil}{}", command.name)))
                    .aria_selected(on)
                    .items_baseline()
                    .gap(ui(10.))
                    .px(ui(10.))
                    .py(ui(5.))
                    .min_w_0()
                    .cursor_pointer()
                    .when(on, |row| row.bg(p.accent_wash))
                    .child(
                        div()
                            .flex_none()
                            .font_family(fonts::MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(ui(12.))
                            .text_color(p.chrome_ink_strong)
                            .child(SharedString::from(format!("{sigil}{}", command.name))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(p.chrome_dim)
                            .child(SharedString::from(command.description.clone())),
                    )
                    .when(!command.argument_hint.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(ui(10.5))
                                .text_color(p.chrome_faint)
                                .child(SharedString::from(command.argument_hint.clone())),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.slash_accept(Some(index), window, cx);
                    })),
            )
        });
        div()
            .id("chat-commands")
            .test_support()
            .role(Role::ListBox)
            .aria_label("Commands")
            .absolute()
            .bottom_full()
            .left_0()
            .right_0()
            .mb(ui(6.))
            // Over the conversation: a press here is not one on a message.
            .occlude()
            .max_h(ui(SLASH_ROWS * SLASH_ROW_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&self.slash_scroll)
            .border_1()
            .border_color(p.chrome_border)
            .rounded(ui(9.))
            .bg(p.chrome_raised)
            .shadow_lg()
            .text_size(ui(12.5))
            .pb(ui(4.))
            .children(rows)
            .into_any_element()
    }

    /// `.comp`: the composer, in the conversation's column.
    pub(super) fn composer_area(
        &mut self,
        left: Pixels,
        right: Pixels,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let placeholder = self.placeholder_now();
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            self.composer.update(cx, |composer, cx| {
                composer.set_placeholder(placeholder, window, cx);
            });
        }
        // An agent that is not running takes no options either.
        let off =
            !self.is_live() || !self.chat.is_held() || self.connection() != ChatConnection::Ready;
        let (sigil, suggestions) = self.slash_suggestions(cx);
        let banner = self.editing.is_some().then(|| {
            // `.banner`.
            h_flex()
                .id("chat-editing")
                .test_support()
                .role(Role::Status)
                .aria_label("Editing an earlier message")
                .gap(ui(8.))
                .px(ui(12.))
                .py(ui(6.))
                .rounded_t(ui(10.))
                .border_b_1()
                .border_color(p.chrome_hairline)
                .bg(p.accent_wash)
                .text_size(ui(12.))
                .text_color(p.chrome_ink)
                .child(div().flex_1().min_w_0().child(
                    "Editing an earlier message. Sending removes everything after it. Files on disk stay as they are.",
                ))
                .child(
                    Button::new("chat-edit-cancel")
                        .label("Cancel")
                        .ghost()
                        .xsmall()
                        .text_size(ui(12.))
                        .text_color(p.chrome_dim)
                        .on_click(cx.listener(|this, _, window, cx| this.cancel_edit(window, cx))),
                )
        });
        let said = self
            .notice
            .clone()
            .map(|notice| (notice, p.danger_ink, Role::Alert))
            .or_else(|| {
                let ink = if self.connection() == ChatConnection::Failed {
                    p.danger_ink
                } else {
                    p.chrome_dim
                };
                self.agent_status()
                    .map(|status| (status, ink, Role::Status))
            });
        let notice = said.map(|(text, ink, role)| {
            div()
                .id("chat-notice")
                .test_support()
                .role(role)
                .aria_label(text.clone())
                .px(ui(12.))
                .pt(ui(8.))
                .text_size(ui(12.))
                .text_color(ink)
                .child(text)
        });
        let usage = self
            .chat
            .state
            .usage
            .as_ref()
            .filter(|usage| usage.window_tokens > 0)
            .map(|usage| Self::meter(usage, p));
        // In a pane too narrow for one row the bar wraps; the meter and
        // Send stay together at the right of the row they land on.
        let bar = h_flex()
            .flex_wrap()
            .gap(ui(6.))
            .px(ui(8.))
            .pt(ui(6.))
            .pb(ui(8.))
            .children(self.model_pill(off, p, cx))
            .children(self.effort_pill(off, p, cx))
            .children(self.permission_pill(off, p, cx))
            .children(self.speed_toggle(off, p, cx))
            .child(
                h_flex()
                    .flex_none()
                    .ml_auto()
                    .gap(ui(6.))
                    .children(usage)
                    .child(self.send_button(p, cx)),
            );
        let open = !suggestions.is_empty();
        let field = div()
            // Ahead of the field: these keys belong to the command list while
            // it is open, and Escape to the edit being made.
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                if !this.composing(window, cx) && this.slash_step(1, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                if !this.composing(window, cx) && this.slash_step(-1, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if !action.shift
                    && !this.composing(window, cx)
                    && this.slash_accept(None, window, cx)
                {
                    cx.stop_propagation();
                }
            }))
            // Tab takes a command, and otherwise leaves the field for the next
            // control, as it leaves a page's text area.
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                if this.composing(window, cx) {
                    return;
                }
                if !this.slash_accept(None, window, cx) {
                    window.focus_next(cx);
                }
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &OutdentInline, window, cx| {
                if !this.composing(window, cx) {
                    window.focus_prev(cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.composing(window, cx) {
                    return;
                }
                if this.slash_dismiss(cx) {
                    cx.stop_propagation();
                } else if this.editing.is_some() {
                    this.cancel_edit(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                Textarea::new(&self.composer)
                    .aria_label("Message")
                    .appearance(false)
                    .bordered(false)
                    .px(ui(2.))
                    .pt(ui(10.))
                    .pb(ui(4.))
                    .text_size(ui(13.5))
                    .text_color(p.chrome_ink_strong),
            );
        div()
            .flex_none()
            .pl(left)
            .pr(right)
            .pb(ui(14.))
            .child(
                div()
                    .id("chat-composer")
                    .test_support()
                    .role(Role::Group)
                    .aria_label("Composer")
                    .relative()
                    .border_1()
                    .border_color(p.chrome_border)
                    .rounded(ui(11.))
                    .bg(p.chrome_bg)
                    // `box-shadow: 0 1px 0 var(--hair)`.
                    .shadow(vec![gpui_kit::BoxShadow::new(
                        px(0.),
                        px(1.),
                        p.chrome_hairline.into(),
                    )])
                    .when(open, |composer| {
                        composer.child(self.slash_list(sigil, &suggestions, p, cx))
                    })
                    .children(banner)
                    .children(notice)
                    .child(field)
                    .child(bar),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_read_as_the_meter_says_them() {
        assert_eq!(tokens(0), "0");
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(209_000), "209k");
        assert_eq!(tokens(209_499), "209k");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(1_250_000), "1.3M");
        assert_eq!(tokens(200_000), "200k");
        assert_eq!(tokens(999_600), "1M");
    }

    #[test]
    fn an_effort_is_named_with_a_capital() {
        assert_eq!(capitalized("high"), "High");
        assert_eq!(capitalized("xhigh"), "Xhigh");
        assert_eq!(capitalized(""), "");
    }
}
