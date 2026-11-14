//! The New workspace dialog: a name, and the shown host to create it on. Port
//! of `commands/NewWorkspaceDialog.tsx` and the App wiring around it.

use desktop_core::app_state::load_app_state;
use gpui_kit::{
    App, AppContext, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, Role, SharedString, Styled, WeakEntity, Window,
    component::{
        Disableable, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        menu::DropdownMenu,
        v_flex,
    },
    div,
    prelude::*,
};

use crate::{
    app::Core,
    commands::dialogs::{escape_ends_composition, is_composing, open_overlay},
    connection_ui::connection_reducer::ConnectionPhase,
    context_menu::{self, Entry, Item},
    fonts,
    theme::{
        self,
        metrics::{self, ui},
    },
    ui::chrome::phase_color,
};

use super::{HostEntry, Workspaces};

const WIDTH: f32 = 620.;

/// The host the dialog opens on: the remembered one while it is still shown,
/// else the active host if it can create, else any that can, else the active
/// host, else the first.
pub fn default_host(hosts: &[HostEntry], remembered: Option<&str>, active: &str) -> Option<String> {
    let shown: Vec<&HostEntry> = hosts.iter().filter(|host| host.shown).collect();
    if let Some(remembered) =
        remembered.filter(|id| shown.iter().any(|host| host.profile_id == *id))
    {
        return Some(remembered.to_owned());
    }
    shown
        .iter()
        .find(|host| host.profile_id == active && host.can_mutate)
        .or_else(|| shown.iter().find(|host| host.can_mutate))
        .or_else(|| shown.iter().find(|host| host.profile_id == active))
        .or_else(|| shown.first())
        .map(|host| host.profile_id.clone())
}

/// What the picker says for a host: its letter and label, and its phase when
/// it is not connected.
fn host_label(host: &HostEntry) -> String {
    if host.phase == ConnectionPhase::Connected {
        format!("{} {}", host.letter, host.label)
    } else {
        format!(
            "{} {} · {}",
            host.letter,
            host.label,
            host.phase.wire_name()
        )
    }
}

pub struct NewWorkspace {
    workspaces: WeakEntity<Workspaces>,
    selected: String,
    input: Entity<InputState>,
    _changes: [gpui_kit::Subscription; 2],
}

/// The shown hosts, read live: one can connect or drop while the dialog is up.
fn shown_hosts(workspaces: &Entity<Workspaces>, cx: &App) -> Vec<HostEntry> {
    workspaces
        .read(cx)
        .hosts(cx)
        .into_iter()
        .filter(|host| host.shown)
        .collect()
}

impl NewWorkspace {
    fn hosts(&self, cx: &App) -> Vec<HostEntry> {
        self.workspaces
            .upgrade()
            .map(|workspaces| shown_hosts(&workspaces, cx))
            .unwrap_or_default()
    }

    fn selected(&self, cx: &App) -> Option<HostEntry> {
        self.hosts(cx)
            .into_iter()
            .find(|host| host.profile_id == self.selected)
    }

    fn name(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if is_composing(&self.input, window, cx) {
            return;
        }
        let name = self.name(cx);
        if name.is_empty() || !self.selected(cx).is_some_and(|host| host.can_mutate) {
            return;
        }
        window.close_dialog(cx);
        let host = self.selected.clone();
        let Some(workspaces) = self.workspaces.upgrade() else {
            return;
        };
        workspaces.update(cx, |workspaces, cx| {
            workspaces.submit_new_workspace(name, &host, cx)
        });
        super::focus_terminal(&workspaces, window, cx);
    }

    /// The pick is the preference, even if the dialog is then cancelled.
    fn pick(&mut self, host: String, cx: &mut Context<Self>) {
        if !self.hosts(cx).iter().any(|entry| entry.profile_id == host) {
            return;
        }
        super::remember_new_workspace_host(&host, cx);
        self.selected = host;
        cx.notify();
    }
}

/// Opens the dialog, or says why it cannot.
pub fn open(workspaces: &Entity<Workspaces>, window: &mut Window, cx: &mut App) {
    let hosts = shown_hosts(workspaces, cx);
    let active = workspaces.read(cx).view(cx).host;
    let remembered = load_app_state(&cx.global::<Core>().app_state)
        .ok()
        .and_then(|state| state.shell.new_workspace_host_profile_id);
    let Some(selected) = default_host(&hosts, remembered.as_deref(), &active) else {
        workspaces.update(cx, |workspaces, cx| {
            workspaces.report("No shown host is available for workspace creation.", cx);
        });
        return;
    };
    // A removed or hidden remembered host is repaired as soon as there is a
    // real one to show.
    if remembered.as_deref() != Some(selected.as_str()) {
        super::remember_new_workspace_host(&selected, cx);
    }
    let input = cx.new(|cx| InputState::new(window, cx));
    let focus = input.focus_handle(cx);
    let dialog = cx.new(|cx| NewWorkspace {
        workspaces: workspaces.downgrade(),
        selected,
        _changes: [
            cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.observe(workspaces, |_, _, cx| cx.notify()),
        ],
        input,
    });
    let view = dialog.clone();
    open_overlay(
        dialog,
        WIDTH,
        focus,
        false,
        move |window, cx| view.update(cx, |dialog, cx| dialog.submit(window, cx)),
        window,
        cx,
    );
}

