//! Settings › Keyboard: every command's shortcut, grouped as the palette
//! groups them. A row's keys are clicked to record the next chord; each change
//! is saved at once through [`commands::set_overrides`].

use std::collections::HashMap;

use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyDownEvent, Keystroke, ParentElement, Render, Role, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window,
    base::TestSupportExt,
    component::{
        Disableable, Icon, Sizable,
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
    commands::{
        self, Commands,
        glyphs::{shortcut_glyphs, shortcut_spoken},
        keymap::{
            ShortcutOverrides, is_safe_shortcut, normalize_shortcut, shortcut_for,
            shortcut_from_keystroke,
        },
        registry::{COMMANDS, CommandDefinition, CommandId, Platform, command_by_id},
    },
    context_menu,
    theme::{
        self,
        metrics::{TYPE_LABEL, TYPE_SECONDARY, ui},
    },
    ui::{
        dialog::{self, Action},
        icons::Glyph,
    },
};

/// A chord with no command modifier, named as the platform names its keys.
fn needs_modifier(platform: Platform) -> &'static str {
    match platform {
        Platform::Mac => "Shortcuts need ⌘, ⌥ or ⌃.",
        Platform::Linux => "Shortcuts need Ctrl, Alt or Super.",
    }
}

/// `overrides` with `id` bound to `shortcut` (`None`: no key). A binding equal
/// to the command's default is stored as no override at all, so "changed"
/// means changed.
pub fn with_binding(
    overrides: &ShortcutOverrides,
    id: CommandId,
    shortcut: Option<&str>,
    platform: Platform,
) -> ShortcutOverrides {
    let mut next = overrides.clone();
    let default = command_by_id(id)
        .and_then(|command| command.default_shortcut(platform))
        .map(normalize_shortcut);
    let shortcut = shortcut.map(normalize_shortcut);
    if shortcut == default {
        next.remove(id);
    } else {
        next.insert(id.to_owned(), shortcut);
    }
    next
}

/// Whether `command`'s binding is other than its default.
pub fn is_changed(
    command: &CommandDefinition,
    platform: Platform,
    overrides: &ShortcutOverrides,
) -> bool {
    overrides.get(command.id).is_some_and(|stored| {
        stored.as_deref().map(normalize_shortcut)
            != command.default_shortcut(platform).map(normalize_shortcut)
    })
}

/// What giving `id` the chord `shortcut` comes to.
#[derive(Debug, PartialEq, Eq)]
pub enum Assignment {
    /// It already has it.
    Unchanged,
    Bound(ShortcutOverrides),
    /// Another command has it; nothing changes until the user chooses.
    Conflict(CommandId),
}

fn current(id: CommandId, platform: Platform, overrides: &ShortcutOverrides) -> Option<String> {
    command_by_id(id)
        .and_then(|command| shortcut_for(command, platform, overrides))
        .map(normalize_shortcut)
}

pub fn assign(
    id: CommandId,
    shortcut: &str,
    platform: Platform,
    overrides: &ShortcutOverrides,
) -> Assignment {
    let pressed = normalize_shortcut(shortcut);
    if current(id, platform, overrides).as_deref() == Some(pressed.as_str()) {
        return Assignment::Unchanged;
    }
    match COMMANDS.iter().find(|command| {
        command.id != id && current(command.id, platform, overrides).as_deref() == Some(&pressed)
    }) {
        Some(other) => Assignment::Conflict(other.id),
        None => Assignment::Bound(with_binding(overrides, id, Some(&pressed), platform)),
    }
}

/// `id` takes `shortcut` and `other`, which had it, takes `id`'s old one.
pub fn swap(
    id: CommandId,
    shortcut: &str,
    other: CommandId,
    platform: Platform,
    overrides: &ShortcutOverrides,
) -> ShortcutOverrides {
    let previous = current(id, platform, overrides);
    let next = with_binding(overrides, other, previous.as_deref(), platform);
    with_binding(&next, id, Some(shortcut), platform)
}

