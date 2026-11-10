//! S3 probe: prints what the platform gives a GPUI window (decorations, scale)
//! and exercises the clipboard both ways. Dropped files are logged. Disposable.
use gpui_kit::component::button::Button;
use gpui_kit::*;
use std::time::Duration;

struct Probe;

impl Render for Probe {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let line = format!(
            "decorations={:?} scale={}",
            window.window_decorations(),
            window.scale_factor()
        );
        println!("{line}");
        div()
            .size_full()
            .bg(rgb(0x282c34))
            .text_color(rgb(0xffffff))
            .p_8()
            .on_drop(|paths: &ExternalPaths, _, _| println!("dropped={:?}", paths.paths()))
            .child(line)
            .child(
                Button::new("path-prompt")
                    .label("Choose path")
                    .on_click(|_, _, cx| {
                        let result = cx.prompt_for_paths(PathPromptOptions {
                            files: true,
                            directories: true,
                            multiple: false,
                            prompt: Some("S11 path probe".into()),
                        });
                        cx.spawn(async move |_| {
                            println!("path-prompt={:?}", result.await);
                        })
                        .detach();
                    }),
            )
    }
}

fn main() {
    gpui_kit::application().run(|cx: &mut App| {
        gpui_kit::init(cx);
        spike_shared::fonts::register(cx);
        // Never touch a real session's clipboard: set this on a shared compositor.
        if std::env::var_os("PROBE_NO_CLIPBOARD").is_none() {
            cx.write_to_clipboard(ClipboardItem::new_string("muxflow-probe-out".to_owned()));
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(500))
                        .await;
                    let text = cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text()));
                    println!("clipboard={text:?}");
                }
            })
            .detach();
        }
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Muxflow S11 platform probe".into()),
                ..Default::default()
            }),
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        };
        cx.open_window(options, |_, cx| cx.new(|_| Probe))
            .expect("open window");
        cx.activate(true);
    });
}
