//! What the shell shows about the connection, floated over it so that nothing
//! moves the terminal surface: the status pill, the disconnected strip, the
//! recovery offers, the notices and the helper's consent dialog.

use std::{rc::Rc, time::Duration};

use gpui_kit::{
    App, Context, Entity, FontWeight, IntoElement, Render, Role, SharedString, Subscription, Task,
    Window,
    base::RootPlugin,
    component::{
        ActiveTheme, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        h_flex, v_flex,
    },
    div,
    prelude::*,
};

use crate::notices;
use crate::theme::{
    self,
    metrics::{TITLEBAR_HEIGHT, TYPE_META, TYPE_ROW, TYPE_SECONDARY, ui},
};

use super::{
    ConnectionUi,
    app_recovery::AppRecoveryPhase,
    connection_reducer::ConnectionPhase,
    controller::{ConnectionController, ConnectionEvent},
    dialog::{
        Confirmation, open_app_discard_dialog, open_confirmation, open_connection_dialog,
        open_helper_dialog,
    },
    helper_upgrade::HelperUpgradeState,
};

/// Sub-400ms blips are self-healing repairs whose strip would only be a flash.
pub const STRIP_APPEAR_DELAY_MS: u64 = 400;

fn strip_title(phase: ConnectionPhase) -> &'static str {
    match phase {
        ConnectionPhase::Connecting => "Connecting to tmux…",
        ConnectionPhase::Reconnecting => "Reconnecting to tmux…",
        ConnectionPhase::Resyncing => "Reconciling authoritative state…",
        ConnectionPhase::Disconnected | ConnectionPhase::Connected => "Disconnected from tmux",
    }
}

fn phase_word(phase: ConnectionPhase) -> &'static str {
    match phase {
        ConnectionPhase::Connected => "Connected",
        ConnectionPhase::Connecting => "Connecting",
        ConnectionPhase::Reconnecting => "Reconnecting",
        ConnectionPhase::Resyncing => "Reconciling",
        ConnectionPhase::Disconnected => "Disconnected",
    }
}

pub struct ConnectionSurface {
    controller: Option<Entity<ConnectionController>>,
    strip_ready: bool,
    strip_timer: Option<Task<()>>,
    helper_dialog_open: bool,
    discard_dialog_open: bool,
    _subscriptions: Vec<Subscription>,
}

impl RootPlugin for ConnectionSurface {}

