use std::{
    io::Write,
    os::fd::{AsRawFd, RawFd},
    process::ChildStdin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use tmux_agent_protocol::{v1, write_frame_sync};

const CONTROL_WRITE_QUEUE: usize = 512;
const WRITE_POLL: Duration = Duration::from_millis(10);

struct ControlWrite {
    bytes: Vec<u8>,
    deadline: Instant,
    completion: mpsc::SyncSender<Result<(), String>>,
}

#[derive(Clone)]
pub(super) struct ControlWriterHandle {
    sender: mpsc::SyncSender<ControlWrite>,
    closed: Arc<AtomicBool>,
}

impl ControlWriterHandle {
    pub(super) fn start(stdin: ChildStdin, name: &str) -> Result<Self, String> {
        Self::start_with(stdin, name)
    }

    fn start_with<W>(writer: W, name: &str) -> Result<Self, String>
    where
        W: Write + AsRawFd + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(CONTROL_WRITE_QUEUE);
        let closed = Arc::new(AtomicBool::new(false));
        let writer_closed = Arc::clone(&closed);
        thread::Builder::new()
            .name(format!("host-control-writer-{name}"))
            .spawn(move || run_control_writer(writer, receiver, writer_closed))
            .map_err(|error| format!("failed to start host control writer: {error}"))?;
        Ok(Self { sender, closed })
    }

    pub(super) fn write(&self, envelope: v1::Envelope, deadline: Instant) -> Result<(), String> {
        let mut bytes = Vec::new();
        write_frame_sync(&mut bytes, &envelope).map_err(|error| error.to_string())?;
        self.write_bytes(bytes, deadline)
    }

    /// Queues a best-effort control frame without extending an already-expired
    /// request deadline. The writer still bounds the physical write; the
    /// caller intentionally does not wait for its completion.
    pub(super) fn try_write(
        &self,
        envelope: v1::Envelope,
        deadline: Instant,
    ) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err("host bridge control writer is unavailable".into());
        }
        let mut bytes = Vec::new();
        write_frame_sync(&mut bytes, &envelope).map_err(|error| error.to_string())?;
        let (completion, _completed) = mpsc::sync_channel(1);
        self.sender
            .try_send(ControlWrite {
                bytes,
                deadline,
                completion,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "host bridge control writer queue is full".into(),
                mpsc::TrySendError::Disconnected(_) => {
                    "host bridge control writer disconnected".into()
                }
            })
    }

    fn write_bytes(&self, bytes: Vec<u8>, deadline: Instant) -> Result<(), String> {
        let (completion, completed) = mpsc::sync_channel(1);
        let mut command = ControlWrite {
            bytes,
            deadline,
            completion,
        };
        loop {
            if self.closed.load(Ordering::Acquire) {
                return Err("host bridge control writer is closed".into());
            }
            if Instant::now() >= deadline {
                return Err("host request timed out before its bytes were written".into());
            }
            match self.sender.try_send(command) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    command = returned;
                    thread::sleep(
                        WRITE_POLL.min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err("host bridge control writer disconnected".into());
                }
            }
        }
        completed
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "host request timed out while its bytes were being written".to_owned())?
    }

    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

fn run_control_writer<W>(
    mut writer: W,
    receiver: mpsc::Receiver<ControlWrite>,
    closed: Arc<AtomicBool>,
) where
    W: Write + AsRawFd,
{
    let fd = writer.as_raw_fd();
    let setup = set_nonblocking(fd);
    while let Ok(command) = receiver.recv() {
        let result = setup
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|_| write_until(&mut writer, fd, &command.bytes, command.deadline, &closed));
        let _ = command.completion.send(result);
        if closed.load(Ordering::Acquire) {
            break;
        }
    }
    closed.store(true, Ordering::Release);
}

fn set_nonblocking(fd: RawFd) -> Result<(), String> {
    // SAFETY: fcntl receives a live descriptor and does not retain it.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: same live descriptor and the retrieved flags plus O_NONBLOCK.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

fn write_until(
    writer: &mut impl Write,
    fd: RawFd,
    bytes: &[u8],
    deadline: Instant,
    closed: &AtomicBool,
) -> Result<(), String> {
    let mut offset = 0;
    while offset < bytes.len() {
        if closed.load(Ordering::Acquire) {
            return Err("host bridge control writer was cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("host request timed out while writing to the bridge".into());
        }
        match writer.write(&bytes[offset..]) {
            Ok(0) => return Err("host bridge control pipe closed during write".into()),
            Ok(written) => offset += written,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let mut pollfd = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: poll receives one initialized pollfd for this call.
                let result = unsafe { libc::poll(&mut pollfd, 1, 10) };
                if result < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    writer.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, os::fd::FromRawFd};
    use tmux_agent_protocol::{envelope, v1::envelope::Payload};

    #[test]
    fn a_full_os_pipe_obeys_the_write_deadline_and_close_never_needs_the_writer() {
        let mut fds = [0; 2];
        // SAFETY: pipe initializes both descriptors on success.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: each descriptor is newly owned by this test.
        let read_end = unsafe { File::from_raw_fd(fds[0]) };
        let write_end = unsafe { File::from_raw_fd(fds[1]) };
        let writer = ControlWriterHandle::start_with(write_end, "full-pipe-test").unwrap();
        let started = Instant::now();
        let result = writer.write(
            envelope(
                1,
                0,
                Payload::Request(v1::Request {
                    operation: v1::Operation::TerminalInput.into(),
                    data: vec![7; 1024 * 1024],
                    ..Default::default()
                }),
            ),
            Instant::now() + Duration::from_millis(50),
        );
        assert!(result.unwrap_err().contains("timed out"));
        assert!(started.elapsed() < Duration::from_millis(500));
        writer.close();
        drop(read_end);
    }
}
