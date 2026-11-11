//! The connection dialog and the confirmations it raises. Copy and field
//! behavior from `features/shell/SettingsDialog.tsx` and `app/AppDialogLayer.tsx`.

use std::rc::Rc;

use desktop_core::connection::{ConnectionSpec, helper::HelperRollback, profiles::HostProfile};
use gpui_kit::{
    App, Context, Entity, FocusHandle, FontWeight, IntoElement, KeyBinding, NoAction, Render, Role,
    SharedString, Subscription, Window,
    base::TestSupportExt,
    component::{
        Disableable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        dialog::DialogFooter,
        h_flex,
        input::{Input, InputEvent, InputState},
        radio::{Radio, RadioGroup},
        spinner::Spinner,
        v_flex,
    },
    div,
    prelude::*,
};

use crate::theme::{
    self,
    metrics::{TYPE_LABEL, TYPE_META, TYPE_ROW, TYPE_SECONDARY, ui},
};
use crate::ui::surface_error::SurfaceError;

use super::{
    controller::{ConnectionController, ConnectionMode},
    helper_upgrade::{HelperOperation, HelperUpgradeState, RemoteHelperProbe},
    host_profiles::host_letter,
};

const CONFIRM_WIDTH: f32 = 420.;

type DialogAction = Box<dyn Fn(&mut Window, &mut App)>;

/// Key context around a dialog's controls. The Kit binds Enter to the dialog
/// itself, ahead of the focused control; shadowing that here lets Enter reach
/// the control, as it does in the Tauri dialogs.
const CONTROLS: &str = "ConnectionDialogControls";

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("enter", NoAction, Some(CONTROLS))]);
}

pub(crate) fn controls(footer: DialogFooter) -> impl IntoElement {
    div().w_full().key_context(CONTROLS).child(footer)
}

pub struct Confirmation {
    pub title: &'static str,
    pub detail: SharedString,
    pub confirm_label: &'static str,
    pub destructive: bool,
    pub on_confirm: DialogAction,
}

fn detail_text(detail: SharedString, cx: &App) -> impl IntoElement + use<> {
    div()
        .text_size(ui(TYPE_ROW))
        .text_color(theme::palette(cx).chrome_dim)
        .child(detail)
}

/// A confirm/cancel dialog, closed by either of its buttons alone.
pub fn open_confirmation(confirmation: Confirmation, window: &mut Window, cx: &mut App) {
    let confirmation = Rc::new(confirmation);
    window.open_dialog(cx, move |dialog, window, cx| {
        let confirm = confirmation.clone();
        let mut confirm_button = Button::new("confirmation-confirm")
            .label(confirmation.confirm_label)
            .on_click(move |_, window, cx| {
                window.close_dialog(cx);
                (confirm.on_confirm)(window, cx);
            });
        confirm_button = if confirmation.destructive {
            confirm_button.danger()
        } else {
            confirm_button.primary()
        };
        dialog
            .title(confirmation.title)
            .w(ui(CONFIRM_WIDTH).to_pixels(window.rem_size()))
            .close_button(false)
            .overlay_closable(false)
            .child(detail_text(confirmation.detail.clone(), cx))
            .footer(controls(
                DialogFooter::new()
                    .child(
                        Button::new("confirmation-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(confirm_button),
            ))
    });
}

fn helper_title(busy: bool, installed: bool) -> &'static str {
    match (busy, installed) {
        (true, true) => "Upgrading remote helper…",
        (true, false) => "Installing remote helper…",
        (false, true) => "Upgrade remote helper?",
        (false, false) => "Install remote helper?",
    }
}

fn helper_detail(busy: bool, probe: &RemoteHelperProbe) -> String {
    if busy {
        "Uploading and verifying the helper. This may take a little while.".to_owned()
    } else if probe.installed {
        format!(
            "Replace the helper at {}. The current helper is backed up and restored automatically if the new helper cannot complete its handshake.",
            probe.remote_path
        )
    } else {
        format!(
            "Install the packaged helper at {}, verify its digest and handshake, then connect to tmux. Agent status hooks for the agents found on this host will also be set up after connecting.",
            probe.remote_path
        )
    }
}

/// The consent dialog the helper reducer drives. It reads the controller on
/// every build, so a confirmation turning into progress needs no reopening.
pub fn open_helper_dialog(
    controller: Entity<ConnectionController>,
    closed_by_kit: Rc<dyn Fn(&mut App)>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let (busy, probe) = match controller.read(cx).helper() {
            HelperUpgradeState::Confirming { probe, .. } => (false, probe.clone()),
            HelperUpgradeState::Upgrading { probe, .. } => (true, probe.clone()),
            _ => return dialog,
        };
        let confirm_label = match (busy, probe.installed) {
            (true, _) => "Working…",
            (false, true) => "Upgrade and reconnect",
            (false, false) => "Install and connect",
        };
        let cancel = controller.clone();
        let confirm = controller.clone();
        let escaped = controller.clone();
        let closed_by_kit = closed_by_kit.clone();
        dialog
            .title(helper_title(busy, probe.installed))
            .w(ui(CONFIRM_WIDTH).to_pixels(window.rem_size()))
            .close_button(false)
            .keyboard(!busy)
            .overlay_closable(false)
            .on_close(move |_, _, cx| {
                closed_by_kit(cx);
                escaped.update(cx, |state, cx| state.cancel_helper_install(cx));
            })
            .child(
                div()
                    .id("helper-dialog-detail")
                    .test_support()
                    .when(busy, |detail| {
                        detail
                            .role(Role::Status)
                            .aria_label(helper_detail(busy, &probe))
                    })
                    // Tauri's SetupProgress: a spinner says the wait is work.
                    .when(busy, |detail| {
                        detail
                            .flex()
                            .gap(ui(8.))
                            .items_center()
                            .child(Spinner::new().small())
                    })
                    .child(detail_text(helper_detail(busy, &probe).into(), cx)),
            )
            .footer(controls(
                DialogFooter::new()
                    .child(
                        Button::new("helper-cancel")
                            .ghost()
                            .label("Cancel")
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                cancel.update(cx, |state, cx| state.cancel_helper_install(cx));
                            }),
                    )
                    .child(
                        Button::new("helper-confirm")
                            .primary()
                            .label(confirm_label)
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                confirm.update(cx, |state, cx| state.confirm_helper_install(cx));
                            }),
                    ),
            ))
    });
}

