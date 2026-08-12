use std::{
    io::{BufReader, Read, Write},
    os::fd::AsRawFd,
    process::{ChildStdin, ChildStdout},
};

use tmux_agent_protocol::{
    FrameAccumulator, HELPER_VERSION, HOST_CAPABILITIES, encode_frame, envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
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
    decoder: FrameAccumulator,
}

impl<'a> BulkProtocolClient<'a> {
    pub(super) fn connect(
        stdin: &'a mut ChildStdin,
        reader: &'a mut BufReader<ChildStdout>,
        binding: &BulkBinding,
    ) -> Result<Self, String> {
        write_frame_sync(
            stdin,
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
        )
        .map_err(|error| error.to_string())?;
        let frame = read_frame_sync(reader)
            .map_err(|error| error.to_string())?
            .ok_or("bulk bridge closed during handshake")?;
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
        for (fd, label) in [
            (reader.get_ref().as_raw_fd(), "response"),
            (stdin.as_raw_fd(), "request"),
        ] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(format!(
                    "could not make bulk {label} stream cancellable: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        Ok(Self {
            stdin,
            reader,
            decoder: FrameAccumulator::default(),
        })
    }

    pub(super) fn request(
        &mut self,
        request_id: u64,
        request: v1::Request,
    ) -> Result<v1::Response, String> {
        self.request_classified(request_id, request)
            .map_err(|error| error.to_string())
    }

    pub(super) fn request_classified(
        &mut self,
        request_id: u64,
        request: v1::Request,
    ) -> Result<v1::Response, RequestFailure> {
        self.request_classified_inner(request_id, request, None, None)
    }

    pub(super) fn request_with_deadline(
        &mut self,
        request_id: u64,
        request: v1::Request,
        deadline: &DeadlineGuard,
    ) -> Result<v1::Response, String> {
        self.request_classified_inner(request_id, request, None, Some(deadline))
            .map_err(|error| error.to_string())
    }

    pub(super) fn request_classified_with_deadline(
        &mut self,
        request_id: u64,
        request: v1::Request,
        deadline: &DeadlineGuard,
    ) -> Result<v1::Response, RequestFailure> {
        self.request_classified_inner(request_id, request, None, Some(deadline))
    }

    pub(super) fn request_cancellable(
        &mut self,
        request_id: u64,
        request: v1::Request,
        cancellation: &CancelState,
        deadline: &DeadlineGuard,
    ) -> Result<v1::Response, String> {
        self.request_classified_inner(request_id, request, Some(cancellation), Some(deadline))
            .map_err(|error| error.to_string())
    }

    pub(super) fn request_classified_cancellable(
        &mut self,
        request_id: u64,
        request: v1::Request,
        cancellation: &CancelState,
        deadline: &DeadlineGuard,
    ) -> Result<v1::Response, RequestFailure> {
        self.request_classified_inner(request_id, request, Some(cancellation), Some(deadline))
    }

    fn request_classified_inner(
        &mut self,
        request_id: u64,
        request: v1::Request,
        cancellation: Option<&CancelState>,
        deadline: Option<&DeadlineGuard>,
    ) -> Result<v1::Response, RequestFailure> {
        self.write_envelope_cancellable(
            &envelope(request_id, 0, Payload::Request(request)),
            cancellation,
            deadline,
        )?;
        loop {
            if cancellation.is_some_and(CancelState::is_cancelled) {
                let cancel = encode_frame(&envelope(
                    0,
                    0,
                    Payload::Cancel(v1::Cancel {
                        target_request_id: request_id,
                    }),
                ))
                .map_err(|error| RequestFailure::Transport(error.to_string()))?;
                let _ = self.stdin.write(&cancel);
                return Err(RequestFailure::Cancelled);
            }
            if let Some(frame) = self
                .decoder
                .next_frame()
                .map_err(|error| RequestFailure::Transport(error.to_string()))?
            {
                if frame.request_id != request_id {
                    continue;
                }
                let Some(Payload::Response(response)) = frame.payload else {
                    continue;
                };
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

    pub(super) fn cancel_download(
        &mut self,
        transfer_id: &str,
        request_id: u64,
    ) -> Result<(), String> {
        self.request(
            request_id,
            v1::Request {
                operation: v1::Operation::CancelDownload.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: transfer_id.into(),
                    transfer_id: transfer_id.into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    pub(super) fn cancel_write(
        &mut self,
        operation_id: &str,
        transfer_id: &str,
        request_id: u64,
    ) -> Result<(), String> {
        self.request(
            request_id,
            v1::Request {
                operation: v1::Operation::CancelFileWrite.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: operation_id.into(),
                    transfer_id: transfer_id.into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    pub(super) fn cancel_terminal_upload(
        &mut self,
        transfer_id: &str,
        request_id: u64,
    ) -> Result<String, String> {
        let response = self.request(
            request_id,
            v1::Request {
                operation: v1::Operation::CancelTerminalUpload.into(),
                file: Some(v1::FileServiceRequest {
                    operation_id: transfer_id.into(),
                    transfer_id: transfer_id.into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?;
        Ok(response
            .file
            .and_then(|file| file.upload)
            .map(|upload| upload.cleanup_error)
            .unwrap_or_default())
    }
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
