//! Rebinding commands. Port of the Tauri app's `commands/ShortcutEditorDialog.tsx`.

use gpui_kit::{
    App, AppContext, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, Role, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Subscription, TestSupportExt, Window,
    component::{
        Disableable, IconName, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    div,
    prelude::FluentBuilder,
};

use super::{
    Commands,
    dialogs::{escape_ends_composition, open_overlay},
    glyphs::shortcut_glyphs,
    keymap::{ShortcutOverrides, shortcut_collisions, unsafe_shortcut_bindings},
    registry::{COMMANDS, CommandDefinition, Platform},
    set_overrides,
};
use crate::theme::{
    self,
    metrics::{NARROW_DIALOGS, TYPE_META, TYPE_SECONDARY, narrower_than, ui},
};

const WIDTH: f32 = 620.;
const MAX_HEIGHT: f32 = 720.;
const VIEWPORT_LIMIT: f32 = 0.78;

struct Row {
    command: &'static CommandDefinition,
    input: Entity<InputState>,
}

pub struct ShortcutEditor {
    platform: Platform,
    draft: ShortcutOverrides,
    rows: Vec<Row>,
    focused_row: Option<usize>,
    scroll: ScrollHandle,
    _changes: Vec<Subscription>,
}

/// What a field shows: a removed binding shows empty, not the default it no
/// longer has, and a field left alone shows its default.
fn field_value(
    command: &CommandDefinition,
    platform: Platform,
    draft: &ShortcutOverrides,
) -> String {
    match draft.get(command.id) {
        Some(stored) => stored.clone().unwrap_or_default(),
        None => command
            .default_shortcut(platform)
            .unwrap_or_default()
            .to_owned(),
    }
}

impl ShortcutEditor {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let commands = cx.global::<Commands>();
        let platform = commands.platform();
        let draft = commands.overrides().clone();
        let mut rows = Vec::with_capacity(COMMANDS.len());
        let mut changes = Vec::with_capacity(COMMANDS.len());
        for command in &COMMANDS {
            let value = field_value(command, platform, &draft);
            let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
            changes.push(
                cx.subscribe(&input, move |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        let value = input.read(cx).value().to_string();
                        let binding = (!value.is_empty()).then_some(value);
                        this.draft.insert(command.id.to_owned(), binding);
                        cx.notify();
                    }
                }),
            );
            rows.push(Row { command, input });
        }
        Self {
            platform,
            draft,
            rows,
            focused_row: None,
            scroll: ScrollHandle::new(),
            _changes: changes,
        }
    }

    fn restore_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.clear();
        for row in &self.rows {
            let value = field_value(row.command, self.platform, &self.draft);
            row.input
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        cx.notify();
    }

    /// `narrow` stacks the field under the title, as `styles.css` does in a
    /// window 600 CSS px or narrower.
    fn render_row(&self, row: &Row, narrow: bool, cx: &App) -> impl IntoElement + use<> {
        let colors = theme::palette(cx);
        let value = field_value(row.command, self.platform, &self.draft);
        let glyphs = shortcut_glyphs(Some(&value), self.platform);
        div()
            .flex()
            .map(|row| {
                if narrow {
                    row.flex_col().items_start()
                } else {
                    row.items_center()
                }
            })
            .py(ui(6.))
            .gap(ui(if narrow { 4. } else { 12. }))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(row.command.title),
                    )
                    .child(
                        div()
                            .text_size(ui(TYPE_META))
                            .text_color(colors.chrome_dim)
                            .child(row.command.group),
                    ),
            )
            .child(
                h_flex()
                    .map(|field| {
                        if narrow {
                            field.w_full()
                        } else {
                            field.w(ui(200.))
                        }
                    })
                    .flex_none()
                    .gap(ui(6.))
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&row.input)
                                .id(SharedString::from(format!("shortcut-{}", row.command.id)))
                                .aria_label(format!("Shortcut for {}", row.command.title)),
                        ),
                    )
                    .when_some(glyphs, |field, glyphs| {
                        field.child(
                            div()
                                .flex_none()
                                .text_size(ui(TYPE_META))
                                .text_color(colors.chrome_faint)
                                .child(glyphs),
                        )
                    }),
            )
    }
}