/// Driven by the recovery reducer, like the helper dialog: a scope change or a
/// new offer takes it down, so Confirm only ever answers the offer it shows.
pub fn open_app_discard_dialog(
    controller: Entity<ConnectionController>,
    closed_by_kit: Rc<dyn Fn(&mut App)>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let Some(offer) = controller.read(cx).app_recovery() else {
            return dialog;
        };
        let count = offer.count;
        let plural = if count == 1 { "" } else { "s" };
        let cancel = controller.clone();
        let confirm = controller.clone();
        let escaped = controller.clone();
        let closed_by_kit = closed_by_kit.clone();
        dialog
            .title("Discard app tabs from replaced server?")
            .w(ui(CONFIRM_WIDTH).to_pixels(window.rem_size()))
            .close_button(false)
            .overlay_closable(false)
            .on_close(move |_, _, cx| {
                closed_by_kit(cx);
                escaped.update(cx, |state, cx| state.cancel_app_tab_discard(cx));
            })
            .child(detail_text(
                format!(
                    "{count} app-owned tab record{plural} from the replaced server will be removed. tmux sessions and terminal processes are not changed."
                )
                .into(),
                cx,
            ))
            .footer(controls(
                DialogFooter::new()
                    .child(Button::new("app-discard-cancel").ghost().label("Cancel").on_click(
                        move |_, _, cx| {
                            cancel.update(cx, |state, cx| state.cancel_app_tab_discard(cx));
                        },
                    ))
                    .child(Button::new("app-discard-confirm").danger().label("Discard old tabs").on_click(
                        move |_, _, cx| {
                            confirm.update(cx, |state, cx| state.confirm_app_tab_discard(cx));
                        },
                    )),
            ))
    });
}

