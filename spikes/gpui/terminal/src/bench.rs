//! The repeatable measurement driver. Every scenario runs against the real
//! path: private tmux, control-mode parsing, a `Term` per pane, the GPUI
//! element, and in-process keystrokes via `Window::dispatch_keystroke`.
//! Results are one JSON file per scenario; `measure.sh` runs them in order.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::*;

use alacritty_terminal::grid::Dimensions;
use crate::control::{PaneSetup, Tmux};
use crate::probe::KeySample;
use crate::stats::{anon_huge_kb, cpu_seconds, dist, dist_json, pid_cpu_seconds, rss_kb, thread_cpu_json, thread_cpu_seconds};
use crate::view::{FrameRec, Measure, Shared, Workspace};

/// One line at a time: exercises rich colors with a printable payload of
/// hexadecimal only, so a typed marker from [`MARKERS`] is always unique.
pub fn run_flood_writer(lines: Option<u64>) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let colors = ["31", "32", "33", "34", "35", "36", "1;37", "2;36", "4;33", "7;35"];
    // Text differs on every line, so GPUI's frame-to-frame line layout cache
    // cannot serve it. Hex only: the typing markers never appear in it.
    let mut rng = 0x9e37_79b9_7f4a_7c15_u64;
    let mut payload = || {
        let mut text = String::with_capacity(128);
        for _ in 0..8 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{rng:016x}"));
        }
        text
    };
    let mut counter: u64 = 0;
    let mut block = Vec::with_capacity(1 << 16);
    while lines.is_none_or(|limit| counter < limit) {
        block.clear();
        for _ in 0..64 {
            let color = colors[(counter % colors.len() as u64) as usize];
            let _ = write!(
                block,
                "\x1b[{color}m{counter:010x} {}\x1b[0m {}\r\n",
                payload(),
                payload()
            );
            counter = counter.wrapping_add(1);
        }
        if out.write_all(&block).is_err() || out.flush().is_err() {
            return;
        }
    }
}

/// Rewrites one line at about 30 Hz: the single-pane change for the view-cache test.
pub fn run_ticker() {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let mut counter: u64 = 0;
    loop {
        let _ = write!(out, "\rtick {counter:08}");
        let _ = out.flush();
        counter = counter.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(33));
    }
}

/// Typed characters: none appear in the flood's hexadecimal/SGR output.
const MARKERS: &[u8] = b"ghijklnopqrstuvwxyz";

/// A different seed per run, so repeated runs sample different tick phases.
fn jitter_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
}

fn lcg(seed: &mut u64, modulus: u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*seed >> 33) % modulus
}

#[derive(Clone)]
pub struct BenchArgs {
    pub scenario: String,
    pub panes: usize,
    pub seconds: u64,
    pub samples: usize,
    pub gap_min_ms: u64,
    pub gap_max_ms: u64,
    pub out: String,
    pub flood_all: bool,
    pub key_timeout_s: u64,
    pub fill_panes: bool,
    pub flood_during: bool,
}

struct Bench {
    handle: AnyWindowHandle,
    workspace: Entity<Workspace>,
    shared: Arc<Shared>,
    setup: PaneSetup,
    args: BenchArgs,
    tmux: Arc<Tmux>,
}

