use super::*;
use std::{
    fs::File,
    io::Cursor,
    os::fd::FromRawFd,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use tauri::ipc::{Channel, InvokeResponseBody};

use crate::connection::DeliveryWindow;

struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

impl Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct PipelinedReader {
    writes: Arc<Mutex<Vec<u8>>>,
    responses: Cursor<Vec<u8>>,
    checked: bool,
}

impl Read for PipelinedReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if !self.checked {
            let written = self.writes.lock().unwrap().clone();
            let mut requests = Cursor::new(written);
            let hello = read_frame_sync(&mut requests).unwrap().unwrap();
            let subscribe = read_frame_sync(&mut requests).unwrap().unwrap();
            assert_eq!(hello.request_id, 1);
            assert!(matches!(hello.payload, Some(Payload::ClientHello(_))));
            assert_eq!(subscribe.request_id, 2);
            assert!(matches!(subscribe.payload, Some(Payload::Request(_))));
            assert_eq!(requests.position(), requests.get_ref().len() as u64);
            self.checked = true;
        }
        self.responses.read(bytes)
    }
}

fn event(sequence: u64) -> v1::Envelope {
    envelope(0, sequence, Payload::Event(v1::HostEvent::default()))
}

#[test]
fn initial_snapshot_barrier_supersedes_events_already_in_its_sequence() {
    assert!(!event_follows_snapshot_barrier(&event(40), 41));
    assert!(!event_follows_snapshot_barrier(&event(41), 41));
    assert!(event_follows_snapshot_barrier(&event(42), 41));

    let response = envelope(9, 0, Payload::Response(v1::Response::default()));
    assert!(!event_follows_snapshot_barrier(&response, 41));
}

#[test]
fn client_hello_and_subscribe_are_written_before_the_first_read() {
    let hello = envelope(
        1,
        0,
        Payload::ServerHello(v1::ServerHello {
            capabilities: HOST_CAPABILITIES,
            server_identity: "server-a".into(),
            ..Default::default()
        }),
    );
    let response = envelope(
        2,
        0,
        Payload::Response(v1::Response {
            ok: true,
            accepted_sequence: 4,
            snapshot: Some(v1::Snapshot {
                server_identity: "server-a".into(),
                generation: 3,
                ..Default::default()
            }),
            ..Default::default()
        }),
    );
    let mut responses = tmux_agent_protocol::encode_frame(&hello).unwrap();
    responses.extend(tmux_agent_protocol::encode_frame(&response).unwrap());
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut writer = RecordingWriter(Arc::clone(&writes));
    let mut reader = PipelinedReader {
        writes,
        responses: Cursor::new(responses),
        checked: false,
    };

    let (_, initial, admission, accepted_sequence) =
        handshake_and_snapshot(&mut writer, &mut reader, 9).unwrap();

    assert!(reader.checked);
    assert!(admission.is_ok());
    assert_eq!(accepted_sequence, 4);
    assert_eq!(initial.unwrap().accepted_sequence, 4);
}

