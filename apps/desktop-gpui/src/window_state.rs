//! Window geometry: restored at launch and saved as the window moves. Port of
//! `apps/desktop/src/features/shell/windowGeometry.ts`.
//!
//! The saved form is the one Tauri wrote, so an upgrade keeps the window where
//! it was: physical pixels plus the scale they were captured at. GPUI measures in
//! logical pixels, so both directions go through that scale. A window restored on
//! a screen with another scale keeps its logical size, as before.
//!
//! The save never overlaps another writer: it goes through `update_app_state`,
//! which loads, changes and writes under the store's lock.

use std::time::Duration;

use desktop_core::{
    app_state::{WindowGeometry, load_app_state, update_app_state},
    platform,
};
use gpui_kit::{App, Bounds, Global, Task, Window, WindowBounds, point, px, size};

use crate::{
    app::{Core, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH},
    ui::shell::Shell,
};

/// How long the window rests before its geometry is written.
const SAVE_DELAY: Duration = Duration::from_millis(400);
const MIN_SCALE: f64 = 0.5;
const MAX_SCALE: f64 = 8.;
/// How much of a restored window must stay on a screen, so it can be grabbed.
const MINIMUM_VISIBLE: f64 = 64.;

/// A rectangle in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    fn intersects(&self, other: &Rect) -> bool {
        self.x.max(other.x) < (self.x + self.width).min(other.x + other.width)
            && self.y.max(other.y) < (self.y + self.height).min(other.y + other.height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Restored {
    pub bounds: Rect,
    pub maximized: bool,
}

fn safe_scale(scale: f64) -> f64 {
    if scale.is_finite() && (MIN_SCALE..=MAX_SCALE).contains(&scale) {
        scale
    } else {
        1.
    }
}

/// The geometry to save: physical pixels with the scale they were measured at.
/// `bounds` is the restore size, so a maximized window comes back as it was.
pub fn capture(bounds: Rect, maximized: bool, scale: f64) -> WindowGeometry {
    let scale = safe_scale(scale);
    // `update_app_state` refuses a size under the minimum, so never offer one.
    let at_least = |value: f64, minimum: f64| (value * scale).round().max((minimum * scale).ceil());
    WindowGeometry {
        x: (bounds.x * scale).round() as i32,
        y: (bounds.y * scale).round() as i32,
        width: at_least(bounds.width, MIN_WINDOW_WIDTH as f64) as u32,
        height: at_least(bounds.height, MIN_WINDOW_HEIGHT as f64) as u32,
        maximized,
        scale_factor_milli: Some((scale * 1_000.).round() as u32),
    }
}

/// Where to open: the saved rectangle, moved onto a screen when none shows it.
/// `displays` are in logical pixels. With no display at all the saved position
/// is kept.
pub fn restore(geometry: &WindowGeometry, displays: &[Rect], primary: Option<Rect>) -> Restored {
    // A save from before scales were recorded is read as 100%.
    let scale = safe_scale(
        geometry
            .scale_factor_milli
            .map_or(1., |milli| f64::from(milli) / 1_000.),
    );
    let width = (f64::from(geometry.width) / scale).max(MIN_WINDOW_WIDTH as f64);
    let height = (f64::from(geometry.height) / scale).max(MIN_WINDOW_HEIGHT as f64);
    let saved = Rect {
        x: f64::from(geometry.x) / scale,
        y: f64::from(geometry.y) / scale,
        width,
        height,
    };
    let bounds = match displays.iter().find(|display| display.intersects(&saved)) {
        Some(display) => Rect {
            x: saved
                .x
                .max(display.x - width + MINIMUM_VISIBLE)
                .min(display.x + display.width - MINIMUM_VISIBLE),
            y: saved
                .y
                .max(display.y - height + MINIMUM_VISIBLE)
                .min(display.y + display.height - MINIMUM_VISIBLE),
            width,
            height,
        },
        None => match primary.or_else(|| displays.first().copied()) {
            Some(display) => Rect {
                x: display.x + (display.width - width) / 2.,
                y: display.y + (display.height - height) / 2.,
                width,
                height,
            },
            None => saved,
        },
    };
    Restored {
        bounds,
        maximized: geometry.maximized,
    }
}

fn rect(bounds: Bounds<gpui_kit::Pixels>) -> Rect {
    Rect {
        x: f64::from(bounds.origin.x),
        y: f64::from(bounds.origin.y),
        width: f64::from(bounds.size.width),
        height: f64::from(bounds.size.height),
    }
}

/// The saved window placement, for the first window's options.
pub fn saved_bounds(cx: &App) -> Option<WindowBounds> {
    let state = load_app_state(&cx.global::<Core>().app_state).ok()?;
    let geometry = state.shell.window_geometry?;
    let displays: Vec<Rect> = cx
        .displays()
        .iter()
        .map(|display| rect(display.bounds()))
        .collect();
    let primary = cx.primary_display().map(|display| rect(display.bounds()));
    let restored = restore(&geometry, &displays, primary);
    let bounds = Bounds {
        origin: point(px(restored.bounds.x as f32), px(restored.bounds.y as f32)),
        size: size(
            px(restored.bounds.width as f32),
            px(restored.bounds.height as f32),
        ),
    };
    Some(if restored.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    })
}

