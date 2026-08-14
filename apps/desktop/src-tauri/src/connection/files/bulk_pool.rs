use std::{
    io::BufReader,
    os::fd::AsRawFd,
    process::{Child, ChildStdin, ChildStdout},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use tmux_agent_protocol::FrameAccumulator;

use super::super::{ConnectionSpec, transport::spawn_bulk_bridge};
use super::bulk_protocol::BulkProtocolClient;
use super::scheduler::{BulkBinding, CancelState};

/// How long a bulk connection is kept alive with nothing to do.
///
/// Long enough that a person reading one file and then another pays the
/// handshake once, short enough that a laptop closed on a live app is not
/// holding an ssh channel and a remote helper process open indefinitely. The
/// ssh master behind it has its own `ControlPersist=60`, so this deliberately
/// does not outlive the transport it rides.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Idle connections kept at once. Two is the concurrent-transfer bound
/// (`acquire_bulk_permit`), so this can hold what that many jobs left behind
/// and never more.
const MAX_IDLE: usize = 2;

/// The first request id a fresh connection may use. 1 is the handshake's.
const FIRST_REQUEST_ID: u64 = 2;

/// What makes two bulk connections interchangeable.
///
/// The handshake binds a bulk connection to one server identity and one control
/// epoch (`BulkProtocolClient::handshake`), and the host builds its
/// per-connection file-transfer state when the connection is made. A pooled
/// connection may therefore only be handed to a job that would have made
/// exactly that handshake — anything else and the reuse would be laundering a
/// stale binding past the check that exists to catch it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BulkKey {
    connection: ConnectionSpec,
    server_identity: String,
    connection_epoch: u64,
}

/// The pool's whole policy, over any payload.
///
/// Generic so that taking, expiring and evicting can be exercised without an
/// ssh process: the bugs a pool has are in *when it hands something back*, and
/// a policy that can only be tested by spawning `ssh` is a policy that is not
/// tested. Both operations hand the caller the entries they retired rather than
/// closing anything themselves — this type knows nothing about processes.
struct IdlePool<T> {
    entries: Vec<(BulkKey, T, Instant)>,
    idle_timeout: Duration,
    capacity: usize,
}

impl<T> IdlePool<T> {
    fn new(idle_timeout: Duration, capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            idle_timeout,
            capacity,
        }
    }

    /// The newest entry matching `key`, plus everything that had timed out.
    ///
    /// Newest first because the oldest entry is the likeliest to have been
    /// dropped by something outside this process — an ssh idle timeout, a
    /// suspended laptop — while the one released a moment ago is the likeliest
    /// to still be warm.
    fn take(&mut self, key: &BulkKey, now: Instant) -> (Option<T>, Vec<T>) {
        let expired = self.expire(now);
        let index = self.entries.iter().rposition(|(entry, _, _)| entry == key);
        let taken = index.map(|index| self.entries.remove(index).1);
        (taken, expired)
    }

    fn release(&mut self, key: BulkKey, value: T, now: Instant) -> Vec<T> {
        let mut retired = self.expire(now);
        self.entries.push((key, value, now));
        while self.entries.len() > self.capacity {
            retired.push(self.entries.remove(0).1);
        }
        retired
    }

    fn expire(&mut self, now: Instant) -> Vec<T> {
        let timeout = self.idle_timeout;
        let (live, expired): (Vec<_>, Vec<_>) = self
            .entries
            .drain(..)
            .partition(|(_, _, since)| now.duration_since(*since) < timeout);
        self.entries = live;
        expired.into_iter().map(|(_, value, _)| value).collect()
    }

    fn drain(&mut self) -> Vec<T> {
        self.entries.drain(..).map(|(_, value, _)| value).collect()
    }
}

/// The handles a live bulk bridge is: the process, its pipes, and the frame
/// decoder that may still be holding bytes read past the last response.
struct Bridge {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    decoder: FrameAccumulator,
    /// The next request id this *connection* will use. Ids belong to the
    /// connection, so they carry across the jobs that share it.
    next_request_id: u64,
    /// Whether every request written to this bridge was answered in full.
    ///
    /// This, not "the job succeeded", is what makes a bridge reusable. A remote
    /// refusal — no such file — is a complete response and leaves the stream
    /// exactly where the next request expects it. A transport error or a
    /// cancellation does not: a half-written request, or a response still in
    /// flight when the job walked away, would be read by whoever came next.
    /// `BulkProtocolClient` clears this itself, so no job has to remember to.
    clean: bool,
}

