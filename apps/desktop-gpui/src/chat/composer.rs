//! The composer under the conversation: the field, the pills that choose the
//! model, effort and permissions, the context meter and Send. Send is Stop
//! while the agent works. A typed `/` opens the provider's commands above it.

use std::rc::Rc;

use desktop_core::connection::chat::{
    ChatCommand, ChatConnection, ChatOptions, ChatPermissionMode, ChatSpeed, ChatTaskKind,
    ChatTurnState, ChatUsage,
};
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    Anchor, AnyElement, App, Context, EntityInputHandler as _, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Pixels, Role, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window,
    accesskit::Toggled,
    component::{
        Disableable, Icon, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Enter, Escape, IndentInline, MoveDown, MoveUp, OutdentInline, Paste, Textarea},
        menu::DropdownMenu,
        popover::Popover,
        tooltip::Tooltip,
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
    icon::{IconName, icon},
    pane::ChatPane,
    slash::{self, Section},
    timeline,
};

/// The rows of the command list on show before it scrolls.
const SLASH_ROWS: f32 = 9.;
const SLASH_ROW_HEIGHT: f32 = 27.;

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
/// `.pill.pending` while the host has not shown the choice yet: dimmed as a
/// message still being sent is.
fn pill(
    id: &'static str,
    label: SharedString,
    name: SharedString,
    off: bool,
    pending: bool,
    p: &Palette,
) -> Button {
    let pill = Button::new(id)
        .label(label)
        // `.pill .ic`: tucked in towards the pill's edge.
        .child(icon(IconName::ChevronDown, 12., p.chrome_dim).mr(ui(-3.)))
        .accessibility_label(name)
        .xsmall()
        .h_auto()
        .px(ui(9.))
        .py(ui(2.))
        .border_1()
        .border_color(p.chrome_border)
        .rounded_full()
        .bg(p.chrome_bg)
        .text_size(ui(12.5))
        .text_color(p.chrome_ink)
        .disabled(off);
    if off {
        pill.opacity(0.42)
    } else if pending {
        pill.opacity(0.6)
    } else {
        pill
    }
}

/// One row of the background list, as it is drawn.
struct TaskRow {
    id: String,
    glyph: IconName,
    description: SharedString,
    /// How long it has run, when the host said when it started.
    time: Option<SharedString>,
    stoppable: bool,
    /// Stop was pressed for it, and it has not left the list yet.
    stopping: bool,
}