/// The newest geometry seen, and the one on disk.
#[derive(Default)]
struct Saver {
    latest: Option<WindowGeometry>,
    saved: Option<WindowGeometry>,
    delay: Option<Task<()>>,
}

impl Global for Saver {}

/// Starts the write of whatever is newest and not yet saved. It runs on the
/// backend's runtime, so it outlives the window.
fn save_now(cx: &mut App) -> Option<impl Future<Output = ()> + use<>> {
    let store = cx.global::<Core>().app_state.clone();
    let saver = cx.global_mut::<Saver>();
    saver.delay = None;
    let geometry = saver.latest.filter(|latest| saver.saved != Some(*latest))?;
    saver.saved = Some(geometry);
    let save = platform::spawn(async move {
        let saved = update_app_state(&store, move |state| {
            state.shell.window_geometry = Some(geometry);
        })
        .await;
        if let Err(error) = saved {
            eprintln!("muxflow: could not save the window geometry: {error}");
        }
    });
    Some(async move {
        save.await.ok();
    })
}

/// The geometry to keep after a placement change. A maximized window keeps the
/// size it had before: X11 reports the screen rectangle as its bounds, and the
/// restore size is the one worth coming back to.
fn next_geometry(
    previous: Option<WindowGeometry>,
    bounds: Rect,
    maximized: bool,
    scale: f64,
) -> WindowGeometry {
    match previous {
        Some(previous) if maximized => WindowGeometry {
            maximized: true,
            ..previous
        },
        _ => capture(bounds, maximized, scale),
    }
}

