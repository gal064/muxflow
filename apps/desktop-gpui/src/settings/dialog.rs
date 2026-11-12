//! The Settings dialog: six tabs over [`Prefs`](super::Prefs), with the
//! connection form on the first. Copy, order and behavior from
//! `features/shell/SettingsDialog.tsx`.

use std::rc::Rc;

use desktop_core::{
    app_state::{AgentSoundMode, AppTabViewMode, MarkdownAppearance, ThemePreference},
    notifications::{emit_test_notification, notification_permission_status},
};
use gpui_kit::{
    AccessibleAction, App, Context, Entity, FocusHandle, FontWeight, IntoElement, KeyDownEvent,
    Orientation, ParentElement, Render, Role, SharedString, Styled, Subscription, Window,
    accesskit::ActionData,
    base::{SliderIndicator, SliderThumb, SliderTrack, TestSupportExt},
    component::{
        ActiveTheme, Disableable, Sizable, WindowExt,
        button::Button,
        checkbox::Checkbox,
        dialog::DialogFooter,
        input::{Input, InputEvent, InputState},
        radio::{Radio, RadioGroup},
        slider::{SliderEvent, SliderState},
        tab::{Tab as TabItem, TabBar},
        v_flex,
    },
    div,
    prelude::*,
    px, relative,
};

use super::{
    AgentSetupStatus, agent_setup, get, notifier, on_runtime,
    prefs::{AUTOMATIONS_EXTENSION, UI_SCALES, clamped_terminal_font_size},
    update,
};
use crate::ui::{
    dialog::{self, Action},
    surface_error::SurfaceError,
};
use crate::{
    commands,
    connection_ui::{
        ConnectionUi,
        controller::ConnectionController,
        dialog::{ConnectionForm, confirm_delete_host, controls},
    },
    theme::{
        self,
        metrics::{TYPE_LABEL, TYPE_SECONDARY, ui},
    },
};

const DIALOG_WIDTH: f32 = 620.;
/// `.settings-dialog { max-height: min(640px, 78vh) }`.
const DIALOG_MAX_HEIGHT: f32 = 640.;
const DIALOG_MAX_VIEWPORT: f32 = 0.78;
/// What the dialog needs besides the body: the header with its tabs and the footer,
/// so a short window shrinks the body and the footer stays on screen.
const CHROME_HEIGHT: f32 = 136.;
/// The body never shrinks below this, even in a window too short for the rest.
const BODY_MIN_HEIGHT: f32 = 120.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Connection,
    Workspace,
    Terminal,
    Appearance,
    Sounds,
    Accessibility,
}

const TABS: [(Tab, &str); 6] = [
    (Tab::Connection, "Connection"),
    (Tab::Workspace, "Workspace"),
    (Tab::Terminal, "Terminal"),
    (Tab::Appearance, "Appearance"),
    (Tab::Sounds, "Sounds"),
    (Tab::Accessibility, "Accessibility"),
];

const THEMES: [(ThemePreference, &str); 3] = [
    (ThemePreference::System, "System"),
    (ThemePreference::Light, "Light"),
    (ThemePreference::Dark, "Dark"),
];

const MARKDOWN_APPEARANCES: [(MarkdownAppearance, &str); 3] = [
    (MarkdownAppearance::Default, "Muxflow default"),
    (MarkdownAppearance::Light, "Light"),
    (MarkdownAppearance::Dark, "Dark"),
];

/// No Live: this app has no Live Markdown view yet.
const MARKDOWN_VIEWS: [(AppTabViewMode, &str); 3] = [
    (AppTabViewMode::Source, "Source"),
    (AppTabViewMode::Preview, "Preview"),
    (AppTabViewMode::Split, "Split"),
];

/// A saved Live opens as Source, so it reads as Source here; the saved value
/// is left alone until another view is picked.
fn shown_markdown_view(saved: AppTabViewMode) -> AppTabViewMode {
    match saved {
        AppTabViewMode::Live => AppTabViewMode::Source,
        other => other,
    }
}

/// Called once from [`super::init`]: `host.delete` is the Delete button of the
/// open Connection tab, so it is offered only while there is a host to delete.
pub fn init(cx: &mut App) {
    commands::on_command("settings.show", cx, |window, cx| {
        open_with(
            Tab::Connection,
            cx.try_global::<ConnectionUi>().map(|ui| ui.0.clone()),
            window,
            cx,
        );
    });
    commands::on_command("host.delete", cx, |window, cx| {
        let Some(controller) = cx.try_global::<ConnectionUi>().map(|ui| ui.0.clone()) else {
            return;
        };
        if let Some(profile) = controller.read(cx).deletable_profile().cloned() {
            confirm_delete_host(controller, profile, window, cx);
        }
    });
}

