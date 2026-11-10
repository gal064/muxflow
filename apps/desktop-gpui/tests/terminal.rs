//! A terminal pane through GPUI's test platform, on the real engine thread:
//! frames flood in, the engine's credit comes back, and the pane paints from
//! damage. The poll is timer-driven, so nothing here wakes a GPUI task from the
//! engine thread, which is what GPUI's test scheduler refuses.

use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc::Sender};
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use desktop_core::app_state::ThemePreference;
use futures::FutureExt;
use futures::future::BoxFuture;
use gpui_kit::{AppContext, Entity, TestAppContext, font, px};
use muxflow_gpui::{
    connection_ui::backend::{EngineChannel, TerminalBackend},
    terminal::{
        TerminalLink, TerminalPane,
        cell_metrics::CellMetrics,
        link::{LinkEvent, POLL_ACTIVE},
    },
    theme,
};
use serde_json::Value;
use terminal_engine::{EngineOutput, GridSize, HubLimits};

#[derive(Default)]
struct Calls {
    seeds: Vec<String>,
    visibility: Vec<(String, bool)>,
    input: Vec<Vec<u8>>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Calls>>);

impl TerminalBackend for Fake {
    fn spawn_engine(&self, outputs: Sender<EngineOutput>) -> Box<dyn EngineChannel> {
        Box::new(terminal_engine::spawn(HubLimits::default(), outputs))
    }

    fn request_seed(
        &self,
        _client: String,
        pane_id: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        self.0.lock().unwrap().seeds.push(pane_id);
        async { Ok(()) }.boxed()
    }

    fn request_history(
        &self,
        _: String,
        _: String,
        _: u32,
        _: u32,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn set_pane_visibility(
        &self,
        _client: String,
        pane_id: String,
        visible: bool,
        _: bool,
        _: u64,
        _: u64,
    ) -> BoxFuture<'static, Result<(), String>> {
        self.0.lock().unwrap().visibility.push((pane_id, visible));
        async { Ok(()) }.boxed()
    }

    fn resize_client(
        &self,
        _: String,
        _: u16,
        _: u16,
        _: String,
        _: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn send_input(&self, _: String, _: String, bytes: Vec<u8>) -> Result<(), String> {
        self.0.lock().unwrap().input.push(bytes);
        Ok(())
    }

    fn send_paste(&self, _: String, _: String, text: Vec<u8>) -> Result<(), String> {
        self.0.lock().unwrap().input.push(text);
        Ok(())
    }

    fn record_incident(&self, _: &str, _: Value) {}

    fn set_terminal_colors(&self, _: String, _: String) -> Result<(), String> {
        Ok(())
    }

    fn tmux_action(&self, _: String, _: Value) -> BoxFuture<'static, Result<Value, String>> {
        async { Ok(Value::Null) }.boxed()
    }

    fn start_upload(
        &self,
        _: muxflow_gpui::terminal::integration::transfers::UploadRequest,
        _: Arc<dyn desktop_core::platform::EventSink>,
    ) -> Result<String, String> {
        Err("no uploads here".into())
    }

    fn cancel_upload(&self, _: String) -> Result<Value, String> {
        Ok(Value::Null)
    }
}

fn frame(kind: u8, label: &str, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(label.len() as u16).to_be_bytes());
    bytes.extend_from_slice(label.as_bytes());
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn epoch_frame(epoch: u64) -> Vec<u8> {
    frame(10, "terminal", 0, &epoch.to_be_bytes())
}

fn seed_frame(pane: &str, generation: u64, sequence: u64, text: &[u8]) -> Vec<u8> {
    let mut payload = generation.to_be_bytes().to_vec();
    payload.extend_from_slice(text);
    frame(1, pane, sequence, &payload)
}

fn output_frame(pane: &str, generation: u64, sequence: u64, text: &[u8]) -> Vec<u8> {
    let mut payload = generation.to_be_bytes().to_vec();
    payload.extend_from_slice(text);
    frame(2, pane, sequence, &payload)
}

fn new_link(fake: &Fake, cx: &mut TestAppContext) -> Entity<TerminalLink> {
    let backend: Arc<dyn TerminalBackend> = Arc::new(fake.clone());
    cx.new(|cx| TerminalLink::new("qa".into(), backend, cx))
}

