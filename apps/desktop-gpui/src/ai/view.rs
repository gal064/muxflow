//! The assistant as drawn: the transcript, the approval cards, and the compose
//! box. Port of the render half of `AiPanel.tsx`; the state and the run loop
//! are in [`super::panel`].
//!
//! The React version ran answers through `renderSafeMarkdown`; the answer here
//! is drawn as its text. A model's Markdown without the webview's sanitizer is
//! formatting the app would have to trust unseen, and the plain text of an
//! answer is what the conversation actually is.

use gpui_kit::{
    AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Role,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    component::{
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        h_flex,
        input::{InputEvent, Textarea, TextareaState},
        v_flex,
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::theme::{
    self, Palette,
    metrics::{RADIUS_CONTROL, TYPE_LABEL, TYPE_META, TYPE_SECONDARY, ui},
};

use super::panel::AiPanel;
use super::store::AiTurn;
use super::tools::approval_title;

/// The compose box, made with the first frame: like the Automations panel's
/// fields, the inputs need a window.
pub(super) struct Fields {
    question: Entity<TextareaState>,
    _changes: Subscription,
}

impl Render for AiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A closed tab's thread goes the moment its absence is drawn.
        self.prune(cx);
        if self.fields.is_none() {
            let state = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, 6)
                    .submit_on_enter(true)
            });
            let changes = cx.subscribe_in(
                &state,
                window,
                |panel: &mut AiPanel, input, event, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                    if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                        let text = input.read(cx).value().to_string();
                        if text.trim().is_empty() {
                            return;
                        }
                        input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
                        panel.send(text, cx);
                    }
                },
            );
            self.fields = Some(Fields {
                question: state,
                _changes: changes,
            });
        }

        let p = theme::palette(cx);
        let key = self.current_key(cx);
        let configured = AiPanel::settings(cx).configured();
        let no_host = self.controller.read(cx).active_link().is_none();
        let streaming = key.as_deref().is_some_and(|key| {
            self.conversations()
                .read(key)
                .is_some_and(|entry| entry.streaming)
        });
        let entry = key
            .as_deref()
            .and_then(|key| self.conversations().read(key));
        let error = entry.and_then(|entry| entry.error.clone());
        let activity = entry.and_then(|entry| entry.activity.clone());
        let continuation_rounds = entry
            .and_then(|entry| entry.continuation.as_ref())
            .map(|waiting| waiting.rounds_used);
        let turns: Vec<AiTurn> = entry.map_or_else(Vec::new, |entry| entry.turns.clone());
        let pending: Vec<Pending> = entry.map_or_else(Vec::new, |entry| {
            entry
                .pending
                .iter()
                .map(|approval| Pending {
                    id: approval.id.clone(),
                    title: approval_title(&approval.name).to_owned(),
                    summary: approval.summary.clone(),
                    detail: approval.detail.clone(),
                    expanded: self.is_expanded(&approval.id),
                })
                .collect()
        });
        let gathering = self.gathering();
        let sources = self.sources().to_vec();

        // Keep the newest turn in view: the newest turn is the reason to have
        // scrolled.
        let mark = (turns.len(), turns.last().map_or(0, |turn| turn.text.len()));
        if self.transcript_mark != Some(mark) {
            self.transcript_mark = Some(mark);
            self.scroll.scroll_to_bottom();
        }

        let this = cx.entity().downgrade();
        let header =
            h_flex()
                .gap(ui(8.))
                .items_center()
                .flex_wrap()
                .child(
                    div()
                        .text_size(ui(TYPE_LABEL))
                        .text_color(p.chrome_dim)
                        .child("Ask about"),
                )
                .child(
                    div()
                        .id("ai-sources")
                        .role(Role::Group)
                        .aria_label("What the assistant is shown")
                        .child(h_flex().gap(ui(8.)).children(
                            super::context::AI_SOURCES.iter().map(|source| {
                                let checked = sources.contains(source);
                                let source = *source;
                                let toggle = this.clone();
                                Checkbox::new(SharedString::from(format!(
                                    "ai-source-{}",
                                    source.id()
                                )))
                                .label(source.label())
                                .checked(checked)
                                .on_change(move |_, _, cx| {
                                    let _ = toggle.update(cx, |panel, cx| {
                                        panel.toggle_source(source);
                                        cx.notify();
                                    });
                                })
                            }),
                        )),
                )
                .when(!turns.is_empty() && !streaming && key.is_some(), |row| {
                    let clear = this.clone();
                    let clear_key = key.clone().unwrap_or_default();
                    row.child(
                        div()
                            .ml_auto()
                            .id("ai-clear")
                            .text_size(ui(TYPE_LABEL))
                            .text_color(p.chrome_dim)
                            .cursor_pointer()
                            .child("Clear")
                            .on_click(move |_, _, cx| {
                                let _ = clear.update(cx, |panel, cx| panel.clear(&clear_key, cx));
                            }),
                    )
                });

        let setup_note = (!configured && !no_host).then(|| {
            v_flex()
                .gap(ui(4.))
                .p(ui(8.))
                .rounded(px(RADIUS_CONTROL))
                .bg(p.chrome_raised)
                .child(
                    div()
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(p.chrome_ink)
                        .child("No model is configured"),
                )
                .child(
                    div()
                        .text_size(ui(TYPE_META))
                        .text_color(p.chrome_dim)
                        .child(
                            "Set an Endpoint and a Model under Settings → Assistant. \
                             Any OpenAI-compatible server answers — Ollama, MLX, vLLM, or a hosted API.",
                        ),
                )
        });
        let error_note = error.map(|message| {
            div()
                .px(ui(8.))
                .py(ui(6.))
                .rounded(px(RADIUS_CONTROL))
                .bg(p.danger_wash)
                .text_size(ui(TYPE_META))
                .text_color(p.danger_ink)
                .child(message)
        });

        let body: gpui_kit::AnyElement = if no_host {
            empty_note("No host is shown.", &p).into_any_element()
        } else if turns.is_empty() {
            empty_note(
                if key.is_some() {
                    "Nothing asked yet. The assistant is shown what the Session, Files, Git, Ports and Automations tabs hold, and this tab keeps its own thread."
                } else {
                    "Open a tab — each tab of the strip keeps its own conversation."
                },
                &p,
            )
            .into_any_element()
        } else {
            let rows = turns.iter().enumerate().map(|(index, turn)| {
                let thinking = streaming && index + 1 == turns.len() && pending.is_empty();
                turn_bubble(turn, thinking, &p)
            });
            div()
                .id("ai-transcript")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(v_flex().gap(ui(6.)).children(rows))
                .into_any_element()
        };

        let cards = v_flex()
            .gap(ui(6.))
            .children(pending.into_iter().map(|card| {
                let expand_panel = this.clone();
                let expand_id = card.id.clone();
                let summary = if card.expanded {
                    card.detail.clone().unwrap_or_else(|| card.summary.clone())
                } else {
                    match &card.detail {
                        Some(_) => format!("{} …", card.summary),
                        None => card.summary.clone(),
                    }
                };
                let allow_id = card.id.clone();
                let deny_id = card.id.clone();
                v_flex()
                    .gap(ui(6.))
                    .p(ui(8.))
                    .rounded(px(RADIUS_CONTROL))
                    .border_1()
                    .border_color(p.chrome_border)
                    .bg(p.chrome_raised)
                    .child(
                        div()
                            .text_size(ui(TYPE_SECONDARY))
                            .text_color(p.chrome_ink_strong)
                            .child(card.title),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("ai-approval-{}", card.id)))
                            .text_size(ui(TYPE_META))
                            .text_color(p.chrome_dim)
                            .text_ellipsis()
                            .when(card.detail.is_some(), |row| row.cursor_pointer())
                            .child(summary)
                            .on_click(move |_, _, cx| {
                                let _ = expand_panel.update(cx, |panel, cx| {
                                    panel.toggle_expanded(&expand_id);
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        h_flex()
                            .gap(ui(6.))
                            .child(
                                Button::new(SharedString::from(format!("ai-allow-{allow_id}")))
                                    .primary()
                                    .label("Allow")
                                    .on_click(cx.listener(move |panel, _, _, cx| {
                                        let key = panel.current_key(cx).unwrap_or_default();
                                        panel.resolve_approval(&key, &allow_id, true, cx);
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("ai-deny-{deny_id}")))
                                    .outline()
                                    .label("Deny")
                                    .on_click(cx.listener(move |panel, _, _, cx| {
                                        let key = panel.current_key(cx).unwrap_or_default();
                                        panel.resolve_approval(&key, &deny_id, false, cx);
                                    })),
                            ),
                    )
            }));

        let continuation = continuation_rounds.map(|rounds| {
            v_flex()
                .gap(ui(6.))
                .p(ui(8.))
                .rounded(px(RADIUS_CONTROL))
                .border_1()
                .border_color(p.chrome_border)
                .bg(p.chrome_raised)
                .child(
                    div()
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(p.chrome_ink_strong)
                        .child("This is taking a while"),
                )
                .child(
                    div()
                        .text_size(ui(TYPE_META))
                        .text_color(p.chrome_dim)
                        .child(format!(
                            "The assistant has spent its {rounds} rounds that ask to type or run something, \
                             without finishing its answer. Continue spends {rounds} more on the same run; \
                             reading the screen stays free."
                        )),
                )
                .child(
                    h_flex()
                        .gap(ui(6.))
                        .child(
                            Button::new("ai-continue")
                                .primary()
                                .label("Continue")
                                .on_click(cx.listener(move |panel, _, _, cx| {
                                    if let Some(key) = panel.current_key(cx) {
                                        panel.continue_run(&key, cx);
                                    }
                                })),
                        )
                        .child(
                            Button::new("ai-stop-here")
                                .outline()
                                .label("Stop")
                                .on_click(cx.listener(move |panel, _, _, cx| {
                                    if let Some(key) = panel.current_key(cx) {
                                        panel.stop(&key, cx);
                                    }
                                })),
                        ),
                )
        });

        let activity_note = activity.map(|text| {
            div()
                .text_size(ui(TYPE_META))
                .text_color(p.chrome_dim)
                .child(format!("→ {text}"))
        });

        let footer = h_flex()
            .gap(ui(6.))
            .items_end()
            .child(div().flex_1().child(Textarea::new(
                &self.fields.as_ref().expect("made above").question,
            )));
        let footer = if streaming {
            footer.child(
                Button::new("ai-stop")
                    .primary()
                    .label("Stop")
                    .on_click(cx.listener(|panel, _, _, cx| {
                        if let Some(key) = panel.current_key(cx) {
                            panel.stop(&key, cx);
                        }
                    })),
            )
        } else {
            footer.child(
                Button::new("ai-ask")
                    .primary()
                    .label(if gathering { "Reading…" } else { "Ask" })
                    .on_click(cx.listener(|panel, _, window, cx| {
                        let Some(fields) = &panel.fields else { return };
                        let text = fields.question.read(cx).value().to_string();
                        if text.trim().is_empty()
                            || !AiPanel::settings(cx).configured()
                            || panel.gathering()
                        {
                            return;
                        }
                        fields.question.update(cx, |input, cx| {
                            input.set_value(String::new(), window, cx);
                        });
                        panel.send(text, cx);
                    })),
            )
        };

        v_flex()
            .size_full()
            .gap(ui(6.))
            .p(ui(8.))
            .bg(p.chrome_bg)
            .child(header)
            .children(setup_note)
            .children(error_note)
            .child(body)
            .child(cards)
            .children(continuation)
            .children(activity_note)
            .child(footer)
    }
}

/// One bubble: the user's question, or the answer with its action chips,
/// stamps and token count.
fn turn_bubble(turn: &AiTurn, thinking: bool, p: &Palette) -> impl IntoElement {
    let user = turn.role == "user";
    v_flex()
        .max_w_full()
        .gap(ui(3.))
        .px(ui(8.))
        .py(ui(5.))
        .rounded(px(RADIUS_CONTROL))
        .when(user, |row| row.bg(p.accent_wash))
        .when(!user, |row| row.bg(p.chrome_raised))
        .when(user, |row| {
            row.child(
                div()
                    .text_size(ui(TYPE_SECONDARY))
                    .text_color(p.chrome_ink)
                    .child(turn.text.clone()),
            )
        })
        .when(!user, |row| {
            let row = row.child(h_flex().flex_wrap().gap(ui(6.)).children(
                turn.actions.iter().map(|action| {
                    div()
                        .text_size(ui(TYPE_META))
                        .text_color(match action.outcome {
                            super::store::ActionOutcome::Done => p.chrome_dim,
                            super::store::ActionOutcome::Denied => p.warn,
                            super::store::ActionOutcome::Failed => p.danger,
                        })
                        .child(format!("{} — {}", action.summary, action.outcome.label()))
                }),
            ));
            if !turn.text.is_empty() {
                row.child(
                    div()
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(p.chrome_ink)
                        .child(turn.text.clone()),
                )
            } else if thinking {
                row.child(
                    div()
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(p.chrome_dim)
                        .child("Thinking…"),
                )
            } else if turn.actions.is_empty() {
                row.child(
                    div()
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(p.chrome_faint)
                        .child("(no answer)"),
                )
            } else {
                row
            }
        })
        .when(turn.created_at.is_some() || turn.tokens.is_some(), |row| {
            row.child(
                h_flex()
                    .gap(ui(6.))
                    .px(ui(4.))
                    .children(turn.created_at.map(|moment| {
                        div()
                            .text_size(ui(TYPE_LABEL))
                            .text_color(p.chrome_faint)
                            .child(stamp_text(moment))
                    }))
                    .children(turn.tokens.map(|tokens| {
                        div()
                            .text_size(ui(TYPE_LABEL))
                            .text_color(p.chrome_faint)
                            .child(format!("{tokens} tok"))
                    })),
            )
        })
}

fn empty_note(text: &'static str, p: &Palette) -> impl IntoElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(ui(12.))
        .text_size(ui(TYPE_META))
        .text_color(p.chrome_faint)
        .child(text)
}

/// The clock stamp under a bubble, in the user's locale and clock.
fn stamp_text(epoch_ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(epoch_ms as i64)
        .map(|moment| {
            moment
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// One waiting card, cloned out of the store so the view can draw it while the
/// loop owns the conversation.
struct Pending {
    id: String,
    title: String,
    summary: String,
    detail: Option<String>,
    expanded: bool,
}