/// The form behind "Connection…": saved hosts, the connect fields and the
/// remote helper.
pub struct ConnectionForm {
    controller: Entity<ConnectionController>,
    ssh_target: Entity<InputState>,
    ssh_config: Entity<InputState>,
    letter: Entity<InputState>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl ConnectionForm {
    pub fn new(
        controller: Entity<ConnectionController>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let ssh_target =
            cx.new(|cx| InputState::new(window, cx).placeholder("Host or config alias"));
        let ssh_config = cx.new(|cx| InputState::new(window, cx).placeholder("Optional path"));
        let letter = cx.new(|cx| InputState::new(window, cx));
        let mut subscriptions = vec![cx.observe_in(&controller, window, |this, _, window, cx| {
            this.sync_inputs(window, cx);
            cx.notify();
        })];
        controller.update(cx, |state, cx| state.hold_form(true, cx));
        subscriptions.push(cx.on_release({
            let controller = controller.downgrade();
            move |_, cx| {
                controller
                    .update(cx, |state, cx| state.hold_form(false, cx))
                    .ok();
            }
        }));
        subscriptions.push(
            cx.subscribe_in(&ssh_target, window, |this, input, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    this.controller
                        .update(cx, |state, cx| state.set_ssh_target(value, cx));
                }
            }),
        );
        subscriptions.push(
            cx.subscribe_in(&ssh_config, window, |this, input, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    this.controller
                        .update(cx, |state, cx| state.set_ssh_config_path(value, cx));
                }
            }),
        );
        subscriptions.push(
            cx.subscribe_in(&letter, window, |this, input, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let typed = input.read(cx).value().trim().chars().next();
                    this.controller.update(cx, |state, cx| {
                        let selected = state.selected_profile_id().to_owned();
                        let shown = state
                            .profiles()
                            .iter()
                            .find(|profile| profile.id == selected)
                            .is_some_and(|profile| profile.shown);
                        state.save_profile_fields(
                            &selected,
                            typed.map(|letter| letter.to_string()),
                            shown,
                            cx,
                        );
                    });
                }
            }),
        );
        let mut this = Self {
            focus: cx.focus_handle(),
            controller,
            ssh_target,
            ssh_config,
            letter,
            _subscriptions: subscriptions,
        };
        this.sync_inputs(window, cx);
        this
    }

    /// A picked host fills the form; typing updates the controller, so an
    /// unchanged value is left alone and the caret stays where it is.
    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (target, config, letter, placeholder) = {
            let state = self.controller.read(cx);
            let selected = selected_profile(state);
            (
                state.ssh_target().to_owned(),
                state.ssh_config_path().to_owned(),
                selected
                    .and_then(|profile| profile.letter.clone())
                    .unwrap_or_default(),
                selected.map(|profile| {
                    let mut derived = profile.clone();
                    derived.letter = None;
                    host_letter(&derived)
                }),
            )
        };
        set_if_different(&self.ssh_target, target, window, cx);
        set_if_different(&self.ssh_config, config, window, cx);
        set_if_different(&self.letter, letter, window, cx);
        if let Some(placeholder) = placeholder {
            self.letter.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
    }
}

