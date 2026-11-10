//! The window root: title bar over the feature chrome, themed and scaled.

use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, Window,
    component::{ActiveTheme, v_flex},
    div,
};

use crate::{
    theme::{
        self,
        metrics::{TYPE_ROW, ui},
    },
    ui::{chrome::Chrome, title_bar::TitleBar},
};

pub struct Shell {
    chrome: Entity<Chrome>,
    _appearance: Subscription,
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Kit reads the OS appearance per window on Linux; follow it live.
        theme::follow_system(window.appearance(), cx);
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            theme::follow_system(window.appearance(), cx);
        });
        Self {
            chrome: Chrome::new(window, cx),
            _appearance: appearance,
        }
    }

    pub fn open(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title: AnyElement = self.chrome.read(cx).title_bar_content().into_any_element();
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_size(ui(TYPE_ROW))
            .child(TitleBar::new(title))
            .child(div().flex_1().min_h_0().child(self.chrome.clone()))
    }
}