/// Notes the window's placement and writes it once it rests.
fn record(window: &mut Window, cx: &mut App) {
    // X11 reports a full-screen window as a windowed one of screen size.
    if window.is_fullscreen() {
        return;
    }
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) => (bounds, true),
        // Leaving full screen reports the real placement again.
        WindowBounds::Fullscreen(_) => return,
    };
    let scale = f64::from(window.scale_factor());
    let saver = cx.global_mut::<Saver>();
    saver.latest = Some(next_geometry(saver.latest, rect(bounds), maximized, scale));
    let delay = window.spawn(cx, async move |cx| {
        cx.background_executor().timer(SAVE_DELAY).await;
        cx.update(|_, cx| drop(save_now(cx))).ok();
    });
    cx.global_mut::<Saver>().delay = Some(delay);
}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    // Seeded from disk, so a window that starts maximized keeps its restore size.
    let on_disk = load_app_state(&cx.global::<Core>().app_state)
        .ok()
        .and_then(|state| state.shell.window_geometry);
    cx.set_global(Saver {
        latest: on_disk,
        saved: on_disk,
        delay: None,
    });
    // A window closed inside the delay still writes where it ended.
    cx.on_app_quit(|cx| {
        let save = save_now(cx);
        async move {
            if let Some(save) = save {
                save.await;
            }
        }
    })
    .detach();
    cx.observe_new::<Shell>(|_, window, cx| {
        let Some(window) = window else {
            return;
        };
        cx.observe_window_bounds(window, |_, window, cx| record(window, cx))
            .detach();
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        x: 0.,
        y: 0.,
        width: 1920.,
        height: 1080.,
    };

    fn saved(x: i32, y: i32, width: u32, height: u32, scale: Option<u32>) -> WindowGeometry {
        WindowGeometry {
            x,
            y,
            width,
            height,
            maximized: false,
            scale_factor_milli: scale,
        }
    }

    #[test]
    fn capture_stores_physical_pixels_with_their_scale() {
        let bounds = Rect {
            x: 100.4,
            y: 50.,
            width: 800.,
            height: 600.,
        };
        let geometry = capture(bounds, true, 1.5);
        assert_eq!(
            geometry,
            WindowGeometry {
                x: 151,
                y: 75,
                width: 1200,
                height: 900,
                maximized: true,
                scale_factor_milli: Some(1500),
            }
        );
    }

    #[test]
    fn capture_never_offers_a_size_the_store_refuses() {
        let tiny = Rect {
            x: 0.,
            y: 0.,
            width: 10.,
            height: 10.,
        };
        let geometry = capture(tiny, false, 2.);
        assert_eq!((geometry.width, geometry.height), (640, 480));
        assert_eq!(
            capture(tiny, false, f64::NAN).scale_factor_milli,
            Some(1000)
        );
    }

    #[test]
    fn restore_round_trips_a_visible_window() {
        let bounds = Rect {
            x: 200.,
            y: 120.,
            width: 1000.,
            height: 700.,
        };
        for scale in [1., 1.25, 2.] {
            let restored = restore(&capture(bounds, false, scale), &[SCREEN], Some(SCREEN));
            assert_eq!(restored.bounds, bounds, "scale {scale}");
        }
    }

    #[test]
    fn a_window_keeps_its_logical_size_on_a_screen_with_another_scale() {
        // Saved at 200%, restored where logical sizes are all that is known.
        let restored = restore(&saved(400, 200, 2000, 1400, Some(2000)), &[SCREEN], None);
        assert_eq!(
            (restored.bounds.width, restored.bounds.height),
            (1000., 700.)
        );
    }

    #[test]
    fn a_save_without_a_scale_is_read_as_physical_at_100_percent() {
        let restored = restore(&saved(10, 20, 900, 600, None), &[SCREEN], None);
        assert_eq!(restored.bounds.width, 900.);
    }

    #[test]
    fn a_window_on_a_missing_screen_is_centered_on_the_primary() {
        let restored = restore(
            &saved(5000, 100, 800, 600, Some(1000)),
            &[SCREEN],
            Some(SCREEN),
        );
        assert_eq!(
            restored.bounds,
            Rect {
                x: 560.,
                y: 240.,
                width: 800.,
                height: 600.
            }
        );
    }

    #[test]
    fn a_window_mostly_off_screen_keeps_a_grabbable_corner() {
        let restored = restore(&saved(1900, 1070, 800, 600, Some(1000)), &[SCREEN], None);
        assert_eq!((restored.bounds.x, restored.bounds.y), (1856., 1016.));
        let restored = restore(&saved(-790, -590, 800, 600, Some(1000)), &[SCREEN], None);
        assert_eq!((restored.bounds.x, restored.bounds.y), (-736., -536.));
    }

    #[test]
    fn restoring_with_no_display_keeps_the_saved_position() {
        let restored = restore(&saved(30, 40, 800, 600, Some(1000)), &[], None);
        assert_eq!((restored.bounds.x, restored.bounds.y), (30., 40.));
    }

    #[test]
    fn a_maximized_window_keeps_the_size_it_had() {
        let windowed = capture(
            Rect {
                x: 100.,
                y: 80.,
                width: 900.,
                height: 600.,
            },
            false,
            1.,
        );
        let screen = Rect {
            x: 0.,
            y: 0.,
            width: 1920.,
            height: 1080.,
        };
        let maximized = next_geometry(Some(windowed), screen, true, 1.);
        assert_eq!(
            maximized,
            WindowGeometry {
                maximized: true,
                ..windowed
            }
        );
        // With nothing earlier to keep, the bounds given are all there is.
        assert_eq!(next_geometry(None, screen, true, 1.).width, 1920);
        // Restoring takes the new bounds.
        assert_eq!(
            next_geometry(Some(maximized), screen, false, 1.).width,
            1920
        );
    }

    #[test]
    fn maximized_is_carried_through() {
        let mut geometry = saved(0, 0, 800, 600, Some(1000));
        geometry.maximized = true;
        assert!(restore(&geometry, &[SCREEN], None).maximized);
    }
}