/// Opens Settings on `tab`. `controller` is the running connection, absent only
/// in a window with no connection.
pub fn open_with(
    tab: Tab,
    controller: Option<Entity<ConnectionController>>,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| SettingsDialog::new(tab, controller, window, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        dialog::place(dialog::card(dialog, &theme::palette(cx)), window)
            .close_button(false)
            .p_0()
            // Enter belongs to the focused control, not to a default button.
            .on_ok(|_, _, _| false)
            // Escape closes without Done; typed text still has to count.
            .on_cancel({
                let view = view.clone();
                move |_, window, cx| {
                    view.update(cx, |this, cx| this.commit_pending(window, cx));
                    true
                }
            })
            .w(ui(DIALOG_WIDTH).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}

/// Which words the notification permission line takes.
pub fn notification_status_hint(status: Option<&str>) -> &'static str {
    match status {
        None => "Checking whether this system will deliver notifications…",
        Some("authorized") => "Notifications are allowed for this app.",
        Some("provisional") => {
            "Notifications are delivered quietly. Allow them in System Settings → Notifications → Muxflow to get banners."
        }
        Some("denied") => {
            "Notifications are turned off for this app. Enable them in System Settings → Notifications → Muxflow."
        }
        Some("notDetermined") => {
            "Not requested yet — sending a test notification is what asks for permission."
        }
        Some("unreadable") => {
            "macOS did not answer. This usually means the running build is not a packaged app; only the output of release/macos/build-package.sh can deliver notifications."
        }
        // No notification daemon is answering, or a permission state this build
        // has never heard of.
        Some(_) => "This system did not report a notification permission this app understands.",
    }
}

/// What the agent setup row says about itself, in the words of why it is off.
pub fn agent_setup_hint(status: AgentSetupStatus) -> &'static str {
    if status.available {
        "Adds this app’s lifecycle hooks alongside the ones already configured on this host."
    } else if status.reports {
        "This host already reports agent status."
    } else if !status.connected {
        "Connect to a host to check whether it can report agent status."
    } else {
        "Nothing here to set up: no supported agent is installed on this host, or its configuration could not be read."
    }
}

/// The terminal font size field's text, resolved: what the field should show and
/// whether to commit a new size. Nothing is committed for an empty or
/// unparseable field, so the input goes back to the size still in force.
pub fn font_size_from_text(typed: &str, current: u8) -> (u8, bool) {
    let typed = typed.trim();
    match typed.parse::<f64>() {
        Ok(number) if !typed.is_empty() && number.is_finite() => {
            let size = clamped_terminal_font_size(number);
            (size, size != current)
        }
        _ => (current, false),
    }
}

/// `<input type="range" min="0" max="1" step="0.05">`.
const VOLUME_STEP: f32 = 0.05;

/// The volume a range input moves to on `key` from `volume`, as WebKit's does:
/// an arrow steps, Page Up and Page Down step a tenth of the range, and Home
/// and End go to the ends.
pub fn volume_for_key(key: &str, volume: f32) -> Option<f32> {
    let volume = match key {
        "left" | "down" => volume - VOLUME_STEP,
        "right" | "up" => volume + VOLUME_STEP,
        "pagedown" => volume - 0.1,
        "pageup" => volume + 0.1,
        "home" => 0.,
        "end" => 1.,
        _ => return None,
    };
    Some(snap_volume(volume))
}

/// On the step grid and in range, where a range input keeps its value.
fn snap_volume(volume: f32) -> f32 {
    ((volume / VOLUME_STEP).round() * VOLUME_STEP).clamp(0., 1.)
}

/// The slider is f32; two decimals keep the saved file as Tauri writes it.
fn save_volume(volume: f32, window: &mut Window, cx: &mut App) {
    let volume = (f64::from(volume) * 100.).round() / 100.;
    update(|prefs| prefs.agent_sounds.volume = volume, window, cx);
}

pub struct SettingsDialog {
    tab: Tab,
    controller: Option<Entity<ConnectionController>>,
    form: Option<Entity<ConnectionForm>>,
    font_size: Entity<InputState>,
    start_directory: Entity<InputState>,
    startup_command: Entity<InputState>,
    /// The host the two workspace fields describe: the one on screen.
    defaults_host: String,
    volume: Entity<SliderState>,
    volume_focus: FocusHandle,
    notification_status: Option<String>,
    sending_test: bool,
    test_failure: Option<String>,
    test_sent: bool,
    _subscriptions: Vec<Subscription>,
}