/// Polls the link on virtual time while the real engine thread catches up.
fn wait_until(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.executor().advance_clock(POLL_ACTIVE);
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[gpui_kit::test]
fn every_flooded_frame_earns_its_credit_and_a_resize_lands_mid_flood(cx: &mut TestAppContext) {
    themed(cx);
    let fake = Fake::default();
    let link = new_link(&fake, cx);
    let consumed = Rc::new(Mutex::new((0_u64, 0_u64)));
    let total_consumed = consumed.clone();
    cx.update(|cx| {
        cx.subscribe(&link, move |_, event, _| {
            if let LinkEvent::FramesConsumed(frames) = event {
                let mut tally = total_consumed.lock().unwrap();
                for (byte_length, epoch) in frames {
                    assert_eq!(*epoch, Some(7), "every frame applied under the live epoch");
                    tally.0 += 1;
                    tally.1 += *byte_length as u64;
                }
            }
        })
        .detach();
    });
    let link_for_pane = link.clone();
    let (pane, visual) = cx.add_window_view(|window, cx| {
        let probes = link_for_pane.read(cx).probes();
        TerminalPane::new("%1".into(), link_for_pane, metrics(), probes, window, cx)
    });
    let size = GridSize {
        columns: 80,
        rows: 24,
    };
    link.update(visual, |link, _| {
        link.set_client(Some("client-1".into()), 1);
        link.mount("%1", &pane, size);
    });
    let mut expected = (0_u64, 0_u64);
    let mut send = |frame: Vec<u8>, visual: &mut gpui_kit::VisualTestContext| {
        expected.0 += 1;
        expected.1 += frame.len() as u64;
        link.read_with(visual, |link, _| link.forward_frame(frame));
    };
    send(epoch_frame(7), visual);
    wait_until(visual, |_| !fake.0.lock().unwrap().seeds.is_empty());
    send(seed_frame("%1", 1, 1, b"\x1b[2J\x1b[Hseed\r\n"), visual);
    let line = vec![b'x'; 4096];
    let frames = 20_000;
    for sequence in 0..frames {
        send(
            output_frame("%1", 2 + sequence, 2 + sequence, &line),
            visual,
        );
        if sequence == frames / 2 {
            link.read_with(visual, |link, _| {
                link.set_grid(
                    "%1",
                    GridSize {
                        columns: 50,
                        rows: 10,
                    },
                )
            });
        }
    }
    let (want_frames, want_bytes) = expected;
    wait_until(visual, |_| {
        *consumed.lock().unwrap() == (want_frames, want_bytes)
    });
    pane.read_with(visual, |pane, _| {
        let terminal = pane.terminal().expect("mounted");
        let grid = terminal.lock();
        assert_eq!((grid.columns(), grid.screen_lines()), (50, 10));
    });
}

fn themed(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::apply(
            ThemePreference::Dark,
            1.0,
            gpui_kit::WindowAppearance::Dark,
            cx,
        );
    });
}

fn metrics() -> Rc<CellMetrics> {
    Rc::new(CellMetrics {
        font: font("JetBrains Mono"),
        font_size: px(13.),
        cell_width: px(8.),
        cell_height: px(17.),
        line_width: px(1.),
        scrollbar_width: px(14.),
    })
}

