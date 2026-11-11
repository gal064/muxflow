//! The right panel: Files, Git, Ports and any extensions as segments of one
//! surface, a port of `features/shell/RightPanel.tsx`. A closed panel is not
//! rendered and reserves no width.

use std::{collections::HashMap, rc::Rc};

use gpui_kit::{
    AnyElement, App, Global, InteractiveElement, IntoElement, ParentElement, RenderOnce, Role,
    StatefulInteractiveElement, Styled, WeakEntity, Window,
    base::{HandleEdge, Tab, Tabs, TestSupportExt},
    component::{h_flex, v_flex},
    div,
    prelude::FluentBuilder,
};

use crate::{
    theme::{
        self,
        metrics::{BAR_HEIGHT, TYPE_TITLEBAR, ui},
    },
    ui::chrome::{
        Chrome,
        divider::{Divider, divider},
    },
};

type PanelBody = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// What each panel segment draws, registered by the feature that owns it. A
/// segment without a body shows its name until its feature lands.
#[derive(Default)]
pub struct PanelBodies(HashMap<&'static str, PanelBody>);

impl Global for PanelBodies {}

impl PanelBodies {
    pub fn register(
        id: &'static str,
        body: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
        cx: &mut App,
    ) {
        cx.default_global::<Self>().0.insert(id, Rc::new(body));
    }
}

#[derive(IntoElement)]
pub struct RightPanel {
    pub chrome: WeakEntity<Chrome>,
    pub width: f32,
    pub surfaces: Vec<(String, String)>,
    pub surface: String,
}

impl RenderOnce for RightPanel {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let body_for = cx
            .try_global::<PanelBodies>()
            .and_then(|bodies| bodies.0.get(self.surface.as_str()).cloned());
        let tab_count = self.surfaces.len();
        let selected_label = self
            .surfaces
            .iter()
            .find(|(id, _)| *id == self.surface)
            .map_or("Files", |(_, label)| label.as_str())
            .to_owned();
        v_flex()
            .id("right-panel")
            .test_support()
            .role(Role::Complementary)
            .aria_label("Files, Git and Ports")
            .relative()
            .flex_none()
            .h_full()
            .w(ui(self.width))
            .min_h_0()
            .bg(palette.chrome_bg)
            .border_l_1()
            .border_color(palette.chrome_hairline)
            .child(
                Tabs::new("panel-tabs")
                    .aria_label("Panel surface")
                    .flex()
                    .items_stretch()
                    .h(ui(BAR_HEIGHT))
                    .flex_none()
                    .px(ui(6.))
                    .border_b_1()
                    .border_color(palette.chrome_hairline)
                    .children(
                        self.surfaces
                            .iter()
                            .enumerate()
                            .map(|(index, (id, label))| {
                                let selected = *id == self.surface;
                                let chrome = self.chrome.clone();
                                let surface = id.clone();
                                Tab::new(format!("panel-tab-{id}"))
                                    .selected(selected)
                                    .accessibility_label(label.clone())
                                    .set_position(index + 1, tab_count)
                                    .on_click(move |_, _, cx| {
                                        chrome
                                            .update(cx, |chrome, cx| {
                                                chrome.show_surface(&surface, cx)
                                            })
                                            .ok();
                                    })
                                    .relative()
                                    .px(ui(11.))
                                    .text_size(ui(TYPE_TITLEBAR))
                                    .text_color(if selected {
                                        palette.chrome_ink_strong
                                    } else {
                                        palette.chrome_dim
                                    })
                                    .child(label.clone())
                                    .when(selected, |tab| {
                                        tab.child(
                                            div()
                                                .absolute()
                                                .left(ui(6.))
                                                .right(ui(6.))
                                                .bottom_0()
                                                .h(ui(2.))
                                                .rounded_t(ui(1.))
                                                .bg(palette.accent),
                                        )
                                    })
                            }),
                    ),
            )
            .child(
                h_flex()
                    .id("panel-body")
                    .role(Role::TabPanel)
                    .aria_label(selected_label.clone())
                    .flex_1()
                    .min_h_0()
                    .map(|body| match body_for {
                        Some(draw) => body.child(draw(window, cx)),
                        None => body
                            .items_center()
                            .justify_center()
                            .text_color(palette.chrome_faint)
                            .child(selected_label),
                    }),
            )
            .child(divider(
                self.chrome,
                Divider::Panel,
                HandleEdge::Leading,
                false,
                self.width,
                window,
                cx,
            ))
    }
}
