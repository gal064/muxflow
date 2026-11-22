//! The window root: title bar over the feature chrome, themed and scaled.

use std::time::Duration;

use desktop_core::app_state::NumberKeys;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, Global, InteractiveElement, IntoElement,
    Modifiers, ParentElement, Render, Styled, Subscription, Task, Window,
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
    _visibility: Subscription,
    _activation: Subscription,
}

/// How long the platform key must be held alone before the agents' numbers
/// show: long enough that ⌘C, ⌘V and the terminal's Ctrl chords never flash
/// them.
pub const MODIFIER_HOLD_DELAY: Duration = Duration::from_millis(400);

/// Whether the platform key has been held alone long enough to show the
/// numbers ⌘1–9 reach in the agent list.
#[derive(Default)]
struct ModifierHold {
    showing: bool,
    /// Set from the press until the hold ends; the numbers show once it fires.
    timer: Option<Task<()>>,
}

impl Global for ModifierHold {}

/// Whether the agent list draws its rows' numbers now.
pub fn modifier_hold_showing(cx: &App) -> bool {
    cx.try_global::<ModifierHold>()
        .is_some_and(|hold| hold.showing)
}

/// Ends a hold: the numbers go, and a pending timer with them.
pub fn end_modifier_hold(window: &mut Window, cx: &mut App) {
    let Some(hold) = cx.try_global::<ModifierHold>() else {
        return;
    };
    let showing = hold.showing;
    if !showing && hold.timer.is_none() {
        return;
    }
    *cx.global_mut::<ModifierHold>() = ModifierHold::default();
    if showing {
        window.refresh();
    }
}

/// Only the platform key, held while ⌘1–9 jump to agents, starts the timer.
/// Any other modifier state ends the hold.
fn modifiers_changed(modifiers: Modifiers, window: &mut Window, cx: &mut App) {
    if modifiers != Modifiers::secondary_key(cx)
        || crate::settings::get(cx).number_keys != NumberKeys::Agents
    {
        end_modifier_hold(window, cx);
        return;
    }
    let hold = cx.default_global::<ModifierHold>();
    if hold.showing || hold.timer.is_some() {
        return;
    }
    let timer = window.spawn(cx, async move |cx| {
        cx.background_executor().timer(MODIFIER_HOLD_DELAY).await;
        cx.update(|window, cx| {
            cx.default_global::<ModifierHold>().showing = true;
            window.refresh();
        })
        .ok();
    });
    cx.default_global::<ModifierHold>().timer = Some(timer);
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Kit reads the OS appearance per window on Linux; follow it live.
        theme::follow_system(window.appearance(), cx);
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            theme::follow_system(window.appearance(), cx);
        });
        // A window left with ⌘ down never sees it come up.
        let activation = cx.observe_window_activation(window, |_, window, cx| {
            if !window.is_window_active() {
                end_modifier_hold(window, cx);
            }
        });
        Self {
            chrome: Chrome::new(window, cx),
            _appearance: appearance,
            _visibility: crate::ui::visibility::track(window, cx),
            _activation: activation,
        }
    }

    pub fn open(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::notices::fit_to(window, cx);
        let chrome = self.chrome.read(cx);
        let (title, tabs_in_bar): (AnyElement, bool) = (
            chrome.title_bar_content().into_any_element(),
            chrome.tabs_in_title_bar(),
        );
        v_flex()
            .size_full()
            .on_modifiers_changed(|event, window, cx| {
                modifiers_changed(event.modifiers, window, cx)
            })
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_size(ui(TYPE_ROW))
            .child(TitleBar::new(title, tabs_in_bar))
            .child(div().flex_1().min_h_0().child(self.chrome.clone()))
            .children(crate::context_menu::keyboard_menu(window, cx))
    }
}