pub fn start(
    cx: &mut App,
    handle: AnyWindowHandle,
    workspace: Entity<Workspace>,
    shared: Arc<Shared>,
    setup: PaneSetup,
    args: BenchArgs,
    tmux: Arc<Tmux>,
) {
    cx.spawn(async move |cx| {
        let bench = Bench {
            handle,
            workspace,
            shared,
            setup,
            args: args.clone(),
            tmux,
        };
        // Let shells print their first prompt and the first frames settle.
        cx.background_executor()
            .timer(Duration::from_millis(1500))
            .await;
        let json = match args.scenario.as_str() {
            "latency" => bench.latency(cx).await,
            "flood" => bench.flood(cx).await,
            "resize" => bench.resize(cx).await,
            "switch" => bench.switch(cx).await,
            "idle" => bench.idle(cx).await,
            "onepane" => bench.onepane(cx).await,
            "ime" => bench.ime(cx).await,
            "ligature" => bench.ligature(cx).await,
            "pixel" => bench.pixel(cx).await,
            other => format!(r#"{{"error":"unknown scenario {other}"}}"#),
        };
        let gpu = bench
            .handle
            .update(cx, |_, window, cx| {
                let refresh = window
                    .refresh_interval(cx)
                    .map(|d| format!("{:.3}", d.as_secs_f64() * 1000.0))
                    .unwrap_or_else(|| "null".to_string());
                let active = window.is_window_active();
                match window.gpu_specs() {
                    Some(g) => format!(
                        r#"{{"software":{},"device":{:?},"driver":{:?},"driver_info":{:?},"refresh_interval_ms":{refresh},"window_active":{active}}}"#,
                        g.is_software_emulated, g.device_name, g.driver_name, g.driver_info
                    ),
                    None => format!(
                        r#"{{"software":null,"refresh_interval_ms":{refresh},"window_active":{active}}}"#
                    ),
                }
            })
            .unwrap_or_else(|_| "null".to_string());
        let display = std::env::var("DISPLAY").unwrap_or_default();
        let wrapped = format!(r#"{{"gpu":{gpu},"display":{display:?},"result":{json}}}"#);
        std::fs::write(&args.out, wrapped).expect("write results");
        cx.update(|cx| cx.quit());
    })
    .detach();
}

fn frames_json(all_frames: &[FrameRec]) -> String {
    // GPUI draws a dirty window at the start of a key dispatch (no present
    // follows until the next tick). Those draws are counted apart so that every
    // field below describes tick-driven frames that really were presented.
    let (key_draws, frames): (Vec<FrameRec>, Vec<FrameRec>) =
        all_frames.iter().cloned().partition(|f| f.key_event_draw);
    let frames = &frames[..];
    let key_draw_build: Vec<u64> = key_draws.iter().map(|f| f.build_us).collect();
    let intervals: Vec<u64> = frames.windows(2).map(|w| w[1].at_us - w[0].at_us).collect();
    let build: Vec<u64> = frames.iter().map(|f| f.build_us).collect();
    let present: Vec<u64> = frames.iter().map(|f| f.present_us).collect();
    let total: Vec<u64> = frames.iter().map(|f| f.build_us + f.present_us).collect();
    let over_16 = total.iter().filter(|v| **v > 16_667).count();
    // Frame builds closer than half a refresh would be more than one paint per tick.
    let begins: Vec<u64> = frames.windows(2).map(|w| w[1].begin_us - w[0].begin_us).collect();
    let under_8 = begins.iter().filter(|v| **v < 8_000).count();
    let reshaped: u64 = frames.iter().map(|f| f.rows_reshaped as u64).sum();
    let reused: u64 = frames.iter().map(|f| f.rows_reused as u64).sum();
    let damaged: u64 = frames.iter().map(|f| f.rows_damaged as u64).sum();
    let views: u64 = frames.iter().map(|f| f.pane_views_rendered as u64).sum();
    let prepainted: u64 = frames.iter().map(|f| f.panes_prepainted as u64).sum();
    let term_copy: Vec<u64> = frames.iter().map(|f| f.term_copy_us).collect();
    let shape: Vec<u64> = frames.iter().map(|f| f.shape_us).collect();
    let n = frames.len().max(1) as f64;
    format!(
        concat!(
            r#"{{"count":{},"interval":{},"build":{},"present":{},"total":{},"#,
            r#""over_16_7ms":{},"begin_interval":{},"begin_intervals_under_8ms":{},"term_copy":{},"shape":{},"#,
            r#""rows_reshaped_per_frame":{:.2},"rows_reused_per_frame":{:.2},"#,
            r#""rows_damaged_per_frame":{:.2},"pane_views_rendered_per_frame":{:.2},"#,
            r#""panes_prepainted_per_frame":{:.2},"#,
            r#""key_event_draws":{},"key_event_draw_build":{}}}"#
        ),
        frames.len(),
        dist_json(dist(&intervals)),
        dist_json(dist(&build)),
        dist_json(dist(&present)),
        dist_json(dist(&total)),
        over_16,
        dist_json(dist(&begins)),
        under_8,
        dist_json(dist(&term_copy)),
        dist_json(dist(&shape)),
        reshaped as f64 / n,
        reused as f64 / n,
        damaged as f64 / n,
        views as f64 / n,
        prepainted as f64 / n,
        key_draws.len(),
        dist_json(dist(&key_draw_build)),
    )
}

fn keys_json(samples: &[KeySample], timeouts: u64, timeouts_after_pause: u64) -> String {
    let d = |f: fn(&KeySample) -> u64| dist(&samples.iter().map(f).collect::<Vec<_>>());
    let verified = samples.iter().filter(|s| s.verified).count();
    let next_equals_marker = samples
        .iter()
        .filter(|s| s.next_output_us == s.marker_output_us)
        .count();
    format!(
        concat!(
            r#"{{"count":{},"timeouts":{},"timeouts_after_pause":{},"verified_glyph":{},"#,
            r#""next_output_is_echo":{},"#,
            r#""a_next_output_echoLagProbe":{},"a_marker_output":{},"#,
            r#""frame_wait":{},"b_frame_paint":{},"b_frame_present":{},"dispatch_wait":{},"dispatch_wait_plus_b_present":{}}}"#
        ),
        samples.len(),
        timeouts,
        timeouts_after_pause,
        verified,
        next_equals_marker,
        dist_json(d(|s| s.next_output_us)),
        dist_json(d(|s| s.marker_output_us)),
        dist_json(d(|s| s.frame_wait_us)),
        dist_json(d(|s| s.frame_paint_us)),
        dist_json(d(|s| s.frame_present_us)),
        dist_json(d(|s| s.dispatch_wait_us)),
        dist_json(d(|s| s.dispatch_wait_us + s.frame_present_us)),
    )
}

struct RssSampler {
    stop: Arc<AtomicBool>,
    series: Arc<Mutex<Vec<(f64, u64, u64)>>>,
    started: Instant,
}

impl RssSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let series = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        {
            let (stop, series) = (stop.clone(), series.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    series
                        .lock()
                        .unwrap()
                        .push((started.elapsed().as_secs_f64(), rss_kb(), anon_huge_kb()));
                    std::thread::sleep(Duration::from_secs(1));
                }
            });
        }
        Self {
            stop,
            series,
            started,
        }
    }

    fn finish(self) -> Vec<(f64, u64, u64)> {
        self.stop.store(true, Ordering::Release);
        let _ = self.started;
        self.series.lock().unwrap().clone()
    }
}

