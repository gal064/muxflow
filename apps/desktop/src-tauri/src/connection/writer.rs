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

pub(crate) use crate::perf_log::input_timing::{ControlWriteTiming, InputConnectionEpoch};

const CONTROL_WRITE_QUEUE: usize = 512;
const WRITE_POLL: Duration = Duration::from_millis(10);

struct ControlWrite {
    bytes: Vec<u8>,
    deadline: Instant,
    measurement: ControlWriteMeasurement,
    completion: mpsc::SyncSender<Result<ControlWriteTiming, String>>,
}

#[cfg(any(debug_assertions, feature = "perf-log"))]
struct ControlWriteMeasurement {
    enqueued_at: Instant,
    queue_depth_at_enqueue: usize,
}

#[cfg(any(debug_assertions, feature = "perf-log"))]
struct ControlWriteStarted {
    write_started: Instant,
    queue_wait: Duration,
    queue_depth_at_enqueue: usize,
}

#[cfg(any(debug_assertions, feature = "perf-log"))]
#[derive(Clone, Default)]
struct ControlQueueDepth(Arc<std::sync::atomic::AtomicUsize>);

#[cfg(any(debug_assertions, feature = "perf-log"))]
impl ControlQueueDepth {
    fn entered(&self) -> ControlWriteMeasurement {
        ControlWriteMeasurement {
            enqueued_at: Instant::now(),
            queue_depth_at_enqueue: self.0.fetch_add(1, Ordering::Relaxed) + 1,
        }
    }

    fn rejected(&self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }

    fn retried(&self, measurement: &mut ControlWriteMeasurement) {
        measurement.queue_depth_at_enqueue = self.0.fetch_add(1, Ordering::Relaxed) + 1;
    }

    fn dequeued(&self, measurement: ControlWriteMeasurement) -> ControlWriteStarted {
        self.0.fetch_sub(1, Ordering::Relaxed);
        let write_started = Instant::now();
        ControlWriteStarted {
            write_started,
            queue_wait: write_started.saturating_duration_since(measurement.enqueued_at),
            queue_depth_at_enqueue: measurement.queue_depth_at_enqueue,
        }
    }
}

#[cfg(any(debug_assertions, feature = "perf-log"))]
impl ControlWriteStarted {
    fn finish(self) -> ControlWriteTiming {
        ControlWriteTiming {
            queue_wait: self.queue_wait,
            physical_write: self.write_started.elapsed(),
            queue_depth_at_enqueue: self.queue_depth_at_enqueue,
        }
    }
}

#[cfg(not(any(debug_assertions, feature = "perf-log")))]
struct ControlWriteMeasurement;

#[cfg(not(any(debug_assertions, feature = "perf-log")))]
struct ControlWriteStarted;

#[cfg(not(any(debug_assertions, feature = "perf-log")))]
#[derive(Clone, Default)]
struct ControlQueueDepth;

#[cfg(not(any(debug_assertions, feature = "perf-log")))]
const _: () = {
    assert!(std::mem::size_of::<ControlWriteMeasurement>() == 0);
    assert!(std::mem::size_of::<ControlWriteStarted>() == 0);
    assert!(std::mem::size_of::<ControlQueueDepth>() == 0);
};

#[cfg(not(any(debug_assertions, feature = "perf-log")))]
impl ControlQueueDepth {
    #[inline(always)]
    fn entered(&self) -> ControlWriteMeasurement {
        ControlWriteMeasurement
    }
    #[inline(always)]
    fn rejected(&self) {}
    #[inline(always)]
    fn retried(&self, _measurement: &mut ControlWriteMeasurement) {}
    #[inline(always)]
    fn dequeued(&self, _measurement: ControlWriteMeasurement) -> ControlWriteStarted {
        ControlWriteStarted
    }
}

#[cfg(not(any(debug_assertions, feature = "perf-log")))]
impl ControlWriteStarted {
    #[inline(always)]
    fn finish(self) -> ControlWriteTiming {
        ControlWriteTiming
    }
}

