//! The Ports panel, with the Tauri copy. Port of the view half of
//! `PortsPanel.tsx`.

use desktop_core::connection::ports::ForwardState;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, Role, SharedString, StatefulInteractiveElement, Styled, Subscription,
    Window,
    assets::IconName,
    base::TestSupportExt,
    component::{
        Disableable, Icon, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    div,
    prelude::FluentBuilder,
};

use crate::{
    context_menu::{Entry, Item, fill},
    fonts,
    theme::{
        self, Palette,
        metrics::{RADIUS_CONTROL, TYPE_LABEL, TYPE_META, TYPE_SECONDARY, ui},
    },
};
use gpui_kit::component::menu::DropdownMenu;

use super::{model::local_url, panel::PortsPanel};

/// The two port fields, made with the first frame: an input needs a window.
pub struct Fields {
    remote: Entity<InputState>,
    local: Entity<InputState>,
    _subscriptions: [Subscription; 2],
}

impl Fields {
    fn new(window: &mut Window, cx: &mut Context<PortsPanel>) -> Self {
        let remote = cx.new(|cx| InputState::new(window, cx).placeholder("Port"));
        let local = cx.new(|cx| InputState::new(window, cx).placeholder("same"));
        let watch =
            |input: &Entity<InputState>, window: &mut Window, cx: &mut Context<PortsPanel>| {
                cx.subscribe_in(
                    input,
                    window,
                    |this, _, event: &InputEvent, window, cx| match event {
                        InputEvent::PressEnter { .. } => this.submit_fields(window, cx),
                        // The placeholder and the Forward button follow the typing.
                        InputEvent::Change => this.typed(window, cx),
                        _ => {}
                    },
                )
            };
        Self {
            _subscriptions: [watch(&remote, window, cx), watch(&local, window, cx)],
            remote,
            local,
        }
    }
}

impl PortsPanel {
    fn fields(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<InputState>, Entity<InputState>) {
        let fields = self.fields.get_or_insert_with(|| Fields::new(window, cx));
        (fields.remote.clone(), fields.local.clone())
    }

    fn typed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(fields) = &self.fields else { return };
        let remote = fields.remote.read(cx).value().trim().to_owned();
        let placeholder = if remote.is_empty() {
            "same".to_owned()
        } else {
            remote
        };
        fields.local.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
        cx.notify();
    }

    fn texts(&self, cx: &App) -> (String, String) {
        self.fields
            .as_ref()
            .map_or_else(Default::default, |fields| {
                (
                    fields.remote.read(cx).value().to_string(),
                    fields.local.read(cx).value().to_string(),
                )
            })
    }

    fn submit_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (remote, local) = self.texts(cx);
        if self.submit(&remote, &local, cx)
            && let Some(fields) = &self.fields
        {
            for input in [&fields.remote, &fields.local] {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
            self.typed(window, cx);
        }
    }
}

fn quiet(text: impl Into<SharedString>, palette: &Palette) -> impl IntoElement {
    div()
        .px(ui(12.))
        .py(ui(10.))
        .text_size(ui(TYPE_SECONDARY))
        .text_color(palette.chrome_faint)
        .child(text.into())
}