/// Each sample is `[seconds, VmRSS KiB, AnonHugePages KiB]`.
fn rss_json(series: &[(f64, u64, u64)]) -> String {
    let body: Vec<String> = series
        .iter()
        .map(|(t, kb, huge)| format!("[{t:.1},{kb},{huge}]"))
        .collect();
    format!("[{}]", body.join(","))
}

impl Bench {
    fn panes(&self) -> &[String] {
        &self.setup.panes
    }

    fn press(&self, cx: &mut AsyncApp, key: &str, control: bool) {
        let keystroke = Keystroke {
            modifiers: Modifiers {
                control,
                ..Default::default()
            },
            key: key.to_string(),
            key_char: None,
        };
        let log = self.shared.log.clone();
        let _ = self.handle.update(cx, |_, window, cx| {
            log.set_in_key_dispatch(true);
            window.dispatch_keystroke(keystroke, cx);
            log.set_in_key_dispatch(false);
        });
    }

    async fn sleep(&self, cx: &mut AsyncApp, duration: Duration) {
        cx.background_executor().timer(duration).await;
    }

    /// Type one marker in `pane` and wait for its frame, recording one sample.
    async fn one_key(
        &self,
        cx: &mut AsyncApp,
        pane: &str,
        marker: u8,
        at_cursor: bool,
        deadline: Instant,
    ) {
        self.shared.probe.arm(pane, marker, at_cursor, deadline);
        let key = (marker as char).to_string();
        self.press(cx, &key, false);
        let started = Instant::now();
        while !self.shared.probe.is_completed() {
            if started.elapsed() > Duration::from_secs(self.args.key_timeout_s) {
                self.shared.probe.time_out_pending();
                break;
            }
            self.sleep(cx, Duration::from_millis(2)).await;
        }
    }

    async fn typing_loop(
        &self,
        cx: &mut AsyncApp,
        pane: &str,
        at_cursor: bool,
        until: impl Fn(usize) -> bool,
    ) -> Vec<KeySample> {
        let mut rng = jitter_seed();
        let mut index = 0usize;
        let mut pressed = 0usize;
        while !until(pressed) {
            if pressed % 30 == 29 {
                // ctrl-u keeps the prompt line short; not a measured key.
                self.press(cx, "u", true);
                self.sleep(cx, Duration::from_millis(30)).await;
            }
            let span = self.args.gap_max_ms.saturating_sub(self.args.gap_min_ms).max(1);
            let gap = self.args.gap_min_ms + lcg(&mut rng, span);
            let deadline = Instant::now() + Duration::from_millis(gap);
            self.sleep(cx, Duration::from_millis(gap)).await;
            let marker = MARKERS[index % MARKERS.len()];
            index += 1;
            self.one_key(cx, pane, marker, at_cursor, deadline).await;
            pressed += 1;
        }
        self.shared.probe.take_samples()
    }

    /// Put real content in `panes`: a mostly empty grid hides what repainting
    /// and reflow cost. Returns once they are back at a prompt.
    async fn fill_panes(&self, cx: &mut AsyncApp, panes: &[String]) {
        let exe = std::env::current_exe().expect("current exe");
        let fill = format!("{} flood-writer --lines 400", exe.display());
        for pane in panes {
            self.tmux
                .run(&["send-keys", "-t", pane, fill.as_str(), "Enter"])
                .expect("fill pane");
        }
        self.sleep(cx, Duration::from_secs(2)).await;
    }

