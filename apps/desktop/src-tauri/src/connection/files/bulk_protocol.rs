use std::{
    io::{BufReader, Read, Write},
    os::fd::AsRawFd,
    process::{ChildStdin, ChildStdout},
};

use tmux_agent_protocol::{
    FrameAccumulator, HELPER_VERSION, HOST_CAPABILITIES, encode_frame, envelope,
    v1::{self, envelope::Payload},
};

use super::scheduler::{BulkBinding, CancelState, DeadlineGuard};

pub(super) enum RequestFailure {
    Transport(String),
    Remote {
        code: String,
        message: String,
        publication_outcome: Option<tmux_agent_protocol::PublicationOutcome>,
        cleanup_failed: bool,
    },
    Cancelled,
}

impl std::fmt::Display for RequestFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => formatter.write_str(error),
            Self::Remote { code, message, .. } => write!(formatter, "{code}: {message}"),
            Self::Cancelled => formatter.write_str("bulk request cancelled"),
        }
    }
}

/// A typed request/response client for one authenticated bulk-helper session.
///
/// Keeping framing and control-session binding here ensures downloads and
/// editor I/O cannot accidentally diverge in handshake or response handling.
pub(super) struct BulkProtocolClient<'a> {
    stdin: &'a mut ChildStdin,
    reader: &'a mut BufReader<ChildStdout>,
    /// Borrowed, not owned: a decoder can be holding bytes read past the last
    /// response, and a bridge that outlives this client must keep them.
    decoder: &'a mut FrameAccumulator,
    /// The connection's request-id cursor.
    ///
    /// Ids belong to the *connection*, not to a job, and the response loop
    /// *skips* frames whose id it is not waiting for rather than failing on
    /// them — so an id reused by a later job on the same connection would be a
    /// stale response silently accepted as the answer to a new request. Every
    /// request therefore takes the next id from here and no caller chooses one,
    /// which is the only arrangement in which that cannot happen.
    next_id: &'a mut u64,
    /// The bridge's reusability flag, cleared here rather than by any job: only
    /// this type knows whether a request left the stream mid-frame.
    clean: &'a mut bool,
}

impl<'a> BulkProtocolClient<'a> {
    /// Performs the bulk handshake on a freshly spawned bridge.
    ///
    /// Separate from `resumed` because it happens once per *connection* rather
    /// than once per job: a pooled bridge has already made this exchange, and
    /// re-making it is one of the round trips pooling exists to stop paying.
    pub(super) fn handshake(
        stdin: &mut ChildStdin,
        reader: &mut BufReader<ChildStdout>,
        binding: &BulkBinding,
        cancelled: &dyn Fn() -> bool,
        deadline: &DeadlineGuard,
    ) -> Result<(), String> {
        set_nonblocking(reader.get_ref().as_raw_fd(), "response")?;
        set_nonblocking(stdin.as_raw_fd(), "request")?;
        let mut decoder = FrameAccumulator::default();
        let mut next_id = 2;
        let mut clean = true;
        let mut client =
            BulkProtocolClient::resumed(stdin, reader, &mut decoder, &mut next_id, &mut clean);
        client
            .write_envelope_cancellable(
                &envelope(
                    1,
                    0,
                    Payload::ClientHello(v1::ClientHello {
                        desktop_version: env!("CARGO_PKG_VERSION").into(),
                        requested_capabilities: HOST_CAPABILITIES,
                        expected_helper_version: HELPER_VERSION.into(),
                        bulk_connection: true,
                        expected_server_identity: binding.expected_server_identity.clone(),
                        connection_epoch: binding.connection_epoch,
                    }),
                ),
                None,
                Some(deadline),
            )
            .map_err(|error| error.to_string())?;
        let frame = loop {
            if cancelled() {
                return Err("bulk bridge handshake cancelled".into());
            }
            if let Some(frame) = client
                .decoder
                .next_frame()
                .map_err(|error| error.to_string())?
            {
                break frame;
            }
            let mut bytes = [0_u8; 64 * 1024];
            match client.reader.read(&mut bytes) {
                Ok(0) => return Err("bulk bridge closed during handshake".into()),
                Ok(count) => {
                    client
                        .decoder
                        .push(&bytes[..count])
                        .map_err(|error| error.to_string())?;
                    deadline.touch();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    wait_ready(client.reader.get_ref().as_raw_fd(), libc::POLLIN, 20)?;
                }
                Err(error) => return Err(error.to_string()),
            }
        };
        let Some(Payload::ServerHello(hello)) = frame.payload else {
            return Err("bulk bridge omitted ServerHello".into());
        };
        if hello.read_only {
            return Err(format!(
                "bulk bridge is read-only: {}",
                hello.incompatibility
            ));
        }
        if hello.server_identity != binding.expected_server_identity
            || hello.connection_epoch != binding.connection_epoch
        {
            return Err(
                "bulk bridge handshake did not match its control identity/epoch binding".into(),
            );
        }
        binding.validate()?;
        Ok(())
    }