/// `id` takes `shortcut` and `other`, which had it, is left with none.
pub fn replace(
    id: CommandId,
    shortcut: &str,
    other: CommandId,
    platform: Platform,
    overrides: &ShortcutOverrides,
) -> ShortcutOverrides {
    let next = with_binding(overrides, other, None, platform);
    with_binding(&next, id, Some(shortcut), platform)
}

/// Whether the search matches the command's title, group or keys.
pub fn matches_search(
    command: &CommandDefinition,
    query: &str,
    platform: Platform,
    overrides: &ShortcutOverrides,
) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let shortcut = shortcut_for(command, platform, overrides);
    [
        Some(command.title.to_owned()),
        Some(command.group.to_owned()),
        shortcut.map(str::to_owned),
        shortcut_glyphs(shortcut, platform),
    ]
    .into_iter()
    .flatten()
    .any(|text| text.to_lowercase().contains(&query))
}

/// The commands under their group headings, in the palette's group order.
/// The positional selectors sit apart at the end of the registry, so groups
/// are gathered rather than read as runs.
pub fn grouped<'a>(
    commands: impl IntoIterator<Item = &'a CommandDefinition>,
) -> Vec<(&'static str, Vec<&'a CommandDefinition>)> {
    let mut groups: Vec<(&'static str, Vec<&CommandDefinition>)> = Vec::new();
    for command in commands {
        match groups.iter_mut().find(|(group, _)| *group == command.group) {
            Some((_, members)) => members.push(command),
            None => groups.push((command.group, vec![command])),
        }
    }
    groups
}

/// What a row says under its keys.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowNotice {
    /// No ⌘, ⌥ or ⌃ (Ctrl, Alt or Super).
    NeedsModifier,
    /// A key GPUI has no name for, so the chord could never match a press.
    Unnamed,
    Conflict {
        shortcut: String,
        other: CommandId,
    },
}

pub struct KeyboardSettings {
    search: Entity<InputState>,
    changed_only: bool,
    /// The row whose keys take the next chord.
    recording: Option<CommandId>,
    recorder: FocusHandle,
    /// Each row's keys, so a recording hands the keyboard back to its row and
    /// a row the keyboard reaches scrolls into view.
    keys_focus: HashMap<CommandId, FocusHandle>,
    /// The shortcut list, the one part of the tab that scrolls.
    scroll: ScrollHandle,
    /// The row whose keys had the keyboard when last drawn.
    revealed: Option<CommandId>,
    notice: Option<(CommandId, RowNotice)>,
    _subscriptions: Vec<Subscription>,
}

