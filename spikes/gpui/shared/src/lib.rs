//! Shared bootstrap for the GPUI spikes: embedded fonts, the app's dark palette
//! and a window helper. Disposable; later spikes (T0.4 terminal, T0.5 markdown)
//! build on this.

pub mod fonts;
pub mod showcase;
pub mod theme;

use gpui_kit::*;

/// Start the application, initialise GPUI Kit, register the embedded fonts and
/// open one window whose content `build` creates.
///
/// Blocks until the app quits. Closing the last window quits.
pub fn run<V: Render + 'static>(
    title: &str,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V> + 'static,
) {
    let title = SharedString::from(title.to_owned());
    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        fonts::register(cx);

        // Quit with the last window; otherwise a headless run never ends.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(720.), px(420.)), cx);
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title),
                    ..Default::default()
                }),
                ..Default::default()
            },
            cx,
            build,
        )
        .expect("failed to open the spike window");
        cx.activate(true);
    });
}
