//! The card for what the agent waits on: a permission to allow or deny, or
//! questions to answer. It is the last row of the conversation while the turn
//! waits.

use desktop_core::connection::chat::{
    ChatAnswer, ChatPendingRequest, ChatQuestion, ChatQuestionAnswer, ChatRequestKind,
};
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    AnyElement, Context, Focusable as _, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Role, SharedString, StatefulInteractiveElement, Styled, Window,
    accesskit::Toggled,
    component::{
        Disableable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::Input,
        v_flex,
    },
    div,
    prelude::FluentBuilder,
};

use crate::{
    fonts,
    theme::{Palette, metrics::ui},
};

use super::{
    pane::{ChatPane, Pick},
    view::copyable,
};

/// `.b`: a card's button. `.pri` fills it; `.ghost` has no border.
fn button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    disabled: bool,
    p: &Palette,
) -> Button {
    let button = Button::new(id)
        .label(label)
        .xsmall()
        .h_auto()
        .px(ui(10.))
        .py(ui(4.))
        .border_1()
        .text_size(ui(13.))
        .disabled(disabled);
    let button = if primary {
        button.primary().border_color(p.accent)
    } else {
        button
            .ghost()
            .border_color(gpui_kit::transparent_black())
            .text_color(p.chrome_dim)
    };
    // Kit replays these colors inside its disabled state.
    if disabled {
        button.opacity(0.42)
    } else {
        button
    }
}

/// `.card .hd`: what is asked, and where in the request it is.
fn head(title: SharedString, note: SharedString, p: &Palette) -> impl IntoElement {
    h_flex()
        .items_start()
        .gap(ui(8.))
        .px(ui(10.))
        .py(ui(7.))
        .border_b_1()
        .border_color(p.chrome_hairline)
        .text_size(ui(13.5))
        .text_color(p.chrome_ink_strong)
        .child(div().flex_1().min_w_0().child(title))
        // "1 of 3" and "waiting for you" are read: the dim ink (S8, C1).
        .child(
            div()
                .flex_none()
                .text_size(ui(12.))
                .text_color(p.chrome_dim)
                .child(note),
        )
}

/// The request the turn waits on, as its card.
pub(super) fn request(
    pane: &mut ChatPane,
    palette: &Palette,
    window: &mut Window,
    cx: &mut Context<ChatPane>,
) -> AnyElement {
    let Some(request) = pane.pending().cloned() else {
        return div().into_any_element();
    };
    match ChatRequestKind::try_from(request.kind).unwrap_or_default() {
        ChatRequestKind::Question if !request.questions.is_empty() => {
            questions(pane, &request, palette, window, cx)
        }
        _ => permission(pane, &request, palette, cx),
    }
}

/// `.card.ask`: Allow or Deny, for an action the provider would not decide.
fn permission(
    pane: &ChatPane,
    request: &ChatPendingRequest,
    p: &Palette,
    cx: &mut Context<ChatPane>,
) -> AnyElement {
    let title: SharedString = if request.title.is_empty() {
        "The agent is asking for permission".into()
    } else {
        request.title.clone().into()
    };
    let busy = pane.answering.is_some() || !pane.is_live();
    let answer = |allow: bool| {
        let request_id = request.request_id.clone();
        cx.listener(move |this: &mut ChatPane, _, _, cx| {
            this.answer(
                &request_id,
                ChatAnswer {
                    allow,
                    ..Default::default()
                },
                cx,
            );
        })
    };
    v_flex()
        .id("chat-request")
        .test_support()
        .role(Role::Group)
        .aria_label(title.clone())
        .w_full()
        .border_1()
        .border_color(p.danger)
        .rounded(ui(9.))
        .bg(p.chrome_bg)
        .overflow_hidden()
        .child(
            div()
                .bg(p.danger_wash)
                .child(head(title, "waiting for you".into(), p)),
        )
        .child(
            v_flex()
                .gap(ui(8.))
                .px(ui(10.))
                .py(ui(9.))
                .when(!request.detail.is_empty(), |body| {
                    // `.cmd`. Wrapped, not scrolled: what is allowed is read whole.
                    let detail = SharedString::from(request.detail.clone());
                    body.child(copyable(
                        div()
                            .id("chat-request-detail")
                            .px(ui(8.))
                            .py(ui(5.))
                            .border_1()
                            .border_color(p.chrome_hairline)
                            .rounded(ui(5.))
                            .bg(p.chrome_raised)
                            .font_family(fonts::MONO)
                            .text_size(ui(13.))
                            .text_color(p.chrome_ink_strong)
                            .child(detail.clone()),
                        vec![("Copy", detail)],
                    ))
                })
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(ui(6.))
                        .child(button("chat-allow", "Allow", true, busy, p).on_click(answer(true)))
                        .child(button("chat-deny", "Deny", false, busy, p).on_click(answer(false))),
                ),
        )
        .into_any_element()
}

