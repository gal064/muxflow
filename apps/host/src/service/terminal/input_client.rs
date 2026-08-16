//! One output-independent tmux command lane for terminal input.
//!
//! A control client producing pane output can stop reading stdin while its
//! stdout is backpressured. Input and its authoritative `%end`/`%error` fence
//! therefore cannot share that client. This sidecar requests `no-output`, owns
//! one global FIFO across every mounted pane, and keeps at most one input
//! command in flight.

use std::{
    io::{BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use tmux_agent_protocol::v1;
use tmux_control::{CommandTag, ControlParser, ControlRecord, MAX_INPUT_REQUEST_BYTES};
use tokio::sync::mpsc;

use super::{
    correlation::{MarkerBlock, classify_marker_block, error_reason, wants_error_line},
    input::{InputDispatch, run_input_dispatch},
    validate_tmux_id,
};
use crate::service::{SequencerControl, emit_event, snapshot::tmux_command};

const INPUT_FENCE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
enum CommandBlock {
    #[default]
    None,
    Unknown {
        tag: CommandTag,
        lines: Vec<Vec<u8>>,
    },
    Input {
        tag: CommandTag,
        input_id: u64,
        lines: Vec<Vec<u8>>,
    },
}

pub(super) struct PersistentInputClient {
    child: Arc<Mutex<Child>>,
    input_tx: std_mpsc::SyncSender<InputDispatch>,
    failed: Arc<AtomicBool>,
    next_input_id: u64,
}

impl PersistentInputClient {
    pub(super) fn start(
        session_id: &str,
        event_tx: mpsc::Sender<SequencerControl>,
        overflowed: Arc<AtomicBool>,
    ) -> anyhow::Result<Self> {
        Self::start_with_command(session_id, event_tx, overflowed, tmux_command())
    }

    fn start_with_command(
        session_id: &str,
        event_tx: mpsc::Sender<SequencerControl>,
        overflowed: Arc<AtomicBool>,
        mut command: Command,
    ) -> anyhow::Result<Self> {
        validate_tmux_id(session_id, '$')?;
        let mut child = command
            .args([
                "-C",
                "attach-session",
                "-f",
                "no-output,ignore-size,no-detach-on-destroy",
                "-t",
                session_id,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start persistent tmux input client")?;
        let stdout = child
            .stdout
            .take()
            .context("persistent tmux input stdout unavailable")?;
        let stdin = child
            .stdin
            .take()
            .context("persistent tmux input stdin unavailable")?;
        let stdin = Arc::new(Mutex::new(stdin));
        let child = Arc::new(Mutex::new(child));
        let failed = Arc::new(AtomicBool::new(false));
        let (completion_tx, completion_rx) = std_mpsc::channel();
        let reader_failed = Arc::clone(&failed);
        std::thread::Builder::new()
            .name("host-tmux-input-reader".into())
            .spawn(move || read_input_stream(stdout, completion_tx, reader_failed))?;

        let (input_tx, input_rx) = std_mpsc::sync_channel(super::super::TERMINAL_INPUT_QUEUE);
        let dispatch_failed = Arc::clone(&failed);
        std::thread::Builder::new()
            .name("host-tmux-input-dispatch".into())
            .spawn(move || {
                run_input_dispatch(
                    input_rx,
                    stdin,
                    completion_rx,
                    dispatch_failed,
                    |pane_id, error| {
                        emit_event(
                            &event_tx,
                            &overflowed,
                            v1::HostEvent {
                                kind: v1::EventKind::TerminalResnapshotRequired.into(),
                                scope: pane_id.to_owned(),
                                detail: format!(
                                    "terminal input outcome was not successful: {error}"
                                ),
                                ..Default::default()
                            },
                        );
                    },
                )
            })?;

        Ok(Self {
            child,
            input_tx,
            failed,
            next_input_id: 0,
        })
    }

    pub(super) fn is_ready(&self) -> bool {
        !self.failed.load(Ordering::Acquire)
    }

    pub(super) fn send_input(&mut self, pane_id: &str, data: &[u8]) -> anyhow::Result<()> {
        validate_tmux_id(pane_id, '%')?;
        if !self.is_ready() {
            bail!("persistent terminal input client is unavailable");
        }
        if data.len() > MAX_INPUT_REQUEST_BYTES {
            bail!("terminal input request exceeds the 1 MiB atomic commit limit");
        }
        if data.is_empty() {
            return Ok(());
        }
        self.next_input_id = self
            .next_input_id
            .checked_add(1)
            .context("terminal input correlation sequence exhausted")?;
        self.input_tx
            .try_send(InputDispatch::Bytes {
                input_id: self.next_input_id,
                pane_id: pane_id.to_owned(),
                data: data.to_vec(),
            })
            .map_err(|error| match error {
                std_mpsc::TrySendError::Full(_) => {
                    crate::diagnostics::record_terminal_input_backpressure();
                    anyhow::anyhow!(
                        "terminal input queue is full; caller must retry instead of dropping bytes"
                    )
                }
                std_mpsc::TrySendError::Disconnected(_) => {
                    anyhow::anyhow!("persistent terminal input dispatcher is disconnected")
                }
            })
    }

    pub(super) fn fence(&self) -> anyhow::Result<()> {
        if !self.is_ready() {
            bail!("persistent terminal input client is unavailable");
        }
        let (sender, receiver) = std_mpsc::sync_channel(1);
        let deadline = Instant::now() + INPUT_FENCE_TIMEOUT;
        admit_fence(&self.input_tx, sender)?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow::anyhow!("terminal input fence timed out during admission"))?;
        match receiver.recv_timeout(remaining) {
            Ok(result) => result.map_err(anyhow::Error::msg),
            Err(_) => {
                self.failed.store(true, Ordering::Release);
                bail!("terminal input fence timed out")
            }
        }
    }

    pub(super) fn stop(&mut self) {
        self.failed.store(true, Ordering::Release);
        let _ = self.input_tx.try_send(InputDispatch::Stop);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn admit_fence(
    input_tx: &std_mpsc::SyncSender<InputDispatch>,
    completion: std_mpsc::SyncSender<Result<(), String>>,
) -> anyhow::Result<()> {
    input_tx
        .try_send(InputDispatch::Barrier(completion))
        .map_err(|error| match error {
            std_mpsc::TrySendError::Full(_) => anyhow::anyhow!(
                "terminal input fence could not enter the full sidecar queue; action refused"
            ),
            std_mpsc::TrySendError::Disconnected(_) => {
                anyhow::anyhow!("persistent terminal input dispatcher is disconnected")
            }
        })
}

impl Drop for PersistentInputClient {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_input_stream(
    stdout: impl Read,
    completion: std_mpsc::Sender<super::input::InputCompletion>,
    failed: Arc<AtomicBool>,
) {
    let mut reader = BufReader::new(stdout);
    let mut parser = ControlParser::default();
    let mut buffer = [0_u8; 16 * 1024];
    let mut state = InputStreamState::default();
    'reader: loop {
        let length = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(length) => length,
        };
        parser.push(&buffer[..length]);
        while let Some(record) = parser.next_record() {
            let record = match record {
                Ok(record) => record,
                Err(error) => {
                    state.abort(&completion, error.to_string());
                    failed.store(true, Ordering::Release);
                    break 'reader;
                }
            };
            if state.handle(record, &completion).is_err() {
                failed.store(true, Ordering::Release);
                break 'reader;
            }
        }
    }
    state.abort(
        &completion,
        "persistent tmux input stream ended before completion".into(),
    );
    failed.store(true, Ordering::Release);
}

#[derive(Default)]
struct InputStreamState {
    expected_input: Option<(u64, String)>,
    active: CommandBlock,
}

impl InputStreamState {
    fn handle(
        &mut self,
        record: ControlRecord,
        completion: &std_mpsc::Sender<super::input::InputCompletion>,
    ) -> Result<(), ()> {
        match record {
            ControlRecord::Begin { tag, .. } => {
                abort_input(
                    &mut self.active,
                    completion,
                    "overlapping tmux input command block".into(),
                );
                self.active = match self.expected_input.take() {
                    Some((input_id, _pane_id)) => CommandBlock::Input {
                        tag,
                        input_id,
                        lines: Vec::new(),
                    },
                    None => CommandBlock::Unknown {
                        tag,
                        lines: Vec::new(),
                    },
                };
            }
            ControlRecord::CommandOutput(line) => match &mut self.active {
                CommandBlock::Unknown { lines, .. } | CommandBlock::Input { lines, .. }
                    if wants_error_line(lines.len()) =>
                {
                    lines.push(line)
                }
                _ => {}
            },
            ControlRecord::End { tag, .. } => {
                match std::mem::replace(&mut self.active, CommandBlock::None) {
                    CommandBlock::Unknown { tag: active, lines } if active == tag => {
                        if let MarkerBlock::Input { input_id, pane_id } =
                            classify_marker_block(None, &lines)
                        {
                            self.expected_input = Some((input_id, pane_id));
                        }
                    }
                    CommandBlock::Input {
                        tag: active,
                        input_id,
                        ..
                    } if active == tag => {
                        let _ = completion.send((input_id, Ok(())));
                    }
                    CommandBlock::Input { input_id, .. } => {
                        let _ = completion
                            .send((input_id, Err("tmux input completion tag mismatched".into())));
                        return Err(());
                    }
                    _ => {}
                }
            }
            ControlRecord::Error { tag, arguments } => {
                match std::mem::replace(&mut self.active, CommandBlock::None) {
                    CommandBlock::Input {
                        tag: active,
                        input_id,
                        lines,
                        ..
                    } if active == tag => {
                        let _ = completion.send((input_id, Err(error_reason(&arguments, &lines))));
                    }
                    CommandBlock::Input { input_id, .. } => {
                        let _ = completion
                            .send((input_id, Err("tmux input error tag mismatched".into())));
                        return Err(());
                    }
                    _ => {}
                }
            }
            ControlRecord::Output { .. } => {
                self.abort(
                    completion,
                    "no-output tmux input client emitted pane output".into(),
                );
                return Err(());
            }
            ControlRecord::Exit { .. } => return Err(()),
            ControlRecord::Notification { .. } => {}
        }
        Ok(())
    }

    fn abort(
        &mut self,
        completion: &std_mpsc::Sender<super::input::InputCompletion>,
        reason: String,
    ) {
        abort_input(&mut self.active, completion, reason.clone());
        if let Some((input_id, _)) = self.expected_input.take() {
            let _ = completion.send((input_id, Err(reason)));
        }
    }
}

fn abort_input(
    active: &mut CommandBlock,
    completion: &std_mpsc::Sender<super::input::InputCompletion>,
    reason: String,
) {
    if let CommandBlock::Input { input_id, .. } = std::mem::replace(active, CommandBlock::None) {
        let _ = completion.send((input_id, Err(reason)));
    }
}

#[cfg(test)]
mod tests {
    use std::{process::Output, sync::atomic::AtomicU64, time::Instant};

    use super::*;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

    struct TmuxFixture {
        socket: String,
    }

    impl TmuxFixture {
        fn new() -> Self {
            let fixture_id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let fixture = Self {
                socket: format!("ade-input-client-test-{}-{fixture_id}", std::process::id()),
            };
            let _ = fixture.run(&["kill-server"]);
            fixture
        }

        fn run(&self, arguments: &[&str]) -> Output {
            Command::new("tmux")
                .args(["-L", &self.socket, "-f", "/dev/null"])
                .args(arguments)
                .output()
                .expect("tmux must be installed for persistent input lifecycle tests")
        }

        fn successful(&self, arguments: &[&str]) -> Output {
            let output = self.run(arguments);
            assert!(
                output.status.success(),
                "tmux {:?} failed: {}",
                arguments,
                String::from_utf8_lossy(&output.stderr)
            );
            output
        }

        fn id(&self, kind: &str, target: &str) -> String {
            let format = if kind == "session" {
                "#{session_id}"
            } else {
                "#{pane_id}"
            };
            String::from_utf8(
                self.successful(&["display-message", "-p", "-t", target, format])
                    .stdout,
            )
            .unwrap()
            .lines()
            .next()
            .expect("tmux fixture identity")
            .to_owned()
        }

        fn input_command(&self) -> Command {
            let mut command = Command::new("tmux");
            command.args(["-L", &self.socket, "-f", "/dev/null"]);
            command
        }

        fn output_client(&self, target: &str) -> Child {
            Command::new("tmux")
                .args([
                    "-L",
                    &self.socket,
                    "-f",
                    "/dev/null",
                    "-C",
                    "attach-session",
                    "-f",
                    "pause-after=5,ignore-size",
                    "-t",
                    target,
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }

        fn wait_until(&self, mut predicate: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !predicate() {
                assert!(
                    Instant::now() < deadline,
                    "tmux fixture condition timed out"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn capture_contains(&self, pane_id: &str, needle: &str) -> bool {
            let output = self.run(&["capture-pane", "-p", "-t", pane_id]);
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains(needle)
        }

        fn capture_occurrences(&self, pane_id: &str, needle: &str) -> usize {
            let output = self.run(&["capture-pane", "-p", "-t", pane_id]);
            if !output.status.success() {
                return 0;
            }
            String::from_utf8_lossy(&output.stdout)
                .matches(needle)
                .count()
        }

        fn send_and_observe(&self, client: &mut PersistentInputClient, pane_id: &str, token: &str) {
            client
                .send_input(pane_id, format!("printf '{token}\\n'\r").as_bytes())
                .unwrap();
            client.fence().unwrap();
            self.wait_until(|| self.capture_contains(pane_id, token));
        }
    }

    impl Drop for TmuxFixture {
        fn drop(&mut self) {
            let _ = self.run(&["kill-server"]);
        }
    }

    fn start_fixture_client(fixture: &TmuxFixture, session_id: &str) -> PersistentInputClient {
        let (events, _event_receiver) = mpsc::channel(32);
        PersistentInputClient::start_with_command(
            session_id,
            events,
            Arc::new(AtomicBool::new(false)),
            fixture.input_command(),
        )
        .unwrap()
    }

    fn tag(number: u64) -> CommandTag {
        CommandTag {
            timestamp: 1,
            number,
            flags: 0,
        }
    }

    fn begin(
        state: &mut InputStreamState,
        completion: &std_mpsc::Sender<super::super::input::InputCompletion>,
        number: u64,
    ) {
        state
            .handle(
                ControlRecord::Begin {
                    tag: tag(number),
                    arguments: String::new(),
                },
                completion,
            )
            .unwrap();
    }

    fn end(
        state: &mut InputStreamState,
        completion: &std_mpsc::Sender<super::super::input::InputCompletion>,
        number: u64,
    ) -> Result<(), ()> {
        state.handle(
            ControlRecord::End {
                tag: tag(number),
                arguments: String::new(),
            },
            completion,
        )
    }

    fn marker(
        state: &mut InputStreamState,
        completion: &std_mpsc::Sender<super::super::input::InputCompletion>,
        input_id: u64,
        pane: u64,
        number: u64,
    ) {
        begin(state, completion, number);
        state
            .handle(
                ControlRecord::CommandOutput(
                    format!("__ADE_INPUT__:{input_id}:{pane}").into_bytes(),
                ),
                completion,
            )
            .unwrap();
        end(state, completion, number).unwrap();
    }

    #[test]
    fn marker_and_end_complete_the_exact_input_id() {
        let (sender, receiver) = std_mpsc::channel();
        let mut state = InputStreamState::default();
        marker(&mut state, &sender, 41, 7, 1);
        begin(&mut state, &sender, 2);
        end(&mut state, &sender, 2).unwrap();
        assert_eq!(receiver.try_recv().unwrap(), (41, Ok(())));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn ordinary_tmux_error_is_attributed_but_does_not_poison_later_input() {
        let (sender, receiver) = std_mpsc::channel();
        let mut state = InputStreamState::default();
        marker(&mut state, &sender, 5, 1, 1);
        begin(&mut state, &sender, 2);
        state
            .handle(
                ControlRecord::CommandOutput(b"can't find pane: %1".to_vec()),
                &sender,
            )
            .unwrap();
        state
            .handle(
                ControlRecord::Error {
                    tag: tag(2),
                    arguments: "1 2 0".into(),
                },
                &sender,
            )
            .unwrap();
        marker(&mut state, &sender, 6, 2, 3);
        begin(&mut state, &sender, 4);
        end(&mut state, &sender, 4).unwrap();
        let first = receiver.try_recv().unwrap();
        assert_eq!(first.0, 5);
        assert!(first.1.unwrap_err().contains("can't find pane"));
        assert_eq!(receiver.try_recv().unwrap(), (6, Ok(())));
    }

    #[test]
    fn overlap_and_no_output_violation_abort_once_and_fail_closed() {
        let (sender, receiver) = std_mpsc::channel();
        let mut state = InputStreamState::default();
        marker(&mut state, &sender, 9, 1, 1);
        begin(&mut state, &sender, 2);
        begin(&mut state, &sender, 3);
        assert_eq!(receiver.try_recv().unwrap().0, 9);
        assert!(receiver.try_recv().is_err());
        assert!(
            state
                .handle(
                    ControlRecord::Output {
                        pane_id: "%1".into(),
                        data: b"forbidden".to_vec(),
                    },
                    &sender,
                )
                .is_err()
        );
    }

    #[test]
    fn full_sidecar_queue_refuses_fence_within_its_total_deadline() {
        let (input_tx, _input_rx) = std_mpsc::sync_channel(1);
        input_tx.try_send(InputDispatch::Stop).unwrap();
        let (completion, _receiver) = std_mpsc::sync_channel(1);
        let started = Instant::now();
        let error = admit_fence(&input_tx, completion).unwrap_err().to_string();
        assert!(error.contains("full sidecar queue"), "{error}");
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn one_sidecar_survives_its_target_session_and_restarts_after_all_sessions_end() {
        let fixture = TmuxFixture::new();
        fixture.successful(&["new-session", "-d", "-s", "one", "exec bash --norc"]);
        fixture.successful(&["new-session", "-d", "-s", "two", "exec bash --norc"]);
        let session_one = fixture.id("session", "one");
        let session_two = fixture.id("session", "two");
        let pane_one = fixture.id("pane", "one");
        let pane_two = fixture.id("pane", "two");
        let mut output_one = fixture.output_client(&session_one);
        let mut output_two = fixture.output_client(&session_two);
        let mut input = start_fixture_client(&fixture, &session_one);

        fixture.wait_until(|| {
            let clients = fixture.successful(&["list-clients", "-F", "#{client_flags}"]);
            let flags: Vec<_> = String::from_utf8_lossy(&clients.stdout)
                .lines()
                .map(str::to_owned)
                .collect();
            flags.len() == 3
                && flags
                    .iter()
                    .filter(|value| value.contains("no-output"))
                    .count()
                    == 1
        });
        fixture.send_and_observe(&mut input, &pane_one, "ADE_SIDE_ONE");
        fixture.send_and_observe(&mut input, &pane_two, "ADE_SIDE_TWO");

        fixture.successful(&["kill-session", "-t", &session_one]);
        fixture.send_and_observe(&mut input, &pane_two, "ADE_AFTER_TARGET_DESTROY");

        fixture.successful(&["kill-session", "-t", &session_two]);
        fixture.wait_until(|| !input.is_ready());
        assert!(input.fence().is_err());

        fixture.successful(&["new-session", "-d", "-s", "three", "exec bash --norc"]);
        let session_three = fixture.id("session", "three");
        let pane_three = fixture.id("pane", "three");
        let mut replacement = start_fixture_client(&fixture, &session_three);
        fixture.send_and_observe(&mut replacement, &pane_three, "ADE_AFTER_RECREATE");

        let _ = output_one.kill();
        let _ = output_one.wait();
        let _ = output_two.kill();
        let _ = output_two.wait();
    }

    #[test]
    fn sidecar_death_with_admitted_input_fails_the_fence_without_replay() {
        let fixture = TmuxFixture::new();
        fixture.successful(&["new-session", "-d", "-s", "one", "exec bash --norc"]);
        let session = fixture.id("session", "one");
        let pane = fixture.id("pane", "one");
        let mut input = start_fixture_client(&fixture, &session);
        let child_pid = input.child.lock().unwrap().id().to_string();
        assert!(
            Command::new("kill")
                .args(["-STOP", &child_pid])
                .status()
                .unwrap()
                .success()
        );
        input
            .send_input(&pane, b"printf 'ADE_MUST_NOT_REPLAY\\n'\r")
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            Command::new("kill")
                .args(["-KILL", &child_pid])
                .status()
                .unwrap()
                .success()
        );
        fixture.wait_until(|| !input.is_ready());
        assert!(
            input.fence().is_err(),
            "unknown input outcome must fail its fence"
        );
        // Killing the client races the tmux commit point by construction: the
        // command may be absent or may have landed once. The only forbidden
        // behavior is replaying that unknown outcome on the replacement.
        let occurrences_after_death = fixture.capture_occurrences(&pane, "ADE_MUST_NOT_REPLAY");

        let mut replacement = start_fixture_client(&fixture, &session);
        replacement.fence().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            fixture.capture_occurrences(&pane, "ADE_MUST_NOT_REPLAY"),
            occurrences_after_death,
            "a replacement client must never replay an input with unknown outcome"
        );
        fixture.send_and_observe(&mut replacement, &pane, "ADE_REPLACEMENT_ONLY");
    }
}
