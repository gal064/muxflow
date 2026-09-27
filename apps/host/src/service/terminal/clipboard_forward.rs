//! Forwards tmux paste-buffer changes to the desktop clipboard.
//!
//! A terminal application that copies inside tmux (Codex's `load-buffer -w`,
//! Claude Code's `set-buffer`, a copy-mode yank) lands in a tmux paste buffer.
//! tmux then pushes OSC 52 only to the target client's tty — and a control-mode
//! client has none — so nothing of that copy ever reaches pane output. What
//! every control client does get is `%paste-buffer-changed <name>`; this reads
//! the named buffer back and hands it to the desktop as the existing
//! clipboard-write event, which the desktop gates behind its own setting.
//!
//! The read is a forked tmux client on its own thread: the control-stream
//! reader must never write tmux stdin, and one attachment's reader runs per
//! session, so the same notification arrives once per attached session. One
//! read is in flight process-wide; while it runs only the newest other change
//! is remembered, together with the connection that reported it.

use std::{
    process::Command,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use tmux_agent_protocol::v1;
use tmux_control::MAX_INPUT_REQUEST_BYTES;
use tokio::sync::mpsc;

use super::super::SequencerControl;
use super::input::HOST_INPUT_BUFFER_PREFIX;
use crate::service::snapshot::tmux_command;

/// One buffer to read and every connection whose readers reported it. The
/// daemon serves connections concurrently (a desktop and a phone, or the old
/// and new sides of a reconnect), each with its own event queue, so the senders
/// travel with the name rather than being captured once by the worker.
struct Job {
    name: String,
    senders: Vec<mpsc::Sender<SequencerControl>>,
}

impl Job {
    fn new(name: &str, sender: &mpsc::Sender<SequencerControl>) -> Self {
        Self {
            name: name.to_owned(),
            senders: vec![sender.clone()],
        }
    }

    /// One connection has a reader per attached session, all sharing one queue.
    fn add_connection(&mut self, sender: &mpsc::Sender<SequencerControl>) {
        if !self.senders.iter().any(|known| known.same_channel(sender)) {
            self.senders.push(sender.clone());
        }
    }
}

/// One buffer read in flight process-wide; while it runs only the newest other
/// change is kept, so a burst of changes costs one read per read duration.
struct Coalescer {
    in_flight: bool,
    pending: Option<Job>,
}

impl Coalescer {
    const fn new() -> Self {
        Self {
            in_flight: false,
            pending: None,
        }
    }

    /// Hands a job back when the caller must start a worker for it. While a
    /// read is in flight the change is queued behind it instead — even for the
    /// same name, since the running `show-buffer` may already have read the
    /// buffer before this change — and only the newest queued name is kept,
    /// with every connection that reported it.
    fn enqueue(&mut self, name: &str, sender: &mpsc::Sender<SequencerControl>) -> Option<Job> {
        if !self.in_flight {
            self.in_flight = true;
            return Some(Job::new(name, sender));
        }
        match &mut self.pending {
            Some(job) if job.name == name => job.add_connection(sender),
            _ => self.pending = Some(Job::new(name, sender)),
        }
        None
    }

    /// The next job the finished worker must run, or `None` when it may exit.
    fn finished(&mut self) -> Option<Job> {
        let next = self.pending.take();
        self.in_flight = next.is_some();
        next
    }
}

static COALESCER: Mutex<Coalescer> = Mutex::new(Coalescer::new());

/// Entry from the control-stream reader. Returns at once; the read happens on
/// its own thread through a forked tmux client.
pub(super) fn forward_paste_buffer(name: &str, sender: &mpsc::Sender<SequencerControl>) {
    if name.is_empty() || name.starts_with(HOST_INPUT_BUFFER_PREFIX) {
        return;
    }
    let Some(job) = lock(&COALESCER).enqueue(name, sender) else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("host-clipboard-forward".into())
        .spawn(move || {
            drain(
                &COALESCER,
                job,
                |name| tmux_command().ok().and_then(|tmux| read_buffer(tmux, name)),
                |text, senders| {
                    for sender in senders {
                        send_within(sender, clipboard_event(text.clone()), SEND_DEADLINE);
                    }
                },
            );
        });
    if spawned.is_err() {
        // No worker exists to drain the queue: clear it so the next change tries again.
        *lock(&COALESCER) = Coalescer::new();
    }
}

/// How long one connection may keep the shared worker waiting for queue space.
///
/// The worker waits rather than taking `emit_event`'s drop-when-full path:
/// under heavy pane output a connection's ordered queue is often full for a
/// moment, and a copy that never arrives is the bug this module fixes. The wait
/// is bounded because the worker serves every connection: a peer that stopped
/// reading but has not yet been torn down must not stall the others' copies.
const SEND_DEADLINE: Duration = Duration::from_secs(10);

/// Queues one event, waiting up to `deadline` for space. A closed queue means
/// that connection is gone; a queue still full at the deadline drops the event
/// and counts the overflow like `emit_event` does.
fn send_within(sender: &mpsc::Sender<SequencerControl>, event: v1::HostEvent, deadline: Duration) {
    let started = Instant::now();
    let mut message = SequencerControl::OrderedEvent(event);
    loop {
        match sender.try_send(message) {
            Ok(()) => return,
            Err(mpsc::error::TrySendError::Closed(_)) => return,
            Err(mpsc::error::TrySendError::Full(returned)) => {
                if started.elapsed() >= deadline {
                    crate::diagnostics::record_event_queue_overflow();
                    return;
                }
                message = returned;
                std::thread::sleep(SEND_POLL);
            }
        }
    }
}

const SEND_POLL: Duration = Duration::from_millis(10);

fn drain(
    coalescer: &Mutex<Coalescer>,
    mut job: Job,
    fetch: impl Fn(&str) -> Option<String>,
    emit: impl Fn(String, &[mpsc::Sender<SequencerControl>]),
) {
    loop {
        if let Some(text) = fetch(&job.name) {
            emit(text, &job.senders);
        }
        match lock(coalescer).finished() {
            Some(next) => job = next,
            None => return,
        }
    }
}

/// The buffer's exact bytes, or `None` for a missing, empty, oversized or
/// non-UTF-8 buffer. `show-buffer` from a forked client writes the raw
/// contents to stdout with nothing appended.
fn read_buffer(mut tmux: Command, name: &str) -> Option<String> {
    let output = tmux.args(["show-buffer", "-b", name]).output().ok()?;
    if !output.status.success()
        || output.stdout.is_empty()
        || output.stdout.len() > MAX_INPUT_REQUEST_BYTES
    {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn clipboard_event(text: String) -> v1::HostEvent {
    v1::HostEvent {
        kind: v1::EventKind::TerminalClipboardWrite.into(),
        scope: "terminal-clipboard".into(),
        detail: text,
        ..Default::default()
    }
}

fn lock(coalescer: &Mutex<Coalescer>) -> MutexGuard<'_, Coalescer> {
    coalescer.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Output},
        sync::{Mutex, atomic::AtomicU64, atomic::Ordering, mpsc as std_mpsc},
    };

    use super::*;

    /// A stand-in for tmux: the function appends `show-buffer -b <name>`, which
    /// the script receives as `$0 $1 $2`.
    fn fake_tmux(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    fn connection() -> mpsc::Sender<SequencerControl> {
        mpsc::channel(1).0
    }

    #[test]
    fn coalescer_runs_one_fetch_and_keeps_only_the_newest_pending_name() {
        let sender = connection();
        let mut coalescer = Coalescer::new();
        assert_eq!(
            coalescer.enqueue("a", &sender).map(|j| j.name).as_deref(),
            Some("a")
        );
        assert!(coalescer.enqueue("a", &sender).is_none());
        assert!(coalescer.enqueue("b", &sender).is_none());
        assert!(coalescer.enqueue("c", &sender).is_none());
        assert_eq!(coalescer.finished().map(|j| j.name).as_deref(), Some("c"));
        assert!(coalescer.finished().is_none());
        assert_eq!(
            coalescer.enqueue("d", &sender).map(|j| j.name).as_deref(),
            Some("d")
        );
    }

    #[test]
    fn a_change_to_the_in_flight_buffer_is_read_again() {
        let sender = connection();
        let mut coalescer = Coalescer::new();
        assert!(coalescer.enqueue("fixed", &sender).is_some());
        assert!(coalescer.enqueue("fixed", &sender).is_none());
        assert_eq!(
            coalescer.finished().map(|j| j.name).as_deref(),
            Some("fixed")
        );
        assert!(coalescer.finished().is_none());
    }

    #[test]
    fn every_connection_that_reported_a_queued_change_receives_it_once() {
        let desktop = connection();
        let phone = connection();
        let mut coalescer = Coalescer::new();
        assert!(coalescer.enqueue("a", &desktop).is_some());
        // Three session readers on the desktop connection and one on the phone
        // all report the same change while the first read is running.
        assert!(coalescer.enqueue("a", &desktop).is_none());
        assert!(coalescer.enqueue("a", &phone).is_none());
        assert!(coalescer.enqueue("a", &desktop).is_none());
        let next = coalescer.finished().expect("the queued change runs next");
        assert_eq!(next.name, "a");
        assert_eq!(next.senders.len(), 2);
        assert!(next.senders[0].same_channel(&desktop));
        assert!(next.senders[1].same_channel(&phone));
    }

    #[test]
    fn a_newer_change_replaces_the_queued_one_and_its_connections() {
        let older = connection();
        let newer = connection();
        let mut coalescer = Coalescer::new();
        assert!(coalescer.enqueue("a", &older).is_some());
        assert!(coalescer.enqueue("a", &older).is_none());
        assert!(coalescer.enqueue("b", &newer).is_none());
        let next = coalescer.finished().expect("the queued change runs next");
        assert_eq!(next.name, "b");
        assert_eq!(next.senders.len(), 1);
        assert!(next.senders[0].same_channel(&newer));
    }

    #[test]
    fn drain_fetches_changes_that_arrived_while_a_fetch_was_running() {
        let sender = connection();
        let coalescer = Mutex::new(Coalescer::new());
        let first = lock(&coalescer)
            .enqueue("a", &sender)
            .expect("nothing in flight");
        let (release, released) = std_mpsc::channel::<()>();
        let released = Mutex::new(released);
        let fetched = Mutex::new(Vec::new());
        let emitted = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                drain(
                    &coalescer,
                    first,
                    |name| {
                        let is_first = fetched.lock().unwrap().is_empty();
                        fetched.lock().unwrap().push(name.to_owned());
                        if is_first {
                            released.lock().unwrap().recv().unwrap();
                        }
                        Some(format!("text:{name}"))
                    },
                    |text, senders| emitted.lock().unwrap().push((text, senders.len())),
                );
            });
            while fetched.lock().unwrap().is_empty() {
                std::thread::yield_now();
            }
            assert!(lock(&coalescer).enqueue("b", &sender).is_none());
            assert!(lock(&coalescer).enqueue("c", &sender).is_none());
            release.send(()).unwrap();
        });
        assert_eq!(
            *fetched.lock().unwrap(),
            vec!["a".to_owned(), "c".to_owned()]
        );
        assert_eq!(
            *emitted.lock().unwrap(),
            vec![("text:a".to_owned(), 1), ("text:c".to_owned(), 1)]
        );
        assert!(lock(&coalescer).enqueue("d", &sender).is_some());
    }

    #[test]
    fn read_buffer_returns_exact_bytes() {
        assert_eq!(
            read_buffer(fake_tmux("printf 'line1\\nline2\\n'"), "b").as_deref(),
            Some("line1\nline2\n")
        );
        assert_eq!(
            read_buffer(fake_tmux("printf 'x'"), "b").as_deref(),
            Some("x")
        );
        assert_eq!(
            read_buffer(fake_tmux("printf '%s' \"$2\""), "named").as_deref(),
            Some("named")
        );
    }

    #[test]
    fn read_buffer_skips_empty_failed_oversized_and_non_utf8() {
        assert_eq!(read_buffer(fake_tmux("true"), "b"), None);
        assert_eq!(read_buffer(fake_tmux("printf 'x'; exit 1"), "b"), None);
        assert_eq!(
            read_buffer(fake_tmux("head -c 1048577 /dev/zero"), "b"),
            None
        );
        assert_eq!(read_buffer(fake_tmux("printf '\\377'"), "b"), None);
    }

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

    struct TmuxFixture {
        socket: String,
    }

    impl TmuxFixture {
        fn new() -> Self {
            let fixture_id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let fixture = Self {
                socket: format!("ade-clipboard-test-{}-{fixture_id}", std::process::id()),
            };
            let _ = fixture.run(&["kill-server"]);
            fixture
        }

        fn command(&self) -> Command {
            let mut command = Command::new("tmux");
            command.args(["-L", &self.socket, "-f", "/dev/null"]);
            command
        }

        fn run(&self, arguments: &[&str]) -> Output {
            self.command()
                .args(arguments)
                .output()
                .expect("tmux must be installed for clipboard forwarding tests")
        }
    }

    impl Drop for TmuxFixture {
        fn drop(&mut self) {
            let _ = self.run(&["kill-server"]);
        }
    }

    #[test]
    fn read_buffer_reads_a_real_tmux_buffer_verbatim() {
        let fixture = TmuxFixture::new();
        assert!(fixture.run(&["new-session", "-d"]).status.success());
        assert!(
            fixture
                .run(&["set-buffer", "-b", "copy", "a\nb\n"])
                .status
                .success()
        );
        assert_eq!(
            read_buffer(fixture.command(), "copy").as_deref(),
            Some("a\nb\n")
        );
        assert_eq!(read_buffer(fixture.command(), "missing"), None);
    }

    #[test]
    fn send_within_delivers_when_the_queue_has_space() {
        let (sender, mut events) = mpsc::channel(1);
        send_within(
            &sender,
            clipboard_event("copied".into()),
            Duration::from_millis(50),
        );
        let SequencerControl::OrderedEvent(event) = events.try_recv().expect("queued") else {
            panic!("expected an ordered event");
        };
        assert_eq!(event.detail, "copied");
    }

    #[test]
    fn send_within_gives_up_on_a_queue_nobody_drains_and_on_a_closed_one() {
        let (sender, _events) = mpsc::channel(1);
        sender
            .try_send(SequencerControl::OrderedEvent(clipboard_event(
                "first".into(),
            )))
            .expect("fills the queue");
        let started = Instant::now();
        send_within(
            &sender,
            clipboard_event("second".into()),
            Duration::from_millis(50),
        );
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(started.elapsed() < Duration::from_secs(2));
        let (closed, events) = mpsc::channel(1);
        drop(events);
        let started = Instant::now();
        send_within(
            &closed,
            clipboard_event("gone".into()),
            Duration::from_secs(10),
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn clipboard_event_targets_the_desktop_clipboard_scope() {
        let event = clipboard_event("copied".into());
        assert_eq!(event.kind, v1::EventKind::TerminalClipboardWrite as i32);
        assert_eq!(event.scope, "terminal-clipboard");
        assert_eq!(event.detail, "copied");
    }

    #[test]
    fn host_owned_and_empty_buffers_are_not_forwarded() {
        let (sender, mut events) = mpsc::channel(4);
        forward_paste_buffer("ade-input-abc", &sender);
        forward_paste_buffer("", &sender);
        assert!(events.try_recv().is_err());
        assert!(!lock(&COALESCER).in_flight);
    }
}
