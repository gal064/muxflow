//! The command palette. Port of `apps/desktop/src/commands/CommandPalette.tsx`:
//! 560px wide, 28px rows, group names as inline headings, subsequence matching.

use gpui_kit::{
    App, AppContext, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, Role, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Subscription, TestSupportExt, Window,
    component::{
        Icon, Sizable, WindowExt, h_flex,
        input::{Input, InputEvent, InputState, MoveDown, MoveUp},
        v_flex,
    },
    div,
    prelude::FluentBuilder,
};

use super::{
    Commands,
    dialogs::{escape_ends_composition, is_composing, open_overlay},
    fuzzy::fuzzy_rank,
    glyphs::{shortcut_glyphs, shortcut_spoken},
    keymap::shortcut_for,
    registry::{CommandDefinition, palette_commands},
    run,
};
use crate::theme::{
    self,
    metrics::{
        PALETTE_ROW_HEIGHT, PALETTE_WIDTH, RADIUS_CONTROL, TYPE_LABEL, TYPE_MENU, TYPE_META,
        TYPE_ROW, ui,
    },
};
use crate::ui::icons::Glyph;

/// Fourteen rows plus the padding.
const VISIBLE_ROWS: f32 = 14.;
const LIST_PADDING: f32 = 12.;
const VIEWPORT_LIMIT: f32 = 0.6;

pub struct CommandPalette {
    input: Entity<InputState>,
    active: usize,
    scroll: ScrollHandle,
    _subscriptions: [Subscription; 2],
}

impl CommandPalette {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type a command"));
        let changes = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.active = 0;
                this.scroll.scroll_to_item(0);
                cx.notify();
            }
        });
        let context = cx.observe_global::<Commands>(|_, cx| cx.notify());
        Self {
            input,
            active: 0,
            scroll: ScrollHandle::new(),
            _subscriptions: [changes, context],
        }
    }

    fn query(&self, cx: &App) -> SharedString {
        self.input.read(cx).value()
    }

    fn rows(&self, cx: &App) -> Vec<&'static CommandDefinition> {
        let commands: Vec<_> = palette_commands().collect();
        fuzzy_rank(&commands, &self.query(cx), |command| {
            format!("{} {}", command.title, command.group)
        })
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if is_composing(&self.input, window, cx) {
            return;
        }
        cx.stop_propagation();
        let last = self.rows(cx).len().saturating_sub(1);
        self.active = self.active.saturating_add_signed(delta).min(last);
        self.scroll.scroll_to_item(self.active);
        cx.notify();
    }

    fn invoke(
        &mut self,
        command: &'static CommandDefinition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !cx.global::<Commands>().is_available(command) {
            return;
        }
        // Close first: the command may open a dialog of its own.
        window.close_dialog(cx);
        run(command.id, window, cx);
    }

    fn invoke_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if is_composing(&self.input, window, cx) {
            return;
        }
        if let Some(command) = self.rows(cx).get(self.active).copied() {
            self.invoke(command, window, cx);
        }
    }

    fn render_row(
        &self,
        index: usize,
        command: &'static CommandDefinition,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let commands = cx.global::<Commands>();
        let platform = commands.platform();
        let enabled = commands.is_available(command);
        let shortcut = shortcut_for(command, platform, commands.overrides());
        let glyphs = shortcut_glyphs(shortcut, platform);
        let spoken = shortcut_spoken(shortcut, platform);
        let colors = theme::palette(cx);
        let selected = index == self.active && enabled;
        let row = h_flex()
            .id(SharedString::from(format!("command-{}", command.id)))
            .role(Role::ListBoxOption)
            .aria_label(command.title)
            .aria_selected(index == self.active)
            .h(ui(PALETTE_ROW_HEIGHT))
            .px(ui(14.))
            .gap(ui(10.))
            .w_full()
            .text_size(ui(TYPE_ROW))
            .text_color(if selected {
                colors.accent_ink
            } else {
                colors.chrome_ink
            })
            .when(selected, |row| row.bg(colors.accent))
            .when(!enabled, |row| {
                row.opacity(0.42)
                    .cursor_not_allowed()
                    .aria_description("Unavailable")
            })
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.active != index {
                    this.active = index;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.invoke(command, window, cx)))
            .child(div().flex_1().min_w_0().truncate().child(command.title));
        row.when_some(spoken, |row, spoken| row.aria_keyshortcuts(spoken))
            .when_some(glyphs, |row, glyphs| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(ui(TYPE_META))
                        .text_color(if selected {
                            colors.accent_ink
                        } else {
                            colors.chrome_faint
                        })
                        .rounded(ui(RADIUS_CONTROL))
                        .child(glyphs),
                )
            })
            .test_support()
    }
}

