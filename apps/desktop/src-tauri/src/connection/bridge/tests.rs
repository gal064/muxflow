use super::*;
use std::{
    io::Cursor,
    sync::{Arc, Mutex},
};

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

    let (_, initial, compatible, accepted_sequence) =
        handshake_and_snapshot(&mut writer, &mut reader, 9).unwrap();

    assert!(reader.checked);
    assert!(compatible);
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

    let (_, initial, writable, accepted_sequence) =
        handshake_and_snapshot(&mut writer, &mut reader, 9).unwrap();

    assert!(!writable);
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
