//! How a chat is drawn: one column of the conversation over the composer.
//! The measures are the design's (`docs/design/chat-mock.html`), in CSS
//! pixels at 100% UI scale.

use std::{rc::Rc, sync::Arc};

use desktop_core::connection::chat::{
    ChatConnection, ChatItem, ChatItemKind, ChatTool, ChatToolKind, ChatToolStatus, ChatTurnState,
};
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, Div, ExternalPaths, FontWeight,
    HighlightStyle, Hsla, ImageCacheError, ImageSource, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, Render, Role, SharedString, StatefulInteractiveElement,
    StyleRefinement, Styled, Window,
    base::{TextView, TextViewStyle},
    canvas,
    component::{
        Icon, h_flex, menu::ContextMenuExt, message_scroller::MessageScroller,
        scroll::ScrollableElement, tooltip::Tooltip, v_flex,
    },
    div,
    prelude::FluentBuilder,
    px, relative, rems,
};

use crate::{
    context_menu::{self, Entry, Item},
    fonts,
    markdown::is_click,
    terminal::integration::{agents::note_activity, transfers::InternalPathDrag},
    theme::{self, Palette, metrics::ui},
    ui::{
        dialog::{self, Action},
        motion::WorkingIcon,
    },
};

use super::{
    DETAILS_ROOM, DetailsContext, cards, details_card,
    icon::{IconName, icon},
    open,
    pane::{ChatPane, Row},
    timeline::{self, Entry as Laid},
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
/// How the host's notice of a compaction starts, whichever agent it was.
const COMPACTED: &str = "Context compacted";

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

/// `.u`: one of the user's messages, its files' chips above its text.
fn bubble(text: SharedString, chips: Option<AnyElement>, palette: &Palette) -> Div {
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
        .children(chips)
        .when(!text.is_empty(), |bubble| bubble.child(text))
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

/// `.mb`: one of a message's icon buttons, faint until pointed at. A hidden
/// one keeps its room, so nothing moves when the row shows on hover or focus.
fn message_button(
    id: String,
    name: &'static str,
    glyph: IconName,
    ink: Hsla,
    // The message's hover group, while the button shows only on hover.
    hidden_in: Option<SharedString>,
    p: &Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let (hover, wash) = (p.chrome_hover, p.accent_wash);
    // Faint turns strong under the pointer; a button in a state keeps its colour.
    let pointed = if ink == Hsla::from(p.chrome_faint) {
        p.chrome_ink_strong.into()
    } else {
        ink
    };
    div()
        .id(SharedString::from(id))
        .test_support()
        .role(Role::Button)
        .aria_label(name)
        .flex_none()
        .size(ui(24.))
        .rounded(ui(6.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tab_index(0)
        .text_color(ink)
        .hover(move |style| style.bg(hover).text_color(pointed))
        .focus_visible(move |style| style.opacity(1.).bg(wash).text_color(pointed))
        .when_some(hidden_in, |button, group| {
            button
                .opacity(0.)
                .group_hover(group, |style| style.opacity(1.))
        })
        .tooltip(move |window, cx| Tooltip::new(name).build(window, cx))
        // Drawn in the button's colour, which hover changes.
        .child(Icon::new(glyph).size(ui(14.)))
        .on_click(on_click)
        .into_any_element()
}

/// `.mrow`: a message's buttons, 24px tall whether shown or not.
fn message_buttons(buttons: Vec<AnyElement>) -> Div {
    h_flex().h(ui(24.)).gap(ui(2.)).children(buttons)
}

/// `+120 −14`: what edits added and removed.
fn line_counts(added: u32, removed: u32, palette: &Palette) -> Div {
    h_flex()
        .flex_none()
        .gap(ui(5.))
        .child(div().text_color(palette.ok).child(format!("+{added}")))
        .child(
            div()
                .text_color(palette.danger)
                // A minus sign, as wide as the plus.
                .child(format!("\u{2212}{removed}")),
        )
}

/// A tool row's leading icon, by its kind.
fn tool_icon(tool: &ChatTool) -> IconName {
    match ChatToolKind::try_from(tool.kind).unwrap_or_default() {
        ChatToolKind::Command => IconName::SquareTerminal,
        ChatToolKind::Read => IconName::FileText,
        ChatToolKind::Edit => IconName::FilePenLine,
        ChatToolKind::Search => IconName::Search,
        ChatToolKind::Web => IconName::Globe,
        ChatToolKind::Subagent => IconName::Bot,
        ChatToolKind::Mcp | ChatToolKind::Other | ChatToolKind::Unspecified => IconName::Wrench,
    }
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

/// A finished subagent's row as it reads: "Explore: Find where uploads are
/// staged · 12 tools · 61k tokens".
fn subagent_title(tool: &ChatTool) -> SharedString {
    let (kind, asked) = timeline::subagent_name(tool);
    let figures = timeline::figures(tool);
    if figures.is_empty() {
        format!("{kind}: {asked}").into()
    } else {
        format!("{kind}: {asked} · {figures}").into()
    }
}

/// `.tool .r`: the row's outcome, or its line counts for an edit that made any.
fn outcome(tool: &ChatTool, palette: &Palette) -> AnyElement {
    let status = ChatToolStatus::try_from(tool.status).unwrap_or_default();
    let edit = tool.kind == i32::from(ChatToolKind::Edit);
    if edit && status != ChatToolStatus::Failed && (tool.added > 0 || tool.removed > 0) {
        return line_counts(tool.added, tool.removed, palette).into_any_element();
    }
    if status == ChatToolStatus::Running && tool.result_summary.is_empty() {
        // The still working mark: what runs is said by the working line.
        return WorkingIcon::new(10., palette.state_working).into_any_element();
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
            Some(Row::Entry(entry)) => match self.entries.get(entry).copied() {
                Some(Laid::Item { index, reply_end }) => {
                    self.item_row(index, reply_end, &palette, cx)
                }
                Some(group @ Laid::Group { .. }) => self.group_row(group, &palette, cx),
                None => div().into_any_element(),
            },
            Some(Row::Working) => self.working_row(&palette, cx),
            Some(Row::Sending(sending)) => {
                let sending = &self.sending[sending];
                let text = sending.text.clone();
                let sent: Vec<_> = sending
                    .attachments
                    .iter()
                    .filter_map(super::attach::Attachment::wire)
                    .collect();
                let chips = self.message_chips(&sending.id, &sent, &palette);
                // Dimmed until the host has it.
                h_flex()
                    .w_full()
                    .justify_end()
                    .child(
                        bubble(text.clone(), chips, &palette)
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

    fn item_row(
        &mut self,
        index: usize,
        reply_end: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(kind) = self.chat.items.get(index).map(|item| item.kind) else {
            return div().into_any_element();
        };
        match ChatItemKind::try_from(kind).unwrap_or_default() {
            ChatItemKind::User => self.user_row(index, palette, cx),
            ChatItemKind::Assistant => self.assistant_row(index, reply_end, palette, cx),
            ChatItemKind::Tool => self.tool_row(index, palette, cx),
            ChatItemKind::Reasoning => self.thought_row(index, palette, cx),
            ChatItemKind::Notice | ChatItemKind::Unspecified => {
                let text: SharedString = self.chat.items[index].text.clone().into();
                // A notice carries no kind: a compaction's is known by what
                // the host words it as, and has its mark (S9).
                let compacted = text.starts_with(COMPACTED);
                h_flex()
                    .w_full()
                    .justify_center()
                    .gap(ui(6.))
                    .text_size(ui(12.5))
                    .text_color(palette.chrome_faint)
                    .when(compacted, |notice| {
                        notice.child(icon(IconName::Shrink, 12., palette.chrome_faint))
                    })
                    .child(div().min_w_0().text_center().child(text))
                    .into_any_element()
            }
        }
    }

    /// A message's Copy: a check for a second once it has copied.
    fn copy_button(
        &self,
        index: usize,
        shown: bool,
        group: &SharedString,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = &self.chat.items[index].id;
        let copied = self.just_copied(id);
        let (name, glyph, ink) = if copied {
            ("Copied", IconName::Check, palette.ok.into())
        } else {
            ("Copy", IconName::Copy, palette.chrome_faint.into())
        };
        message_button(
            format!("chat-copy-{id}"),
            name,
            glyph,
            ink,
            (!shown && !copied).then(|| group.clone()),
            palette,
            cx.listener(move |this, _, _, cx| this.copy_message(index, cx)),
        )
    }

    /// `.msg.me`: the bubble, and under it at the right Copy and Edit, shown
    /// while the pointer or the keyboard is on the message, and while it is
    /// the one being edited.
    fn user_row(&self, index: usize, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let item: &ChatItem = &self.chat.items[index];
        let id = item.id.clone();
        let text: SharedString = item.text.clone().into();
        let chips = self.message_chips(&id, &item.attachments, palette);
        let editing = self.editing.as_deref() == Some(id.as_str());
        let group = SharedString::from(format!("chat-user-{id}"));
        let name = SharedString::from(format!("chat-item-{id}"));
        let mut buttons = vec![self.copy_button(index, editing, &group, palette, cx)];
        if self.editable(item) {
            let ink = if editing {
                palette.accent
            } else {
                palette.chrome_faint
            };
            buttons.push(message_button(
                format!("chat-edit-{id}"),
                "Edit and resend",
                IconName::Pencil,
                ink.into(),
                (!editing).then(|| group.clone()),
                palette,
                cx.listener(move |this, _, window, cx| this.edit(&id, window, cx)),
            ));
        }
        v_flex()
            .group(group)
            .w_full()
            .items_end()
            .gap(ui(3.))
            .child(copyable(
                bubble(text.clone(), chips, palette)
                    .when(editing, |bubble| bubble.border_color(palette.accent))
                    .id(name)
                    .test_support()
                    .aria_label(text.clone()),
                vec![("Copy message", text)],
            ))
            .child(message_buttons(buttons))
            .into_any_element()
    }

    /// The agent's text. The last text of a finished reply has the reply's
    /// buttons under it, at the left: Copy, and Fork from here.
    fn assistant_row(
        &mut self,
        index: usize,
        reply_end: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = self.shown_text(index, cx);
        let id = self.chat.items[index].id.clone();
        let dark = cx.global::<theme::Appearance>().is_dark;
        let pane_id = self.pane_id.clone();
        let pressed = self.pressed.clone();
        let group = SharedString::from(format!("chat-reply-{id}"));
        let buttons = reply_end.then(|| {
            let mut buttons = vec![self.copy_button(index, false, &group, palette, cx)];
            if self.capabilities().fork {
                let id = id.clone();
                buttons.push(message_button(
                    format!("chat-fork-{id}"),
                    "Fork from here",
                    IconName::GitFork,
                    palette.chrome_faint.into(),
                    Some(group.clone()),
                    palette,
                    cx.listener(move |this, _, _, cx| this.fork_reply(&id, cx)),
                ));
            }
            message_buttons(buttons)
        });
        v_flex()
            .id(SharedString::from(format!("chat-item-{id}")))
            .test_support()
            .group(group.clone())
            .w_full()
            .min_w_0()
            .gap(ui(3.))
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
            .children(buttons)
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
        let subagent = tool.kind == i32::from(ChatToolKind::Subagent);
        let name = if subagent {
            subagent_title(&tool)
        } else {
            SharedString::from(format!("{verb} {detail}"))
        };
        let mut copies = vec![("Copy", detail.clone())];
        if !tool.output_preview.is_empty() {
            copies.push(("Copy output", tool.output_preview.clone().into()));
        }
        // A subagent's row says who it was and what it was asked, in prose,
        // with its figures faint after it (S9).
        let what = if subagent {
            let (kind, asked) = timeline::subagent_name(&tool);
            let figures = timeline::figures(&tool);
            h_flex()
                .flex_1()
                .min_w_0()
                .text_size(ui(13.))
                .text_color(palette.chrome_ink)
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(format!("{kind}: {asked}"))),
                )
                .when(!figures.is_empty(), |what| {
                    what.child(
                        div()
                            .flex_none()
                            .text_color(palette.chrome_faint)
                            .child(SharedString::from(format!(" · {figures}"))),
                    )
                })
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(fonts::MONO)
                .text_size(ui(13.))
                .text_color(palette.chrome_ink)
                .child(detail)
                .into_any_element()
        };
        let head = h_flex()
            .gap(ui(8.))
            .px(ui(9.))
            .py(ui(5.))
            .text_size(ui(13.5))
            .text_color(palette.chrome_dim)
            .child(icon(tool_icon(&tool), 14., palette.chrome_faint))
            .when(!subagent, |head| {
                head.child(
                    div()
                        .flex_none()
                        .font_family(fonts::MONO)
                        .text_size(ui(12.))
                        .text_color(palette.chrome_faint)
                        .child(verb),
                )
            })
            .child(what)
            .child(
                div()
                    .flex_none()
                    .text_size(ui(12.))
                    .child(outcome(&tool, palette)),
            );
        let row = v_flex()
            .id(SharedString::from(format!("chat-tool-{id}")))
            .test_support()
            .when(subagent, |row| row.aria_label(name.clone()))
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
                this.remeasure_item(index, cx);
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

    /// A finished thought: one row, "Thought for 8s" when this pane watched
    /// it stream, that opens to its text in the box tool output uses.
    fn thought_row(&self, index: usize, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let item = &self.chat.items[index];
        let id = item.id.clone();
        let text: SharedString = item.text.clone().into();
        let open = self.expanded.contains(&id);
        let label: SharedString = match self.thought_took(&id) {
            Some(took) => format!(
                "Thought for {}",
                timeline::elapsed(took.max(std::time::Duration::from_secs(1)))
            )
            .into(),
            None => "Thought".into(),
        };
        let (hover, wash) = (palette.chrome_hover, palette.accent_wash);
        let chevron = if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        let head = h_flex()
            .id(SharedString::from(format!("chat-thought-open-{id}")))
            .test_support()
            .role(Role::Button)
            .aria_label(label.clone())
            .aria_expanded(open)
            .cursor_pointer()
            .tab_index(0)
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.bg(wash))
            .gap(ui(8.))
            .px(ui(9.))
            .py(ui(5.))
            .text_size(ui(13.5))
            .child(icon(IconName::Brain, 14., palette.chrome_faint))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(palette.chrome_ink)
                    .child(label),
            )
            .child(icon(chevron, 12., palette.chrome_faint))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded.remove(&id) {
                    this.expanded.insert(id.clone());
                }
                this.remeasure_item(index, cx);
            }));
        let body = open.then(|| {
            div()
                .id(SharedString::from(format!("chat-thought-text-{}", item.id)))
                .test_support()
                .border_t_1()
                .border_color(palette.chrome_hairline)
                .bg(palette.chrome_raised)
                .px(ui(10.))
                .py(ui(6.))
                .text_size(ui(13.5))
                .line_height(relative(1.5))
                .text_color(palette.chrome_dim)
                .child(text.clone())
        });
        copyable(
            v_flex()
                .id(SharedString::from(format!("chat-thought-{}", item.id)))
                .test_support()
                .w_full()
                .min_w_0()
                .border_1()
                .border_color(palette.chrome_hairline)
                .rounded(ui(7.))
                .bg(palette.chrome_bg)
                .overflow_hidden()
                .child(head)
                .children(body),
            vec![("Copy", text)],
        )
    }

    /// A run of steps as one line, its rows under it while it is open. The
    /// run a working turn adds to reads "N steps so far" and says nothing of
    /// its kinds: the working line under it says what runs.
    fn group_row(&self, group: Laid, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let Laid::Group {
            first,
            end,
            steps,
            live,
        } = group
        else {
            return div().into_any_element();
        };
        let turn = self.turn();
        let items = &self.chat.items;
        let key = items[first].id.clone();
        let open = self.open_groups.contains(&key);
        let drawn: Vec<usize> = (first..end)
            .filter(|index| timeline::is_drawn(&items[*index], turn))
            .collect();
        let summary = timeline::summary(drawn.iter().map(|index| &items[*index]), |item| {
            self.thought_took(&item.id)
        });
        let title = timeline::steps_label(steps, live);
        let failed = (summary.failed > 0).then(|| format!("{} failed", summary.failed));
        let counts = if live { String::new() } else { summary.counts };
        let name: SharedString = [Some(title.clone()), failed.clone(), Some(counts.clone())]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
            .into();
        // Closed, the edited files stay in sight, each opening its diff.
        let chips = (!open).then(|| {
            let (names, rest) = timeline::chips(&summary.files);
            let chip = |id: String, label: String| {
                let (accent, ring) = (palette.accent, palette.accent);
                div()
                    .id(SharedString::from(id))
                    .test_support()
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .flex_none()
                    .cursor_pointer()
                    .tab_index(0)
                    .font_family(fonts::MONO)
                    .text_size(ui(12.))
                    .line_height(relative(1.6))
                    .text_color(palette.chrome_ink)
                    .border_1()
                    .border_color(palette.chrome_hairline)
                    .bg(palette.chrome_raised)
                    .rounded(ui(4.))
                    .px(ui(5.))
                    .hover(move |style| style.border_color(accent).text_color(accent))
                    .focus_visible(move |style| style.border_color(ring))
                    .child(label)
            };
            let mut chips: Vec<AnyElement> = names
                .iter()
                .zip(&summary.files)
                .enumerate()
                .map(|(at, (label, path))| {
                    let path = path.clone();
                    let tip = SharedString::from(path.clone());
                    chip(format!("chat-group-file-{key}-{at}"), (*label).to_owned())
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // The file's diff, not the group's line under it.
                            cx.stop_propagation();
                            this.open_edit(&path, cx);
                        }))
                        .into_any_element()
                })
                .collect();
            if let Some(rest) = rest {
                let key = key.clone();
                chips.push(
                    chip(format!("chat-group-more-{key}"), format!("+{rest}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_group(&key, cx);
                        }))
                        .into_any_element(),
                );
            }
            chips
        });
        let lines = (summary.added > 0 || summary.removed > 0).then(|| {
            line_counts(summary.added, summary.removed, palette)
                .text_size(ui(12.))
                .into_any_element()
        });
        let (hover, wash) = (palette.chrome_hover, palette.accent_wash);
        let chevron = if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        let toggle = key.clone();
        let head = h_flex()
            .id(SharedString::from(format!("chat-group-{key}")))
            .test_support()
            .role(Role::Button)
            .aria_label(name)
            .aria_expanded(open)
            .cursor_pointer()
            .tab_index(0)
            .gap(ui(8.))
            .px(ui(6.))
            .mx(ui(-6.))
            .py(ui(3.))
            .rounded(ui(6.))
            .min_w_0()
            .text_size(ui(13.5))
            .text_color(palette.chrome_dim)
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.bg(wash))
            .child(icon(chevron, 14., palette.chrome_faint))
            .child(
                div()
                    .flex_none()
                    .text_color(palette.chrome_ink)
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .children(failed.map(|failed| {
                div()
                    .flex_none()
                    .text_color(palette.danger)
                    .font_weight(FontWeight::MEDIUM)
                    .child(failed)
            }))
            // Cut first when the line is short of room; the chips and counts stay.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(palette.chrome_faint)
                    .child(counts),
            )
            .children(chips.into_iter().flatten())
            .children(lines)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(&toggle, cx)));
        let body = open.then(|| {
            v_flex()
                .gap(ui(6.))
                .pl(ui(22.))
                .children(drawn.iter().map(|index| {
                    if items[*index].tool.is_some() {
                        self.tool_row(*index, palette, cx)
                    } else {
                        self.thought_row(*index, palette, cx)
                    }
                }))
        });
        v_flex()
            .w_full()
            .min_w_0()
            .gap(ui(6.))
            .child(head)
            .children(body)
            .into_any_element()
    }

    /// `.stl`: the one line that says the turn works, under the conversation.
    /// Its mark holds still; only the seconds change, once a second. Stop,
    /// once pressed, says so here until the turn ends.
    fn working_row(&self, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let said = |label: &str| timeline::Doing {
            label: label.into(),
            preview: None,
        };
        let doing = if self.stopping {
            said("Stopping…")
        } else if self.compacting() {
            // Whatever else the turn is doing.
            said("Compacting context…")
        } else {
            timeline::doing(&self.chat.items)
        };
        let label: SharedString = doing.label.into();
        let escape = self.stoppable() && !self.stopping;
        let time = timeline::elapsed(self.elapsed(cx));
        v_flex()
            .id("chat-working")
            .test_support()
            .role(Role::Status)
            .aria_label(label.clone())
            .min_w_0()
            .gap(ui(2.))
            .child(
                h_flex()
                    .gap(ui(8.))
                    .px(ui(1.))
                    .min_w_0()
                    .text_size(ui(13.5))
                    .text_color(palette.chrome_dim)
                    .child(
                        // `.stl .mk`.
                        h_flex()
                            .flex_none()
                            .w(ui(14.))
                            .justify_center()
                            .child(WorkingIcon::new(10., palette.state_working)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(palette.chrome_ink)
                            .child(label),
                    )
                    .child(
                        div()
                            .id("chat-working-time")
                            .test_support()
                            .aria_label(SharedString::from(time.clone()))
                            .flex_none()
                            .text_color(palette.chrome_faint)
                            .child(time),
                    )
                    .when(escape, |line| {
                        line.child(
                            div()
                                .id("chat-working-escape")
                                .test_support()
                                .flex_none()
                                .text_color(palette.chrome_faint)
                                .child("· Esc to stop"),
                        )
                    }),
            )
            .children(doing.preview.map(|preview| {
                // `.prev`: the latest sentence of the thinking, one faint line.
                div()
                    .id("chat-working-preview")
                    .test_support()
                    .aria_label(SharedString::from(preview.clone()))
                    .pl(ui(23.))
                    .min_w_0()
                    .truncate()
                    .text_size(ui(13.))
                    .text_color(palette.chrome_faint)
                    .child(preview)
            }))
            .children(self.subagent_rows(palette))
            .into_any_element()
    }

    /// `.subs`: a row for each subagent that runs while the turn works,
    /// under the working line and outside any group (D16), in the order
    /// they started.
    fn subagent_rows(&self, palette: &Palette) -> Option<AnyElement> {
        if self.turn() != ChatTurnState::Working {
            return None;
        }
        let rows: Vec<AnyElement> = timeline::running_subagents(&self.chat.items)
            .filter_map(|item| {
                let tool = item.tool.as_ref()?;
                let (kind, asked) = timeline::subagent_name(tool);
                let line =
                    timeline::subagent_line(tool, Self::since_unix_millis(item.at_unix_millis));
                let name = SharedString::from(format!("{kind} {asked}"));
                Some(
                    h_flex()
                        .id(SharedString::from(format!("chat-subagent-{}", item.id)))
                        .test_support()
                        .role(Role::Status)
                        .aria_label(name)
                        .items_start()
                        .gap(ui(8.))
                        .min_w_0()
                        .text_size(ui(12.5))
                        .child(
                            // `.sub .mk`: the mark, on the first line.
                            h_flex()
                                .flex_none()
                                .w(ui(14.))
                                .h(ui(18.))
                                .justify_center()
                                .child(WorkingIcon::new(10., palette.state_working)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(ui(1.))
                                .child(
                                    h_flex()
                                        .gap(ui(7.))
                                        .min_w_0()
                                        .child(
                                            div()
                                                .flex_none()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(palette.chrome_ink_strong)
                                                .child(SharedString::from(kind.to_owned())),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(palette.chrome_ink)
                                                .child(SharedString::from(asked.to_owned())),
                                        ),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "chat-subagent-line-{}",
                                            item.id
                                        )))
                                        .test_support()
                                        .aria_label(SharedString::from(line.clone()))
                                        .min_w_0()
                                        .truncate()
                                        .text_size(ui(12.))
                                        .text_color(palette.chrome_dim)
                                        .child(line),
                                ),
                        )
                        .into_any_element(),
                )
            })
            .collect();
        (!rows.is_empty()).then(|| {
            v_flex()
                .gap(ui(8.))
                .pl(ui(22.))
                .pt(ui(6.))
                .min_w_0()
                .children(rows)
                .into_any_element()
        })
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
            // Files dropped anywhere on the chat are attached (S6).
            .group(super::attach::DROP_GROUP)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.dropped(paths, window, cx);
            }))
            .on_drop(cx.listener(|this, drag: &InternalPathDrag, window, cx| {
                this.dropped_from_app(drag, window, cx);
            }))
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
            // Last, over everything, while files are dragged over the pane.
            .child(super::attach::drop_hint(&palette))
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