    /// A client for one job on an already-handshaken bridge.
    pub(super) fn resumed(
        stdin: &'a mut ChildStdin,
        reader: &'a mut BufReader<ChildStdout>,
        decoder: &'a mut FrameAccumulator,
        next_id: &'a mut u64,
        clean: &'a mut bool,
    ) -> Self {
        Self {
            stdin,
            reader,
            decoder,
            next_id,
            clean,
        }
    }

    fn take_request_id(&mut self) -> u64 {
        let id = *self.next_id;
        *self.next_id = id.saturating_add(1);
        id
    }

    pub(super) fn request(&mut self, request: v1::Request) -> Result<v1::Response, String> {
        self.request_classified(request)
            .map_err(|error| error.to_string())
    }

    pub(super) fn request_classified(
        &mut self,
        request: v1::Request,
    ) -> Result<v1::Response, RequestFailure> {
        self.request_classified_inner(request, None, None, None)
    }

    pub(super) fn request_with_deadline(
        &mut self,
        request: v1::Request,
        deadline: &DeadlineGuard,
    ) -> Result<v1::Response, String> {
        self.request_classified_inner(request, None, Some(deadline), None)
            .map_err(|error| error.to_string())
    }

    pub(super) fn request_classified_with_deadline(
        &mut self,
        request: v1::Request,
        deadline: &DeadlineGuard,
    ) -> Result<v1::Response, RequestFailure> {
        self.request_classified_inner(request, None, Some(deadline), None)
    }

    pub(super) fn request_cancellable(
        &mut self,
        request: v1::Request,
        cancellation: &CancelState,
        deadline: &DeadlineGuard,
    ) -> Result<v1::Response, String> {
        self.request_classified_inner(request, Some(cancellation), Some(deadline), None)
            .map_err(|error| error.to_string())
    }

    pub(super) fn request_classified_cancellable(
        &mut self,
        request: v1::Request,
        cancellation: &CancelState,
        deadline: &DeadlineGuard,
    ) -> Result<v1::Response, RequestFailure> {
        self.request_classified_inner(request, Some(cancellation), Some(deadline), None)
    }

    /// Issues one request whose answer is a stream of body frames followed by
    /// exactly one terminal response.
    ///
    /// The body frames carry the request's own id, so this is one exchange on
    /// one bridge: the bridge is only handed back after the terminal response,
    /// and a body observer that refuses a frame poisons it exactly like any
    /// other partial exchange.
    pub(super) fn stream_cancellable(
        &mut self,
        request: v1::Request,
        cancellation: &CancelState,
        deadline: &DeadlineGuard,
        on_frame: &mut dyn FnMut(v1::FileStreamFrame) -> Result<(), String>,
    ) -> Result<v1::Response, RequestFailure> {
        self.request_classified_inner(request, Some(cancellation), Some(deadline), Some(on_frame))
    }