impl Bridge {
    /// Whether this bridge is worth handing to another job.
    ///
    /// Checked on the way into the pool *and* on the way out, because the two
    /// answer different questions. On release: the bridge process is killed
    /// asynchronously — by a cancellation, by the binding monitor, by the
    /// inactivity deadline — and that can land after a job's last successful
    /// request, so `clean` alone would return a corpse to the pool. On acquire:
    /// the ssh channel can die while nobody is holding the bridge at all, which
    /// used to cost nothing because every operation dialled its own.
    ///
    /// The stream check is a zero-timeout `poll`, so it costs no round trip and
    /// consumes nothing. An idle bulk connection is silent: readable means
    /// either EOF or bytes nobody asked for, and hangup means the far end is
    /// gone. Neither is something to hand to the next job.
    fn reusable(&mut self) -> bool {
        if !self.clean {
            return false;
        }
        if !matches!(self.child.try_wait(), Ok(None)) {
            return false;
        }
        let mut poll = libc::pollfd {
            fd: self.reader.get_ref().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, 0) };
        ready == 0 && poll.revents == 0
    }
}

/// Kills and reaps. A `Child` that is merely dropped is neither killed nor
/// waited for, which on this path would strand an ssh channel and a remote
/// helper process with nothing left holding a handle to them.
impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pool() -> &'static Mutex<IdlePool<Bridge>> {
    static POOL: OnceLock<Mutex<IdlePool<Bridge>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(IdlePool::new(IDLE_TIMEOUT, MAX_IDLE)))
}

/// A bulk bridge held for the duration of one job.
///
/// Every file operation used to spawn its own: a local `ssh -O check`, a local
/// `ssh` fork, a fresh SSH channel, a remote `tmux-ide-host` fork, a daemon
/// connection whose setup forks `tmux -V`, `git --version` and
/// `tmux display-message` and starts a filesystem watcher, and only then a
/// ClientHello round trip — before the first byte of the file moved. Over a
/// tailscale link that is the "opening a file takes seconds" the user reported.
///
/// A lease reuses all of it, and reuses nothing whose stream position or
/// liveness is in doubt (`Bridge::reusable`).
pub(super) struct BulkLease {
    key: BulkKey,
    bridge: Option<Bridge>,
    cancellation: Arc<CancelState>,
}

impl BulkLease {
    pub(super) fn acquire(
        connection: &ConnectionSpec,
        binding: &BulkBinding,
        cancellation: &Arc<CancelState>,
    ) -> Result<Self, String> {
        let cancellation = Arc::clone(cancellation);
        let key = BulkKey {
            connection: connection.clone(),
            server_identity: binding.expected_server_identity.clone(),
            connection_epoch: binding.connection_epoch,
        };
        let (mut pooled, _expired) = pool().lock().unwrap().take(&key, Instant::now());
        if let Some(bridge) = pooled.as_mut()
            && !bridge.reusable()
        {
            pooled = None;
        }
        if let Some(bridge) = pooled {
            // Still only usable while the control connection it was bound to is
            // the live one, which is what the handshake checked when it was
            // made. Re-checking the binding keeps that guarantee without
            // re-paying for the handshake. A failure here drops the bridge,
            // which closes it — it is not put back.
            binding.validate()?;
            return Ok(Self {
                key,
                bridge: Some(bridge),
                cancellation,
            });
        }

        let mut child = spawn_bulk_bridge(connection)?;
        let stdin = child.stdin.take().ok_or("bulk bridge stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("bulk bridge stdout unavailable")?;
        let mut bridge = Bridge {
            child,
            stdin,
            reader: BufReader::new(stdout),
            decoder: FrameAccumulator::default(),
            next_request_id: FIRST_REQUEST_ID,
            clean: true,
        };
        // The handshake is part of establishing the bridge, not part of the job:
        // a reused bridge has already made it, which is one of the round trips
        // this pool exists to stop paying.
        BulkProtocolClient::handshake(&mut bridge.stdin, &mut bridge.reader, binding)?;
        Ok(Self {
            key,
            bridge: Some(bridge),
            cancellation,
        })
    }

    /// The process id a cancellation kills. See `CancelState::bind_process`.
    pub(super) fn process_id(&self) -> u32 {
        self.bridge
            .as_ref()
            .map(|bridge| bridge.child.id())
            .unwrap_or(0)
    }

    pub(super) fn client(&mut self) -> BulkProtocolClient<'_> {
        let bridge = self.bridge.as_mut().expect("a lease holds its bridge");
        BulkProtocolClient::resumed(
            &mut bridge.stdin,
            &mut bridge.reader,
            &mut bridge.decoder,
            &mut bridge.next_request_id,
            &mut bridge.clean,
        )
    }
}

