//! How a chat is drawn: one column of the conversation over the composer.
//! The measures are the design's (`docs/design/chat-mock.html`), in CSS
//! pixels at 100% UI scale.

use std::{rc::Rc, sync::Arc};

use desktop_core::connection::chat::{
    ChatConnection, ChatItem, ChatItemKind, ChatTool, ChatToolKind, ChatToolStatus,
};
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    AnyElement, Bounds, ClickEvent, ClipboardItem, Context, Div, FontWeight, HighlightStyle,
    ImageCacheError, ImageSource, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Pixels, Render, Role, SharedString, StatefulInteractiveElement, StyleRefinement, Styled,
    Window,
    base::{TextView, TextViewStyle},
    canvas,
    component::{
        h_flex, menu::ContextMenuExt, message_scroller::MessageScroller, scroll::ScrollableElement,
        v_flex,
    },
    div,
    prelude::FluentBuilder,
    px, relative, rems,
};

use crate::{
    context_menu::{self, Entry, Item},
    fonts,
    markdown::is_click,
    terminal::integration::agents::note_activity,
    theme::{self, Palette, metrics::ui},
    ui::dialog::{self, Action},
};

use super::{
    DETAILS_ROOM, DetailsContext, cards, details_card, open,
    pane::{ChatPane, Row},
};

/// `.tl { width: min(640px, 100%) }`.
const COLUMN: f32 = 640.;
/// The least space between the column and the pane's edge.
const GUTTER: f32 = 16.;
/// `.chat.wide`: what the details card takes on the right, and the least
/// margin left of the column beside it.
const CARD_ROOM: f32 = 252.;
const CARD_MARGIN: f32 = 24.;
/// `.dcard { top: 14px; right: 14px; width: 238px }`.
const CARD_INSET: f32 = 14.;
const CARD_WIDTH: f32 = 238.;
/// `.tl { gap: 14px }`.
const ROW_GAP: f32 = 14.;
/// The tallest a tool's output is shown before it scrolls.
const OUTPUT_HEIGHT: f32 = 260.;

/// The space left and right of the column in a pane `width` wide, in pixels
/// at `rem`. With the details card on show and room for both, the column
/// moves left: `max(24px, (100% - 252px - 640px) / 2)`.
pub(super) fn column(width: Pixels, card: bool, rem: Pixels) -> (Pixels, Pixels) {
    let scaled = |css: f32| ui(css).to_pixels(rem);
    if card && width >= scaled(DETAILS_ROOM) {
        let left = ((width - scaled(CARD_ROOM) - scaled(COLUMN)) / 2.).max(scaled(CARD_MARGIN));
        return (left, (width - left - scaled(COLUMN)).max(px(0.)));
    }
    let side = ((width - scaled(COLUMN)) / 2.).max(scaled(GUTTER));
    (side, side)
}

/// The agent's Markdown, in the chat's own type.
fn markdown_style(p: &Palette, dark: bool) -> TextViewStyle {
    let rounded = StyleRefinement::default().rounded(px(7.));
    let strong = p.chrome_ink_strong;
    TextViewStyle::default()
        .with_foreground(p.chrome_ink.into())
        .with_muted_foreground(p.chrome_dim.into())
        .with_link(p.accent.into())
        .with_selection(p.accent_wash.into())
        .with_code_background(p.chrome_raised.into())
        .with_border(p.chrome_border.into())
        // `.a p + p { margin-top: 8px }`.
        .with_paragraph_gap(rems(0.5))
        .with_heading(move |level| {
            let size = match level {
                1 => 1.35,
                2 => 1.2,
                3 => 1.08,
                _ => 1.,
            };
            StyleRefinement::default()
                .text_size(rems(size * 14.5 / 16.))
                .text_color(strong)
                .font_weight(FontWeight::SEMIBOLD)
        })
        .with_code_block(rounded.clone().border_1().border_color(p.chrome_hairline))
        .with_table(rounded.bg(p.chrome_bg))
        .with_table_head(
            StyleRefinement::default()
                .bg(p.chrome_raised)
                .text_color(strong)
                .font_weight(FontWeight::SEMIBOLD),
        )
        .with_inline_code(HighlightStyle {
            background_color: Some(p.chrome_raised.into()),
            ..Default::default()
        })
        .with_dark(dark)
}