pub fn open(window: &mut Window, cx: &mut App) {
    let palette = cx.new(|cx| CommandPalette::new(window, cx));
    let focus = palette.read(cx).input.focus_handle(cx);
    let view = palette.clone();
    open_overlay(
        palette,
        PALETTE_WIDTH,
        focus,
        true,
        move |window, cx| view.update(cx, |palette, cx| palette.invoke_active(window, cx)),
        window,
        cx,
    );
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let rows = self.rows(cx);
        // Headings only while the list is in registry order; once a query has
        // reordered rows by score, a heading would lie about what follows it.
        let grouped = self.query(cx).trim().is_empty();
        let max_height = ui(PALETTE_ROW_HEIGHT * VISIBLE_ROWS + LIST_PADDING)
            .to_pixels(window.rem_size())
            .min(window.viewport_size().height * VIEWPORT_LIMIT);

        let mut previous_group = None;
        let list: Vec<_> = rows
            .iter()
            .copied()
            .enumerate()
            .map(|(index, command)| {
                let heading =
                    (grouped && previous_group != Some(command.group)).then_some(command.group);
                previous_group = Some(command.group);
                v_flex()
                    .when_some(heading, |row, heading| {
                        row.child(
                            div()
                                .pt(ui(8.))
                                .px(ui(14.))
                                .pb(ui(2.))
                                .text_size(ui(TYPE_LABEL))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors.chrome_faint)
                                .child(heading.to_uppercase()),
                        )
                    })
                    .child(self.render_row(index, command, cx))
            })
            .collect();

        v_flex()
            .id("command-palette")
            .role(Role::Group)
            .aria_label("Command palette")
            .capture_action(escape_ends_composition)
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| this.step(1, window, cx)))
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| this.step(-1, window, cx)))
            .child(
                h_flex()
                    .px(ui(14.))
                    .py(ui(8.))
                    .gap(ui(8.))
                    .border_b_1()
                    .border_color(colors.chrome_hairline)
                    .child(
                        Icon::new(Glyph::Search)
                            .xsmall()
                            .text_color(colors.chrome_faint),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.input)
                                .id("command-palette-input")
                                .role(Role::ComboBox)
                                .aria_label("Search commands")
                                .appearance(false)
                                .text_size(ui(TYPE_MENU)),
                        ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui(TYPE_META))
                            .text_color(colors.chrome_faint)
                            .child("↑↓ navigate · ↩ run · esc"),
                    ),
            )
            .child(if list.is_empty() {
                div()
                    .id("command-palette-empty")
                    .aria_label("No matching command.")
                    .test_support()
                    .px(ui(14.))
                    .py(ui(12.))
                    .text_color(colors.chrome_dim)
                    .child("No matching command.")
                    .into_any_element()
            } else {
                div()
                    .id("command-results")
                    .role(Role::ListBox)
                    .aria_label("Commands")
                    .max_h(max_height)
                    .pt(ui(4.))
                    .pb(ui(8.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(list)
                    .test_support()
                    .into_any_element()
            })
    }
}
