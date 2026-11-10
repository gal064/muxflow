//! The real input path: a private tmux server, one `tmux -C` client for output
//! and one for input (`send-keys -H`), mirroring how the host talks to tmux
//! (`apps/host/src/service/terminal/*`), minus the daemon and the wire protocol.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, mpsc};
use std::task::Waker;

use tmux_control::{TmuxSnapshot, discover_with_socket_name, parse_layout};

use crate::feed::{Feed, Session};
use crate::probe::Probe;

pub const SESSION: &str = "muxflow-spike";
const SOCKET_PREFIX: &str = "muxflow-qa-";

/// A private tmux server. Refuses any socket that is not `muxflow-qa-*`, and
/// requires `TMUX_TMPDIR`, so a mistake cannot reach the owner's tmux.
pub struct Tmux {
    socket: String,
}

impl Tmux {
    pub fn new(socket: &str) -> Self {
        assert!(
            socket.starts_with(SOCKET_PREFIX),
            "refusing tmux socket {socket:?}: must start with {SOCKET_PREFIX}"
        );
        assert!(
            std::env::var_os("TMUX_TMPDIR").is_some(),
            "TMUX_TMPDIR must be set to a private directory"
        );
        Self {
            socket: socket.to_string(),
        }
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command.args(["-L", &self.socket, "-f", "/dev/null"]);
        command.env_remove("TMUX");
        command
    }