impl Render for NewWorkspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let selected = self.selected(cx);
        let can_save =
            !self.name(cx).is_empty() && selected.as_ref().is_some_and(|host| host.can_mutate);
        let picker_label: SharedString = match &selected {
            Some(host) if host.phase == ConnectionPhase::Connected => {
                format!("Host: {}", host.label).into()
            }
            Some(host) => format!("Host: {}, {}", host.label, host.phase.wire_name()).into(),
            None => "Host: unavailable".into(),
        };
        let hosts = self.hosts(cx);
        let current = self.selected.clone();
        let this = cx.entity().downgrade();
        // Tauri's picker: the link's dot, the letter, the label, and the phase
        // pushed right while it is not connected.
        let shows = match &selected {
            Some(host) => h_flex()
                .flex_1()
                .min_w_0()
                .gap(ui(7.))
                .child(
                    div()
                        .flex_none()
                        .size(ui(7.))
                        .rounded_full()
                        .bg(phase_color(host.phase, cx)),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(fonts::MONO)
                        .text_size(ui(metrics::TYPE_META))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(palette.chrome_dim)
                        .child(host.letter.clone()),
                )
                .child(div().min_w_0().truncate().child(host.label.clone()))
                .when(host.phase != ConnectionPhase::Connected, |this| {
                    this.child(
                        div()
                            .ml_auto()
                            .text_size(ui(metrics::TYPE_SECONDARY))
                            .text_color(palette.chrome_faint)
                            .child(host.phase.wire_name()),
                    )
                }),
            None => h_flex().flex_1().child("No host available"),
        };
        let picker = Button::new("new-workspace-host")
            .outline()
            .w_full()
            .min_h(ui(30.))
            .child(shows)
            .dropdown_caret(true)
            .accessibility_label(picker_label)
            .disabled(hosts.is_empty())
            .dropdown_menu(move |menu, window, cx| {
                let entries: Vec<Entry> = hosts
                    .iter()
                    .map(|host| {
                        let (this, id) = (this.clone(), host.profile_id.clone());
                        Item::new(host_label(host), move |_, cx| {
                            this.update(cx, |dialog, cx| dialog.pick(id.clone(), cx))
                                .ok();
                        })
                        .checked(host.profile_id == current)
                        .disabled(!host.can_mutate)
                        .into()
                    })
                    .collect();
                context_menu::fill(menu, &entries, window, cx)
            });
        v_flex()
            .id("new-workspace")
            .role(Role::Group)
            .aria_label("New workspace")
            .capture_action(escape_ends_composition)
            .p(ui(20.))
            .child(
                div()
                    .mb(ui(10.))
                    .text_size(ui(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("New workspace"),
            )
            .child(
                v_flex().gap(ui(8.)).child("Workspace name").child(
                    Input::new(&self.input)
                        .id("new-workspace-name")
                        .aria_label("Workspace name"),
                ),
            )
            .child(v_flex().mt(ui(12.)).gap(ui(8.)).child("Host").child(picker))
            .child(
                h_flex()
                    .mt(ui(20.))
                    .justify_end()
                    .gap(ui(8.))
                    .child(
                        Button::new("new-workspace-cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("new-workspace-save")
                            .label("Save")
                            .primary()
                            .disabled(!can_save)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str, can_mutate: bool, shown: bool) -> HostEntry {
        HostEntry {
            profile_id: id.into(),
            label: id.into(),
            letter: id[..1].to_uppercase(),
            phase: ConnectionPhase::Connected,
            can_mutate,
            shown,
            active: false,
            transport: "ssh",
        }
    }

    #[test]
    fn the_remembered_host_wins_only_while_it_is_shown() {
        let hosts = [
            host("a", true, true),
            host("b", true, true),
            host("c", true, false),
        ];
        assert_eq!(default_host(&hosts, Some("b"), "a").as_deref(), Some("b"));
        assert_eq!(default_host(&hosts, Some("c"), "a").as_deref(), Some("a"));
        assert_eq!(
            default_host(&hosts, Some("gone"), "b").as_deref(),
            Some("b")
        );
    }

    #[test]
    fn a_writable_host_is_preferred_over_the_active_read_only_one() {
        let hosts = [host("a", false, true), host("b", true, true)];
        assert_eq!(default_host(&hosts, None, "a").as_deref(), Some("b"));
        let frozen = [host("a", false, true), host("b", false, true)];
        assert_eq!(default_host(&frozen, None, "b").as_deref(), Some("b"));
        assert_eq!(default_host(&[], None, "a"), None);
    }
}