pub fn open(window: &mut Window, cx: &mut App) {
    let editor = cx.new(|cx| ShortcutEditor::new(window, cx));
    let focus = editor.read(cx).rows[0].input.focus_handle(cx);
    open_overlay(editor, WIDTH, focus, false, |_, _| {}, window, cx);
}

impl Render for ShortcutEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        // Tab must bring the field it lands on into view.
        let focused_row = self
            .rows
            .iter()
            .position(|row| row.input.focus_handle(cx).is_focused(window));
        if focused_row != self.focused_row {
            self.focused_row = focused_row;
            if let Some(row) = focused_row {
                self.scroll.scroll_to_item(row);
            }
        }
        let unsafe_bindings = !unsafe_shortcut_bindings(&self.draft).is_empty();
        let conflicts: Vec<String> = shortcut_collisions(self.platform, &self.draft)
            .iter()
            .map(|collision| {
                shortcut_glyphs(Some(&collision.shortcut), self.platform)
                    .unwrap_or_else(|| collision.shortcut.clone())
            })
            .collect();
        let blocked = unsafe_bindings || !conflicts.is_empty();
        let narrow = narrower_than(NARROW_DIALOGS, window);
        let max_height = ui(MAX_HEIGHT)
            .to_pixels(window.rem_size())
            .min(window.viewport_size().height * VIEWPORT_LIMIT);
        let warning = |id: &'static str, text: String| {
            div()
                .id(id)
                .role(Role::Alert)
                .aria_label(text.clone())
                .px(ui(16.))
                .py(ui(8.))
                .text_size(ui(TYPE_SECONDARY))
                .text_color(colors.warn)
                .child(text)
                .test_support()
        };
        let rows: Vec<_> = self
            .rows
            .iter()
            .map(|row| self.render_row(row, narrow, cx))
            .collect();
        v_flex()
            .id("shortcut-editor")
            .role(Role::Group)
            .aria_label("Keyboard shortcuts")
            .capture_action(escape_ends_composition)
            .max_h(max_height)
            .child(
                h_flex()
                    .justify_between()
                    .px(ui(16.))
                    .py(ui(12.))
                    .border_b_1()
                    .border_color(colors.chrome_hairline)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui(16.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Keyboard shortcuts"),
                            )
                            .child(
                                div()
                                    .text_size(ui(TYPE_META))
                                    .text_color(colors.chrome_dim)
                                    .child(
                                        "Application shortcuts control the outer tmux workspace without a prefix.",
                                    ),
                            ),
                    )
                    .child(
                        Button::new("shortcut-editor-close")
                            .ghost()
                            .icon(IconName::Close)
                            .accessibility_label("Close keyboard shortcuts")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
            .when(!conflicts.is_empty(), |this| {
                this.child(warning("shortcut-warning-conflicts", format!(
                    "Conflicting shortcuts are disabled: {}",
                    conflicts.join(", ")
                )))
            })
            .when(unsafe_bindings, |this| {
                this.child(warning(
                    "shortcut-warning-unsafe",
                    "Shortcuts must include Ctrl, Alt, or Meta. Unsafe bindings are not saved."
                        .to_owned(),
                ))
            })
            .child(
                v_flex()
                    .id("shortcut-list")
                    .flex_1()
                    .min_h_0()
                    .px(ui(16.))
                    .py(ui(8.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap(ui(8.))
                    .px(ui(16.))
                    .py(ui(12.))
                    .border_t_1()
                    .border_color(colors.chrome_hairline)
                    .child(
                        Button::new("shortcut-editor-restore")
                            .label("Restore defaults")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.restore_defaults(window, cx)
                            })),
                    )
                    .child(
                        Button::new("shortcut-editor-done")
                            .label("Done")
                            .primary()
                            .disabled(blocked)
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.close_dialog(cx);
                                set_overrides(this.draft.clone(), window, cx);
                            })),
                    ),
            )
    }
}