    pub fn run<S: AsRef<std::ffi::OsStr>>(&self, args: &[S]) -> Result<String, String> {
        let output = self
            .command()
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn snapshot(&self) -> TmuxSnapshot {
        discover_with_socket_name(&self.socket).expect("tmux discovery failed")
    }

    pub fn kill_server(&self) {
        let _ = self
            .command()
            .args(["kill-server"])
            .stderr(Stdio::null())
            .status();
    }

    /// Create the private session, with one pane running a real shell.
    pub fn start_session(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.kill_server();
        let (cols, rows) = (cols.to_string(), rows.to_string());
        let args = [
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-x",
            &cols,
            "-y",
            &rows,
            "-e",
            "HISTFILE=/dev/null",
            "-e",
            "LANG=C.UTF-8",
            "-e",
            "PS1=$ ",
            "bash --norc --noprofile",
        ];
        // A server still shutting down from the previous run answers
        // "server exited unexpectedly"; give it a moment.
        let mut attempt = 0;
        while let Err(error) = self.run(&args) {
            attempt += 1;
            if attempt == 100 {
                return Err(error);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        for setup in [
            ["set-option", "-g", "status", "off"],
            ["set-environment", "-g", "HISTFILE", "/dev/null"],
            ["set-environment", "-g", "LANG", "C.UTF-8"],
            ["set-option", "-g", "history-limit", "2000"],
        ] {
            self.run(&setup)?;
        }
        Ok(())
    }

    /// Spawn a control-mode client on the session with the given client flags.
    pub fn control_client(&self, flags: &str) -> Result<(Child, ChildStdin, ChildStdout), String> {
        let mut child = self
            .command()
            .args(["-C", "attach-session", "-f", flags, "-t", SESSION])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        Ok((child, stdin, stdout))
    }
}

/// `SPIKE_NO_PAUSE=1` drops `pause-after`, the control for telling output tmux
/// discarded during a pause from a rendering or ordering defect.
fn output_client_flags() -> &'static str {
    if std::env::var_os("SPIKE_NO_PAUSE").is_some() {
        ""
    } else {
        "pause-after=5"
    }
}

/// Wakes the UI task from the feed thread, coalescing bursts into one wake.
/// Lives here rather than in the view so the reader thread needs no GPUI types.
#[derive(Default)]
pub struct Signal {
    flag: std::sync::atomic::AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Signal {
    pub fn notify(&self) {
        self.flag.store(true, Ordering::Release);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }

    pub async fn wait(&self) {
        std::future::poll_fn(|cx| {
            if self.flag.swap(false, Ordering::AcqRel) {
                return std::task::Poll::Ready(());
            }
            *self.waker.lock().unwrap() = Some(cx.waker().clone());
            if self.flag.swap(false, Ordering::AcqRel) {
                return std::task::Poll::Ready(());
            }
            std::task::Poll::Pending
        })
        .await
    }
}

/// A live control-mode connection: output reader thread plus input writer.
pub struct LocalConnection {
    output: Arc<Mutex<ChildStdin>>,
    input: Sender<(String, Vec<u8>)>,
    _children: Vec<Child>,
}

impl LocalConnection {
    pub fn start(
        tmux: &Tmux,
        session: Arc<Session>,
        probe: Arc<Probe>,
        wake: Arc<Signal>,
    ) -> Result<Self, String> {
        // Flags mirror the host's attachment; `ignore-size` is omitted because
        // this client is the one that sizes the session.
        let (output_child, output_stdin, stdout) = tmux.control_client(output_client_flags())?;
        let (input_child, input_stdin, input_stdout) =
            tmux.control_client("no-output,ignore-size,no-detach-on-destroy")?;
        let output = Arc::new(Mutex::new(output_stdin));

        // The parser and the terminal state machine run here, off the main thread.
        {
            let output = output.clone();
            std::thread::Builder::new()
                .name("spike-feed".into())
                .spawn(move || reader_thread(stdout, session, probe, output, wake))
                .map_err(|e| e.to_string())?;
        }
        // Input client replies are `%begin`/`%end` only; drain so tmux never blocks.
        std::thread::Builder::new()
            .name("spike-input-drain".into())
            .spawn(move || drain(input_stdout))
            .map_err(|e| e.to_string())?;

        let (input_tx, input_rx) = mpsc::channel::<(String, Vec<u8>)>();
        std::thread::Builder::new()
            .name("spike-input-writer".into())
            .spawn(move || input_writer(input_rx, input_stdin))
            .map_err(|e| e.to_string())?;

        Ok(Self {
            output,
            input: input_tx,
            _children: vec![output_child, input_child],
        })
    }

    /// Send bytes to a pane on the input client (`send-keys -H`, as the host does).
    pub fn send_input(&self, pane: &str, bytes: Vec<u8>) {
        let _ = self.input.send((pane.to_string(), bytes));
    }

    /// Write one command on the output control client.
    pub fn command(&self, line: &str) {
        let mut stdin = self.output.lock().unwrap();
        let _ = writeln!(stdin, "{line}");
        let _ = stdin.flush();
    }

    pub fn resize_client(&self, cols: u16, rows: u16) {
        self.command(&format!("refresh-client -C {cols},{rows}"));
    }

    pub fn select_window(&self, window: &str) {
        self.command(&format!("select-window -t {window}"));
    }

    pub fn select_pane(&self, pane: &str) {
        self.command(&format!("select-pane -t {pane}"));
    }
}

fn reader_thread(
    mut stdout: ChildStdout,
    session: Arc<Session>,
    probe: Arc<Probe>,
    output: Arc<Mutex<ChildStdin>>,
    wake: Arc<Signal>,
) {
    let mut feed = Feed::new(session.clone(), probe);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let waiting = std::time::Instant::now();
        let read = match stdout.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let stats = &session.feed_stats;
        stats
            .read_wait_us
            .fetch_add(waiting.elapsed().as_micros() as u64, Ordering::Relaxed);
        stats.chunks.fetch_add(1, Ordering::Relaxed);
        stats.chunk_bytes.fetch_add(read as u64, Ordering::Relaxed);
        let effects = feed.push(&buffer[..read]);
        if !effects.resume_panes.is_empty() {
            let mut stdin = output.lock().unwrap();
            for pane in effects.resume_panes {
                let _ = writeln!(stdin, "refresh-client -A '{pane}:continue'");
            }
            let _ = stdin.flush();
        }
        if effects.woke_ui {
            wake.notify();
        }
    }
    session.exited.store(true, Ordering::Release);
    wake.notify();
}

fn drain(mut stdout: ChildStdout) {
    let mut sink = [0u8; 8192];
    while matches!(stdout.read(&mut sink), Ok(n) if n > 0) {}
}

/// Same wire shape the host uses for a non-paste input request.
fn input_writer(rx: mpsc::Receiver<(String, Vec<u8>)>, mut stdin: ChildStdin) {
    while let Ok((pane, bytes)) = rx.recv() {
        let mut line = format!("send-keys -H -t {pane}");
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(line, " {byte:02x}");
        }
        line.push('\n');
        if stdin.write_all(line.as_bytes()).is_err() || stdin.flush().is_err() {
            break;
        }
    }
}

/// Build the requested number of panes in the session's first window and one
/// extra window (the tab-switch target), then publish the real layout.
pub fn build_panes(tmux: &Tmux, session: &Session, panes: usize) -> Result<PaneSetup, String> {
    let target = format!("{SESSION}:0");
    for _ in 1..panes {
        tmux.run(&["split-window", "-d", "-t", &target, "bash --norc --noprofile"])?;
        // Tile after every split: tiled splits always have room.
        tmux.run(&["select-layout", "-t", &target, "tiled"])?;
    }
    tmux.run(&[
        "new-window",
        "-d",
        "-t",
        &format!("{SESSION}:1"),
        "bash --norc --noprofile",
    ])?;
    let snapshot = tmux.snapshot();
    let active = snapshot
        .windows
        .iter()
        .find(|w| w.active)
        .or_else(|| snapshot.windows.first())
        .ok_or("no windows")?
        .id
        .clone();
    for window in &snapshot.windows {
        let tree = parse_layout(&window.layout).map_err(|e| e.to_string())?;
        let mut rects = Vec::new();
        crate::feed::layout_panes(&tree, &mut rects);
        session.apply_layout(&window.id, rects);
    }
    *session.active_window.lock().unwrap() = Some(active.clone());
    let main_panes: Vec<String> = snapshot
        .panes
        .iter()
        .filter(|p| p.window_id == active)
        .map(|p| p.id.clone())
        .collect();
    let other_window = snapshot
        .windows
        .iter()
        .find(|w| w.id != active)
        .ok_or("no second window")?
        .id
        .clone();
    for pane in &main_panes {
        session
            .active_pane
            .lock()
            .unwrap()
            .entry(active.clone())
            .or_insert_with(|| pane.clone());
    }
    // The first shell printed its prompt before the control client attached.
    tmux.run(&["send-keys", "-t", &format!("{SESSION}:0.0"), "C-l"])?;
    Ok(PaneSetup {
        window: active,
        other_window,
        panes: main_panes,
    })
}

pub struct PaneSetup {
    pub window: String,
    pub other_window: String,
    pub panes: Vec<String>,
}

/// S11 adds only a transport, retaining the existing grid and view.
pub enum Connection {
    Local(LocalConnection),
    Host(crate::host::Host),
}
impl Connection {
    pub fn start(
        tmux: &Tmux,
        session: Arc<Session>,
        probe: Arc<Probe>,
        wake: Arc<Signal>,
    ) -> Result<Self, String> {
        LocalConnection::start(tmux, session, probe, wake).map(Self::Local)
    }
    pub fn send_input(&self, pane: &str, bytes: Vec<u8>) {
        match self {
            Self::Local(c) => c.send_input(pane, bytes),
            Self::Host(c) => c.send_input(pane, bytes),
        }
    }
    pub fn resize_client(&self, cols: u16, rows: u16) {
        match self {
            Self::Local(c) => c.resize_client(cols, rows),
            Self::Host(c) => c.resize_client(cols, rows),
        }
    }
    pub fn select_window(&self, window: &str) {
        match self {
            Self::Local(c) => c.select_window(window),
            Self::Host(c) => c.select_window(window),
        }
    }
    pub fn select_pane(&self, pane: &str) {
        match self {
            Self::Local(c) => c.select_pane(pane),
            Self::Host(c) => c.select_pane(pane),
        }
    }
}