impl KeyboardSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search shortcuts"));
        let recorder = cx.focus_handle();
        let subscriptions = vec![
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            // Clicking or tabbing away ends a recording, as Escape does.
            cx.on_blur(&recorder, window, |this, _, cx| {
                this.recording = None;
                if matches!(
                    this.notice,
                    Some((_, RowNotice::NeedsModifier | RowNotice::Unnamed))
                ) {
                    this.notice = None;
                }
                cx.notify();
            }),
        ];
        Self {
            search,
            changed_only: false,
            recording: None,
            recorder,
            keys_focus: HashMap::new(),
            scroll: ScrollHandle::new(),
            revealed: None,
            notice: None,
            _subscriptions: subscriptions,
        }
    }

    /// Ends a recording; whether there was one. Escape asks this before it
    /// closes Settings, so it ends the recording and nothing else.
    pub fn cancel_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.recording.is_none() {
            return false;
        }
        self.stop_recording(window, cx);
        self.notice = None;
        cx.notify();
        true
    }

    /// The recorder goes and the keyboard goes back to the row's keys, inside
    /// Settings, so a second Escape still closes it.
    fn stop_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.recording.take() else {
            return;
        };
        if self.recorder.is_focused(window)
            && let Some(keys) = self.keys_focus.get(id)
        {
            keys.focus(window, cx);
        }
    }

    fn start_recording(&mut self, id: CommandId, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = Some(id);
        self.notice = None;
        self.recorder.focus(window, cx);
        cx.notify();
    }

    /// The rows the search and the Changed filter leave.
    fn shown(
        &self,
        platform: Platform,
        overrides: &ShortcutOverrides,
        cx: &App,
    ) -> Vec<&'static CommandDefinition> {
        let query = self.search.read(cx).value().to_string();
        COMMANDS
            .iter()
            .filter(|command| {
                (!self.changed_only || is_changed(command, platform, overrides))
                    && matches_search(command, &query, platform, overrides)
            })
            .collect()
    }

    /// After a button that goes with what it did: the keyboard to the row's
    /// keys, or to the search when the Changed filter dropped the row.
    fn return_focus(&mut self, id: CommandId, window: &mut Window, cx: &mut Context<Self>) {
        let (platform, overrides) = Self::keymap(cx);
        let shown = self
            .shown(platform, &overrides, cx)
            .iter()
            .any(|command| command.id == id);
        match self.keys_focus.get(id).filter(|_| shown) {
            Some(keys) => keys.focus(window, cx),
            None => self
                .search
                .update(cx, |search, cx| search.focus(window, cx)),
        }
    }

    fn keymap(cx: &App) -> (Platform, ShortcutOverrides) {
        let commands = cx.global::<Commands>();
        (commands.platform(), commands.overrides().clone())
    }

    fn save(&mut self, overrides: ShortcutOverrides, window: &mut Window, cx: &mut Context<Self>) {
        self.notice = None;
        commands::set_overrides(overrides, window, cx);
        cx.notify();
    }

    /// The chord pressed while `recording`.
    fn record(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.recording else {
            return;
        };
        if keystroke.is_modifier_key() {
            return;
        }
        let bare = !keystroke.modifiers.modified();
        let (platform, overrides) = Self::keymap(cx);
        match keystroke.key.as_str() {
            "escape" if bare => {
                self.cancel_recording(window, cx);
                return;
            }
            "backspace" if bare => {
                self.stop_recording(window, cx);
                self.save(with_binding(&overrides, id, None, platform), window, cx);
                return;
            }
            _ => {}
        }
        let shortcut = shortcut_from_keystroke(keystroke);
        // Still recording after either: the next chord may be one it can take.
        if !is_safe_shortcut(&shortcut) {
            self.notice = Some((id, RowNotice::NeedsModifier));
            cx.notify();
            return;
        }
        if crate::menus::gpui_keystroke(&shortcut).is_none() {
            self.notice = Some((id, RowNotice::Unnamed));
            cx.notify();
            return;
        }
        self.stop_recording(window, cx);
        self.offer(id, &shortcut, window, cx);
    }

    fn offer(
        &mut self,
        id: CommandId,
        shortcut: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (platform, overrides) = Self::keymap(cx);
        match assign(id, shortcut, platform, &overrides) {
            Assignment::Unchanged => self.notice = None,
            Assignment::Bound(next) => return self.save(next, window, cx),
            Assignment::Conflict(other) => {
                self.notice = Some((
                    id,
                    RowNotice::Conflict {
                        shortcut: normalize_shortcut(shortcut),
                        other,
                    },
                ));
            }
        }
        cx.notify();
    }

    /// Back to the default, which another command may hold by now.
    fn reset(&mut self, id: CommandId, window: &mut Window, cx: &mut Context<Self>) {
        let (platform, overrides) = Self::keymap(cx);
        match command_by_id(id).and_then(|command| command.default_shortcut(platform)) {
            Some(default) => self.offer(id, default, window, cx),
            None => self.save(with_binding(&overrides, id, None, platform), window, cx),
        }
    }

    fn resolve(&mut self, replace_other: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, RowNotice::Conflict { shortcut, other })) = self.notice.clone() else {
            return;
        };
        let (platform, overrides) = Self::keymap(cx);
        let next = if replace_other {
            replace(id, &shortcut, other, platform, &overrides)
        } else {
            swap(id, &shortcut, other, platform, &overrides)
        };
        self.save(next, window, cx);
    }

    /// What the row's notice says.
    fn notice_text(
        notice: &RowNotice,
        command: &CommandDefinition,
        platform: Platform,
        overrides: &ShortcutOverrides,
    ) -> String {
        match notice {
            RowNotice::NeedsModifier => needs_modifier(platform).to_owned(),
            RowNotice::Unnamed => "That key can't be part of a shortcut.".to_owned(),
            RowNotice::Conflict { shortcut, other } => {
                let glyphs = |shortcut: Option<&str>| shortcut_glyphs(shortcut, platform);
                let taken = format!(
                    "{} is used by {}.",
                    glyphs(Some(shortcut)).unwrap_or_default(),
                    command_by_id(other).map_or("", |other| other.title),
                );
                match glyphs(shortcut_for(command, platform, overrides)) {
                    Some(own) => format!("{taken} Swap gives it {own}."),
                    None => taken,
                }
            }
        }
    }

    fn row(
        &self,
        command: &'static CommandDefinition,
        keys_focus: &FocusHandle,
        platform: Platform,
        overrides: &ShortcutOverrides,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let palette = theme::palette(cx);
        let id = command.id;
        let shortcut = shortcut_for(command, platform, overrides);
        let keys: gpui_kit::AnyElement = if self.recording == Some(id) {
            div()
                .id("shortcut-recorder")
                .test_support()
                .role(Role::Button)
                .aria_label(format!("Press the new shortcut for {}", command.title))
                .track_focus(&self.recorder)
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.record(&event.keystroke, window, cx);
                }))
                .px(ui(10.))
                .py(ui(5.))
                .rounded(ui(4.))
                .border_1()
                .border_color(palette.accent)
                .text_size(ui(TYPE_SECONDARY))
                .text_color(palette.chrome_dim)
                .child("Press a shortcut…")
                .into_any_element()
        } else {
            let spoken = shortcut_spoken(shortcut, platform).unwrap_or_else(|| "none".into());
            let glyphs = shortcut_glyphs(shortcut, platform);
            let unbound = glyphs.is_none();
            // The cell takes the hover, so the keys read as something to press.
            div()
                .id(SharedString::from(format!("shortcut-keys-cell-{id}")))
                .rounded(ui(4.))
                .hover(|style| style.bg(palette.chrome_hover))
                .child(
                    Button::new(SharedString::from(format!("shortcut-keys-{id}")))
                        .small()
                        .ghost()
                        .track_focus(keys_focus)
                        .label(glyphs.unwrap_or_else(|| "Add shortcut".to_owned()))
                        .when(unbound, |button| button.text_color(palette.chrome_faint))
                        .accessibility_label(format!("Shortcut for {}: {spoken}", command.title))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_recording(id, window, cx);
                        })),
                )
                .into_any_element()
        };
        let changed = is_changed(command, platform, overrides);
        let notice = self
            .notice
            .as_ref()
            .filter(|(row, _)| *row == id)
            .map(|(_, notice)| notice.clone());
        v_flex()
            .id(SharedString::from(format!("shortcut-row-{id}")))
            .relative()
            .py(ui(3.))
            .child(context_menu::anchor(keys_focus))
            .child(
                h_flex()
                    .items_center()
                    .gap(ui(8.))
                    .child(div().flex_1().min_w_0().truncate().child(command.title))
                    .child(div().flex_none().child(keys))
                    // A fixed column, so the keys line up whether or not a
                    // row has changed.
                    .child(div().flex_none().w(ui(24.)).when(changed, |this| {
                        this.child(
                            Button::new(SharedString::from(format!("shortcut-reset-{id}")))
                                .xsmall()
                                .ghost()
                                .child(
                                    Icon::new(Glyph::Refresh)
                                        .size(ui(12.))
                                        .text_color(palette.chrome_dim),
                                )
                                .tooltip("Reset to default")
                                .accessibility_label(format!("Reset {}", command.title))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.reset(id, window, cx);
                                    this.return_focus(id, window, cx);
                                })),
                        )
                    })),
            )
            .when_some(notice, |this, notice| {
                let text = Self::notice_text(&notice, command, platform, overrides);
                let choices = matches!(notice, RowNotice::Conflict { .. });
                this.child(
                    h_flex()
                        .id(SharedString::from(format!("shortcut-notice-{id}")))
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(text.clone())
                        .flex_wrap()
                        .items_center()
                        .gap(ui(6.))
                        .pt(ui(2.))
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(palette.warn)
                        .child(text)
                        .when(choices, |this| {
                            let choice = |id: &'static str, label: &'static str| {
                                Button::new(id).outline().xsmall().compact().label(label)
                            };
                            this.child(choice("shortcut-swap", "Swap").on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.resolve(false, window, cx);
                                    this.return_focus(id, window, cx);
                                },
                            )))
                            .child(choice("shortcut-replace", "Replace").on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.resolve(true, window, cx);
                                    this.return_focus(id, window, cx);
                                },
                            )))
                            .child(
                                choice("shortcut-cancel", "Cancel").on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.notice = None;
                                        this.return_focus(id, window, cx);
                                        cx.notify();
                                    },
                                )),
                            )
                        }),
                )
            })
    }
}

