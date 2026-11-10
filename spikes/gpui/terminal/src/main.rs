//! Terminal feasibility spike (T0.4): a GPUI element fed by Muxflow's real tmux
//! control-mode parser through `alacritty_terminal`. Disposable.
//!
//! `run` opens a window; `bench` runs one measurement scenario and exits.
//! Display, tmux socket and TMUX_TMPDIR come from the environment; `measure.sh`
//! sources `spikes/gpui/env.sh` before running this.

mod bench;
mod control;
mod element;
mod feed;
mod grid;
mod host;
mod probe;
mod stats;
mod view;
mod x11_pixels;

use std::sync::Arc;

use gpui_kit::*;

use control::{Connection, Signal, Tmux, build_panes};
use feed::Session;
use probe::Probe;
use view::{FrameLog, Shared, Workspace, spawn_ui_pump};

const USAGE: &str = "usage: spike-terminal run [--panes N] [--socket NAME]\n       spike-terminal host (private S11 fixture; see macOS harness README)\n       spike-terminal bench --scenario NAME --out PATH [--panes N] [--seconds S] [--samples N] [--gap-min-ms N] [--gap-max-ms N] [--flood-all] [--no-view-cache] [--no-paint-panes] [--socket NAME]\n       spike-terminal control-baseline --out PATH [--panes N] [--seconds S] [--with-term] [--input-client]\n       spike-terminal flood-writer | ticker\n";

struct Args {
    lines: Option<u64>,
    input_client: bool,
    paint_panes: bool,
    with_term: bool,
    command: String,
    socket: String,
    panes: usize,
    bench: bench::BenchArgs,
    view_cache: bool,
}

fn parse_args() -> Args {
    let mut iter = std::env::args().skip(1);
    let command = iter.next().unwrap_or_default();
    let mut args = Args {
        lines: None,
        input_client: false,
        paint_panes: true,
        with_term: false,
        command,
        socket: "muxflow-qa-t04".to_string(),
        panes: 8,
        bench: bench::BenchArgs {
            scenario: String::new(),
            panes: 8,
            seconds: 60,
            samples: 600,
            gap_min_ms: 20,
            gap_max_ms: 80,
            out: "bench.json".to_string(),
            flood_all: false,
            key_timeout_s: 3,
            fill_panes: false,
            flood_during: false,
        },
        view_cache: true,
    };
    let value = |flag: &str, iter: &mut std::iter::Skip<std::env::Args>| -> String {
        iter.next().unwrap_or_else(|| panic!("missing value for {flag}"))
    };
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--panes" => {
                args.panes = value(&flag, &mut iter).parse().expect("--panes");
                args.bench.panes = args.panes;
            }
            "--socket" => args.socket = value(&flag, &mut iter),
            "--scenario" => args.bench.scenario = value(&flag, &mut iter),
            "--out" => args.bench.out = value(&flag, &mut iter),
            "--seconds" => args.bench.seconds = value(&flag, &mut iter).parse().expect("--seconds"),
            "--samples" => args.bench.samples = value(&flag, &mut iter).parse().expect("--samples"),
            "--gap-min-ms" => args.bench.gap_min_ms = value(&flag, &mut iter).parse().expect("n"),
            "--gap-max-ms" => args.bench.gap_max_ms = value(&flag, &mut iter).parse().expect("n"),
            "--flood-all" => args.bench.flood_all = true,
            "--fill-panes" => args.bench.fill_panes = true,
            "--flood-during" => args.bench.flood_during = true,
            "--no-view-cache" => args.view_cache = false,
            "--with-term" => args.with_term = true,
            "--no-paint-panes" => args.paint_panes = false,
            "--input-client" => args.input_client = true,
            "--lines" => args.lines = Some(value(&flag, &mut iter).parse().expect("--lines")),
            "--key-timeout-s" => {
                args.bench.key_timeout_s = value(&flag, &mut iter).parse().expect("n");
            }
            other => panic!("unknown flag {other}\n{USAGE}"),
        }
    }
    args
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn prctl(option: i32, ...) -> i32;
}