    /// Every request goes through here, so this is the one place that knows
    /// whether the stream is still where the next request expects it. A remote
    /// refusal is a complete response and leaves the bridge reusable; a
    /// transport failure or a cancellation leaves a partial write or an
    /// in-flight response behind, and the bridge must not be handed on.
    fn request_classified_inner(
        &mut self,
        request: v1::Request,
        cancellation: Option<&CancelState>,
        deadline: Option<&DeadlineGuard>,
        on_frame: Option<&mut dyn FnMut(v1::FileStreamFrame) -> Result<(), String>>,
    ) -> Result<v1::Response, RequestFailure> {
        let request_id = self.take_request_id();
        let outcome = self.request_framed(request_id, request, cancellation, deadline, on_frame);
        // A cancellation that read its terminal response left the stream exactly
        // where the next request expects it; only an abandoned exchange did not.
        if matches!(outcome, Err(RequestFailure::Transport(_))) {
            *self.clean = false;
        }
        outcome
    }

    fn request_framed(
        &mut self,
        request_id: u64,
        request: v1::Request,
        cancellation: Option<&CancelState>,
        deadline: Option<&DeadlineGuard>,
        mut on_frame: Option<&mut dyn FnMut(v1::FileStreamFrame) -> Result<(), String>>,
    ) -> Result<v1::Response, RequestFailure> {
        self.write_envelope_cancellable(
            &envelope(request_id, 0, Payload::Request(request)),
            cancellation,
            deadline,
        )?;
        let mut cancel_sent = false;
        loop {
            if !cancel_sent && cancellation.is_some_and(CancelState::is_cancelled) {
                let cancel = encode_frame(&envelope(
                    0,
                    0,
                    Payload::Cancel(v1::Cancel {
                        target_request_id: request_id,
                    }),
                ))
                .map_err(|error| RequestFailure::Transport(error.to_string()))?;
                let _ = self.stdin.write(&cancel);
                let _ = self.stdin.flush();
                cancel_sent = true;
                // Deliberately not returning here. The host answers every
                // request with exactly one terminal response, cancelled or not,
                // and leaving it unread would abandon the bridge mid-exchange —
                // so every superseded preview would cost a fresh SSH child and
                // handshake on the next open. The loop below reads through to
                // that response, then reports the cancellation.
            }
            if let Some(frame) = self
                .decoder
                .next_frame()
                .map_err(|error| RequestFailure::Transport(error.to_string()))?
            {
                if frame.request_id != request_id {
                    continue;
                }
                let response = match frame.payload {
                    Some(Payload::FileStream(stream)) => {
                        let Some(observer) = on_frame.as_deref_mut() else {
                            return Err(RequestFailure::Transport(
                                "bulk bridge streamed a body for a request that asked for none"
                                    .into(),
                            ));
                        };
                        observer(stream).map_err(RequestFailure::Transport)?;
                        continue;
                    }
                    Some(Payload::Response(response)) => response,
                    _ => continue,
                };
                if cancel_sent {
                    // The exchange is complete, so the bridge is reusable; the
                    // caller still learns it was cancelled.
                    return Err(RequestFailure::Cancelled);
                }
                return if response.ok {
                    Ok(response)
                } else {
                    Err(RequestFailure::Remote {
                        code: response.error_code,
                        message: response.display_message,
                        publication_outcome: match v1::PublicationOutcome::try_from(
                            response.publication_outcome,
                        )
                        .unwrap_or_default()
                        {
                            v1::PublicationOutcome::NotPublished => {
                                Some(tmux_agent_protocol::PublicationOutcome::NotPublished)
                            }
                            v1::PublicationOutcome::Published => {
                                Some(tmux_agent_protocol::PublicationOutcome::Published)
                            }
                            v1::PublicationOutcome::Unknown => {
                                Some(tmux_agent_protocol::PublicationOutcome::Unknown)
                            }
                            v1::PublicationOutcome::Unspecified => None,
                        },
                        cleanup_failed: response.cleanup_failed,
                    })
                };
            }
            let mut bytes = [0_u8; 64 * 1024];
            let count = match self.reader.read(&mut bytes) {
                Ok(0) => return Err(RequestFailure::Transport("bulk bridge disconnected".into())),
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    wait_ready(self.reader.get_ref().as_raw_fd(), libc::POLLIN, 20)
                        .map_err(RequestFailure::Transport)?;
                    continue;
                }
                Err(error) => return Err(RequestFailure::Transport(error.to_string())),
            };
            self.decoder
                .push(&bytes[..count])
                .map_err(|error| RequestFailure::Transport(error.to_string()))?;
            if let Some(deadline) = deadline {
                deadline.touch();
            }
        }
    }

    fn write_envelope_cancellable(
        &mut self,
        value: &v1::Envelope,
        cancellation: Option<&CancelState>,
        deadline: Option<&DeadlineGuard>,
    ) -> Result<(), RequestFailure> {
        let bytes =
            encode_frame(value).map_err(|error| RequestFailure::Transport(error.to_string()))?;
        let mut offset = 0;
        while offset < bytes.len() {
            if cancellation.is_some_and(CancelState::is_cancelled) {
                // Abandoned mid-frame, so the peer's parser is left expecting
                // bytes that will never come. Unlike a cancellation that read
                // its terminal response, this one really does poison the lane.
                *self.clean = false;
                return Err(RequestFailure::Cancelled);
            }
            match self.stdin.write(&bytes[offset..]) {
                Ok(0) => {
                    return Err(RequestFailure::Transport(
                        "bulk request stream closed".into(),
                    ));
                }
                Ok(count) => {
                    offset += count;
                    if let Some(deadline) = deadline {
                        deadline.touch();
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    wait_ready(self.stdin.as_raw_fd(), libc::POLLOUT, 10)
                        .map_err(RequestFailure::Transport)?;
                }
                Err(error) => return Err(RequestFailure::Transport(error.to_string())),
            }
        }
        self.stdin
            .flush()
            .map_err(|error| RequestFailure::Transport(error.to_string()))
    }

    pub(super) fn cancel_download(&mut self, transfer_id: &str) -> Result<(), String> {
        self.request(v1::Request {
            operation: v1::Operation::CancelDownload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: transfer_id.into(),
                transfer_id: transfer_id.into(),
                ..Default::default()
            }),
            ..Default::default()
        })?;
        Ok(())
    }

    pub(super) fn cancel_write(
        &mut self,
        operation_id: &str,
        transfer_id: &str,
    ) -> Result<(), String> {
        self.request(v1::Request {
            operation: v1::Operation::CancelFileWrite.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: operation_id.into(),
                transfer_id: transfer_id.into(),
                ..Default::default()
            }),
            ..Default::default()
        })?;
        Ok(())
    }

    pub(super) fn cancel_terminal_upload(&mut self, transfer_id: &str) -> Result<String, String> {
        let response = self.request(v1::Request {
            operation: v1::Operation::CancelTerminalUpload.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: transfer_id.into(),
                transfer_id: transfer_id.into(),
                ..Default::default()
            }),
            ..Default::default()
        })?;
        Ok(response
            .file
            .and_then(|file| file.upload)
            .map(|upload| upload.cleanup_error)
            .unwrap_or_default())
    }
}