impl ConnectionSurface {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let controller = cx.try_global::<ConnectionUi>().map(|ui| ui.0.clone());
        let mut subscriptions = Vec::new();
        if let Some(controller) = &controller {
            subscriptions.push(cx.observe_in(controller, window, |this, _, window, cx| {
                this.sync(window, cx);
                cx.notify();
            }));
            subscriptions.push(cx.subscribe_in(
                controller,
                window,
                |_, _, event: &ConnectionEvent, window, cx| {
                    let ConnectionEvent::Status(message) = event else {
                        return;
                    };
                    notices::notify(message, window, cx);
                },
            ));
        }
        // Deferred: the Root that holds dialogs does not exist while its plugins
        // are being built.
        cx.defer_in(window, |this, window, cx| this.sync(window, cx));
        Self {
            controller,
            strip_ready: false,
            strip_timer: None,
            helper_dialog_open: false,
            discard_dialog_open: false,
            _subscriptions: subscriptions,
        }
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(controller) = self.controller.clone() else {
            return;
        };
        let (degraded, helper_modal, discard_modal) = {
            let state = controller.read(cx);
            (
                state
                    .active_host_state()
                    .is_some_and(|host| host.phase != ConnectionPhase::Connected),
                matches!(
                    state.helper(),
                    HelperUpgradeState::Confirming { .. } | HelperUpgradeState::Upgrading { .. }
                ),
                state.app_recovery_modal_open(),
            )
        };
        if !degraded {
            self.strip_ready = false;
            self.strip_timer = None;
        } else if !self.strip_ready && self.strip_timer.is_none() {
            self.strip_timer = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(STRIP_APPEAR_DELAY_MS))
                    .await;
                let _ = this.update_in(cx, |this, _, cx| {
                    this.strip_ready = true;
                    this.strip_timer = None;
                    cx.notify();
                });
            }));
        }
        if helper_modal != self.helper_dialog_open {
            self.helper_dialog_open = helper_modal;
            if helper_modal {
                // Escape closes it inside the Kit: `sync` must not close another.
                let surface = cx.weak_entity();
                let closed_by_kit: Rc<dyn Fn(&mut App)> = Rc::new(move |cx| {
                    let _ = surface.update(cx, |this, _| this.helper_dialog_open = false);
                });
                open_helper_dialog(controller.clone(), closed_by_kit, window, cx);
            } else {
                window.close_dialog(cx);
            }
        }
        if discard_modal != self.discard_dialog_open {
            self.discard_dialog_open = discard_modal;
            if discard_modal {
                let surface = cx.weak_entity();
                let closed_by_kit: Rc<dyn Fn(&mut App)> = Rc::new(move |cx| {
                    let _ = surface.update(cx, |this, _| this.discard_dialog_open = false);
                });
                open_app_discard_dialog(controller, closed_by_kit, window, cx);
            } else {
                window.close_dialog(cx);
            }
        }
        // Those dialogs' bodies are built from the controller.
        if self.helper_dialog_open || self.discard_dialog_open {
            window.refresh();
        }
    }

    fn status_pill(
        controller: &Entity<ConnectionController>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let state = controller.read(cx);
        let phase = state
            .active_host_state()
            .map_or(ConnectionPhase::Disconnected, |host| host.phase);
        let color = match phase {
            ConnectionPhase::Connected => theme::palette(cx).ok,
            ConnectionPhase::Disconnected => theme::palette(cx).danger,
            _ => theme::palette(cx).warn,
        };
        let label: SharedString =
            format!("{} · {}", state.active_host_label(), phase_word(phase)).into();
        let open = controller.clone();
        h_flex()
            .id("connection-status-pill")
            .absolute()
            .left(ui(8.))
            .bottom(ui(6.))
            .gap(ui(6.))
            .items_center()
            .child(div().size(ui(7.)).rounded_full().bg(color))
            .child(
                Button::new("connection-status")
                    .ghost()
                    .xsmall()
                    .label(label.clone())
                    .accessibility_label(label)
                    .on_click(move |_, window, cx| {
                        open_connection_dialog(open.clone(), window, cx)
                    }),
            )
    }

    fn disconnected_strip(
        controller: &Entity<ConnectionController>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let state = controller.read(cx);
        let link = state.active_link()?;
        let phase = link.host_state.phase;
        if phase == ConnectionPhase::Connected {
            return None;
        }
        let detail: SharedString = if !link.detail.is_empty() {
            link.detail.clone().into()
        } else if link.host_state.snapshot.sessions.is_empty() {
            "Workspace data appears after a complete authoritative snapshot.".into()
        } else {
            "The last known workspace stays visible; writes are frozen and are not queued.".into()
        };
        let title = strip_title(phase);
        let palette = theme::palette(cx);
        let reconnect = controller.clone();
        let settings = controller.clone();
        let top = if cfg!(target_os = "macos") {
            ui(38.)
        } else {
            ui(TITLEBAR_HEIGHT)
        };
        Some(
            h_flex()
                .id("connection-strip")
                .occlude()
                .absolute()
                .top(top)
                .left_0()
                .right_0()
                .gap(ui(8.))
                .px(ui(10.))
                .py(ui(4.))
                .items_center()
                .text_size(ui(TYPE_SECONDARY))
                .bg(palette.chrome_raised)
                .border_b_1()
                .border_color(palette.warn)
                .child(
                    h_flex()
                        .id("connection-strip-message")
                        .role(Role::Status)
                        .aria_label(format!("{title} {detail}"))
                        .flex_1()
                        .min_w_0()
                        .gap(ui(8.))
                        .child(div().flex_none().font_weight(FontWeight(600.)).child(title))
                        .child(
                            div()
                                .truncate()
                                .text_color(palette.chrome_dim)
                                .child(detail),
                        ),
                )
                .child(
                    Button::new("connection-strip-reconnect")
                        .ghost()
                        .xsmall()
                        .label("Reconnect")
                        .on_click(move |_, _, cx| {
                            reconnect.update(cx, |controller, cx| controller.reconnect(cx));
                        }),
                )
                .child(
                    Button::new("connection-strip-settings")
                        .ghost()
                        .xsmall()
                        .label("Connection…")
                        .on_click(move |_, window, cx| {
                            open_connection_dialog(settings.clone(), window, cx);
                        }),
                ),
        )
    }

    fn profile_recovery_toast(
        controller: &Entity<ConnectionController>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let recovery = controller.read(cx).profile_recovery()?.clone();
        let reset = controller.clone();
        Some(toast(
            "connection-profile-recovery",
            Role::Alert,
            "Saved host profiles were recovered".into(),
            format!(
                "{} The original was preserved at {}.",
                recovery.error, recovery.preserved_path
            )
            .into(),
            cx,
        )
        .child(
            Button::new("connection-profile-recovery-confirm")
                .xsmall()
                .label("Confirm recovered defaults…")
                .on_click(move |_, window, cx| {
                    let target = reset.clone();
                    open_confirmation(
                        Confirmation {
                            title: "Reset saved host profiles?",
                            detail: "Keep the preserved invalid profile file and write a fresh Local profile. tmux sessions and terminal processes are not changed.".into(),
                            confirm_label: "Use recovered defaults",
                            destructive: false,
                            on_confirm: Box::new(move |_, cx| {
                                target.update(cx, |controller, cx| controller.confirm_profile_reset(cx));
                            }),
                        },
                        window,
                        cx,
                    );
                }),
        ))
    }

    fn app_recovery_toast(
        controller: &Entity<ConnectionController>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let offer = controller.read(cx).app_recovery()?.clone();
        if offer.phase != AppRecoveryPhase::Offered {
            return None;
        }
        let plural = if offer.count == 1 { "" } else { "s" };
        let restore = controller.clone();
        let discard = controller.clone();
        Some(
            toast(
                "connection-app-recovery",
                Role::Status,
                "App tabs found from the replaced tmux server".into(),
                format!(
                    "{} tab{plural} can be rebound by unique workspace name. Terminal and pane identities are never reused.",
                    offer.count
                )
                .into(),
                cx,
            )
            .child(
                h_flex()
                    .gap(ui(6.))
                    .child(
                        Button::new("connection-app-recovery-restore")
                            .xsmall()
                            .primary()
                            .label("Restore app tabs")
                            .on_click(move |_, _, cx| {
                                restore.update(cx, |controller, cx| controller.restore_app_tabs(cx));
                            }),
                    )
                    .child(
                        Button::new("connection-app-recovery-discard")
                            .xsmall()
                            .ghost()
                            .label("Discard old tabs…")
                            .on_click(move |_, _, cx| {
                                discard.update(cx, |state, cx| state.request_app_tab_discard(cx));
                            }),
                    ),
            ),
        )
    }
}