/// `SPIKE_NO_THP=1` turns transparent huge pages off for this process, the
/// control for telling khugepaged's RSS steps from real allocation growth.
#[cfg(target_os = "linux")]
fn apply_thp_control() {
    const PR_SET_THP_DISABLE: i32 = 41;
    if std::env::var_os("SPIKE_NO_THP").is_some() {
        let result = unsafe { prctl(PR_SET_THP_DISABLE, 1u64, 0u64, 0u64, 0u64) };
        assert_eq!(result, 0, "prctl(PR_SET_THP_DISABLE) failed");
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    apply_thp_control();
    let args = parse_args();
    match args.command.as_str() {
        "flood-writer" => bench::run_flood_writer(args.lines),
        "ticker" => bench::run_ticker(),
        "run" | "bench" | "host" => start_app(args),
        "control-baseline" => control_baseline(&args),
        _ => {
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    }
}

/// Headless control: how fast tmux itself can deliver the flood to one control
/// client, with and without alacritty_terminal in the loop. No GPUI, no window.
/// Separates tmux's own ceiling from what the GPUI path adds.
fn control_baseline(args: &Args) {
    use std::io::Read;
    use tmux_control::{ControlDispatcher, ControlParser, DispatchEvent};
    use std::time::{Duration, Instant};
    use std::sync::atomic::Ordering;

    let tmux = Tmux::new(&args.socket);
    tmux.start_session(160, 44).expect("start tmux session");
    let session = Arc::new(Session::default());
    let (mut child, stdin, mut stdout) = tmux.control_client("pause-after=5").expect("control client");
    let setup = build_panes(&tmux, &session, args.panes).expect("build panes");
    // The host and the GUI path both hold a second, `no-output` control client
    // open for input. Keeping it attached here makes this directly comparable.
    // Drained as on the GUI path, so the two differ only in what they measure.
    let mut input_client = args.input_client.then(|| {
        let (child, stdin, mut out) = tmux
            .control_client("no-output,ignore-size,no-detach-on-destroy")
            .expect("input client");
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut out, &mut std::io::sink());
        });
        (child, stdin)
    });
    // Size the window so tmux lays the panes out as the GUI scenarios do.
    {
        use std::io::Write;
        let mut stdin_handle = stdin;
        let _ = writeln!(stdin_handle, "refresh-client -C 158,44");
        let exe = std::env::current_exe().expect("current exe");
        let command = format!("exec {} flood-writer", exe.display());
        for pane in &setup.panes {
            tmux.run(&["send-keys", "-t", pane, command.as_str(), "Enter"]).expect("flood");
        }
        std::thread::sleep(Duration::from_secs(3));
        let mut parser = ControlParser::default();
        let mut dispatcher = ControlDispatcher::default();
        let probe = Arc::new(Probe::default());
        let mut feed = args.with_term.then(|| feed::Feed::new(session.clone(), probe));
        let mut buffer = vec![0u8; 64 * 1024];
        let mut bytes: u64 = 0;
        let mut pauses: u64 = 0;
        let started = Instant::now();
        let seconds = args.bench.seconds as f64;
        while started.elapsed().as_secs_f64() < seconds {
            let read = stdout.read(&mut buffer).expect("control read");
            if read == 0 {
                break;
            }
            if let Some(feed) = feed.as_mut() {
                let effects = feed.push(&buffer[..read]);
                for pane in effects.resume_panes {
                    let _ = writeln!(stdin_handle, "refresh-client -A '{pane}:continue'");
                }
                pauses = session.pauses.load(Ordering::Relaxed);
                bytes = setup
                    .panes
                    .iter()
                    .map(|p| session.pane(p).bytes_fed.load(Ordering::Relaxed))
                    .sum();
            } else {
                parser.push(&buffer[..read]);
                while let Some(record) = parser.next_record() {
                    for event in dispatcher.handle(record) {
                        match event {
                            DispatchEvent::Output { data, .. } => bytes += data.len() as u64,
                            DispatchEvent::Notification { name, arguments } if name == "pause" => {
                                pauses += 1;
                                if let Some(pane) = arguments.split(' ').next() {
                                    let _ = writeln!(stdin_handle, "refresh-client -A '{pane}:continue'");
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        let elapsed = started.elapsed().as_secs_f64();
        let json = format!(
            r#"{{"result":{{"scenario":"control-baseline","with_term":{},"input_client":{},"panes":{},"throughput_mb_s":{:.2},"pauses":{},"seconds":{:.1}}}}}"#,
            args.with_term,
            args.input_client,
            setup.panes.len(),
            bytes as f64 / 1_000_000.0 / elapsed,
            pauses,
            elapsed
        );
        std::fs::write(&args.bench.out, json).expect("write results");
    }
    if let Some((client, _)) = input_client.as_mut() {
        let _ = client.kill();
    }
    let _ = child.kill();
    tmux.kill_server();
}

fn start_app(args: Args) {
    let bench_args = (args.command == "bench").then(|| args.bench.clone());
    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        spike_shared::fonts::register(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let tmux = Arc::new(Tmux::new(&args.socket));
        if args.command != "host" { tmux.start_session(160, 44).expect("start tmux session"); }
        let session = Arc::new(Session::default());
        let probe = Arc::new(Probe::default());
        let log = Arc::new(FrameLog::default());
        let wake = Arc::new(Signal::default());
        let conn = if args.command == "host" {
            Connection::Host(host::Host::start(session.clone(),probe.clone(),wake.clone()).expect("host bridge"))
        } else { Connection::start(&tmux, session.clone(), probe.clone(), wake.clone()).expect("start control clients") };
        let setup = if args.command == "host" { None } else { Some(build_panes(&tmux, &session, args.panes).expect("build panes")) };
        let shared = Arc::new(Shared {
            session: session.clone(),
            probe,
            log,
            conn,
            view_cache: args.view_cache,
            paint_panes: args.paint_panes,
        });

        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(800.)));
        let (handle, workspace) = {
            let shared = shared.clone();
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // GPUI throttles an inactive window to 30 fps by default.
                    // Xvfb has no window manager, so the window never receives
                    // focus and would stay throttled; a user's focused window
                    // is not. Measure the focused behaviour.
                    inactive_frame_interval: None,
                    ..Default::default()
                },
                cx,
                move |_window, cx| cx.new(|_cx| Workspace::new(shared)),
            )
            .expect("open window")
        };
        cx.activate(true);
        spawn_ui_pump(handle, workspace.clone(), shared.clone(), wake.clone(), cx);
        // Kick the first sync even if no output arrives yet.
        wake.notify();

        if let Some(bench_args) = bench_args.clone() {
            bench::start(cx, handle, workspace, shared, setup.unwrap(), bench_args, tmux.clone());
        }
        // Kill only this spike's private server, on every exit path.
        cx.on_app_quit({
            let tmux = tmux.clone();
            move |_| {
                if args.command != "host" { tmux.kill_server(); }
                std::future::ready(())
            }
        })
        .detach();
    });
}