impl Render for KeyboardSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let (platform, overrides) = Self::keymap(cx);
        let shown = self.shown(platform, &overrides, cx);
        for command in &shown {
            self.keys_focus
                .entry(command.id)
                .or_insert_with(|| cx.focus_handle());
        }
        // Tab reaching a row's keys brings the row into view, as a browser
        // scrolls to what it focuses.
        let focused = shown
            .iter()
            .map(|command| command.id)
            .find(|id| self.keys_focus[id].is_focused(window));
        if focused != self.revealed {
            self.revealed = focused;
            if let Some(id) = focused.filter(|_| window.last_input_was_keyboard()) {
                context_menu::scroll_to_row(&self.keys_focus[id], &self.scroll, window, cx);
            }
        }
        let groups = grouped(shown);
        let any_changed = COMMANDS
            .iter()
            .any(|command| is_changed(command, platform, &overrides));
        let changed_only = self.changed_only;
        let mut list = v_flex()
            .id("settings-shortcuts")
            .test_support()
            .role(Role::List)
            .aria_label("Keyboard shortcuts")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .gap(ui(12.));
        if groups.is_empty() {
            list = list.child(
                div()
                    .text_size(ui(TYPE_SECONDARY))
                    .text_color(palette.chrome_faint)
                    .child("No shortcuts match."),
            );
        }
        for (group, commands) in groups {
            let rows: Vec<_> = commands
                .into_iter()
                .map(|command| {
                    let keys_focus = self.keys_focus[command.id].clone();
                    self.row(command, &keys_focus, platform, &overrides, cx)
                })
                .collect();
            list = list.child(
                v_flex()
                    .flex_none()
                    .child(
                        div()
                            .pb(ui(2.))
                            .text_size(ui(TYPE_LABEL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.chrome_faint)
                            .child(group.to_uppercase()),
                    )
                    .children(rows),
            );
        }
        // The toolbar stays put; only the list under it scrolls.
        v_flex()
            .size_full()
            .gap(ui(12.))
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(ui(10.))
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search)
                                .id("settings-shortcuts-search")
                                .aria_label("Search shortcuts"),
                        ),
                    )
                    .child(
                        Checkbox::new("settings-shortcuts-changed")
                            .label("Changed")
                            .checked(changed_only)
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.changed_only = *checked;
                                cx.notify();
                            })),
                    )
                    .child(dialog::button(
                        Button::new("settings-shortcuts-restore")
                            .label("Restore all defaults")
                            .disabled(!any_changed)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.stop_recording(window, cx);
                                this.save(ShortcutOverrides::new(), window, cx);
                            })),
                        Action::Plain,
                        !any_changed,
                        &palette,
                    )),
            )
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overrides(entries: &[(&str, Option<&str>)]) -> ShortcutOverrides {
        entries
            .iter()
            .map(|(id, binding)| ((*id).to_owned(), binding.map(str::to_owned)))
            .collect()
    }

    #[test]
    fn a_binding_equal_to_the_default_is_no_override() {
        let none = ShortcutOverrides::new();
        let bound = with_binding(&none, "session.new", Some("Meta+J"), Platform::Mac);
        assert_eq!(bound, overrides(&[("session.new", Some("Meta+J"))]));
        assert_eq!(
            with_binding(&bound, "session.new", Some("Meta+N"), Platform::Mac),
            none
        );
        // Removing a default is an override; removing nothing is not.
        assert_eq!(
            with_binding(&none, "session.new", None, Platform::Mac),
            overrides(&[("session.new", None)])
        );
        assert_eq!(
            with_binding(&none, "view.showPorts", None, Platform::Mac),
            none
        );
        let session = command_by_id("session.new").unwrap();
        assert!(is_changed(session, Platform::Mac, &bound));
        assert!(!is_changed(
            session,
            Platform::Mac,
            &overrides(&[("session.new", Some("Meta+N"))])
        ));
    }

    #[test]
    fn a_taken_chord_conflicts_and_a_free_one_binds() {
        let none = ShortcutOverrides::new();
        assert_eq!(
            assign("commands.show", "Meta+E", Platform::Mac, &none),
            Assignment::Conflict("pane.zoom")
        );
        assert_eq!(
            assign("commands.show", "Meta+Shift+P", Platform::Mac, &none),
            Assignment::Unchanged
        );
        assert_eq!(
            assign("commands.show", "Meta+J", Platform::Mac, &none),
            Assignment::Bound(overrides(&[("commands.show", Some("Meta+J"))]))
        );
    }

    #[test]
    fn swap_trades_the_chords_and_replace_unbinds_the_other() {
        let none = ShortcutOverrides::new();
        assert_eq!(
            swap("commands.show", "Meta+E", "pane.zoom", Platform::Mac, &none),
            overrides(&[
                ("commands.show", Some("Meta+E")),
                ("pane.zoom", Some("Shift+Meta+P")),
            ])
        );
        assert_eq!(
            replace("commands.show", "Meta+E", "pane.zoom", Platform::Mac, &none),
            overrides(&[("commands.show", Some("Meta+E")), ("pane.zoom", None)])
        );
        // A command with no key gives the other none.
        assert_eq!(
            swap(
                "view.showPorts",
                "Meta+E",
                "pane.zoom",
                Platform::Mac,
                &none
            ),
            overrides(&[("view.showPorts", Some("Meta+E")), ("pane.zoom", None)])
        );
    }

    #[test]
    fn search_reads_titles_groups_and_keys() {
        let none = ShortcutOverrides::new();
        let palette = command_by_id("commands.show").unwrap();
        for query in [
            "palette",
            "application",
            "⌘⇧P",
            "meta+shift+p",
            "  PALETTE ",
        ] {
            assert!(
                matches_search(palette, query, Platform::Mac, &none),
                "{query}"
            );
        }
        assert!(!matches_search(palette, "zoom", Platform::Mac, &none));
    }

    #[test]
    fn the_positional_selectors_join_their_groups() {
        let groups = grouped(COMMANDS.iter());
        let names: Vec<_> = groups.iter().map(|(group, _)| *group).collect();
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(names.len(), unique.len(), "{names:?}");
        let workspace = &groups
            .iter()
            .find(|(group, _)| *group == "Workspace")
            .unwrap()
            .1;
        assert!(
            workspace
                .iter()
                .any(|command| command.id == "workspace.select9")
        );
    }
}