#[test]
fn incompatible_handshake_drains_pipelined_subscribe_and_keeps_its_watermark() {
    let hello = envelope(
        1,
        0,
        Payload::ServerHello(v1::ServerHello {
            read_only: true,
            server_identity: "server-old".into(),
            ..Default::default()
        }),
    );
    let response = envelope(
        2,
        0,
        Payload::Response(v1::Response {
            ok: true,
            accepted_sequence: 17,
            snapshot: Some(v1::Snapshot {
                server_identity: "server-old".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
    );
    let later_event = event(18);
    let mut bytes = tmux_agent_protocol::encode_frame(&hello).unwrap();
    bytes.extend(tmux_agent_protocol::encode_frame(&response).unwrap());
    bytes.extend(tmux_agent_protocol::encode_frame(&later_event).unwrap());
    let mut reader = Cursor::new(bytes);
    let mut writer = Vec::new();

    let (_, initial, admission, accepted_sequence) =
        handshake_and_snapshot(&mut writer, &mut reader, 9).unwrap();

    // The refusal doubles as the reason the read-only path reports, so a
    // quarantined handshake can never be silent.
    assert!(
        !admission.unwrap_err().to_string().is_empty(),
        "a quarantined handshake must carry its reason"
    );
    assert!(initial.is_none());
    assert_eq!(accepted_sequence, 17);
    assert_eq!(read_frame_sync(&mut reader).unwrap(), Some(later_event));
}

#[test]
fn short_post_handshake_failures_continue_the_same_backoff_run() {
    assert_eq!(next_reconnect_attempt(0, None), 1);
    assert_eq!(
        next_reconnect_attempt(1, Some(Duration::from_millis(50))),
        2
    );
    assert_eq!(
        next_reconnect_attempt(2, Some(STABLE_CONNECTION_RESET - Duration::from_millis(1))),
        3
    );
}

#[test]
fn stable_connection_resets_the_backoff_run() {
    assert_eq!(next_reconnect_attempt(9, Some(STABLE_CONNECTION_RESET)), 1);
}

const TEST_EPOCH: u64 = 7;

fn os_pipe() -> (File, File) {
    let mut fds = [0; 2];
    // SAFETY: pipe initializes both descriptors on success.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: each descriptor is newly owned by this test.
    unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
}

/// Drives the real read loop over two OS pipes.
///
/// `host` is the host's stdout as `read_protocol_stream` sees it, and
/// `host_requests` is everything the loop writes back through the client's own
/// control writer, so a resync request can be read and answered with the request
/// ID the loop actually chose.
struct StreamHarness {
    host: Option<File>,
    host_requests: File,
    frames: mpsc::Receiver<Vec<u8>>,
    client: Arc<TerminalClient>,
    run: Option<thread::JoinHandle<Result<(), String>>>,
}

impl StreamHarness {
    fn start(from_sequence: u64) -> Self {
        let (host_read, host_write) = os_pipe();
        let (request_read, request_write) = os_pipe();
        let client = Arc::new(TerminalClient::new());
        client
            .terminal_epoch
            .store(TEST_EPOCH, std::sync::atomic::Ordering::Release);
        *client.delivery_window.lock().unwrap() = Some(DeliveryWindow::new(TEST_EPOCH));
        *client.writer.lock().unwrap() =
            Some(ControlWriterHandle::start_with(request_write, "resync-test").unwrap());
        let (sender, frames) = mpsc::channel();
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(frame) = body {
                let _ = sender.send(frame);
            }
            Ok(())
        });
        let channel =
            TerminalEventChannel::new(Uuid::new_v4(), channel, Arc::clone(&client.delivery_window));
        let run_client = Arc::clone(&client);
        let run = thread::spawn(move || {
            read_protocol_stream(
                host_read,
                from_sequence,
                "server-a",
                &channel,
                &run_client,
                true,
            )
        });
        Self {
            host: Some(host_write),
            host_requests: request_read,
            frames,
            client,
            run: Some(run),
        }
    }

    fn send(&mut self, frame: &v1::Envelope) {
        write_frame_sync(self.host.as_mut().unwrap(), frame).unwrap();
    }

    /// The next frame the loop published, as (kind, label, sequence).
    fn frame(&self) -> (u8, String, u64) {
        let (kind, label, sequence, _) = self.frame_with_payload();
        (kind, label, sequence)
    }

    /// The next frame plus its opaque payload, for state-detail assertions.
    fn frame_with_payload(&self) -> (u8, String, u64, Vec<u8>) {
        let frame = self
            .frames
            .recv_timeout(Duration::from_secs(5))
            .expect("the bridge published no frame");
        let label_len = usize::from(u16::from_be_bytes([frame[1], frame[2]]));
        let label = String::from_utf8(frame[3..3 + label_len].to_vec()).unwrap();
        let sequence = u64::from_be_bytes(frame[3 + label_len..11 + label_len].try_into().unwrap());
        (frame[0], label, sequence, frame[11 + label_len..].to_vec())
    }

    fn next_control(&mut self) -> v1::Envelope {
        read_frame_sync(&mut self.host_requests).unwrap().unwrap()
    }

    /// The next control frame that is a request, skipping delivery
    /// acknowledgements the loop emitted along the way.
    fn next_request(&mut self) -> v1::Envelope {
        loop {
            let frame = self.next_control();
            if matches!(frame.payload, Some(Payload::Request(_))) {
                return frame;
            }
        }
    }

    /// Ends the host stream and returns how the run finished.
    fn finish(mut self) -> Result<(), String> {
        self.host.take();
        self.run.take().unwrap().join().unwrap()
    }
}

impl Drop for StreamHarness {
    fn drop(&mut self) {
        self.host.take();
        if let Some(run) = self.run.take() {
            let _ = run.join();
        }
    }
}

fn ordered_event(sequence: u64, name: &str) -> v1::Envelope {
    envelope(
        0,
        sequence,
        Payload::Event(v1::HostEvent {
            kind: v1::EventKind::TopologyDirty.into(),
            detail: name.into(),
            ..Default::default()
        }),
    )
}

fn charged_output(sequence: u64, bytes: &[u8]) -> v1::Envelope {
    envelope(
        0,
        sequence,
        Payload::Event(v1::HostEvent {
            kind: v1::EventKind::TerminalOutput.into(),
            terminal: Some(v1::TerminalBytes {
                pane_id: "%1".into(),
                data: bytes.to_vec(),
                generation: 1,
                ..Default::default()
            }),
            terminal_delivery_bytes: bytes.len() as u64,
            terminal_delivery_records: 1,
            ..Default::default()
        }),
    )
}

fn resync_required(sequence: u64, detail: &str) -> v1::Envelope {
    envelope(
        0,
        sequence,
        Payload::Event(v1::HostEvent {
            kind: v1::EventKind::ResyncRequired.into(),
            detail: detail.into(),
            ..Default::default()
        }),
    )
}

fn resync_response(request_id: u64, accepted_sequence: u64) -> v1::Envelope {
    envelope(
        request_id,
        0,
        Payload::Response(v1::Response {
            ok: true,
            accepted_sequence,
            snapshot: Some(v1::Snapshot {
                server_identity: "server-a".into(),
                generation: accepted_sequence,
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
}

/// A resync used to end the run and cost a full transport reconnect. It now
/// resumes the same connection at the barrier the host handed back, and does so
/// as many times as the host makes it necessary.
#[test]
fn a_resync_barrier_resumes_the_same_run_instead_of_reconnecting() {
    let mut harness = StreamHarness::start(0);

    harness.send(&ordered_event(1, "one"));
    assert_eq!(harness.frame(), (3, "one".into(), 1));

    for (gap_at, accepted, resumed_at) in [(3_u64, 10_u64, 11_u64), (13, 20, 21)] {
        // The event that trips the gap is refused before it is forwarded, so
        // nothing of it reaches the renderer.
        harness.send(&charged_output(gap_at, b"lost!"));
        let (kind, label, sequence, detail) = harness.frame_with_payload();
        assert_eq!((kind, label, sequence), (6, "resyncing".into(), 0));
        assert_eq!(
            String::from_utf8(detail).unwrap(),
            format!("sequence gap: expected {}, received {gap_at}", gap_at - 1)
        );

        let request = harness.next_request();
        let Some(Payload::Request(resync)) = &request.payload else {
            panic!("the bridge did not request a resync: {request:?}");
        };
        assert_eq!(
            v1::Operation::try_from(resync.operation).unwrap(),
            v1::Operation::Resync
        );
        assert_eq!(resync.scope, "full");
        assert!(
            !harness
                .client
                .ready
                .load(std::sync::atomic::Ordering::Acquire)
        );

        // Everything between the gap and the barrier is superseded by it.
        harness.send(&charged_output(gap_at + 1, b"superseded"));

        harness.send(&resync_response(request.request_id, accepted));
        assert_eq!(harness.frame(), (7, "snapshot".into(), accepted));
        assert_eq!(harness.frame(), (6, "connected".into(), 0));
        assert!(
            harness
                .client
                .ready
                .load(std::sync::atomic::Ordering::Acquire)
        );

        // The host numbers the next ordered event `accepted_sequence + 1`, so
        // the run must have adopted the barrier as its watermark rather than
        // kept counting from before the gap.
        harness.send(&ordered_event(resumed_at, "resumed"));
        assert_eq!(harness.frame(), (3, "resumed".into(), resumed_at));
    }

    assert_eq!(harness.finish(), Err("host bridge closed".into()));
}

#[test]
fn a_host_requested_resync_preserves_its_exact_reason_for_the_journal() {
    let mut harness = StreamHarness::start(0);
    harness.send(&resync_required(1, "terminal event queue overflowed"));

    let (kind, label, sequence, detail) = harness.frame_with_payload();
    assert_eq!((kind, label, sequence), (6, "resyncing".into(), 0));
    assert_eq!(
        String::from_utf8(detail).unwrap(),
        "host requested resync: terminal event queue overflowed"
    );

    let request = harness.next_request();
    harness.send(&resync_response(request.request_id, 1));
    assert_eq!(harness.frame(), (7, "snapshot".into(), 1));
    assert_eq!(harness.frame(), (6, "connected".into(), 0));
    assert_eq!(harness.finish(), Err("host bridge closed".into()));
}

/// The host reserved delivery credit for every quarantined event and only
/// releases it against this client's cumulative acknowledgement. Resuming in
/// place means nothing else ever will.
#[test]
fn credit_for_quarantined_events_is_acknowledged_at_the_barrier() {
    let mut harness = StreamHarness::start(0);

    harness.send(&charged_output(2, b"12345"));
    assert_eq!(harness.frame(), (6, "resyncing".into(), 0));
    let request = harness.next_request();
    harness.send(&charged_output(3, b"1234567"));
    harness.send(&resync_response(request.request_id, 9));
    assert_eq!(harness.frame(), (7, "snapshot".into(), 9));

    let ack = harness.next_control();
    let Some(Payload::TerminalOutputAck(ack)) = ack.payload else {
        panic!("quarantined delivery credit was never returned to the host: {ack:?}");
    };
    assert_eq!(ack.connection_epoch, TEST_EPOCH);
    assert_eq!(ack.cumulative_bytes, 12);
    assert_eq!(ack.cumulative_records, 2);
}

/// The symptom the reader reports is the same whether the network dropped the
/// link or this side killed it; only the annotation separates them, and a
/// bridge nothing on this side tore down must not be blamed for a teardown.
#[test]
fn a_local_teardown_is_named_on_the_bridge_error_and_only_then() {
    assert_eq!(
        super::name_local_teardown(
            "frame I/O failed: failed to fill whole buffer".into(),
            Some("a control write missed its deadline"),
        ),
        "frame I/O failed: failed to fill whole buffer (torn down locally: a control write missed its deadline)"
    );
    assert_eq!(
        super::name_local_teardown("host bridge closed".into(), None),
        "host bridge closed"
    );
}