/// `.u`: one of the user's messages.
fn bubble(text: SharedString, palette: &Palette) -> Div {
    div()
        .max_w(relative(0.82))
        .min_w_0()
        .bg(palette.chrome_raised)
        .border_1()
        .border_color(palette.chrome_hairline)
        .rounded_tl(ui(12.))
        .rounded_tr(ui(12.))
        .rounded_bl(ui(12.))
        .rounded_br(ui(3.))
        .px(ui(12.))
        .py(ui(8.))
        .text_color(palette.chrome_ink_strong)
        .child(text)
}

/// A right click on `element` offers to copy each of `texts` under its label.
/// Only the agent's own prose can be selected; the rest is copied whole.
pub(super) fn copyable<E>(element: E, texts: Vec<(&'static str, SharedString)>) -> AnyElement
where
    E: ContextMenuExt + IntoElement + 'static,
{
    let entries: Rc<Vec<Entry>> = Rc::new(
        texts
            .into_iter()
            .map(|(label, text)| {
                Entry::from(Item::new(label, move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                }))
            })
            .collect(),
    );
    element
        .context_menu(move |menu, window, cx| context_menu::fill(menu, &entries, window, cx))
        .into_any_element()
}

/// The row's verb when the provider gave none.
fn verb(tool: &ChatTool) -> SharedString {
    if !tool.title.is_empty() {
        return tool.title.clone().into();
    }
    match ChatToolKind::try_from(tool.kind).unwrap_or_default() {
        ChatToolKind::Command => "run",
        ChatToolKind::Read => "read",
        ChatToolKind::Edit => "edit",
        ChatToolKind::Search => "search",
        ChatToolKind::Web => "web",
        ChatToolKind::Mcp => "mcp",
        ChatToolKind::Subagent => "agent",
        ChatToolKind::Other | ChatToolKind::Unspecified => "tool",
    }
    .into()
}

/// `.tool .r`: the row's outcome, or its line counts for an edit that made any.
fn outcome(tool: &ChatTool, palette: &Palette) -> AnyElement {
    let status = ChatToolStatus::try_from(tool.status).unwrap_or_default();
    let edit = tool.kind == i32::from(ChatToolKind::Edit);
    if edit && status != ChatToolStatus::Failed && (tool.added > 0 || tool.removed > 0) {
        return h_flex()
            .gap(ui(5.))
            .child(
                div()
                    .text_color(palette.ok)
                    .child(format!("+{}", tool.added)),
            )
            .child(
                div()
                    .text_color(palette.danger)
                    // A minus sign, as wide as the plus.
                    .child(format!("\u{2212}{}", tool.removed)),
            )
            .into_any_element();
    }
    let (fallback, color) = match status {
        ChatToolStatus::Running => ("running", palette.state_working),
        ChatToolStatus::Failed => ("failed", palette.danger),
        ChatToolStatus::Ok | ChatToolStatus::Unspecified => ("done", palette.ok),
    };
    let text: SharedString = if tool.result_summary.is_empty() {
        fallback.into()
    } else {
        tool.result_summary.clone().into()
    };
    div().text_color(color).child(text).into_any_element()
}

impl ChatPane {
    pub(super) fn render_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = theme::palette(cx);
        match self.row(index) {
            Some(Row::Header) => self.header_row(&palette, cx),
            Some(Row::Item(item)) => self.item_row(item, &palette, cx),
            Some(Row::Sending(sending)) => {
                let sending = &self.sending[sending];
                let text = sending.text.clone();
                // Dimmed until the host has it.
                h_flex()
                    .w_full()
                    .justify_end()
                    .child(
                        bubble(text.clone(), &palette)
                            .opacity(0.6)
                            .id(SharedString::from(format!("chat-sending-{}", sending.id)))
                            .test_support()
                            .aria_label(text),
                    )
                    .into_any_element()
            }
            Some(Row::Request) => cards::request(self, &palette, window, cx),
            None => div().into_any_element(),
        }
    }

    /// What is above the oldest item held: more to load, or that there was
    /// more and it is gone.
    fn header_row(&mut self, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let note = |text: &'static str| {
            div()
                .w_full()
                .text_center()
                .text_size(ui(12.5))
                .text_color(palette.chrome_faint)
                .child(text)
        };
        if !self.chat.is_held() {
            return div().into_any_element();
        }
        if self.chat.older_cursor.is_empty() {
            return if self.chat.state.history_complete {
                div().into_any_element()
            } else {
                note("Earlier messages are not available").into_any_element()
            };
        }
        if self.loading_older {
            return note("Loading earlier messages…").into_any_element();
        }
        if self.older_failed || !self.is_live() {
            let wash = palette.accent_wash;
            return h_flex()
                .w_full()
                .justify_center()
                .child(
                    div()
                        .id("chat-load-older")
                        .test_support()
                        .role(Role::Button)
                        .aria_label("Load earlier messages")
                        .cursor_pointer()
                        .tab_index(0)
                        .px(ui(6.))
                        .rounded(ui(4.))
                        .focus_visible(move |style| style.bg(wash))
                        .text_size(ui(12.5))
                        .text_color(palette.accent)
                        .child("Load earlier messages")
                        .on_click(cx.listener(|this, _, _, cx| this.load_older(cx))),
                )
                .into_any_element();
        }
        // Reached by scrolling up to it: the page before is wanted.
        let this = cx.entity().downgrade();
        cx.defer(move |cx| {
            this.update(cx, |this, cx| this.load_older(cx)).ok();
        });
        note("Loading earlier messages…").into_any_element()
    }

    fn item_row(&mut self, index: usize, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(kind) = self.chat.items.get(index).map(|item| item.kind) else {
            return div().into_any_element();
        };
        match ChatItemKind::try_from(kind).unwrap_or_default() {
            ChatItemKind::User => self.user_row(index, palette, cx),
            ChatItemKind::Assistant => self.assistant_row(index, palette, cx),
            ChatItemKind::Tool => self.tool_row(index, palette, cx),
            ChatItemKind::Reasoning => {
                let text: SharedString = self.chat.items[index].text.clone().into();
                div()
                    .w_full()
                    .pl(ui(10.))
                    .border_l_2()
                    .border_color(palette.chrome_hairline)
                    .text_size(ui(13.5))
                    .text_color(palette.chrome_dim)
                    .child(text)
                    .into_any_element()
            }
            ChatItemKind::Notice | ChatItemKind::Unspecified => {
                let text: SharedString = self.chat.items[index].text.clone().into();
                div()
                    .w_full()
                    .text_center()
                    .text_size(ui(12.5))
                    .text_color(palette.chrome_faint)
                    .child(text)
                    .into_any_element()
            }
        }
    }

    fn user_row(&self, index: usize, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let item: &ChatItem = &self.chat.items[index];
        let id = item.id.clone();
        let text: SharedString = item.text.clone().into();
        let editing = self.editing.as_deref() == Some(id.as_str());
        let group = SharedString::from(format!("chat-user-{id}"));
        let name = SharedString::from(format!("chat-item-{id}"));
        let ring = palette.accent;
        let edit = self.editable(item).then(|| {
            // `.u .ed`: shown while the pointer is on the message, and while
            // the message is the one being edited.
            div()
                .id(SharedString::from(format!("chat-edit-{id}")))
                .test_support()
                .role(Role::Button)
                .aria_label("Edit message")
                .flex_none()
                .mt(ui(6.))
                .px(ui(6.))
                .py(ui(1.))
                .border_1()
                .border_color(palette.chrome_border)
                .rounded(ui(5.))
                .bg(palette.chrome_bg)
                .text_size(ui(12.))
                .text_color(palette.chrome_faint)
                .cursor_pointer()
                .tab_index(0)
                .when(!editing, |edit| {
                    edit.opacity(0.)
                        .group_hover(group.clone(), |style| style.opacity(1.))
                })
                .focus_visible(move |style| style.opacity(1.).border_color(ring))
                .child("Edit")
                .on_click(cx.listener(move |this, _, window, cx| this.edit(&id, window, cx)))
        });
        h_flex()
            .group(group.clone())
            .w_full()
            .justify_end()
            .items_start()
            .gap(ui(6.))
            .children(edit)
            .child(copyable(
                bubble(text.clone(), palette)
                    .when(editing, |bubble| bubble.border_color(palette.accent))
                    .id(name)
                    .test_support()
                    .aria_label(text.clone()),
                vec![("Copy message", text)],
            ))
            .into_any_element()
    }

    fn assistant_row(
        &mut self,
        index: usize,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = self.shown_text(index, cx);
        let id = self.chat.items[index].id.clone();
        let dark = cx.global::<theme::Appearance>().is_dark;
        let pane_id = self.pane_id.clone();
        let pressed = self.pressed.clone();
        div()
            .id(SharedString::from(format!("chat-item-{id}")))
            .test_support()
            .w_full()
            .min_w_0()
            .child(
                TextView::markdown(SharedString::from(format!("chat-text-{id}")), text)
                    .style(markdown_style(palette, dark))
                    .selectable(true)
                    // An agent's text never fetches anything: an image is
                    // shown as its alt text instead.
                    .image_source(|_| {
                        ImageSource::Custom(Arc::new(|_, _| {
                            Some(Err(ImageCacheError::Asset("image not allowed".into())))
                        }))
                    })
                    .on_link_click(move |href: &SharedString, event: &ClickEvent, window, cx| {
                        let pressed = *pressed.lock().expect("the press is never poisoned");
                        if is_click(event, pressed) {
                            open::link(&pane_id, href, window, cx);
                        }
                    }),
            )
            .into_any_element()
    }

    /// `.tool`: one row for a tool call. An edit opens its file's diff; any
    /// other row with output opens to show it.
    fn tool_row(&self, index: usize, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let item = &self.chat.items[index];
        let tool = item.tool.clone().unwrap_or_default();
        let id = item.id.clone();
        let edit = tool.kind == i32::from(ChatToolKind::Edit) && !tool.path.is_empty();
        let expandable = !edit && !tool.output_preview.is_empty();
        let expanded = expandable && self.expanded.contains(&id);
        let verb = verb(&tool);
        let detail: SharedString = if tool.detail.is_empty() {
            tool.path.clone().into()
        } else {
            tool.detail.clone().into()
        };
        let name = SharedString::from(format!("{verb} {detail}"));
        let mut copies = vec![("Copy", detail.clone())];
        if !tool.output_preview.is_empty() {
            copies.push(("Copy output", tool.output_preview.clone().into()));
        }
        let head = h_flex()
            .gap(ui(8.))
            .px(ui(9.))
            .py(ui(5.))
            .text_size(ui(13.5))
            .text_color(palette.chrome_dim)
            .child(
                div()
                    .flex_none()
                    .font_family(fonts::MONO)
                    .text_size(ui(12.))
                    .text_color(palette.chrome_faint)
                    .child(verb),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(fonts::MONO)
                    .text_size(ui(13.))
                    .text_color(palette.chrome_ink)
                    .child(detail),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(ui(12.))
                    .child(outcome(&tool, palette)),
            );
        let row = v_flex()
            .id(SharedString::from(format!("chat-tool-{id}")))
            .test_support()
            .w_full()
            .min_w_0()
            .border_1()
            .border_color(palette.chrome_hairline)
            .rounded(ui(7.))
            .bg(palette.chrome_bg)
            .overflow_hidden();
        if !edit && !expandable {
            return copyable(row.child(head), copies);
        }
        let (hover, wash) = (palette.chrome_hover, palette.accent_wash);
        let head = div()
            .id(SharedString::from(format!("chat-tool-open-{id}")))
            .test_support()
            .role(Role::Button)
            .aria_label(name)
            .when(expandable, |head| head.aria_expanded(expanded))
            .cursor_pointer()
            .tab_index(0)
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.bg(wash))
            .child(head)
            .on_click(cx.listener(move |this, _, _, cx| {
                if edit {
                    let path = tool.path.clone();
                    this.open_edit(&path, cx);
                    return;
                }
                if !this.expanded.remove(&id) {
                    this.expanded.insert(id.clone());
                }
                this.remeasure_row(index + 1, cx);
            }));
        let item = &self.chat.items[index];
        let output = expanded.then(|| {
            let tool = item.tool.as_ref();
            let text: SharedString = tool
                .map(|tool| tool.output_preview.clone())
                .unwrap_or_default()
                .into();
            let truncated = tool.is_some_and(|tool| tool.output_truncated);
            v_flex()
                .border_t_1()
                .border_color(palette.chrome_hairline)
                .bg(palette.chrome_raised)
                .child(
                    div()
                        .id(SharedString::from(format!("chat-tool-output-{}", item.id)))
                        .test_support()
                        .child(
                            // The Kit's scroll area, not a plain overflow: the
                            // wheel over it is its own, and the conversation
                            // under it stays where it is.
                            div()
                                .max_h(ui(OUTPUT_HEIGHT))
                                .px(ui(10.))
                                .py(ui(6.))
                                .font_family(fonts::MONO)
                                .text_size(ui(12.5))
                                .line_height(relative(1.55))
                                .text_color(palette.chrome_ink)
                                .child(text)
                                .overflow_y_scrollbar()
                                .id(SharedString::from(format!("chat-tool-scroll-{}", item.id))),
                        ),
                )
                .when(truncated, |output| {
                    output.child(
                        div()
                            .px(ui(10.))
                            .pb(ui(6.))
                            .text_size(ui(12.))
                            .text_color(palette.chrome_faint)
                            .child("The rest of the output is not shown."),
                    )
                })
        });
        copyable(row.child(head).children(output), copies)
    }

    fn retry_button(&self, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        dialog::button(
            gpui_kit::component::button::Button::new("chat-open-retry")
                .label("Try again")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.open_error = None;
                    this.open(cx);
                    cx.notify();
                })),
            Action::Plain,
            false,
            palette,
        )
        .into_any_element()
    }

    /// `.top-note` over a conversation shown from what was held while the
    /// host sends it again: a chat come back to after its link dropped, or
    /// after a change to it was lost.
    fn updating(&self, left: Pixels, right: Pixels, palette: &Palette) -> Option<AnyElement> {
        (self.updating && self.chat.is_held()).then(|| {
            div()
                .id("chat-updating")
                .test_support()
                .role(Role::Status)
                .aria_label("Updating…")
                .flex_none()
                .pl(left)
                .pr(right)
                .pt(ui(8.))
                .text_center()
                .text_size(ui(12.5))
                .text_color(palette.chrome_faint)
                .child("Updating…")
                .into_any_element()
        })
    }

    /// Said under a conversation the host would not send again after a
    /// change to it was lost: what is shown may be behind what the host holds.
    fn stale(
        &self,
        left: Pixels,
        right: Pixels,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let error = self.open_error.clone().filter(|_| self.chat.is_held())?;
        let said: SharedString = format!("This conversation may be out of date. {error}").into();
        Some(
            h_flex()
                .flex_none()
                .pl(left)
                .pr(right)
                .pb(ui(8.))
                .gap(ui(10.))
                .child(
                    div()
                        .id("chat-open-error")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(said.clone())
                        .flex_1()
                        .min_w_0()
                        .text_size(ui(13.))
                        .text_color(palette.danger_ink)
                        .child(said),
                )
                .child(self.retry_button(palette, cx))
                .into_any_element(),
        )
    }

    /// What fills the pane before there is a conversation to show.
    fn opening(&self, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let body = match self.open_error.clone() {
            Some(error) => v_flex()
                .items_center()
                .gap(ui(10.))
                .child(
                    div()
                        .id("chat-open-error")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(error.clone())
                        .text_color(palette.chrome_ink)
                        .child(error),
                )
                .child(self.retry_button(palette, cx)),
            None => v_flex().child(div().text_color(palette.chrome_faint).child(
                if self.is_live() {
                    "Opening the chat…"
                } else {
                    "Waiting for the host to reconnect…"
                },
            )),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .px(ui(GUTTER))
            .child(body)
            .into_any_element()
    }

    /// Why the agent is not there to talk to, when it is not.
    pub(super) fn agent_status(&self) -> Option<SharedString> {
        if !self.chat.is_held() {
            return None;
        }
        match self.connection() {
            ChatConnection::Failed => Some(if self.chat.state.failure.is_empty() {
                "The agent could not be started.".into()
            } else {
                self.chat.state.failure.clone().into()
            }),
            ChatConnection::Ended => Some("The agent has exited.".into()),
            _ => None,
        }
    }
}