impl Drop for BulkLease {
    fn drop(&mut self) {
        let Some(mut bridge) = self.bridge.take() else {
            return;
        };
        // A cancellation that has begun must never leave a pooled bridge behind.
        // `CancelState::cancel_for` sets `requested` *before* it swaps the pid
        // out to kill it, and those two steps are not atomic — so a watcher can
        // hold this bridge's pid, be descheduled, and deliver its SIGKILL after
        // the pool has handed the same process to an unrelated job, killing a
        // healthy transfer. Reading the flag the watcher already published is
        // what closes that window: if the kill is coming, this bridge is not
        // reusable, whatever its pipes currently say.
        if self.cancellation.is_cancelled() || !bridge.reusable() {
            return;
        }
        // The retired entries are closed here, outside the lock, by dropping.
        let _retired = pool()
            .lock()
            .unwrap()
            .release(self.key.clone(), bridge, Instant::now());
    }
}

/// Drops every pooled connection.
///
/// Deliberately not filtered: a pooled bridge is keyed by the control
/// connection's identity and epoch, so when a control connection stops, every
/// bridge that could still be handed out belongs to the connection that is
/// going away. Anything left is unreachable, and closing it now frees an ssh
/// channel and a remote helper process rather than waiting out the idle timeout.
pub(crate) fn close_pooled_bulk_bridges() {
    // Taken out of the lock first: closing a bridge kills and reaps a process,
    // and doing that under the pool mutex would block every other file
    // operation for the length of two `waitpid` calls.
    let closing = pool().lock().unwrap().drain();
    drop(closing);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(identity: &str, epoch: u64) -> BulkKey {
        BulkKey {
            connection: ConnectionSpec::Local,
            server_identity: identity.into(),
            connection_epoch: epoch,
        }
    }

    #[test]
    fn a_bridge_is_only_interchangeable_within_one_identity_and_epoch() {
        let base = key("server", 3);
        assert_eq!(base, base.clone());
        assert_ne!(base, key("other", 3));
        assert_ne!(base, key("server", 4));
        assert_ne!(
            base,
            BulkKey {
                connection: ConnectionSpec::Ssh {
                    profile_id: "p".into(),
                    target: "host".into(),
                    config_path: None,
                },
                ..base.clone()
            }
        );

        // And the pool only ever hands one back to a job that matches it: the
        // handshake bound it to that identity and epoch, and the host built its
        // per-connection transfer state then.
        let mut pool = IdlePool::new(IDLE_TIMEOUT, MAX_IDLE);
        let now = Instant::now();
        assert!(pool.release(key("server", 3), "bridge", now).is_empty());
        assert_eq!(pool.take(&key("server", 4), now).0, None);
        assert_eq!(pool.take(&key("other", 3), now).0, None);
        assert_eq!(pool.take(&key("server", 3), now).0, Some("bridge"));
        assert_eq!(pool.take(&key("server", 3), now).0, None);
    }

    #[test]
    fn an_entry_nobody_came_back_for_is_retired_to_the_caller_to_close() {
        let mut pool = IdlePool::new(Duration::from_secs(60), MAX_IDLE);
        let start = Instant::now();
        pool.release(key("server", 1), "stale", start);
        pool.release(key("server", 1), "fresh", start + Duration::from_secs(59));

        // A minute after the first release, only the first has timed out — and
        // it comes back to the caller rather than being closed in here.
        let (taken, expired) = pool.take(&key("server", 1), start + Duration::from_secs(60));
        assert_eq!(expired, vec!["stale"]);
        // Newest first: the entry released a moment ago is the one most likely
        // to still be alive on the far end.
        assert_eq!(taken, Some("fresh"));
        assert!(pool.entries.is_empty());
    }

    #[test]
    fn the_pool_never_grows_past_its_capacity() {
        let mut pool = IdlePool::new(IDLE_TIMEOUT, 2);
        let now = Instant::now();
        pool.release(key("server", 1), "first", now);
        pool.release(key("server", 1), "second", now);
        // The third eviction retires the oldest, not the one just released.
        assert_eq!(pool.release(key("server", 1), "third", now), vec!["first"]);
        assert_eq!(pool.entries.len(), 2);
        assert_eq!(pool.drain(), vec!["second", "third"]);
    }
}