/// `.pop .bt`: the background list's rows. A row being stopped is dimmed,
/// as a message being sent is, and its stop button is off.
fn task_list(rows: &[TaskRow], p: &Palette, pane: &WeakEntity<ChatPane>) -> AnyElement {
    let (hover, strong, wash) = (p.chrome_hover, p.chrome_ink_strong, p.accent_wash);
    v_flex()
        .id("chat-tasks-rows")
        .test_support()
        .role(Role::List)
        .aria_label("Background tasks")
        .py(ui(2.))
        .text_size(ui(12.5))
        .children(rows.iter().map(|row| {
            let stop = row.stoppable.then(|| {
                let (id, pane) = (row.id.clone(), pane.clone());
                div()
                    .id(SharedString::from(format!("chat-task-stop-{}", row.id)))
                    .test_support()
                    .role(Role::Button)
                    .aria_label(SharedString::from(format!("Stop {}", row.description)))
                    .flex_none()
                    .size(ui(20.))
                    .mr(ui(-5.))
                    .rounded(ui(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(p.chrome_faint)
                    .child(Icon::new(IconName::X).size(ui(12.)))
                    .when(!row.stopping, |button| {
                        button
                            .cursor_pointer()
                            .tab_index(0)
                            .hover(move |style| style.bg(hover).text_color(strong))
                            .focus_visible(move |style| style.bg(wash).text_color(strong))
                            .tooltip(|window, cx| Tooltip::new("Stop").build(window, cx))
                            .on_click(move |_, _, cx| {
                                pane.update(cx, |pane, cx| pane.stop_task(&id, cx)).ok();
                            })
                    })
            });
            h_flex()
                .id(SharedString::from(format!("chat-task-{}", row.id)))
                .test_support()
                .role(Role::ListItem)
                .aria_label(row.description.clone())
                .h(ui(30.))
                .px(ui(10.))
                .gap(ui(8.))
                .min_w_0()
                .when(row.stopping, |row| row.opacity(0.6))
                .child(icon(row.glyph, 14., p.chrome_faint))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(p.chrome_ink_strong)
                        .child(row.description.clone()),
                )
                .children(row.time.clone().map(|time| {
                    div()
                        .id(SharedString::from(format!("chat-task-time-{}", row.id)))
                        .test_support()
                        .aria_label(time.clone())
                        .flex_none()
                        .text_size(ui(11.5))
                        .text_color(p.chrome_faint)
                        .child(time)
                }))
                .children(stop)
        }))
        .into_any_element()
}

impl ChatPane {
    fn composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.composer.update(cx, |composer, cx| {
            composer.marked_text_range(window, cx).is_some()
        })
    }

    /// Whether Send is Stop: the agent works, and the host can be told.
    pub(super) fn stoppable(&self) -> bool {
        self.turn() == ChatTurnState::Working && self.is_live()
    }

    /// Closes the command list, or else the background list, or else cancels
    /// the edit. False when none was open.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.slash_dismiss(cx) {
            return true;
        }
        if self.tasks_open {
            self.set_tasks_open(false, cx);
            return true;
        }
        if self.editing.is_some() {
            self.cancel_edit(window, cx);
            return true;
        }
        false
    }

    /// Escape anywhere in the chat that nothing inside it took: closes a
    /// list, else cancels the edit, else stops the agent while it works, as
    /// Stop does. Otherwise it goes on to the app.
    pub(super) fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composing(window, cx) || self.dismiss(window, cx) {
            return;
        }
        if self.stoppable() {
            // Once while it stops: a second press sends nothing more.
            self.interrupt(cx);
            cx.notify();
            return;
        }
        cx.propagate();
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
                self.wanted.model(),
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
                self.wanted.effort(),
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
                self.wanted.permission(),
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
                .text_size(ui(12.))
                .when(off, |toggle| toggle.opacity(0.42))
                .when(!off && self.wanted.speed(), |toggle| toggle.opacity(0.6))
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

    /// `.ctx`: how much of the context window is used. The figures are read,
    /// so they are in the dim ink (S8, C1).
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
            .text_size(ui(12.))
            .text_color(p.chrome_dim)
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

    /// `.send`, or `.send.stop` while the agent works: `.send.stop.off` once
    /// pressed, until the turn ends. Send is off while a file uploads, saying
    /// so, and while one has failed (S6).
    fn send_button(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let stop = self.stoppable();
        let off = if stop {
            self.stopping
        } else {
            self.send_refusal().is_some() || self.attachments_refusal().is_some()
        };
        let uploading = !stop && self.uploading_any();
        let ring = p.chrome_ink_strong;
        let (label, ground) = if stop {
            ("Stop", p.danger)
        } else {
            ("Send", p.accent)
        };
        let mark = if stop {
            // Lucide's `square` filled, as `.send .ic.sq` draws it; an SVG
            // here would draw only its outline.
            div()
                .size(ui(10.))
                .rounded(ui(2.))
                .bg(p.chrome_bg)
                .into_any_element()
        } else {
            icon(IconName::ArrowUp, 15., p.chrome_bg).into_any_element()
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
            .flex()
            .items_center()
            .justify_center()
            .child(mark)
            .when(uploading, |button| {
                button.tooltip(|window, cx| Tooltip::new("Uploading…").build(window, cx))
            })
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
                    .text_size(ui(11.5))
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
                            .text_size(ui(13.))
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
                    // What to type after the command is read; the section
                    // heading over it is a label, and stays faint (S8, C1).
                    .when(!command.argument_hint.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(ui(11.5))
                                .text_color(p.chrome_dim)
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
            .text_size(ui(13.5))
            .pb(ui(4.))
            .children(rows)
            .into_any_element()
    }

    /// `.pill.bgt` on the composer's top edge while anything runs outside the
    /// turn, working or idle (D15): it is all that tells an idle chat with a
    /// command still running from a finished one. It opens the list of what
    /// runs, as the model pill opens its own, with a stop button on a row the
    /// agent can stop (D18).
    fn background_chip(&self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tasks = &self.chat.state.background;
        if tasks.is_empty() || !self.chat.is_held() {
            return None;
        }
        let label = timeline::background_label(tasks.len());
        let open = self.tasks_open;
        let ink = if open { p.accent } else { p.chrome_dim };
        let chip = Button::new("chat-tasks")
            .icon(icon(IconName::Layers, 12., ink))
            .label(label.clone())
            .accessibility_label(label)
            .xsmall()
            .h_auto()
            .px(ui(8.))
            .py(ui(1.))
            .gap(ui(5.))
            .border_1()
            .border_color(if open { p.accent } else { p.chrome_border })
            .rounded_full()
            .bg(p.chrome_bg)
            .text_size(ui(11.))
            .text_color(p.chrome_ink);
        let off = self.agent_refusal().is_some();
        let rows: Vec<TaskRow> = tasks
            .iter()
            .map(|task| TaskRow {
                id: task.id.clone(),
                glyph: match ChatTaskKind::try_from(task.kind).unwrap_or_default() {
                    ChatTaskKind::Command => IconName::SquareTerminal,
                    ChatTaskKind::Subagent => IconName::Bot,
                    ChatTaskKind::Other | ChatTaskKind::Unspecified => IconName::Layers,
                },
                description: task.description.clone().into(),
                time: Self::since_unix_millis(task.started_at_unix_millis)
                    .map(|ran| timeline::elapsed(ran).into()),
                stoppable: task.stoppable && !off,
                stopping: self.stopping_tasks.contains_key(&task.id),
            })
            .collect();
        let palette = *p;
        let this = cx.entity().downgrade();
        let opened = this.clone();
        let popover = Popover::new("chat-tasks-list")
            .anchor(Anchor::BottomLeft)
            .open(open)
            .on_open_change(move |open, _, cx| {
                let open = *open;
                opened
                    .update(cx, |pane, cx| pane.set_tasks_open(open, cx))
                    .ok();
            })
            .trigger(chip)
            // `.pop`, as the model pill's list and the command list are.
            .w(ui(300.))
            .p(px(0.))
            .border_1()
            .border_color(p.chrome_border)
            .rounded(ui(9.))
            .bg(p.chrome_raised)
            .shadow_lg()
            .content(move |_, _, _| task_list(&rows, &palette, &this));
        Some(
            div()
                .absolute()
                .top(ui(-11.))
                .left(ui(10.))
                .child(popover)
                .into_any_element(),
        )
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
                .text_size(ui(13.))
                .text_color(p.chrome_ink)
                .child(div().flex_1().min_w_0().child(
                    "Editing an earlier message. Sending removes everything after it. Files on disk stay as they are.",
                ))
                .child(
                    Button::new("chat-edit-cancel")
                        .label("Cancel")
                        .ghost()
                        .xsmall()
                        .text_size(ui(13.))
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
                .text_size(ui(13.))
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
        let chip = self.background_chip(p, cx);
        let has_chip = chip.is_some();
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
            // Copied files and an image become attachments; text is
            // inserted as the field would have.
            .capture_action(cx.listener(|this, _: &Paste, window, cx| {
                if !this.composing(window, cx) {
                    cx.stop_propagation();
                    this.paste(window, cx);
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                // During an IME composition Escape ends the composition, and
                // nothing else: not the list, the edit, or the agent's turn.
                if this.composing(window, cx) {
                    this.composer
                        .update(cx, |composer, cx| composer.unmark_text(window, cx));
                    cx.stop_propagation();
                } else if this.dismiss(window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(
                Textarea::new(&self.composer)
                    .aria_label("Message")
                    .appearance(false)
                    .bordered(false)
                    .px(ui(2.))
                    // `.comp.hasbg .ta`: clear of the chip on the edge.
                    .pt(ui(if has_chip { 15. } else { 10. }))
                    .pb(ui(4.))
                    .text_size(ui(14.5))
                    .text_color(p.chrome_ink_strong),
            );
        div()
            .flex_none()
            .pl(left)
            .pr(right)
            .pb(ui(14.))
            // `.comp.hasbg`: room above for the chip.
            .when(has_chip, |area| area.pt(ui(6.)))
            .child(
                // The chip beside the composer, not in it: GPUI draws a
                // box's border after its children, and would draw the
                // composer's top edge through the chip's label.
                div()
                    .relative()
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
                            .children(self.composer_chips(p, cx))
                            .child(field)
                            .child(bar),
                    )
                    // After the composer, so it is drawn over the edge it sits on.
                    .children(chip),
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