/// `.opt i`: a radio's dot, or a checkbox's square when several may be picked.
fn mark(on: bool, multi: bool, p: &Palette) -> impl IntoElement {
    let mark = div()
        .flex_none()
        .size(ui(13.))
        .mt(ui(3.))
        .border_1()
        .border_color(if on { p.accent } else { p.chrome_faint })
        .flex()
        .items_center()
        .justify_center();
    let mark = if multi {
        mark.rounded(ui(3.))
    } else {
        mark.rounded_full()
    };
    mark.when(on, |mark| {
        let fill = div().size(ui(7.)).bg(p.accent);
        mark.child(if multi {
            fill.rounded(ui(1.5))
        } else {
            fill.rounded_full()
        })
    })
}

/// What `pick` sends for `question`.
fn question_answer(question: &ChatQuestion, pick: &Pick) -> ChatQuestionAnswer {
    ChatQuestionAnswer {
        question_id: question.id.clone(),
        option_ids: pick.options.clone(),
        other_text: if pick.other {
            pick.other_text.trim().to_owned()
        } else {
            String::new()
        },
    }
}

/// `.card` with `.opt` rows: one question at a time, each option on a row
/// with its description, and a row to type another answer.
fn questions(
    pane: &mut ChatPane,
    request: &ChatPendingRequest,
    p: &Palette,
    window: &mut Window,
    cx: &mut Context<ChatPane>,
) -> AnyElement {
    let busy = pane.answering.is_some() || !pane.is_live();
    let Some(draft) = pane.answer_draft(window, cx) else {
        return div().into_any_element();
    };
    let count = request.questions.len();
    let index = draft.index.min(count - 1);
    let question = &request.questions[index];
    let pick = draft.picks.get(index).cloned().unwrap_or_default();
    let other_field = draft.other.clone();
    let multi = question.multi_select;
    let title: SharedString = if question.prompt.is_empty() {
        question.header.clone().into()
    } else {
        question.prompt.clone().into()
    };
    let (accent, wash, hairline) = (p.accent, p.accent_wash, p.chrome_hairline);
    let ring = p.chrome_ink_strong;
    let row = |id: SharedString, name: SharedString, on: bool| {
        h_flex()
            .id(id)
            .test_support()
            .role(if multi {
                Role::CheckBox
            } else {
                Role::RadioButton
            })
            .aria_label(name)
            .aria_toggled(if on { Toggled::True } else { Toggled::False })
            .items_start()
            .gap(ui(9.))
            .px(ui(8.))
            .py(ui(6.))
            .border_1()
            .border_color(if on { accent } else { hairline })
            .when(on, |row| row.bg(wash))
            .rounded(ui(7.))
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(move |style| style.border_color(ring))
            .child(mark(on, multi, p))
    };
    let words = |label: SharedString, description: SharedString| {
        v_flex()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .text_color(p.chrome_ink_strong)
                    .font_weight(FontWeight::MEDIUM)
                    .child(label),
            )
            .when(!description.is_empty(), |words| {
                words.child(
                    div()
                        .text_size(ui(13.))
                        .text_color(p.chrome_dim)
                        .child(description),
                )
            })
    };
    let options = question.options.iter().enumerate().map(|(at, option)| {
        let on = pick.options.contains(&option.id);
        let id = option.id.clone();
        row(
            SharedString::from(format!("chat-option-{at}")),
            option.label.clone().into(),
            on,
        )
        .child(words(
            option.label.clone().into(),
            option.description.clone().into(),
        ))
        .on_click(cx.listener(move |this, _, _, cx| {
            let Some(pick) = this
                .answer
                .as_mut()
                .and_then(|draft| draft.picks.get_mut(index))
            else {
                return;
            };
            if !multi {
                pick.options = vec![id.clone()];
                pick.other = false;
            } else if let Some(at) = pick.options.iter().position(|picked| *picked == id) {
                pick.options.remove(at);
            } else {
                pick.options.push(id.clone());
            }
            this.sync_trail(cx);
            cx.notify();
        }))
    });
    let other = question.allow_other.then(|| {
        let field = other_field.clone();
        row("chat-option-other".into(), "Other".into(), pick.other)
            .child(words("Other".into(), "Type your own answer.".into()).when(
                pick.other,
                |words| {
                    words.child(
                        div().pt(ui(5.)).child(
                            Input::new(&other_field)
                                .id("chat-option-other-text")
                                .aria_label("Your answer")
                                .xsmall(),
                        ),
                    )
                },
            ))
            .on_click(cx.listener(move |this, _, window, cx| {
                let Some(pick) = this
                    .answer
                    .as_mut()
                    .and_then(|draft| draft.picks.get_mut(index))
                else {
                    return;
                };
                // A press inside the open field is typing, not a second pick.
                if pick.other && field.read(cx).focus_handle(cx).is_focused(window) {
                    return;
                }
                pick.other = !pick.other;
                if pick.other && !multi {
                    pick.options.clear();
                }
                if pick.other {
                    field.update(cx, |field, cx| field.focus(window, cx));
                }
                this.sync_trail(cx);
                cx.notify();
            }))
    });
    let last = index + 1 == count;
    let shown = request.request_id.clone();
    let submit = cx.listener(move |this: &mut ChatPane, _, window, cx| {
        let Some(request) = this
            .pending()
            .filter(|request| request.request_id == shown)
            .cloned()
        else {
            return;
        };
        let Some(draft) = this.answer.as_mut() else {
            return;
        };
        if !draft.picks.get(index).is_some_and(Pick::is_answer) {
            return;
        }
        if !last {
            return this.show_question(index + 1, window, cx);
        }
        let answers = request
            .questions
            .iter()
            .zip(&draft.picks)
            .map(|(question, pick)| question_answer(question, pick))
            .collect();
        this.answer(
            &request.request_id,
            ChatAnswer {
                answers,
                ..Default::default()
            },
            cx,
        );
    });
    let shown = request.request_id.clone();
    let skip = cx.listener(move |this: &mut ChatPane, _, _, cx| {
        this.answer(
            &shown,
            ChatAnswer {
                skipped: true,
                ..Default::default()
            },
            cx,
        );
    });
    let back = cx.listener(move |this: &mut ChatPane, _, window, cx| {
        this.show_question(index.saturating_sub(1), window, cx);
    });
    let note: SharedString = if count > 1 {
        format!("{} of {count}", index + 1).into()
    } else {
        SharedString::default()
    };
    v_flex()
        .id("chat-request")
        .test_support()
        .role(Role::Group)
        .aria_label(title.clone())
        .w_full()
        .border_1()
        .border_color(p.chrome_border)
        .rounded(ui(9.))
        .bg(p.chrome_bg)
        .overflow_hidden()
        .child(head(title, note, p))
        .child(
            v_flex()
                .gap(ui(8.))
                .px(ui(10.))
                .py(ui(9.))
                .children(options)
                .children(other)
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(ui(6.))
                        .child(
                            button("chat-submit", "Submit", true, busy || !pick.is_answer(), p)
                                .on_click(submit),
                        )
                        .when(index > 0, |row| {
                            row.child(button("chat-back", "Back", false, busy, p).on_click(back))
                        })
                        .child(button("chat-skip", "Skip", false, busy, p).on_click(skip)),
                ),
        )
        .into_any_element()
}

impl ChatPane {
    /// Moves a card of several questions to another of them, with the field
    /// showing that question's own text.
    fn show_question(&mut self, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.answer.as_mut() else {
            return;
        };
        draft.index = to;
        let text = draft
            .picks
            .get(to)
            .map(|pick| pick.other_text.clone())
            .unwrap_or_default();
        draft
            .other
            .update(cx, |field, cx| field.set_value(text, window, cx));
        self.sync_trail(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_carries_other_text_only_while_other_is_picked() {
        let question = ChatQuestion {
            id: "scope".into(),
            ..Default::default()
        };
        let mut pick = Pick {
            options: vec!["a".into()],
            other: false,
            other_text: " typed ".into(),
        };
        assert_eq!(question_answer(&question, &pick).other_text, "");
        pick.other = true;
        let answer = question_answer(&question, &pick);
        assert_eq!(answer.question_id, "scope");
        assert_eq!(answer.option_ids, ["a"]);
        assert_eq!(answer.other_text, "typed");
    }
}