fn toast(
    id: &'static str,
    role: Role,
    title: SharedString,
    body: SharedString,
    cx: &App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let palette = theme::palette(cx);
    v_flex()
        .id(id)
        .occlude()
        .role(role)
        .aria_label(title.clone())
        .w(ui(340.))
        .gap(ui(6.))
        .p(ui(10.))
        .rounded(cx.theme().radius)
        .bg(palette.chrome_raised)
        .border_1()
        .border_color(palette.chrome_border)
        .shadow_md()
        .text_size(ui(TYPE_SECONDARY))
        .child(
            div()
                .font_weight(FontWeight(600.))
                .text_size(ui(TYPE_ROW))
                .child(title),
        )
        .child(
            div()
                .text_color(palette.chrome_dim)
                .text_size(ui(TYPE_META))
                .child(body),
        )
}

impl Render for ConnectionSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().id("connection-surface").absolute().size_full();
        let Some(controller) = self.controller.clone() else {
            return root;
        };
        // This layer paints above the Kit's dialogs; a modal owns the screen.
        if window.has_active_dialog(cx) {
            return root;
        }
        let strip = self
            .strip_ready
            .then(|| Self::disconnected_strip(&controller, cx))
            .flatten();
        let toasts = v_flex()
            .id("connection-offers")
            .absolute()
            .right(ui(12.))
            .bottom(ui(12.))
            .gap(ui(8.))
            .items_end()
            .children(
                Self::profile_recovery_toast(&controller, cx).map(IntoElement::into_any_element),
            )
            .children(Self::app_recovery_toast(&controller, cx).map(IntoElement::into_any_element));
        root.child(Self::status_pill(&controller, cx))
            .children(strip)
            .child(toasts)
    }
}