    async fn latency(&self, cx: &mut AsyncApp) -> String {
        if self.args.fill_panes {
            let panes = self.panes().to_vec();
            self.fill_panes(cx, &panes).await;
        }
        let pane = self.panes()[0].clone();
        self.shared.log.take_frames();
        self.focus_pane(cx, &pane).await;
        let cpu0 = cpu_seconds();
        let started = Instant::now();
        let samples_target = self.args.samples;
        let samples = self
            .typing_loop(cx, &pane, true, |n| n >= samples_target)
            .await;
        let elapsed = started.elapsed().as_secs_f64();
        let cpu = (cpu_seconds() - cpu0) / elapsed * 100.0;
        let frames = self.shared.log.take_frames();
        format!(
            r#"{{"scenario":"latency","panes":{},"gap_ms":[{},{}],"cpu_percent":{:.1},"elapsed_s":{:.1},"keys":{},"frames":{}}}"#,
            self.panes().len(),
            self.args.gap_min_ms,
            self.args.gap_max_ms,
            cpu,
            elapsed,
            keys_json(&samples, self.shared.probe.timeouts(), self.shared.probe.timeouts_after_pause()),
            frames_json(&frames),
        )
    }

    /// Calibration of leg (b)'s end point: poll the X server for the cell the
    /// echo lands in, and compare that moment with `Window::present` returning.
    async fn pixel(&self, cx: &mut AsyncApp) -> String {
        let pane = self.panes()[0].clone();
        let window = self.setup.window.clone();
        self.focus_pane(cx, &pane).await;
        let mut reader = match crate::x11_pixels::PixelReader::connect() {
            Ok(reader) => reader,
            Err(error) => return format!(r#"{{"scenario":"pixel","error":{error:?}}}"#),
        };
        let mut rng = 0xbeef_u64;
        let (mut key_to_pixel, mut after_present) = (Vec::new(), Vec::new());
        let mut pixel_before_present = 0usize;
        let mut missed = 0usize;
        self.shared.probe.take_samples();
        for i in 0..self.args.samples {
            if i % 30 == 29 {
                self.press(cx, "u", true);
                self.sleep(cx, Duration::from_millis(200)).await;
            }
            let gap = self.args.gap_min_ms + lcg(&mut rng, self.args.gap_max_ms.saturating_sub(self.args.gap_min_ms).max(1));
            self.sleep(cx, Duration::from_millis(gap)).await;
            let cursor = {
                let term = self.shared.session.pane(&pane);
                let term = term.term.lock();
                term.grid().cursor.point
            };
            let rect = self
                .workspace
                .read_with(cx, |w, _| w.cell_rect(&window, &pane, cursor.column.0, cursor.line.0 as usize))
                .expect("pane rect");
            let baseline = reader.grab(rect.0, rect.1, rect.2, rect.3).expect("grab");
            let poll = std::thread::spawn(move || {
                reader.wait_for_change(baseline, rect, Duration::from_secs(2))
            });
            let marker = MARKERS[i % MARKERS.len()];
            self.one_key(cx, &pane, marker, true, Instant::now()).await;
            let (returned, seen) = poll.join().expect("poll thread");
            reader = returned;
            let present_us = self.shared.probe.take_samples().last().map(|s| s.frame_present_us);
            match (seen, self.shared.probe.last_dispatch(), present_us) {
                (Some(seen), Some(t0), Some(present_us)) => {
                    let pixel_us = seen.saturating_duration_since(t0).as_micros() as u64;
                    key_to_pixel.push(pixel_us);
                    after_present.push(pixel_us.saturating_sub(present_us));
                    if pixel_us < present_us {
                        pixel_before_present += 1;
                    }
                }
                _ => missed += 1,
            }
        }
        format!(
            r#"{{"scenario":"pixel","samples":{},"missed":{},"key_to_x11_pixels":{},"pixels_after_present_returned":{},"pixels_seen_before_present_returned":{}}}"#,
            self.args.samples,
            missed,
            dist_json(dist(&key_to_pixel)),
            dist_json(dist(&after_present)),
            pixel_before_present,
        )
    }

    /// `--flood-during`: resize and switch with 7 of the panes flooding.
    async fn flood_during_scenario(&self, cx: &mut AsyncApp) {
        if !self.args.flood_during {
            return;
        }
        let flooded = self.panes()[..self.panes().len() - 1].to_vec();
        self.start_flooders(&flooded);
        self.sleep(cx, Duration::from_secs(3)).await;
    }

    fn start_flooders(&self, panes: &[String]) {
        let exe = std::env::current_exe().expect("current exe");
        let command = format!("exec {} flood-writer", exe.display());
        for pane in panes {
            self.tmux
                .run(&["send-keys", "-t", pane, command.as_str(), "Enter"])
                .expect("start flooder");
        }
    }

    async fn flood(&self, cx: &mut AsyncApp) -> String {
        let all = self.panes().to_vec();
        let typing_pane = all.last().cloned().expect("at least one pane");
        let flooded: Vec<String> = if self.args.flood_all {
            all.clone()
        } else {
            all[..all.len() - 1].to_vec()
        };
        // Focus first: a tmux command written during a flood queues behind it.
        self.focus_pane(cx, &typing_pane).await;
        self.start_flooders(&flooded);
        // Quiesce ramp: tmux and the kernel buffers fill.
        self.sleep(cx, Duration::from_secs(3)).await;
        self.shared.log.take_frames();
        self.shared.probe.take_samples();
        let bytes0 = self.fed_bytes(&flooded);
        let cpu0 = cpu_seconds();
        let threads0 = thread_cpu_seconds();
        let tmux_pid: u32 = self
            .tmux
            .run(&["display-message", "-p", "#{pid}"])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let tmux_cpu0 = pid_cpu_seconds(tmux_pid);
        let feed0 = self.feed_stats();
        let rss = RssSampler::start();
        let started = Instant::now();
        let seconds = self.args.seconds;
        let at_cursor = !self.args.flood_all;
        // Unpainted panes never close a sample, so a no-paint run does not type.
        let samples = if self.shared.paint_panes {
            self.typing_loop(cx, &typing_pane, at_cursor, |_| {
                started.elapsed() >= Duration::from_secs(seconds)
            })
            .await
        } else {
            self.sleep(cx, Duration::from_secs(seconds)).await;
            Vec::new()
        };
        let elapsed = started.elapsed().as_secs_f64();
        let series = rss.finish();
        let cpu = (cpu_seconds() - cpu0) / elapsed * 100.0;
        let bytes = self.fed_bytes(&flooded) - bytes0;
        let frames = self.shared.log.take_frames();
        let threads = thread_cpu_json(&threads0, &thread_cpu_seconds(), elapsed);
        let feed = self.feed_stats();
        let pct = |a: u64, b: u64| (a - b) as f64 / 1e6 / elapsed * 100.0;
        let chunks = (feed[3] - feed0[3]).max(1);
        let feed_json = format!(
            r#"{{"read_wait_pct":{:.1},"lock_wait_pct":{:.2},"apply_pct":{:.1},"avg_read_bytes":{}}}"#,
            pct(feed[0], feed0[0]),
            pct(feed[1], feed0[1]),
            pct(feed[2], feed0[2]),
            (feed[4] - feed0[4]) / chunks,
        );
        let tmux_cpu = (pid_cpu_seconds(tmux_pid) - tmux_cpu0).max(0.0) / elapsed * 100.0;
        format!(
            concat!(
                r#"{{"scenario":"flood","flood_all":{},"panes":{},"flooded_panes":{},"seconds":{:.1},"#,
                r#""throughput_mb_s":{:.2},"cpu_percent":{:.1},"tmux_server_cpu_percent":{:.1},"thread_cpu_percent":{},"feed_thread":{},"paint_panes":{},"pauses":{},"resnapshots":{},"#,
                r#""keys":{},"frames":{},"rss_kb":{}}}"#
            ),
            self.args.flood_all,
            all.len(),
            flooded.len(),
            elapsed,
            bytes as f64 / 1_000_000.0 / elapsed,
            cpu,
            tmux_cpu,
            threads,
            feed_json,
            self.shared.paint_panes,
            self.shared.session.pauses.load(Ordering::Relaxed),
            self.shared.session.resnapshots.load(Ordering::Relaxed),
            keys_json(&samples, self.shared.probe.timeouts(), self.shared.probe.timeouts_after_pause()),
            frames_json(&frames),
            rss_json(&series),
        )
    }

    fn feed_stats(&self) -> [u64; 5] {
        let s = &self.shared.session.feed_stats;
        [
            s.read_wait_us.load(Ordering::Relaxed),
            s.lock_wait_us.load(Ordering::Relaxed),
            s.apply_us.load(Ordering::Relaxed),
            s.chunks.load(Ordering::Relaxed),
            s.chunk_bytes.load(Ordering::Relaxed),
        ]
    }

    async fn focus_pane(&self, cx: &mut AsyncApp, pane: &str) {
        let focused = self
            .workspace
            .read_with(cx, |w, _| w.focused_pane() == Some(pane));
        if focused {
            return;
        }
        self.shared.log.arm_measure(Measure::Pane {
            pane: pane.to_string(),
        });
        let t0 = Instant::now();
        self.shared.conn.select_pane(pane);
        let _ = self.wait_measure(cx, t0).await;
    }

    fn fed_bytes(&self, panes: &[String]) -> u64 {
        panes
            .iter()
            .map(|p| {
                self.shared
                    .session
                    .pane(p)
                    .bytes_fed
                    .load(Ordering::Relaxed)
            })
            .sum()
    }

    /// Cells that fit a pixel viewport, matching `Workspace::render`.
    fn cells_for(&self, cx: &mut AsyncApp, width: f32, height: f32) -> (u16, u16) {
        self.workspace
            .read_with(cx, |workspace, _| workspace.cells_for(width, height))
    }

    async fn resize(&self, cx: &mut AsyncApp) -> String {
        if self.args.fill_panes {
            let panes = self.panes().to_vec();
            self.fill_panes(cx, &panes).await;
        }
        self.flood_during_scenario(cx).await;
        let mut rng = jitter_seed();
        let (mut applied, mut quiesced) = (Vec::new(), Vec::new());
        let sizes = [(1280.0_f32, 800.0_f32), (1000.0, 640.0)];
        let iterations = self.args.samples.max(1);
        self.shared.log.take_frames();
        for i in 0..iterations {
            let (w, h) = sizes[(i + 1) % 2];
            let (cols, rows) = self.cells_for(cx, w, h);
            self.shared.log.arm_measure(Measure::Resize { cols, rows });
            let t0 = Instant::now();
            let _ = self.handle.update(cx, |_, window, _| {
                window.resize(size(px(w), px(h)));
            });
            let mut timed_out = false;
            let done_at = loop {
                if let Some(at) = self.shared.log.measure_done_at() {
                    break at;
                }
                if t0.elapsed() > Duration::from_secs(self.args.key_timeout_s) {
                    timed_out = true;
                    break Instant::now();
                }
                self.sleep(cx, Duration::from_millis(2)).await;
            };
            self.shared.log.disarm_measure();
            if timed_out {
                eprintln!("resize iteration {i} timed out");
                self.sleep(cx, Duration::from_secs(1)).await;
                continue;
            }
            applied.push(done_at.saturating_duration_since(t0).as_micros() as u64);
            self.sleep(cx, Duration::from_millis(400)).await;
            // Quiescent: the last frame that repainted content since t0.
            let frames = self.shared.log.take_frames();
            let epoch_frames: Vec<&FrameRec> = frames.iter().filter(|f| f.rows_reshaped > 0).collect();
            let last_change = epoch_frames.last().map(|f| f.at_us);
            let t0_us = self.shared.log.us_since_epoch(t0);
            quiesced.push(last_change.unwrap_or(t0_us).saturating_sub(t0_us));
            // Start the next iteration at a random phase of the 16 ms frame tick.
            self.sleep(cx, Duration::from_millis(lcg(&mut rng, 33))).await;
        }
        format!(
            r#"{{"scenario":"resize","panes":{},"filled":{},"flood_during":{},"iterations":{},"timeouts":{},"applied":{},"quiescent":{}}}"#,
            self.panes().len(),
            self.args.fill_panes,
            self.args.flood_during,
            iterations,
            iterations - applied.len(),
            dist_json(dist(&applied)),
            dist_json(dist(&quiesced)),
        )
    }

    async fn switch(&self, cx: &mut AsyncApp) -> String {
        self.flood_during_scenario(cx).await;
        let mut rng = jitter_seed();
        let (mut windows, mut panes) = (Vec::new(), Vec::new());
        let iterations = self.args.samples.max(1);
        let (first, second) = (self.setup.window.clone(), self.setup.other_window.clone());
        self.shared.log.take_frames();
        for i in 0..iterations {
            // Start on the other tab so every step really switches.
            let target = if i % 2 == 0 { &second } else { &first };
            self.shared
                .log
                .arm_measure(Measure::Window { window: target.clone() });
            let t0 = Instant::now();
            self.shared.conn.select_window(target);
            if let Some(us) = self.wait_measure(cx, t0).await {
                windows.push(us);
            }
            self.sleep(cx, Duration::from_millis(50 + lcg(&mut rng, 33))).await;
        }
        // The window loop ends on a target that depends on parity; make sure the
        // 8-pane window is showing before the pane switches.
        if self.active_window() != first {
            self.shared
                .log
                .arm_measure(Measure::Window { window: first.clone() });
            let t0 = Instant::now();
            self.shared.conn.select_window(&first);
            let _ = self.wait_measure(cx, t0).await;
        }
        self.sleep(cx, Duration::from_millis(200)).await;
        let targets = self.panes().to_vec();
        for i in 0..iterations {
            // Start one past the focused pane so every step really changes focus.
            let target = &targets[(i + 1) % targets.len()];
            self.shared
                .log
                .arm_measure(Measure::Pane { pane: target.clone() });
            let t0 = Instant::now();
            self.shared.conn.select_pane(target);
            if let Some(us) = self.wait_measure(cx, t0).await {
                panes.push(us);
            }
            self.sleep(cx, Duration::from_millis(50 + lcg(&mut rng, 33))).await;
        }
        let frames = self.shared.log.take_frames();
        format!(
            r#"{{"scenario":"switch","panes":{},"iterations":{},"tab_timeouts":{},"pane_timeouts":{},"tab_switch":{},"pane_switch":{},"frames":{}}}"#,
            self.panes().len(),
            iterations,
            iterations - windows.len(),
            iterations - panes.len(),
            dist_json(dist(&windows)),
            dist_json(dist(&panes)),
            frames_json(&frames),
        )
    }

    fn active_window(&self) -> String {
        self.shared
            .session
            .active_window
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default()
    }

    async fn wait_measure(&self, cx: &mut AsyncApp, t0: Instant) -> Option<u64> {
        let result = loop {
            if let Some(at) = self.shared.log.measure_done_at() {
                break Some(at.saturating_duration_since(t0).as_micros() as u64);
            }
            if t0.elapsed() > Duration::from_secs(self.args.key_timeout_s) {
                eprintln!("measure timed out");
                break None;
            }
            self.sleep(cx, Duration::from_millis(1)).await;
        };
        self.shared.log.disarm_measure();
        result
    }

    async fn idle(&self, cx: &mut AsyncApp) -> String {
        // Let prompts settle before measuring.
        self.sleep(cx, Duration::from_secs(6)).await;
        self.shared.log.take_frames();
        let rss = RssSampler::start();
        let started = Instant::now();
        let cpu0 = cpu_seconds();
        self.sleep(cx, Duration::from_secs(self.args.seconds)).await;
        let elapsed = started.elapsed().as_secs_f64();
        let cpu = (cpu_seconds() - cpu0) / elapsed * 100.0;
        let series = rss.finish();
        let frames = self.shared.log.take_frames();
        format!(
            r#"{{"scenario":"idle","panes":{},"seconds":{:.1},"cpu_percent":{:.2},"rss_kb":{},"frames":{}}}"#,
            self.panes().len(),
            elapsed,
            cpu,
            rss_json(&series),
            frames_json(&frames),
        )
    }

    async fn onepane(&self, cx: &mut AsyncApp) -> String {
        let pane = self.panes()[0].clone();
        let exe = std::env::current_exe().expect("current exe");
        let others = self.panes()[1..].to_vec();
        self.fill_panes(cx, &others).await;
        let command = format!("exec {} ticker", exe.display());
        self.tmux
            .run(&["send-keys", "-t", &pane, command.as_str(), "Enter"])
            .expect("start ticker");
        self.sleep(cx, Duration::from_secs(2)).await;
        self.shared.log.take_frames();
        let cpu0 = cpu_seconds();
        let started = Instant::now();
        self.sleep(cx, Duration::from_secs(self.args.seconds)).await;
        let elapsed = started.elapsed().as_secs_f64();
        let cpu = (cpu_seconds() - cpu0) / elapsed * 100.0;
        let frames = self.shared.log.take_frames();
        format!(
            r#"{{"scenario":"onepane","panes":{},"view_cache":{},"seconds":{:.1},"cpu_percent":{:.2},"frames":{}}}"#,
            self.panes().len(),
            self.shared.view_cache,
            elapsed,
            cpu,
            frames_json(&frames),
        )
    }

    /// S5 terminal half: drive the view's input handler directly with a CJK
    /// composition, capture frames with preedit showing, commit, and confirm the
    /// committed bytes came back through tmux into the grid.
    async fn ime(&self, cx: &mut AsyncApp) -> String {
        let pane_id = self.panes()[0].clone();
        let view = self
            .workspace
            .read_with(cx, |workspace, _| workspace.view_for(&pane_id))
            .expect("pane view");
        let _ = self.handle.update(cx, |_, window, cx| {
            view.update(cx, |v, cx| v.focus_handle().focus(window, cx));
        });
        self.sleep(cx, Duration::from_millis(200)).await;
        let out = self.args.out.clone();
        let frame = |suffix: &str| format!("{out}.{suffix}.png");

        // Compose: preedit "ni" then its candidate "你".
        let _ = self.handle.update(cx, |_, window, cx| {
            view.update(cx, |v, cx| {
                v.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
            });
        });
        self.sleep(cx, Duration::from_millis(300)).await;
        let after_mark = self.preedit(cx, &view);
        capture_png(&frame("preedit"));
        let bounds_before = self.bounds_for_preedit(cx, &view);

        let _ = self.handle.update(cx, |_, window, cx| {
            view.update(cx, |v, cx| {
                v.replace_and_mark_text_in_range(None, "你", Some(1..1), window, cx);
            });
        });
        self.sleep(cx, Duration::from_millis(300)).await;
        let after_update = self.preedit(cx, &view);
        capture_png(&frame("preedit-cjk"));

        // Commit: bytes must reach the pane and return via tmux's echo.
        let _ = self.handle.update(cx, |_, window, cx| {
            view.update(cx, |v, cx| {
                v.replace_text_in_range(None, "你", window, cx);
            });
        });
        self.sleep(cx, Duration::from_millis(500)).await;
        let after_commit = self.preedit(cx, &view);
        capture_png(&frame("commit"));
        let echoed = {
            let pane = self.shared.session.pane(&pane_id);
            let term = pane.term.lock();
            let cursor = term.grid().cursor.point;
            let mut found = false;
            for col in 0..term.columns() {
                let cell = &term.grid()[cursor.line][alacritty_terminal::index::Column(col)];
                if cell.c == '你' {
                    found = true;
                    break;
                }
            }
            found
        };
        format!(
            concat!(
                r#"{{"scenario":"ime","preedit_after_mark":"{}","preedit_after_update":"{}","#,
                r#""preedit_after_commit":"{}","commit_echoed_in_grid":{},"#,
                r#""caret_bounds":{},"frames":["{}","{}","{}"]}}"#
            ),
            after_mark,
            after_update,
            after_commit,
            echoed,
            bounds_before,
            frame("preedit"),
            frame("preedit-cjk"),
            frame("commit"),
        )
    }

    fn preedit(&self, cx: &mut AsyncApp, view: &Entity<crate::view::PaneView>) -> String {
        self.handle
            .update(cx, |_, _, cx| view.read(cx).preedit().to_string())
            .unwrap_or_default()
    }

    fn bounds_for_preedit(
        &self,
        cx: &mut AsyncApp,
        view: &Entity<crate::view::PaneView>,
    ) -> String {
        let bounds = self
            .handle
            .update(cx, |_, window, cx| {
                view.update(cx, |v, cx| {
                    let element_bounds = Bounds::new(point(px(0.), px(0.)), size(px(1.), px(1.)));
                    v.bounds_for_range(0..1, element_bounds, window, cx)
                })
            })
            .ok()
            .flatten();
        match bounds {
            Some(b) => format!(
                r#"{{"x":{},"y":{},"w":{},"h":{}}}"#,
                f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)
            ),
            None => "null".to_string(),
        }
    }

