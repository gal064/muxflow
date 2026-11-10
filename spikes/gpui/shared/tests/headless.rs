//! S7: a headless GPUI test with a real offscreen render and a pixel check.
//!
//! Opens the spike showcase in a window that is never shown, renders a frame
//! through GPUI's offscreen renderer (wgpu/Vulkan; lavapipe in CI) and fails if
//! the frame is blank. Set
//! `SPIKE_FRAME_OUT=<file.png>` to keep the frame.

use std::{collections::HashSet, sync::Arc};

use gpui_kit::{
    Bounds, HeadlessAppContext, WindowBounds, WindowOptions, assets::Assets, px, size,
};
use spike_shared::showcase::Showcase;

fn main() {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        spike_shared::fonts::register(cx);
    });
    let (handle, _) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(720.), px(420.)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                cx,
                |_, cx| Showcase::new("Muxflow GPUI spike", "Headless frame for CI.", cx),
            )
        })
        .expect("open the headless window");
    let frame = cx
        .capture_screenshot(handle)
        .expect("offscreen rendering is unavailable (no Vulkan driver?)");
    let (width, height) = frame.dimensions();
    let colors: HashSet<[u8; 4]> = frame.pixels().map(|p| p.0).collect();
    println!("headless frame {width}x{height}, {} distinct colors", colors.len());
    if let Ok(path) = std::env::var("SPIKE_FRAME_OUT") {
        frame.save(&path).expect("save the frame");
        println!("saved {path}");
    }
    // A blank or failed render is one flat color; text and the button give dozens.
    assert!(colors.len() >= 32, "the frame looks blank: {} colors", colors.len());
}