fn set_nonblocking(fd: i32, label: &str) -> Result<(), String> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(format!(
            "could not make bulk {label} stream cancellable: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn wait_ready(fd: i32, events: libc::c_short, timeout_ms: libc::c_int) -> Result<(), String> {
    let mut descriptor = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    // SAFETY: descriptor points to one initialized pollfd for the duration of
    // the call. The short timeout retains cancellation/deadline polling.
    let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::File,
        os::fd::{FromRawFd, OwnedFd},
    };

    /// A pipe, as the two halves a bridge's stdio is made of.
    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0; 2];
        // SAFETY: `pipe` writes two descriptors into an array of two.
        let created = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(created, 0, "pipe: {}", std::io::Error::last_os_error());
        // SAFETY: both descriptors are freshly created and owned by nothing else.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn answer(request_id: u64) -> Vec<u8> {
        encode_frame(&envelope(
            request_id,
            0,
            Payload::Response(v1::Response {
                ok: true,
                ..Default::default()
            }),
        ))
        .expect("an encodable response")
    }

    #[test]
    fn cancelled_handshake_never_blocks_on_a_silent_peer() {
        let (_request_read, request_write) = pipe();
        let (response_read, _response_write) = pipe();
        let mut stdin = ChildStdin::from(request_write);
        let mut reader = BufReader::new(ChildStdout::from(response_read));
        let client = std::sync::Arc::new(crate::connection::TerminalClient::new());
        client
            .ready
            .store(true, std::sync::atomic::Ordering::Release);
        client
            .terminal_epoch
            .store(7, std::sync::atomic::Ordering::Release);
        *client.server_identity.lock().unwrap() = "server-7".into();
        let binding = BulkBinding::capture(client, "server-7".into(), 7).unwrap();
        let cancellation = std::sync::Arc::new(CancelState::new());
        let deadline = cancellation.arm_inactivity_deadline();
        cancellation.cancel();
        let started = std::time::Instant::now();
        let cancelled = || cancellation.is_cancelled();
        let error =
            BulkProtocolClient::handshake(&mut stdin, &mut reader, &binding, &cancelled, &deadline)
                .unwrap_err();
        assert!(error.contains("cancel"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
    }

    /// Two jobs on one bridge, over a real pipe pair rather than a mock.
    ///
    /// The cursor lives on the *bridge* and each job builds a fresh client
    /// around it (`BulkLease::client`), so a client that started counting from
    /// its own zero would reuse ids the connection had already spent — and the
    /// response loop skips frames it is not waiting for rather than failing on
    /// them, so the reuse would surface as a stale response silently accepted as
    /// the answer to a new request, not as an error.
    #[test]
    fn request_ids_carry_across_the_jobs_that_share_one_connection() {
        let (request_read, request_write) = pipe();
        let (response_read, response_write) = pipe();
        // Both answers are queued before either question is asked: what is under
        // test is the numbering, and a pipe buffer holds far more than this.
        let mut answers = File::from(response_write);
        answers
            .write_all(&answer(2))
            .expect("queue the first answer");
        // A second answer to the *first* id, so a client that restarted its
        // numbering would find one waiting and fail this test on the cursor
        // rather than blocking on a response that never comes — which is the
        // shape the bug has in production: a stale response, silently accepted.
        // A client that does not restart skips this frame, as the loop must.
        answers
            .write_all(&answer(2))
            .expect("queue the stale answer");
        answers
            .write_all(&answer(3))
            .expect("queue the second answer");

        // Each half becomes the stdio type that owns the corresponding end of a
        // child's pipes, which is what the client under test takes.
        let mut stdin = ChildStdin::from(request_write);
        let mut reader = BufReader::new(ChildStdout::from(response_read));
        let mut decoder = FrameAccumulator::default();
        // What a freshly handshaken bridge starts at: 1 was the handshake's.
        let mut next_id = 2_u64;
        let mut clean = true;

        for expected in [2_u64, 3] {
            BulkProtocolClient::resumed(
                &mut stdin,
                &mut reader,
                &mut decoder,
                &mut next_id,
                &mut clean,
            )
            .request(v1::Request::default())
            .expect("the queued answer");
            assert_eq!(next_id, expected + 1, "the cursor advanced past {expected}");
        }
        assert!(clean, "two complete exchanges leave the bridge reusable");

        // And the ids reached the wire, rather than only being counted in here.
        let mut questions = File::from(request_read);
        let mut asked = Vec::new();
        let mut sent = FrameAccumulator::default();
        let mut bytes = [0_u8; 4096];
        while asked.len() < 2 {
            let count = questions.read(&mut bytes).expect("the requests written");
            assert_ne!(count, 0, "the request stream ended early");
            sent.push(&bytes[..count]).expect("decodable requests");
            while let Some(frame) = sent.next_frame().expect("decodable requests") {
                asked.push(frame.request_id);
            }
        }
        assert_eq!(asked, vec![2, 3]);
        assert!(asked[1] > asked[0], "ids never restart");
    }
}