fn set_if_different(input: &Entity<InputState>, value: String, window: &mut Window, cx: &mut App) {
    if input.read(cx).value() != value.as_str() {
        input.update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

fn selected_profile(state: &ConnectionController) -> Option<&HostProfile> {
    state
        .profiles()
        .iter()
        .find(|profile| profile.id == state.selected_profile_id())
}

fn field(label: &'static str) -> impl IntoElement + use<> {
    div()
        .text_size(ui(TYPE_SECONDARY))
        .font_weight(FontWeight(600.))
        .child(label)
}

fn hint<T: Into<SharedString>>(text: T, cx: &App) -> impl IntoElement + use<T> {
    div()
        .text_size(ui(TYPE_META))
        .text_color(theme::palette(cx).chrome_dim)
        .child(text.into())
}

fn helper_can_install(state: &HelperUpgradeState) -> Option<&RemoteHelperProbe> {
    match state {
        HelperUpgradeState::Ready { probe, .. } if !probe.compatible => Some(probe),
        _ => None,
    }
}

fn helper_failure_summary(
    rollback: HelperRollback,
    probe: Option<&RemoteHelperProbe>,
) -> &'static str {
    match (rollback, probe) {
        (HelperRollback::Restored, _) => "Upgrade failed; the previous helper was restored.",
        (HelperRollback::Failed, _) => {
            "Upgrade and rollback both failed — check the helper on the host before reconnecting."
        }
        (_, Some(probe)) if probe.installed => "The helper upgrade failed.",
        (_, Some(_)) => "The helper installation failed.",
        (_, None) => "The helper check failed.",
    }
}

/// The confirmation Delete raises, here so the `host.delete` command opens the
/// same one.
pub fn confirm_delete_host(
    controller: Entity<ConnectionController>,
    profile: HostProfile,
    window: &mut Window,
    cx: &mut App,
) {
    let label = profile.label.clone();
    open_confirmation(
        Confirmation {
            title: "Delete saved host?",
            detail: format!(
                "“{label}” will be removed from the saved hosts on this machine. Nothing on the host is changed: its tmux sessions, its helper and your SSH config are untouched, and connecting to it again saves it again."
            )
            .into(),
            confirm_label: "Delete host",
            destructive: true,
            on_confirm: Box::new(move |_, cx| {
                controller.update(cx, |state, cx| {
                    state.delete_profile(&profile, cx);
                });
            }),
        },
        window,
        cx,
    );
}

impl ConnectionForm {
    fn saved_hosts(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let controller = self.controller.clone();
        let state = controller.read(cx);
        let profiles: Vec<HostProfile> = state.profiles().to_vec();
        let selected = profiles
            .iter()
            .position(|profile| profile.id == state.selected_profile_id());
        let picked = controller.clone();
        let ids: Vec<String> = profiles.iter().map(|profile| profile.id.clone()).collect();
        let add = controller.clone();
        let deletable = state.deletable_profile().cloned();
        let delete_hint = if deletable.is_some() {
            "Removes it from this machine only. The host itself is not touched."
        } else if profiles.len() <= 1 {
            "The last saved host cannot be removed."
        } else {
            "Pick a saved host above to remove it."
        };
        let delete_label = deletable.as_ref().map_or_else(
            || "Delete saved host…".to_owned(),
            |profile| format!("Delete “{}”…", profile.label),
        );
        let delete_target = controller.clone();
        v_flex()
            .gap(ui(4.))
            .child(field("Host"))
            .child(
                RadioGroup::vertical("connection-saved-hosts")
                    .children(profiles.iter().map(|profile| {
                        Radio::new(SharedString::from(format!(
                            "connection-host-{}",
                            profile.id
                        )))
                        .label(profile.label.clone())
                    }))
                    .selected_index(selected)
                    .on_change(move |index, _, cx| {
                        if let Some(id) = ids.get(*index) {
                            picked.update(cx, |state, cx| state.select_profile(id, cx));
                        }
                    }),
            )
            .child(
                h_flex()
                    .gap(ui(6.))
                    .child(
                        Button::new("connection-add-host")
                            .ghost()
                            .xsmall()
                            .label("+ Add host")
                            .on_click(move |_, _, cx| {
                                add.update(cx, |state, cx| state.add_host(cx))
                            }),
                    )
                    .child(
                        Button::new("connection-delete-host")
                            .ghost()
                            .xsmall()
                            .label(delete_label)
                            .disabled(deletable.is_none())
                            .on_click(move |_, window, cx| {
                                if let Some(profile) = deletable.clone() {
                                    confirm_delete_host(delete_target.clone(), profile, window, cx);
                                }
                            }),
                    ),
            )
            .child(hint(delete_hint, cx))
    }

    fn connection_fields(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let controller = self.controller.clone();
        let state = controller.read(cx);
        let mode = state.connection_mode();
        let on_mode = controller.clone();
        let mut fields = v_flex().gap(ui(6.)).child(
            div()
                .text_size(ui(TYPE_LABEL))
                .text_color(theme::palette(cx).chrome_faint)
                .child("TRANSPORT"),
        );
        fields = fields.child(
            RadioGroup::horizontal("connection-mode")
                .child(Radio::new("connection-mode-local").label("Local"))
                .child(Radio::new("connection-mode-ssh").label("SSH"))
                .selected_index(Some(usize::from(mode == ConnectionMode::Ssh)))
                .on_change(move |index, _, cx| {
                    let next = if *index == 0 {
                        ConnectionMode::Local
                    } else {
                        ConnectionMode::Ssh
                    };
                    on_mode.update(cx, |state, cx| state.set_connection_mode(next, cx));
                }),
        );
        if mode == ConnectionMode::Ssh {
            fields = fields
                .child(field("SSH host"))
                .child(
                    Input::new(&self.ssh_target)
                        .id("connection-ssh-target")
                        .aria_label("SSH host"),
                )
                .child(field("SSH config"))
                .child(
                    Input::new(&self.ssh_config)
                        .id("connection-ssh-config")
                        .aria_label("SSH config"),
                );
        }
        fields
    }

    fn saved_host_fields(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let controller = self.controller.clone();
        let state = controller.read(cx);
        let selected = selected_profile(state)?.clone();
        let is_active = selected.id == state.active_profile_id();
        let toggled = controller.clone();
        Some(
            v_flex()
                .gap(ui(4.))
                .child(field("Letter"))
                .child(Input::new(&self.letter).id("connection-host-letter").aria_label("Host letter"))
                .child(hint(
                    "Drawn before this host’s workspaces and agents when more than one host is shown. Empty uses the first letter of the label.",
                    cx,
                ))
                .child(
                    Checkbox::new("connection-host-shown")
                        .label("Show in sidebar")
                        .checked(is_active || selected.shown)
                        .disabled(is_active)
                        .on_change(move |checked, _, cx| {
                            let checked = *checked;
                            toggled.update(cx, |state, cx| {
                                let letter = selected.letter.clone();
                                state.save_profile_fields(&selected.id, letter, checked, cx);
                            });
                        }),
                )
                .child(hint(
                    if is_active {
                        "The host on screen is always shown."
                    } else {
                        "Keeps a live connection and lists this host’s workspaces and agents beside the active host’s."
                    },
                    cx,
                )),
        )
    }

    fn helper_section(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let controller = self.controller.clone();
        let state = controller.read(cx);
        if matches!(state.connection(), ConnectionSpec::Local) {
            return None;
        }
        let helper = state.helper().clone();
        let probing = matches!(helper, HelperUpgradeState::Probing { .. });
        let upgrading = matches!(helper, HelperUpgradeState::Upgrading { .. });
        let probe_action = controller.clone();
        let mut section = v_flex().gap(ui(6.)).child(
            Button::new("connection-check-helper")
                .ghost()
                .xsmall()
                .label(if probing {
                    "Checking helper…"
                } else {
                    "Check helper"
                })
                .disabled(probing || upgrading)
                .on_click(move |_, _, cx| {
                    probe_action.update(cx, |state, cx| state.probe_helper(cx));
                }),
        );
        if let Some(probe) = helper_can_install(&helper) {
            let request = controller.clone();
            let focus = self.focus.clone();
            section = section.child(
                Button::new("connection-install-helper")
                    .primary()
                    .xsmall()
                    .label(if probe.installed {
                        "Upgrade helper…"
                    } else {
                        "Install helper…"
                    })
                    .on_click(move |_, window, cx| {
                        // This button goes away with the install, and the dialog
                        // returns focus to whatever opened it.
                        window.focus(&focus, cx);
                        request.update(cx, |state, cx| state.request_helper_install(cx));
                    }),
            );
        }
        match &helper {
            HelperUpgradeState::Ready { probe, .. } => {
                let version = if probe.installed {
                    probe
                        .helper_version
                        .clone()
                        .unwrap_or_else(|| "unreadable".to_owned())
                } else {
                    "not installed".to_owned()
                };
                section = section.child(hint(
                    format!(
                        "Remote {} {}  ·  {}  ·  Helper {version}  ·  Path {}",
                        probe.operating_system,
                        probe.architecture,
                        probe.tmux_version,
                        probe.remote_path
                    ),
                    cx,
                ));
            }
            HelperUpgradeState::Upgrading { .. } => {
                section = section.child(
                    div()
                        .id("connection-helper-status")
                        .role(Role::Status)
                        .child(hint(
                            "Upgrading the remote helper; the previous one is retained until the new handshake succeeds.",
                            cx,
                        )),
                );
            }
            HelperUpgradeState::Failed {
                message,
                rollback,
                probe,
                ..
            } => {
                section = section.child(
                    SurfaceError::new("connection-helper-failure", message.clone())
                        .summary(helper_failure_summary(*rollback, probe.as_ref())),
                );
            }
            HelperUpgradeState::Succeeded {
                message, operation, ..
            } => {
                let word = if *operation == HelperOperation::Install {
                    "installed"
                } else {
                    "upgraded"
                };
                section = section.child(
                    div()
                        .id("connection-helper-status")
                        .role(Role::Status)
                        .child(hint(format!("Helper {word}. {message}"), cx)),
                );
            }
            _ => {}
        }
        Some(section)
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("connection-form")
            .track_focus(&self.focus)
            .key_context(CONTROLS)
            .role(Role::Form)
            .aria_label("Connection")
            .gap(ui(12.))
            .child(self.saved_hosts(cx))
            .child(self.connection_fields(cx))
            .children(
                self.saved_host_fields(cx)
                    .map(IntoElement::into_any_element),
            )
            .children(self.helper_section(cx).map(IntoElement::into_any_element))
    }
}

/// Opens Settings on its Connection tab, the form behind "Connection…" and the
/// host row.
pub fn open_connection_dialog(
    controller: Entity<ConnectionController>,
    window: &mut Window,
    cx: &mut App,
) {
    crate::settings::dialog::open_with(
        crate::settings::dialog::Tab::Connection,
        Some(controller),
        window,
        cx,
    );
}