impl SettingsDialog {
    fn new(
        tab: Tab,
        controller: Option<Entity<ConnectionController>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prefs = get(cx).clone();
        let defaults_host = controller.as_ref().map_or_else(
            || "local".to_owned(),
            |controller| controller.read(cx).active_profile_id(),
        );
        let defaults = prefs.defaults_for(&defaults_host);
        let font_size = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(prefs.terminal_font_size.to_string(), window, cx);
            input
        });
        let start_directory = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Home directory");
            input.set_value(defaults.directory.clone().unwrap_or_default(), window, cx);
            input
        });
        let startup_command = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("None");
            input.set_value(
                defaults.startup_command.clone().unwrap_or_default(),
                window,
                cx,
            );
            input
        });
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(1.)
                .step(VOLUME_STEP)
                .default_value(prefs.agent_sounds.volume as f32)
        });
        let mut subscriptions = vec![
            // Typed, then committed once on Enter or blur: each commit is a save
            // and a reflow of every open terminal, so a half-typed size or path
            // must never be one.
            cx.subscribe_in(&font_size, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_font_size(window, cx);
                }
            }),
            cx.subscribe_in(&start_directory, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_defaults(window, cx);
                }
            }),
            cx.subscribe_in(&startup_command, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_defaults(window, cx);
                }
            }),
            // The pointer; the keyboard and a screen reader go through `set_volume`.
            cx.subscribe_in(&volume, window, |_, _, event, window, cx| {
                if let SliderEvent::Change(value) = event {
                    save_volume(value.end(), window, cx);
                }
            }),
        ];
        let form = controller.as_ref().map(|controller| {
            let controller = controller.clone();
            cx.new(|cx| ConnectionForm::new(controller, window, cx))
        });
        if let Some(controller) = controller.as_ref() {
            subscriptions.push(cx.observe_in(controller, window, |this, _, window, cx| {
                this.follow_controller(window, cx);
            }));
        }
        subscriptions.push(cx.on_release(|_, cx| {
            commands::update_context(cx, |context| context.has_host_profile = false);
        }));
        let mut this = Self {
            tab,
            controller,
            form,
            font_size,
            start_directory,
            startup_command,
            defaults_host,
            volume,
            volume_focus: cx.focus_handle().tab_stop(true),
            notification_status: None,
            sending_test: false,
            test_failure: None,
            test_sent: false,
            _subscriptions: subscriptions,
        };
        this.follow_controller(window, cx);
        if tab == Tab::Sounds {
            this.check_notifications(cx);
        }
        this
    }

    /// Keeps what depends on the connection in step with it: whether
    /// `host.delete` has a host to delete, and which host the workspace fields
    /// describe, since a path half-typed for one machine must not be left in the
    /// field once it describes another.
    fn follow_controller(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(controller) = self.controller.clone() else {
            return;
        };
        let (deletable, active) = {
            let controller = controller.read(cx);
            (
                controller.deletable_profile().is_some(),
                controller.active_profile_id(),
            )
        };
        commands::update_context(cx, |context| context.has_host_profile = deletable);
        if active != self.defaults_host {
            self.defaults_host = active;
            let defaults = get(cx).defaults_for(&self.defaults_host);
            set_text(
                &self.start_directory,
                defaults.directory.unwrap_or_default(),
                window,
                cx,
            );
            set_text(
                &self.startup_command,
                defaults.startup_command.unwrap_or_default(),
                window,
                cx,
            );
        }
        cx.notify();
    }

    fn commit_font_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = get(cx).terminal_font_size;
        let typed = self.font_size.read(cx).value().to_string();
        let (size, changed) = font_size_from_text(&typed, current);
        if changed {
            update(|prefs| prefs.terminal_font_size = size, window, cx);
        }
        set_text(&self.font_size, size.to_string(), window, cx);
    }

    fn commit_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.start_directory.read(cx).value().to_string();
        let command = self.startup_command.read(cx).value().to_string();
        let host = self.defaults_host.clone();
        update(
            |prefs| prefs.set_workspace_defaults(&host, Some(&directory), Some(&command)),
            window,
            cx,
        );
    }

    /// Typed text counts when the dialog closes or the tab changes, not only on
    /// Enter or blur: a click on a checkbox or a button does not move focus out
    /// of a field, so blur alone would drop what was typed.
    fn commit_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_font_size(window, cx);
        self.commit_defaults(window, cx);
    }

    fn select_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_pending(window, cx);
        self.tab = tab;
        if tab == Tab::Sounds && self.notification_status.is_none() {
            self.check_notifications(cx);
        }
        cx.notify();
    }

    /// Moves the volume from the keyboard or a screen reader, and saves it.
    fn set_volume(&mut self, volume: f32, window: &mut Window, cx: &mut Context<Self>) {
        let volume = snap_volume(volume);
        self.volume
            .update(cx, |state, cx| state.set_value(volume, window, cx));
        save_volume(volume, window, cx);
    }

    /// A screen reader's request, as `to` turns the volume into a new one.
    fn volume_action(
        to: impl Fn(f32, Option<&ActionData>) -> Option<f32> + 'static,
        cx: &mut Context<Self>,
    ) -> impl FnMut(Option<&ActionData>, &mut Window, &mut App) + 'static {
        let this = cx.entity().downgrade();
        move |data, window, cx| {
            this.update(cx, |this, cx| {
                if let Some(volume) = to(this.volume.read(cx).value().end(), data) {
                    this.set_volume(volume, window, cx);
                }
            })
            .ok();
        }
    }

    /// Kit's slider takes no focus, and a screen reader's step on it is not
    /// saved, so the volume is Kit's base slider parts under one node that
    /// takes focus and carries the slider's role, name and value, as Tauri's
    /// range input does. The look is Kit's slider, without its hover ring.
    fn volume_slider(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let state = self.volume.read(cx);
        let (volume, fill) = (state.value().end(), state.percentage().end);
        let (bar, thumb) = (cx.theme().tokens.slider_bar, cx.theme().tokens.slider_thumb);
        let accent = theme::palette(cx).accent;
        let ring = self.volume_focus.is_focused(window) && window.last_input_was_keyboard();
        div()
            .id("settings-volume")
            .test_support()
            .role(Role::Slider)
            .aria_label("Agent sound volume")
            // In hundredths, as the range input reports it, not with the f32 noise.
            .aria_numeric_value((f64::from(volume) * 100.).round() / 100.)
            .aria_min_numeric_value(0.)
            .aria_max_numeric_value(1.)
            .aria_numeric_value_step(f64::from(VOLUME_STEP))
            .aria_orientation(Orientation::Horizontal)
            .track_focus(&self.volume_focus)
            // A press focuses the slider as it does a range input; the thumb
            // keeps its press from the bubble phase.
            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                this.volume_focus.focus(window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let volume = this.volume.read(cx).value().end();
                if let Some(volume) = volume_for_key(&event.keystroke.key, volume) {
                    this.set_volume(volume, window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_a11y_action(
                AccessibleAction::Increment,
                Self::volume_action(|volume, _| Some(volume + VOLUME_STEP), cx),
            )
            .on_a11y_action(
                AccessibleAction::Decrement,
                Self::volume_action(|volume, _| Some(volume - VOLUME_STEP), cx),
            )
            .on_a11y_action(
                AccessibleAction::SetValue,
                Self::volume_action(
                    |_, data| match data {
                        Some(ActionData::NumericValue(value)) => Some(*value as f32),
                        _ => None,
                    },
                    cx,
                ),
            )
            .relative()
            .child(
                // Inset by half the thumb, so the thumb stays inside the box at
                // either end, as a range input's does.
                SliderTrack::new(&self.volume)
                    .flex()
                    .items_center()
                    .w_full()
                    .h_6()
                    .px(px(8.))
                    .child(
                        SliderIndicator::new(&self.volume)
                            .relative()
                            .w_full()
                            .h_1p5()
                            .rounded_full()
                            .bg(bar.opacity(0.2))
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .h_full()
                                    .w(relative(fill))
                                    .rounded_full()
                                    .bg(bar),
                            )
                            .child(
                                SliderThumb::new(&self.volume)
                                    .absolute()
                                    .top(px(-5.))
                                    .left(relative(fill))
                                    .ml(px(-8.))
                                    .size_4()
                                    .p(px(1.))
                                    .rounded_full()
                                    .bg(bar.opacity(0.5))
                                    .child(div().size_full().rounded_full().bg(thumb)),
                            ),
                    ),
            )
            // A ring for the keyboard only, as Tauri's `:focus-visible`: 2px, 1px
            // outside the box.
            .when(ring, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(ui(-3.))
                        .left(ui(-3.))
                        .right(ui(-3.))
                        .bottom(ui(-3.))
                        .border_2()
                        .border_color(accent),
                )
            })
    }

    /// Asks the OS what it says about notifications. Mounted with the tab
    /// rather than with the dialog: the query is a real round trip.
    fn check_notifications(&mut self, cx: &mut Context<Self>) {
        let Some(notifier) = notifier(cx) else {
            self.notification_status = Some("unsupported".to_owned());
            return;
        };
        let answer = on_runtime(
            async move { notification_permission_status(&notifier).await },
            cx.background_executor().clone(),
        );
        cx.spawn(async move |this, cx| {
            let status = match answer.await {
                Ok(Ok(status)) => status,
                _ => "unreadable".to_owned(),
            };
            this.update(cx, |this, cx| {
                this.notification_status = Some(status);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_test_notification(&mut self, cx: &mut Context<Self>) {
        let Some(notifier) = notifier(cx) else {
            return;
        };
        self.sending_test = true;
        self.test_failure = None;
        self.test_sent = false;
        let sent = on_runtime(
            async move { emit_test_notification(&notifier).await },
            cx.background_executor().clone(),
        );
        cx.spawn(async move |this, cx| {
            let outcome = sent.await.and_then(|outcome| outcome.map(|_| ()));
            this.update(cx, |this, cx| {
                this.sending_test = false;
                match outcome {
                    Ok(()) => this.test_sent = true,
                    Err(error) => this.test_failure = Some(error),
                }
                // The permission may have just been granted or refused by the
                // prompt this raised, so the line above it is read again.
                this.check_notifications(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn set_text(input: &Entity<InputState>, text: String, window: &mut Window, cx: &mut App) {
    if input.read(cx).value() != text.as_str() {
        input.update(cx, |input, cx| input.set_value(text, window, cx));
    }
}

fn hint<T: Into<SharedString>>(text: T, cx: &App) -> impl IntoElement + use<T> {
    div()
        .text_size(ui(TYPE_SECONDARY))
        .line_height(ui(TYPE_SECONDARY * 1.5))
        .text_color(theme::palette(cx).chrome_dim)
        .child(text.into())
}

/// A hint under a checkbox, lined up with its label.
fn check_hint<T: Into<SharedString>>(text: T, cx: &App) -> impl IntoElement + use<T> {
    div().pl(ui(24.)).child(hint(text, cx))
}

/// The name of a group of controls, as Tauri's `legend`.
fn legend(text: &'static str, cx: &App) -> impl IntoElement + use<> {
    div()
        .text_size(ui(TYPE_LABEL))
        .text_color(theme::palette(cx).chrome_faint)
        .child(text.to_uppercase())
}

fn caption(text: &'static str, cx: &App) -> impl IntoElement + use<> {
    div()
        .text_size(ui(TYPE_SECONDARY))
        .text_color(theme::palette(cx).chrome_dim)
        .child(text)
}

/// A checkbox with its hint under it. `change` gets the new value.
fn setting_check<F: Fn(bool, &mut Window, &mut App) + 'static>(
    id: &'static str,
    label: &'static str,
    checked: bool,
    disabled: bool,
    hint_text: Option<&'static str>,
    change: F,
    cx: &App,
) -> impl IntoElement + use<F> {
    v_flex()
        .gap(ui(4.))
        .child(
            Checkbox::new(id)
                .label(label)
                .checked(checked)
                .disabled(disabled)
                .on_change(move |checked, window, cx| change(*checked, window, cx)),
        )
        .children(hint_text.map(|text| check_hint(text, cx)))
}

/// One radio per option, horizontal; `change` gets the picked option's index.
fn radios<T: Copy + PartialEq + 'static>(
    id: &'static str,
    name: &'static str,
    options: &'static [(T, &'static str)],
    selected: T,
    change: impl Fn(T, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // The kit's group has no role of its own, so the caption above it would not
    // name it for a screen reader.
    div()
        .id(SharedString::from(format!("{id}-group")))
        .role(Role::RadioGroup)
        .aria_label(name)
        .child(
            RadioGroup::horizontal(id)
                .children(options.iter().map(|(_, label)| {
                    Radio::new(SharedString::from(format!("{id}-{label}"))).label(*label)
                }))
                .selected_index(options.iter().position(|(value, _)| *value == selected))
                .on_change(move |index, window, cx| {
                    if let Some((value, _)) = options.get(*index) {
                        change(*value, window, cx);
                    }
                }),
        )
}

/// "Set up agent status…" and why it is off, in the Connection tab under Delete.
pub fn agent_setup_row(cx: &App) -> impl IntoElement + use<> {
    let status = agent_setup(cx)
        .map(|setup| (setup.status)(cx))
        .unwrap_or_default();
    let set_up = agent_setup(cx).map(|setup| setup.set_up);
    v_flex()
        .gap(ui(4.))
        .items_start()
        .child(dialog::button(
            Button::new("settings-agent-setup")
                .label("Set up agent status…")
                .disabled(!status.available)
                .on_click(move |_, window, cx| {
                    if let Some(set_up) = set_up.clone() {
                        window.close_dialog(cx);
                        set_up(window, cx);
                    }
                }),
            Action::Plain,
            !status.available,
            &theme::palette(cx),
        ))
        .child(hint(agent_setup_hint(status), cx))
}

impl SettingsDialog {
    fn workspace_tab(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let prefs = get(cx).clone();
        let host_label = self.controller.as_ref().map_or_else(
            || "Local".to_owned(),
            |controller| controller.read(cx).active_host_label(),
        );
        let mut tab = v_flex().gap(ui(12.)).child(setting_check(
            "settings-compact-workspaces",
            "Compact workspace list",
            prefs.compact_workspaces,
            false,
            Some("Hides individual agent lines under each workspace while keeping its activity, unread count, and connection status visible."),
            |on, window, cx| update(|prefs| prefs.compact_workspaces = on, window, cx),
            cx,
        ));
        // One entry per extension this build has.
        tab = tab.child(setting_check(
            "settings-extension-automations",
            "Show Automations",
            prefs.panel_extension_enabled(AUTOMATIONS_EXTENSION),
            false,
            Some("Adds an Automations tab to the right panel: agent runs on a cron schedule, owned by each host's helper."),
            |on, window, cx| {
                update(|prefs| prefs.set_panel_extension(AUTOMATIONS_EXTENSION, on), window, cx);
            },
            cx,
        ));
        tab.child(
            v_flex()
                .gap(ui(4.))
                .child(caption("Default Markdown view", cx))
                .child(radios(
                    "settings-markdown-view",
                    "Default Markdown view",
                    &MARKDOWN_VIEWS,
                    shown_markdown_view(prefs.default_markdown_view),
                    |view, window, cx| update(|prefs| prefs.default_markdown_view = view, window, cx),
                ))
                .child(hint(
                    "Applies to Markdown files opened from now on. Tabs already open keep the view they are in.",
                    cx,
                )),
        )
        .child(
            v_flex()
                .gap(ui(12.))
                .child(legend_for_host(&host_label, cx))
                .child(
                    v_flex()
                        .gap(ui(4.))
                        .child(caption("Start in directory", cx))
                        .child(
                            Input::new(&self.start_directory)
                                .id("settings-start-directory")
                                .aria_label("Workspace start directory"),
                        )
                        .child(hint(
                            format!("Checked on {host_label} when the workspace is created; a path that is not there refuses the create instead of putting the workspace somewhere else."),
                            cx,
                        )),
                )
                .child(
                    v_flex()
                        .gap(ui(4.))
                        .child(caption("Run command in first tab", cx))
                        .child(
                            Input::new(&self.startup_command)
                                .id("settings-startup-command")
                                .aria_label("Workspace startup command"),
                        )
                        .child(hint("Sent to the first terminal once, right after the workspace is created.", cx)),
                ),
        )
    }

    fn terminal_tab(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let prefs = get(cx).clone();
        v_flex()
            .gap(ui(12.))
            .child(
                v_flex()
                    .gap(ui(4.))
                    .child(caption("Font size", cx))
                    .child(
                        div().w(ui(120.)).child(
                            Input::new(&self.font_size)
                                .id("settings-terminal-font-size")
                                .aria_label("Terminal font size"),
                        ),
                    )
                    .child(hint("Applies to all open terminals.", cx)),
            )
            .child(setting_check(
                "settings-copy-on-select",
                "Copy on select",
                prefs.copy_on_select,
                false,
                Some("Copies completed, non-empty terminal selections to the system clipboard."),
                |on, window, cx| update(|prefs| prefs.copy_on_select = on, window, cx),
                cx,
            ))
            .child(setting_check(
                "settings-clean-wrapped",
                "Clean wrapped commands when copying",
                prefs.clean_wrapped_commands,
                false,
                Some("Joins only high-confidence display wraps in code-styled shell commands."),
                |on, window, cx| update(|prefs| prefs.clean_wrapped_commands = on, window, cx),
                cx,
            ))
            .child(setting_check(
                "settings-application-clipboard",
                "Allow terminal apps to copy",
                prefs.terminal_application_clipboard,
                false,
                Some("Allows terminal programs to replace the system clipboard using OSC 52."),
                |on, window, cx| {
                    update(
                        |prefs| prefs.terminal_application_clipboard = on,
                        window,
                        cx,
                    );
                },
                cx,
            ))
            .child(setting_check(
                "settings-focus-follows-mouse",
                "Focus pane on hover",
                prefs.focus_follows_mouse,
                false,
                Some("Moves keyboard focus to a split pane when the pointer rests on it."),
                |on, window, cx| update(|prefs| prefs.focus_follows_mouse = on, window, cx),
                cx,
            ))
    }

    fn appearance_tab(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let prefs = get(cx).clone();
        let scales: &'static [(f64, &'static str)] = &[
            (UI_SCALES[0], "90%"),
            (UI_SCALES[1], "100%"),
            (UI_SCALES[2], "110%"),
            (UI_SCALES[3], "120%"),
            (UI_SCALES[4], "130%"),
        ];
        v_flex()
            .gap(ui(12.))
            .child(
                v_flex()
                    .gap(ui(4.))
                    .child(legend("Theme", cx))
                    .child(radios("settings-theme", "Theme", &THEMES, prefs.theme, |theme, window, cx| {
                        update(|prefs| prefs.theme = theme, window, cx);
                    }))
                    .child(hint("System follows your OS appearance and switches when it does.", cx)),
            )
            .child(
                v_flex()
                    .gap(ui(4.))
                    .child(caption("Interface size", cx))
                    .child(radios("settings-ui-scale", "Interface size", scales, prefs.ui_scale, |scale, window, cx| {
                        update(|prefs| prefs.ui_scale = scale, window, cx);
                    }))
                    .child(hint(
                        "Scales everything, terminals included. The terminal font size setting applies on top.",
                        cx,
                    )),
            )
            .child(setting_check(
                "settings-tabs-in-title-bar",
                "Show tabs in the title bar",
                prefs.tabs_in_title_bar,
                false,
                Some("Gives the terminal the row the tab strip used. New workspace moves to the Workspaces header."),
                |on, window, cx| update(|prefs| prefs.tabs_in_title_bar = on, window, cx),
                cx,
            ))
            .child(
                v_flex()
                    .gap(ui(4.))
                    .child(legend("Markdown appearance", cx))
                    .child(radios(
                        "settings-markdown-appearance",
                        "Markdown appearance",
                        &MARKDOWN_APPEARANCES,
                        prefs.markdown_appearance,
                        |appearance, window, cx| {
                            update(|prefs| prefs.markdown_appearance = appearance, window, cx);
                        },
                    ))
                    .child(hint(
                        "Colors for Markdown in Preview. Muxflow default follows the theme; Source always does.",
                        cx,
                    )),
            )
    }

    fn sounds_tab(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let sounds = get(cx).agent_sounds;
        let cue = |mode: AgentSoundMode| mode != AgentSoundMode::None;
        let mode = |on: bool| {
            if on {
                AgentSoundMode::Subtle
            } else {
                AgentSoundMode::None
            }
        };
        let can_test = notifier(cx).is_some() && !self.sending_test;
        v_flex()
            .gap(ui(12.))
            .child(setting_check(
                "settings-sounds-enabled",
                "Play agent cues",
                sounds.enabled,
                false,
                None,
                |on, window, cx| update(|prefs| prefs.agent_sounds.enabled = on, window, cx),
                cx,
            ))
            .child(setting_check(
                "settings-sounds-blocked",
                "Cue when an agent is blocked",
                cue(sounds.blocked),
                !sounds.enabled,
                None,
                move |on, window, cx| {
                    update(|prefs| prefs.agent_sounds.blocked = mode(on), window, cx);
                },
                cx,
            ))
            .child(setting_check(
                "settings-sounds-completed",
                "Cue when an agent finishes",
                cue(sounds.completed),
                !sounds.enabled,
                None,
                move |on, window, cx| {
                    update(|prefs| prefs.agent_sounds.completed = mode(on), window, cx);
                },
                cx,
            ))
            .child(
                v_flex()
                    .gap(ui(4.))
                    .child(caption("Volume", cx))
                    .child(self.volume_slider(window, cx)),
            )
            // Named, because "Sounds" does not imply system notification
            // permission and an unlabelled button under a volume slider reads as
            // part of the mixer.
            .child(
                v_flex()
                    .gap(ui(4.))
                    .items_start()
                    .child(legend("System notifications", cx))
                    .child(dialog::button(
                        Button::new("settings-test-notification")
                            .label(if self.sending_test { "Sending…" } else { "Send test notification" })
                            .disabled(!can_test)
                            .on_click(cx.listener(|this, _, _, cx| this.send_test_notification(cx))),
                        Action::Plain,
                        !can_test,
                        &theme::palette(cx),
                    ))
                    .child(hint(notification_status_hint(self.notification_status.as_deref()), cx))
                    .children(self.test_failure.as_ref().map(|failure| {
                        SurfaceError::new("settings-test-failure", failure.clone())
                            .summary("The test notification could not be sent.")
                    }))
                    .children(self.test_sent.then(|| {
                        div()
                            .id("settings-test-sent")
                            .role(Role::Status)
                            .child(hint(
                                "Sent. If nothing appeared, check System Settings → Notifications → Muxflow.",
                                cx,
                            ))
                    })),
            )
    }

    fn accessibility_tab(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let prefs = get(cx).clone();
        v_flex().gap(ui(12.)).child(setting_check(
            "settings-agent-state-glyphs",
            "Shapes in agent state dots",
            prefs.agent_state_glyphs,
            false,
            Some("Draws a symbol inside each dot as well as coloring it, so agent state does not depend on telling red from yellow."),
            |on, window, cx| update(|prefs| prefs.agent_state_glyphs = on, window, cx),
            cx,
        ))
    }

    /// `.settings-dialog > footer`: the one button, Connect on the first tab and Done on
    /// the rest. Escape closes the dialog.
    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let connect = self.controller.clone();
        let palette = theme::palette(cx);
        let button = if self.tab == Tab::Connection {
            Button::new("connection-connect")
                .label("Connect")
                .on_click(move |_, window, cx| {
                    if let Some(controller) = connect.as_ref() {
                        controller.update(cx, |state, cx| state.connect(cx));
                    }
                    window.close_dialog(cx);
                })
        } else {
            Button::new("settings-done")
                .label("Done")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.commit_pending(window, cx);
                    window.close_dialog(cx);
                }))
        };
        div()
            .border_t_1()
            .border_color(palette.chrome_hairline)
            .px(ui(16.))
            .py(ui(12.))
            .child(controls(DialogFooter::new().child(dialog::button(
                button,
                Action::Primary,
                false,
                &palette,
            ))))
    }
}

fn legend_for_host(label: &str, cx: &App) -> impl IntoElement + use<> {
    div()
        .text_size(ui(TYPE_LABEL))
        .text_color(theme::palette(cx).chrome_faint)
        .child(format!("NEW WORKSPACES ON {}", label.to_uppercase()))
}

impl Render for SettingsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        // `.segment` is 11.5px; Kit's smallest tab is 12px.
        // One Tab stop, the section shown, as Tauri's roving tablist; the
        // arrows select the next section and take the keyboard to it, wrapping.
        let focus: Rc<[FocusHandle]> = (0..TABS.len())
            .map(|ix| {
                window
                    .use_keyed_state(("settings-tab-focus", ix), cx, |_, cx| cx.focus_handle())
                    .read(cx)
                    .clone()
                    .tab_stop(TABS[ix].0 == self.tab)
            })
            .collect();
        let accent = palette.accent;
        let tabs = TabBar::new("settings-tabs")
            .segmented()
            .xsmall()
            .selected_index(
                TABS.iter()
                    .position(|(tab, _)| *tab == self.tab)
                    .unwrap_or(0),
            )
            .children(TABS.iter().enumerate().map(|(ix, (_, label))| {
                // A ring for the keyboard only, as Tauri's `:focus-visible`.
                let held = focus[ix].is_focused(window) && window.last_input_was_keyboard();
                let focus = focus.clone();
                TabItem::new()
                    .label(*label)
                    .track_focus(&focus[ix])
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        // From the section shown, as Tauri moves.
                        let at = TABS
                            .iter()
                            .position(|(tab, _)| *tab == this.tab)
                            .unwrap_or(0);
                        let next = match event.keystroke.key.as_str() {
                            "left" => (at + TABS.len() - 1) % TABS.len(),
                            "right" => (at + 1) % TABS.len(),
                            _ => return,
                        };
                        this.select_tab(TABS[next].0, window, cx);
                        focus[next].focus(window, cx);
                        cx.stop_propagation();
                    }))
                    .when(held, |tab| {
                        tab.relative().child(
                            div()
                                .absolute()
                                .inset_0()
                                .border_2()
                                .border_color(accent)
                                .rounded(ui(4.)),
                        )
                    })
            }))
            .on_click(cx.listener(|this, index: &usize, window, cx| {
                if let Some((tab, _)) = TABS.get(*index) {
                    this.select_tab(*tab, window, cx);
                }
            }));
        let rem = window.rem_size();
        let max_dialog = ui(DIALOG_MAX_HEIGHT)
            .to_pixels(rem)
            .min(window.viewport_size().height * DIALOG_MAX_VIEWPORT);
        let max_body =
            (max_dialog - ui(CHROME_HEIGHT).to_pixels(rem)).max(ui(BODY_MIN_HEIGHT).to_pixels(rem));
        let body = match self.tab {
            Tab::Connection => div().children(self.form.clone()).into_any_element(),
            Tab::Workspace => self.workspace_tab(cx).into_any_element(),
            Tab::Terminal => self.terminal_tab(cx).into_any_element(),
            Tab::Appearance => self.appearance_tab(cx).into_any_element(),
            Tab::Sounds => self.sounds_tab(window, cx).into_any_element(),
            Tab::Accessibility => self.accessibility_tab(cx).into_any_element(),
        };
        v_flex()
            .id("settings-dialog")
            .role(Role::Dialog)
            .aria_label("Settings")
            // `.settings-dialog > header`.
            .child(
                v_flex()
                    .gap(ui(10.))
                    .px(ui(16.))
                    .py(ui(14.))
                    .border_b_1()
                    .border_color(palette.chrome_hairline)
                    .child(
                        div()
                            .text_size(ui(16.))
                            .font_weight(FontWeight::BOLD)
                            .child("Settings"),
                    )
                    // Tauri's `aria-label="Settings sections"`. Kit's tab bar
                    // takes no name, so a named group holds it.
                    .child(
                        div()
                            .id("settings-sections")
                            .test_support()
                            .role(Role::Group)
                            .aria_label("Settings sections")
                            .child(tabs),
                    ),
            )
            .child(
                div()
                    .id("settings-body")
                    .max_h(max_body)
                    .overflow_y_scroll()
                    .p(ui(16.))
                    .child(body),
            )
            .child(self.footer(cx))
    }
}