#[derive(Clone)]
pub(super) struct ControlWriterHandle {
    sender: mpsc::SyncSender<ControlWrite>,
    closed: Arc<AtomicBool>,
    depth: ControlQueueDepth,
    input_epoch: InputConnectionEpoch,
}

impl ControlWriterHandle {
    pub(super) fn start(stdin: ChildStdin, name: &str, input_epoch: u64) -> Result<Self, String> {
        Self::start_with_epoch(stdin, name, input_epoch)
    }

    #[cfg(test)]
    pub(super) fn start_with<W>(writer: W, name: &str) -> Result<Self, String>
    where
        W: Write + AsRawFd + Send + 'static,
    {
        Self::start_with_epoch(writer, name, 0)
    }

    fn start_with_epoch<W>(writer: W, name: &str, input_epoch: u64) -> Result<Self, String>
    where
        W: Write + AsRawFd + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(CONTROL_WRITE_QUEUE);
        let closed = Arc::new(AtomicBool::new(false));
        let writer_closed = Arc::clone(&closed);
        let depth = ControlQueueDepth::default();
        let writer_depth = depth.clone();
        thread::Builder::new()
            .name(format!("host-control-writer-{name}"))
            .spawn(move || run_control_writer(writer, receiver, writer_closed, writer_depth))
            .map_err(|error| format!("failed to start host control writer: {error}"))?;
        Ok(Self {
            sender,
            closed,
            depth,
            input_epoch: InputConnectionEpoch::new(input_epoch),
        })
    }

    pub(super) fn dispatched_input(
        &self,
        request_id: u64,
        timing: ControlWriteTiming,
    ) -> crate::perf_log::input_timing::DispatchedInput {
        crate::perf_log::input_timing::DispatchedInput::new(request_id, self.input_epoch, timing)
    }

    pub(super) fn write(&self, envelope: v1::Envelope, deadline: Instant) -> Result<(), String> {
        self.write_timed(envelope, deadline).map(|_| ())
    }

    pub(super) fn write_timed(
        &self,
        envelope: v1::Envelope,
        deadline: Instant,
    ) -> Result<ControlWriteTiming, String> {
        let mut bytes = Vec::new();
        write_frame_sync(&mut bytes, &envelope).map_err(|error| error.to_string())?;
        self.write_bytes_timed(bytes, deadline)
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
        let measurement = self.depth.entered();
        self.sender
            .try_send(ControlWrite {
                bytes,
                deadline,
                measurement,
                completion,
            })
            .map_err(|error| {
                self.depth.rejected();
                match error {
                    mpsc::TrySendError::Full(_) => {
                        "host bridge control writer queue is full".into()
                    }
                    mpsc::TrySendError::Disconnected(_) => {
                        "host bridge control writer disconnected".into()
                    }
                }
            })
    }

    fn write_bytes_timed(
        &self,
        bytes: Vec<u8>,
        deadline: Instant,
    ) -> Result<ControlWriteTiming, String> {
        let (completion, completed) = mpsc::sync_channel(1);
        let mut command = ControlWrite {
            bytes,
            deadline,
            measurement: self.depth.entered(),
            completion,
        };
        loop {
            if self.closed.load(Ordering::Acquire) {
                self.depth.rejected();
                return Err("host bridge control writer is closed".into());
            }
            if Instant::now() >= deadline {
                self.depth.rejected();
                return Err("host request timed out before its bytes were written".into());
            }
            match self.sender.try_send(command) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    self.depth.rejected();
                    command = returned;
                    thread::sleep(
                        WRITE_POLL.min(deadline.saturating_duration_since(Instant::now())),
                    );
                    self.depth.retried(&mut command.measurement);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.depth.rejected();
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
    depth: ControlQueueDepth,
) where
    W: Write + AsRawFd,
{
    let fd = writer.as_raw_fd();
    let setup = set_nonblocking(fd);
    while let Ok(command) = receiver.recv() {
        let measurement = depth.dequeued(command.measurement);
        let result = setup
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|_| write_until(&mut writer, fd, &command.bytes, command.deadline, &closed));
        let poisoned = result.is_err();
        let result = result.map(|()| measurement.finish());
        let _ = command.completion.send(result);
        if poisoned {
            // A length-prefixed protobuf frame is indivisible at this layer.
            // After any physical write failure it may already have placed a
            // prefix on the pipe, so appending another frame would corrupt the
            // stream. Poison the lane and reject everything already queued;
            // the connection supervisor must establish a fresh bridge.
            closed.store(true, Ordering::Release);
            while let Ok(pending) = receiver.try_recv() {
                let _ = depth.dequeued(pending.measurement);
                let _ = pending.completion.send(Err(
                    "host bridge control writer is poisoned after a physical write failure".into(),
                ));
            }
            break;
        }
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
    use std::{fs::File, os::fd::FromRawFd, sync::Mutex};
    use tmux_agent_protocol::{envelope, v1::envelope::Payload};

    #[test]
    fn a_full_queue_retry_preserves_the_original_wait_origin() {
        let depth = ControlQueueDepth::default();
        let mut measurement = depth.entered();
        let started = measurement.enqueued_at;
        depth.rejected();
        depth.retried(&mut measurement);
        assert_eq!(measurement.enqueued_at, started);
        depth.rejected();
    }

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

    struct PrefixThenBlock {
        fd: File,
        captured: Arc<Mutex<Vec<u8>>>,
        wrote_prefix: Option<mpsc::SyncSender<()>>,
    }

    impl AsRawFd for PrefixThenBlock {
        fn as_raw_fd(&self) -> RawFd {
            self.fd.as_raw_fd()
        }
    }

    impl Write for PrefixThenBlock {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(wrote_prefix) = self.wrote_prefix.take() {
                let length = bytes.len().min(8);
                self.captured
                    .lock()
                    .unwrap()
                    .extend_from_slice(&bytes[..length]);
                let _ = wrote_prefix.send(());
                return Ok(length);
            }
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_partial_timed_out_frame_poisons_the_lane_and_rejects_queued_followers() {
        let mut fds = [0; 2];
        // SAFETY: pipe initializes both descriptors on success.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: each descriptor is newly owned by this test.
        let read_end = unsafe { File::from_raw_fd(fds[0]) };
        let write_end = unsafe { File::from_raw_fd(fds[1]) };
        let captured = Arc::new(Mutex::new(Vec::new()));
        let (prefix_tx, prefix_rx) = mpsc::sync_channel(1);
        let writer = ControlWriterHandle::start_with(
            PrefixThenBlock {
                fd: write_end,
                captured: Arc::clone(&captured),
                wrote_prefix: Some(prefix_tx),
            },
            "partial-frame-test",
        )
        .unwrap();
        let first = writer.clone();
        let first_result = std::thread::spawn(move || {
            first.write(
                envelope(
                    1,
                    0,
                    Payload::Request(v1::Request {
                        operation: v1::Operation::TerminalInput.into(),
                        data: vec![7; 1024],
                        ..Default::default()
                    }),
                ),
                Instant::now() + Duration::from_millis(75),
            )
        });
        prefix_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let queued = writer.write(
            envelope(
                2,
                0,
                Payload::Request(v1::Request {
                    operation: v1::Operation::FullSnapshot.into(),
                    ..Default::default()
                }),
            ),
            Instant::now() + Duration::from_secs(1),
        );
        assert!(first_result.join().unwrap().is_err());
        assert!(queued.unwrap_err().contains("poisoned"));
        assert!(
            writer
                .write(
                    envelope(3, 0, Payload::Request(v1::Request::default())),
                    Instant::now() + Duration::from_secs(1),
                )
                .unwrap_err()
                .contains("closed")
        );
        assert_eq!(captured.lock().unwrap().len(), 8);
        drop(read_end);
    }
}