    /// Ligature check on the real text system: shape `->`, `!=` and friends with
    /// default features versus the terminal's, and compare glyph ids.
    async fn ligature(&self, cx: &mut AsyncApp) -> String {
        let result = self.handle.update(cx, |_, window, _| {
            let features_default = FontFeatures::default();
            let features_terminal = crate::element::terminal_font_features();
            let system = window.text_system();
            let font_of = |features: FontFeatures| Font {
                family: spike_shared::fonts::MONO.to_string().into(),
                features,
                fallbacks: None,
                weight: FontWeight::NORMAL,
                style: FontStyle::Normal,
            };
            let mut out = String::new();
            for text in ["->", "!=", "==", "=>", "<!--", "www"] {
                let glyphs_for = |features: FontFeatures| {
                    let font = font_of(features);
                    let run = TextRun {
                        len: text.len(),
                        font,
                        color: rgb(0xffffff).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let line = system.shape_line(text.into(), px(13.), &[run], None);
                    let glyphs: Vec<String> = line
                        .runs
                        .iter()
                        .flat_map(|r| r.glyphs.iter())
                        .map(|g| format!("{}", g.id.0))
                        .collect();
                    glyphs.join(",")
                };
                let default_glyphs = glyphs_for(features_default.clone());
                let terminal_glyphs = glyphs_for(features_terminal.clone());
                let _ = write!(
                    out,
                    r#"{{"text":"{text}","default":"{default_glyphs}","terminal":"{terminal_glyphs}","ligated_by_default":{}}},"#,
                    default_glyphs != terminal_glyphs
                );
            }
            out.pop();
            format!(r#"{{"scenario":"ligature","checks":[{out}]}}"#)
        });
        result.unwrap_or_else(|_| r#"{"error":"window gone"}"#.to_string())
    }
}

/// Capture the private display. Uses the same `import` binary as `spike_capture`.
fn capture_png(path: &str) {
    // A root capture of the owner's real display must never land in results/.
    if std::env::var_os("SPIKE_USE_CURRENT_DISPLAY").is_some() {
        eprintln!("capture skipped on the caller's display: {path}");
        return;
    }
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let captured = std::process::Command::new("import")
        .args(["-display", &display, "-window", "root", path])
        .status()
        .is_ok_and(|status| status.success());
    eprintln!("{} {path}", if captured { "captured" } else { "capture FAILED" });
}