impl Render for ChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(text) = self.restore.take() {
            // What was typed since stays, under the text that came back.
            let typed = self.composer.read(cx).value().to_string();
            let text = if typed.trim().is_empty() {
                text
            } else {
                format!("{text}\n{typed}")
            };
            self.set_composer(&text, window, cx);
        }
        if let Some(text) = self.withdraw.take()
            && self.composer.read(cx).value().trim() == text
        {
            self.set_composer("", window, cx);
            self.notice = None;
        }
        let palette = theme::palette(cx);
        let rem = window.rem_size();
        let width = self.width.get();
        let card = width.and_then(|width| {
            let context = DetailsContext {
                profile_id: self.profile_id.clone(),
                server_identity: self.server_identity.clone(),
                pane: self.tmux_pane().clone(),
                usage: self.chat.state.usage,
                width,
            };
            details_card(cx).and_then(|card| card(&context, window, cx))
        });
        let (left, right) = width.map_or((ui(GUTTER).to_pixels(rem), ui(GUTTER).to_pixels(rem)), {
            let card = card.is_some();
            move |width| column(width, card, rem)
        });
        let measured = self.width.clone();
        let press = self.pressed.clone();
        let (profile_id, pane_id) = (self.profile_id.clone(), self.pane_id.clone());
        let timeline = if self.chat.is_held() {
            let this = cx.entity().downgrade();
            div()
                .flex_1()
                .min_h_0()
                .child(
                    MessageScroller::new(
                        "chat-timeline",
                        self.list.clone(),
                        move |index, window, cx| {
                            this.update(cx, |pane, cx| pane.render_row(index, window, cx))
                                .unwrap_or_else(|_| div().into_any_element())
                        },
                    )
                    .with_jump_button_label("Jump to latest")
                    // `.tl { padding: 18px 0 12px }`, less the header row's gap.
                    .with_list_style(StyleRefinement::default().pt(ui(18. - ROW_GAP)).pb(ui(12.)))
                    .with_row_style(
                        StyleRefinement::default()
                            .pl(left)
                            .pr(right)
                            .pb(ui(ROW_GAP)),
                    )
                    .size_full(),
                )
                .into_any_element()
        } else {
            self.opening(&palette, cx)
        };
        div()
            .id(SharedString::from(format!("chat-{}", self.pane_id)))
            .test_support()
            .role(Role::Group)
            .aria_label("Chat")
            .track_focus(&self.focus)
            .key_context(super::CONTEXT)
            // Escape that nothing inside the chat took.
            .on_action(cx.listener(|this, _: &super::Escape, window, cx| this.escape(window, cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(palette.chrome_bg)
            .text_color(palette.chrome_ink)
            .text_size(ui(14.5))
            .line_height(relative(1.45))
            // Captured: Kit's text view takes the press itself. Any button,
            // as in a terminal pane, says the agent's call has been seen.
            .capture_any_mouse_down(move |event, _, cx| {
                if event.button == MouseButton::Left {
                    *press.lock().expect("the press is never poisoned") = Some(event.position);
                }
                note_activity(&profile_id, &pane_id, cx);
            })
            .child(
                canvas(
                    move |bounds: Bounds<Pixels>, window, _| {
                        if measured.replace(Some(bounds.size.width)) != Some(bounds.size.width) {
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(self.updating(left, right, &palette))
            .child(timeline)
            .children(self.stale(left, right, &palette, cx))
            .child(self.composer_area(left, right, &palette, window, cx))
            .when_some(card, |chat, card| {
                chat.child(
                    div()
                        .absolute()
                        .top(ui(CARD_INSET))
                        .right(ui(CARD_INSET))
                        .w(ui(CARD_WIDTH))
                        .child(card),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_column_is_centred_and_keeps_a_gutter() {
        let rem = px(16.);
        assert_eq!(column(px(1000.), false, rem), (px(180.), px(180.)));
        assert_eq!(column(px(640.), false, rem), (px(16.), px(16.)));
        assert_eq!(column(px(300.), false, rem), (px(16.), px(16.)));
    }

    #[test]
    fn the_column_moves_left_of_the_details_card_when_both_fit() {
        let rem = px(16.);
        // `max(24px, (100% - 252px - 640px) / 2)`.
        assert_eq!(column(px(1200.), true, rem), (px(154.), px(406.)));
        assert_eq!(column(px(940.), true, rem), (px(24.), px(276.)));
        // Too narrow for both: the column stays centred under the card.
        assert_eq!(column(px(900.), true, rem), (px(130.), px(130.)));
    }

    #[test]
    fn the_column_follows_the_ui_scale() {
        assert_eq!(column(px(1200.), false, px(20.)), (px(200.), px(200.)));
    }
}