fn text_of(pane: &TerminalPane, row: usize) -> String {
    pane.row_spec(row)
        .map(|spec| {
            spec.segments
                .iter()
                .map(|segment| segment.text.clone())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[gpui_kit::test]
fn a_mounted_pane_paints_in_full_then_only_the_rows_that_changed(cx: &mut TestAppContext) {
    themed(cx);
    let fake = Fake::default();
    let link = new_link(&fake, cx);
    let size = GridSize {
        columns: 40,
        rows: 6,
    };
    let link_for_pane = link.clone();
    let (pane, visual) = cx.add_window_view(|window, cx| {
        let probes = link_for_pane.read(cx).probes();
        TerminalPane::new("%1".into(), link_for_pane, metrics(), probes, window, cx)
    });
    link.update(visual, |link, _| {
        link.set_client(Some("client-1".into()), 1);
        link.mount("%1", &pane, size);
    });
    link.read_with(visual, |link, _| {
        link.forward_frame(epoch_frame(7));
    });
    wait_until(visual, |cx| {
        pane.read_with(cx, |pane, _| pane.terminal().is_some())
    });
    wait_until(visual, |_| {
        !fake.0.lock().unwrap().visibility.is_empty() || !fake.0.lock().unwrap().seeds.is_empty()
    });
    link.read_with(visual, |link, _| {
        link.forward_frame(seed_frame(
            "%1",
            1,
            1,
            b"\x1b[2J\x1b[Hfirst row\r\nsecond row\r\nthird row",
        ));
    });
    wait_until(visual, |cx| {
        pane.read_with(cx, |pane, _| text_of(pane, 0) == "first row")
    });
    pane.read_with(visual, |pane, _| {
        assert_eq!(text_of(pane, 1), "second row");
        assert_eq!(text_of(pane, 2), "third row");
    });

    // The full paint is behind us. One changed row is one row shaped.
    link.read_with(visual, |link, _| {
        link.forward_frame(output_frame("%1", 2, 2, b"\x1b[1;1Hchanged"));
    });
    wait_until(visual, |cx| {
        pane.read_with(cx, |pane, _| text_of(pane, 0).starts_with("changed"))
    });
    pane.read_with(visual, |pane, _| {
        let stats = pane.last_paint_stats();
        assert!(
            (1..=2).contains(&stats.reshaped) && stats.reshaped + stats.reused <= 2,
            "damage repaints only the touched row and the old cursor row: {stats:?}",
        );
        assert_eq!(text_of(pane, 1), "second row");
    });

    // Ten frames between two paints still reach the screen in one.
    link.read_with(visual, |link, _| {
        for sequence in 0..10_u64 {
            link.forward_frame(output_frame(
                "%1",
                3 + sequence,
                3 + sequence,
                format!("\x1b[3;1Hburst-{sequence}").as_bytes(),
            ));
        }
    });
    wait_until(visual, |cx| {
        pane.read_with(cx, |pane, _| text_of(pane, 2).starts_with("burst-9"))
    });
}

#[gpui_kit::test]
fn a_remounted_pane_paints_its_retained_screen_without_new_output(cx: &mut TestAppContext) {
    themed(cx);
    let fake = Fake::default();
    let link = new_link(&fake, cx);
    let size = GridSize {
        columns: 40,
        rows: 6,
    };
    let link_for_pane = link.clone();
    let (first, visual) = cx.add_window_view(|window, cx| {
        let probes = link_for_pane.read(cx).probes();
        TerminalPane::new("%1".into(), link_for_pane, metrics(), probes, window, cx)
    });
    link.update(visual, |link, _| {
        link.set_client(Some("client-1".into()), 1);
        link.mount("%1", &first, size);
    });
    link.read_with(visual, |link, _| link.forward_frame(epoch_frame(7)));
    wait_until(visual, |_| !fake.0.lock().unwrap().seeds.is_empty());
    link.read_with(visual, |link, _| {
        link.forward_frame(seed_frame("%1", 1, 1, b"\x1b[2J\x1b[Hkept"));
    });
    wait_until(visual, |cx| {
        first.read_with(cx, |pane, _| text_of(pane, 0) == "kept")
    });
    link.update(visual, |link, _| link.unmount("%1"));
    wait_until(visual, |_| {
        fake.0
            .lock()
            .unwrap()
            .visibility
            .iter()
            .any(|(_, visible)| !visible)
    });
    let link_for_pane = link.clone();
    let (retained, visual) = cx.add_window_view(|window, cx| {
        let probes = link_for_pane.read(cx).probes();
        TerminalPane::new("%1".into(), link_for_pane, metrics(), probes, window, cx)
    });
    link.update(visual, |link, _| link.mount("%1", &retained, size));
    wait_until(visual, |cx| {
        retained.read_with(cx, |pane, _| pane.terminal().is_some())
    });
    wait_until(visual, |cx| {
        retained.read_with(cx, |pane, _| text_of(pane, 0) == "kept")
    });
}

#[gpui_kit::test]
fn input_reaches_the_backend_and_needs_a_live_client(cx: &mut TestAppContext) {
    themed(cx);
    let fake = Fake::default();
    let link = new_link(&fake, cx);
    let link_for_pane = link.clone();
    let (pane, visual) = cx.add_window_view(|window, cx| {
        let probes = link_for_pane.read(cx).probes();
        TerminalPane::new("%3".into(), link_for_pane, metrics(), probes, window, cx)
    });
    let no_client = pane.update(visual, |pane, cx| pane.send_input(b"x".to_vec(), cx));
    assert!(no_client.is_err());
    link.update(visual, |link, _| {
        link.set_client(Some("client-1".into()), 1);
    });
    pane.update(visual, |pane, cx| pane.send_input(b"y".to_vec(), cx))
        .unwrap();
    assert_eq!(fake.0.lock().unwrap().input, [b"y".to_vec()]);
}