/// `.section-label`: small, spaced capitals.
fn section_label(text: &'static str, palette: &Palette) -> impl IntoElement {
    div()
        .text_size(ui(TYPE_LABEL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(palette.chrome_faint)
        .child(text.to_uppercase())
}

fn mono(text: impl Into<SharedString>, palette: &Palette) -> gpui_kit::Div {
    div()
        .font_family(fonts::MONO)
        .text_size(ui(12.))
        .text_color(palette.chrome_ink)
        .child(text.into())
}

impl Render for PortsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let section = v_flex()
            .id("ports-panel")
            .test_support()
            .role(Role::Region)
            .aria_label("Ports")
            .size_full()
            .min_h_0()
            .overflow_y_scroll();
        let Some(host) = self.host().cloned() else {
            return section.child(quiet("Port forwarding needs an SSH host.", &palette));
        };
        let (remote_input, local_input) = self.fields(window, cx);
        let (remote_text, _) = self.texts(cx);
        let can_forward = !remote_text.trim().is_empty();
        let forwards = self.host_forwards();
        let suggestions = self.suggestions();

        let chooser = (self.hosts().len() > 1).then(|| {
            let hosts: Vec<_> = self
                .hosts()
                .iter()
                .map(|candidate| (candidate.profile_id.clone(), candidate.label.clone()))
                .collect();
            let (this, current) = (cx.entity().downgrade(), host.profile_id.clone());
            h_flex()
                .items_center()
                .gap(ui(8.))
                .px(ui(12.))
                .pt(ui(8.))
                .child(section_label("Host", &palette))
                .child(
                    Button::new("ports-host")
                        .ghost()
                        .xsmall()
                        .label(host.label.clone())
                        .accessibility_label(format!("Host: {}", host.label))
                        .dropdown_menu(move |menu, window, cx| {
                            let entries: Vec<Entry> = hosts
                                .iter()
                                .map(|(id, label)| {
                                    let checked = *id == current;
                                    let (id, this) = (id.clone(), this.clone());
                                    Item::new(label.clone(), move |_, cx| {
                                        this.update(cx, |panel, cx| panel.choose(id.clone(), cx))
                                            .ok();
                                    })
                                    .checked(checked)
                                    .into()
                                })
                                .collect();
                            fill(menu, &entries, window, cx)
                        }),
                )
        });

        let form = h_flex()
            .items_center()
            .gap(ui(6.))
            .px(ui(12.))
            .py(ui(8.))
            .text_color(palette.chrome_faint)
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&remote_input)
                        .id("ports-remote")
                        .aria_label("Remote port"),
                ),
            )
            .child(div().flex_none().child("→"))
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&local_input)
                        .id("ports-local")
                        .aria_label("Local port (same as remote when empty)"),
                ),
            )
            .child(
                Button::new("ports-forward")
                    .primary()
                    .xsmall()
                    .label("Forward")
                    .disabled(!can_forward)
                    .on_click(cx.listener(|this, _, window, cx| this.submit_fields(window, cx))),
            );

        let error = self.error.clone().map(|text| {
            div()
                .id("ports-error")
                .role(Role::Alert)
                .mx(ui(12.))
                .mb(ui(4.))
                .px(ui(8.))
                .py(ui(6.))
                .rounded(ui(RADIUS_CONTROL))
                .bg(palette.danger_wash)
                .text_color(palette.danger_ink)
                .text_size(ui(TYPE_SECONDARY))
                .child(text)
        });

        let rows: Vec<AnyElement> = forwards
            .iter()
            .map(|item| {
                let dot = match item.state {
                    ForwardState::Active => palette.state_done,
                    ForwardState::Starting => palette.state_working,
                    ForwardState::Failed => palette.danger,
                };
                let state = match item.state {
                    ForwardState::Active => "active",
                    ForwardState::Starting => "starting",
                    ForwardState::Failed => "failed",
                };
                let (remote, local) = (item.remote_port, item.local_port);
                let active = item.state == ForwardState::Active;
                let link = Button::new(SharedString::from(format!("port-open-{remote}")))
                    .ghost()
                    .xsmall()
                    .compact()
                    .disabled(!active)
                    .tooltip(format!("Open {}", local_url(local)))
                    .accessibility_label(format!("{remote} → 127.0.0.1:{local}"))
                    .child(mono(format!("{remote} → 127.0.0.1:{local}"), &palette))
                    .on_click(cx.listener(move |this, _, _, cx| this.open(local, cx)));
                let group: SharedString = format!("port-forward-{remote}").into();
                v_flex()
                    .id(SharedString::from(format!("port-forward-{remote}")))
                    .role(Role::ListItem)
                    .group(group.clone())
                    .px(ui(12.))
                    .py(ui(2.))
                    .hover(|style| style.bg(palette.chrome_hover))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(ui(6.))
                            .child(
                                div()
                                    .id(SharedString::from(format!("port-dot-{remote}")))
                                    .flex_none()
                                    .size(ui(7.))
                                    .rounded_full()
                                    .bg(dot)
                                    .tooltip(move |window, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(state)
                                            .build(window, cx)
                                    }),
                            )
                            .child(div().flex_1().min_w_0().child(link))
                            .when(item.state == ForwardState::Failed, |row| {
                                row.child(
                                    Button::new(SharedString::from(format!("port-retry-{remote}")))
                                        .ghost()
                                        .xsmall()
                                        .label("Retry")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.forward(remote, local, cx);
                                        })),
                                )
                            })
                            .child(
                                Button::new(SharedString::from(format!("port-stop-{remote}")))
                                    .ghost()
                                    .xsmall()
                                    .compact()
                                    .w(ui(20.))
                                    .accessibility_label(format!("Stop forwarding {remote}"))
                                    .tooltip("Stop forwarding")
                                    .icon(
                                        Icon::new(IconName::Close)
                                            .size(ui(11.))
                                            .text_color(palette.chrome_faint),
                                    )
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.stop(remote, cx)),
                                    ),
                            ),
                    )
                    .when_some(item.error.clone(), |row, error| {
                        row.child(
                            div()
                                .pl(ui(13.))
                                .text_size(ui(TYPE_META))
                                .text_color(palette.danger_ink)
                                .child(error),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        let detected = v_flex()
            .px(ui(12.))
            .py(ui(8.))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(section_label("Detected on host", &palette))
                    .child(
                        Button::new("ports-detect")
                            .ghost()
                            .xsmall()
                            .compact()
                            .w(ui(22.))
                            .accessibility_label("Detect listening ports")
                            .tooltip("Detect listening ports")
                            .icon(
                                Icon::new(IconName::RefreshCw)
                                    .size(ui(14.))
                                    .text_color(palette.chrome_dim),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.detect(cx))),
                    ),
            )
            .when(!suggestions.is_empty(), |this| {
                this.child(h_flex().flex_wrap().gap(ui(4.)).mt(ui(4.)).children(
                    suggestions.iter().map(|detected| {
                        let port = detected.port;
                        h_flex()
                            .id(SharedString::from(format!("port-suggestion-{port}")))
                            .role(gpui_kit::Role::Button)
                            .aria_label(format!("Forward {port}"))
                            .items_baseline()
                            .gap(ui(4.))
                            .px(ui(7.))
                            .py(ui(2.))
                            .rounded(ui(10.))
                            .border_1()
                            .border_color(palette.chrome_border)
                            .bg(palette.chrome_raised)
                            .hover(|style| style.border_color(palette.accent))
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(format!(
                                    "Forward {port}"
                                ))
                                .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.forward(port, port, cx);
                            }))
                            .child(mono(port.to_string(), &palette))
                            .when_some(detected.process.clone(), |chip, process| {
                                chip.child(
                                    div()
                                        .text_size(ui(TYPE_META))
                                        .text_color(palette.chrome_faint)
                                        .child(process),
                                )
                            })
                    }),
                ))
            });

        section
            .children(chooser)
            .child(form)
            .children(error)
            .child(
                v_flex()
                    .id("ports-list")
                    .role(Role::List)
                    .aria_label("Forwarded ports")
                    .children(rows),
            )
            .child(detected)
    }
}
