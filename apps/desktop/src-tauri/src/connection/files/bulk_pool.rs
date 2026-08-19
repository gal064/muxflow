use std::{
    io::BufReader,
    os::fd::AsRawFd,
    process::{Child, ChildStdin, ChildStdout},
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use tmux_agent_protocol::FrameAccumulator;

use super::super::{ConnectionSpec, transport::spawn_bulk_bridge};
use super::bulk_protocol::BulkProtocolClient;
use super::scheduler::{BulkBinding, CancelState, DeadlineGuard};

/// How long a bulk connection is kept alive with nothing to do.
///
/// Long enough that a person reading one file and then another pays the
/// handshake once, short enough that a laptop closed on a live app is not
/// holding an SSH connection and a remote helper process open indefinitely.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Idle connections kept at once, across every connection.
///
/// Two for the scheduler's concurrent-transfer bound — which is process-global,
/// and which editor file opens go through as well — plus one for the Git
/// diff-body lane. That lane is bounded to a single read *per connection*, so
/// with several profiles open at once the concurrent bulk consumers are 2 + N
/// rather than 3: this is a retention budget for the common single-connection
/// case, not a bound on how many bulk bridges can exist. Exceeding it costs a
/// re-dial, never correctness — a lease that finds nothing pooled opens its own
/// bridge.
const MAX_IDLE: usize = 3;

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
    client_scope: uuid::Uuid,
    connection: ConnectionSpec,
    server_identity: String,
    connection_epoch: u64,
}

impl BulkKey {
    fn new(connection: &ConnectionSpec, binding: &BulkBinding) -> Self {
        Self {
            client_scope: binding.client.bulk_scope,
            connection: connection.clone(),
            server_identity: binding.expected_server_identity.clone(),
            connection_epoch: binding.connection_epoch,
        }
    }
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

    /// How long until the oldest entry times out. `None` means there is nothing
    /// here to time out, which is a wait with no deadline rather than a wait of
    /// zero — the difference between a parked reaper and a spinning one.
    fn time_to_next_expiry(&self, now: Instant) -> Option<Duration> {
        let oldest = self.entries.iter().map(|(_, _, since)| *since).min()?;
        Some(self.idle_timeout.saturating_sub(now.duration_since(oldest)))
    }

    /// Whether a bridge for `key` is sitting idle right now.
    ///
    /// Deliberately does not expire first: this only ever decides whether to
    /// skip a pre-warm, and treating a not-yet-reaped entry as warm costs one
    /// missed pre-warm, while expiring here would make a read mutate the pool.
    fn holds(&self, key: &BulkKey) -> bool {
        self.entries.iter().any(|(entry, _, _)| entry == key)
    }

    fn drain_matching(&mut self, mut matches: impl FnMut(&BulkKey) -> bool) -> Vec<T> {
        let (drained, kept): (Vec<_>, Vec<_>) =
            self.entries.drain(..).partition(|(key, _, _)| matches(key));
        self.entries = kept;
        drained.into_iter().map(|(_, value, _)| value).collect()
    }
}

/// The pool, plus the way the reaper is told to look again.
///
/// `IDLE_TIMEOUT` is a promise about wall-clock time, and expiry that only runs
/// inside `take` and `release` cannot keep it: after the last file operation
/// nothing calls either, so the entry left behind — an ssh child, a remote
/// helper process and a stdin held open — would sit there until the control
/// connection stopped. One thread parked on this condition variable is what
/// makes the number above mean what it says.
struct SharedPool<T> {
    idle: Mutex<IdlePool<T>>,
    changed: Condvar,
}

impl<T> SharedPool<T> {
    fn new(idle_timeout: Duration, capacity: usize) -> Self {
        Self {
            idle: Mutex::new(IdlePool::new(idle_timeout, capacity)),
            changed: Condvar::new(),
        }
    }

    /// No wake: removing an entry can only move the next deadline later, and the
    /// reaper recomputes the deadline from scratch every time it wakes.
    fn take(&self, key: &BulkKey) -> (Option<T>, Vec<T>) {
        self.idle.lock().unwrap().take(key, Instant::now())
    }

    fn release(&self, key: BulkKey, value: T) -> Vec<T> {
        let retired = self
            .idle
            .lock()
            .unwrap()
            .release(key, value, Instant::now());
        // After the entry is in and outside the lock. A reaper with nothing to
        // watch waits without a deadline, so this notification is the only thing
        // that ever starts its clock.
        self.changed.notify_all();
        retired
    }

    fn drain_matching(&self, matches: impl FnMut(&BulkKey) -> bool) -> Vec<T> {
        self.idle.lock().unwrap().drain_matching(matches)
    }

    fn holds(&self, key: &BulkKey) -> bool {
        self.idle.lock().unwrap().holds(key)
    }

    /// Blocks until at least one entry has timed out, and hands them over.
    ///
    /// Returns with the lock released, so whoever closes these is not holding
    /// every other file operation up behind two `waitpid` calls. Only ever waits
    /// on this pool's mutex, in the order `take` and `release` take it, so a
    /// reaper cannot deadlock against either.
    fn reap(&self) -> Vec<T> {
        let mut idle = self.idle.lock().unwrap();
        loop {
            let expired = idle.expire(Instant::now());
            if !expired.is_empty() {
                return expired;
            }
            idle = match idle.time_to_next_expiry(Instant::now()) {
                Some(wait) => self.changed.wait_timeout(idle, wait).unwrap().0,
                None => self.changed.wait(idle).unwrap(),
            };
        }
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
        self.clean && self.alive()
    }

    /// Whether the process and its stream are still in the state an idle bulk
    /// connection is in. Split from `reusable` because the release path pairs it
    /// with a fact this type does not have — see `returnable`.
    fn alive(&mut self) -> bool {
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

fn pool() -> &'static SharedPool<Bridge> {
    static POOL: OnceLock<SharedPool<Bridge>> = OnceLock::new();
    POOL.get_or_init(|| {
        // Started with the pool it watches, so there is no window in which an
        // entry can be pooled and nothing is waiting for it to time out.
        start_reaper();
        SharedPool::new(IDLE_TIMEOUT, MAX_IDLE)
    })
}

/// The one thread that makes `IDLE_TIMEOUT` a wall-clock promise.
///
/// Parked, not polling: it holds no lock while it waits and wakes only for an
/// entry that exists. It is deliberately never joined — a thread that is not the
/// main one cannot hold the process open, since the runtime exits when `main`
/// returns whatever this is doing, and it owns nothing whose drop has to run.
fn start_reaper() {
    let started = thread::Builder::new()
        .name("bulk-pool-reaper".into())
        .spawn(|| {
            loop {
                // `reap` returns with the lock released, so the kill and reap
                // each of these performs on the way out happens off it.
                drop(pool().reap());
            }
        });
    if let Err(error) = started {
        // Worth saying, not worth failing a file operation over: expiry still
        // happens inside the next `take` or `release`, as it always did.
        eprintln!("bulk pool reaper could not start: {error}");
    }
}

/// Whether a finished lease may put its bridge back.
///
/// Three facts, one predicate, so the one that is easiest to lose can be pinned
/// by a test: `cancelled` is not implied by either of the others, and the case
/// that matters is a cancelled lease whose bridge still looks perfect.
/// `CancelState::cancel_for` sets `requested` *before* it swaps the pid out to
/// kill it, and those two steps are not atomic — so a watcher can hold this
/// bridge's pid, be descheduled, and deliver its SIGKILL after the pool has
/// handed the same process to an unrelated job, killing a healthy transfer.
/// Reading the flag the watcher already published is what closes that window.
fn returnable(cancelled: bool, clean: bool, alive: bool) -> bool {
    !cancelled && clean && alive
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
pub(crate) struct BulkLease {
    key: BulkKey,
    bridge: Option<Bridge>,
    cancellation: Arc<CancelState>,
    /// Whether this lease reused an idle pooled bridge rather than paying for
    /// a fresh spawn and handshake. A fact for measurement, never for policy.
    reused: bool,
}

#[derive(Clone, Copy)]
enum AcquisitionMode {
    Request,
    AuthoritativeReconciliation,
}

impl BulkLease {
    pub(crate) fn acquire(
        connection: &ConnectionSpec,
        binding: &BulkBinding,
        cancellation: &Arc<CancelState>,
        deadline: &DeadlineGuard,
    ) -> Result<Self, String> {
        Self::acquire_with_mode(
            connection,
            binding,
            cancellation,
            deadline,
            AcquisitionMode::Request,
        )
    }

    pub(super) fn acquire_authoritative(
        connection: &ConnectionSpec,
        binding: &BulkBinding,
        cancellation: &Arc<CancelState>,
        deadline: &DeadlineGuard,
    ) -> Result<Self, String> {
        Self::acquire_with_mode(
            connection,
            binding,
            cancellation,
            deadline,
            AcquisitionMode::AuthoritativeReconciliation,
        )
    }

    fn acquire_with_mode(
        connection: &ConnectionSpec,
        binding: &BulkBinding,
        cancellation: &Arc<CancelState>,
        deadline: &DeadlineGuard,
        mode: AcquisitionMode,
    ) -> Result<Self, String> {
        Self::acquire_with_spawn(
            connection,
            binding,
            cancellation,
            deadline,
            mode,
            spawn_bulk_bridge,
        )
    }

    fn acquire_with_spawn(
        connection: &ConnectionSpec,
        binding: &BulkBinding,
        cancellation: &Arc<CancelState>,
        deadline: &DeadlineGuard,
        mode: AcquisitionMode,
        spawn: impl FnOnce(&ConnectionSpec, &dyn Fn() -> bool) -> Result<Child, String>,
    ) -> Result<Self, String> {
        let cancellation = Arc::clone(cancellation);
        let key = BulkKey::new(connection, binding);
        let (mut pooled, _expired) = pool().take(&key);
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
                reused: true,
            });
        }

        let cancelled = || match mode {
            AcquisitionMode::Request => cancellation.is_cancelled(),
            AcquisitionMode::AuthoritativeReconciliation => {
                cancellation.transport_termination_requested()
            }
        };
        let mut child = spawn(connection, &cancelled)?;
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
        // The helper is cancellation-owned before any handshake I/O. User,
        // stale-binding, and inactivity cancellation can therefore interrupt
        // a silent peer instead of occupying one of the two engine lanes.
        let _process_binding = match mode {
            AcquisitionMode::Request => cancellation.bind_process(bridge.child.id())?,
            AcquisitionMode::AuthoritativeReconciliation => {
                cancellation.bind_authoritative_process(bridge.child.id())?
            }
        };
        // The handshake is part of establishing the bridge, not part of the job:
        // a reused bridge has already made it, which is one of the round trips
        // this pool exists to stop paying.
        BulkProtocolClient::handshake(
            &mut bridge.stdin,
            &mut bridge.reader,
            binding,
            &cancelled,
            deadline,
        )?;
        drop(_process_binding);
        Ok(Self {
            key,
            bridge: Some(bridge),
            cancellation,
            reused: false,
        })
    }

    /// Whether this lease came from the idle pool. See the field.
    pub(crate) fn reused(&self) -> bool {
        self.reused
    }

    /// The process id a cancellation kills. See `CancelState::bind_process`.
    pub(crate) fn process_id(&self) -> u32 {
        self.bridge
            .as_ref()
            .map(|bridge| bridge.child.id())
            .unwrap_or(0)
    }

    pub(crate) fn client(&mut self) -> BulkProtocolClient<'_> {
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
        // All three facts are read before the decision, rather than short-cut
        // one by one, because `alive` is a `try_wait` and a zero-timeout `poll`:
        // it costs nothing and consumes nothing even when the answer is already
        // no.
        if !returnable(
            self.cancellation.is_cancelled(),
            bridge.clean,
            bridge.alive(),
        ) {
            return;
        }
        // The retired entries are closed here, outside the lock, by dropping.
        let _retired = pool().release(self.key.clone(), bridge);
    }
}

/// What a pre-warm attempt did, for the tests and for nobody else.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Prewarm {
    /// A bridge was established and left idle in the pool.
    Warmed,
    /// One was already idle for this key; nothing to do.
    AlreadyWarm,
}

/// Establishes one idle bulk bridge ahead of the first file operation.
///
/// Every first-of-session file open paid for a fresh bridge — a local
/// `ssh -O check`, an `ssh` fork, a new SSH channel, a remote `tmux-ide-host`
/// fork and a ClientHello round trip — before the first byte of the file moved.
/// `leaseReuse` was false on every measured first open, at 245–325 ms, and it
/// grows with RTT because the handshake is multi-round-trip
/// (tests/performance/benchmark/decomposition.md). Doing it once at connect moves that cost
/// off the click and onto a moment when nobody is waiting.
///
/// This is the same work an ordinary lease does, in the same order, and it
/// leaves the bridge in the pool by dropping the lease: `BulkLease::drop`
/// already returns a clean, live, uncancelled bridge. There is no second
/// establishment path to keep in step with the first.
///
/// **Only ever for a settled writable connection.** `BulkBinding::validate` is
/// the gate, and it is the whole reason this takes a binding rather than the
/// parts of one: a connection that is read-only or still settling fails it with
/// "bulk job is not bound to a writable live control connection" — the exact
/// error that replaced a painted editor in decomposition.md's "Observed once".
/// A pre-warm has no user behind it, so it must never turn that state into
/// anything the user can see; it gives up silently instead.
fn prewarm_with_spawn(
    connection: &ConnectionSpec,
    binding: &BulkBinding,
    spawn: impl FnOnce(&ConnectionSpec, &dyn Fn() -> bool) -> Result<Child, String>,
) -> Result<Prewarm, String> {
    binding.validate()?;
    let key = BulkKey::new(connection, binding);
    if pool().holds(&key) {
        return Ok(Prewarm::AlreadyWarm);
    }
    let cancellation = Arc::new(CancelState::new());
    let deadline = cancellation.arm_inactivity_deadline();
    let lease = BulkLease::acquire_with_spawn(
        connection,
        binding,
        &cancellation,
        &deadline,
        AcquisitionMode::Request,
        spawn,
    );
    // Completed whatever happened: the guard owns a timer thread, and an early
    // return that left it armed would keep firing at a connection that has
    // nothing in flight.
    deadline.complete();
    // Dropping the lease is what pools the bridge.
    drop(lease?);
    Ok(Prewarm::Warmed)
}

/// Pre-warms one bulk bridge for a connection that has just gone live.
///
/// On its own thread because establishing a bridge is several hundred
/// milliseconds of ssh and handshake, and the caller is the terminal bridge's
/// own reader loop — the thread that carries every keystroke and every frame of
/// output. Blocking it to make a later file open faster would trade the
/// common interaction for the rare one.
///
/// Failures are dropped rather than reported. Nothing is waiting on this: if it
/// does not happen, the first file open pays what it pays today, which is
/// exactly the behaviour that shipped before this existed.
pub(crate) fn prewarm_bulk_bridge(connection: ConnectionSpec, binding: BulkBinding) {
    let started = thread::Builder::new()
        .name("bulk-pool-prewarm".into())
        .spawn(move || {
            let _ = prewarm_with_spawn(&connection, &binding, spawn_bulk_bridge);
        });
    if let Err(error) = started {
        eprintln!("bulk pool pre-warm could not start: {error}");
    }
}

/// Drops every pooled bridge owned by one exact desktop control client.
///
/// The client token is narrower than server identity: two windows may attach
/// to the same server without owning one another's bridges. The token remains
/// stable across reconnect so draining immediately before epoch replacement
/// retires every now-unreachable old-epoch bridge.
pub(crate) fn close_pooled_bulk_bridges(client_scope: uuid::Uuid) {
    // Taken out of the lock first: closing a bridge kills and reaps a process,
    // and doing that under the pool mutex would block every other file
    // operation for the length of two `waitpid` calls.
    let closing = pool().drain_matching(|key| key.client_scope == client_scope);
    drop(closing);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn key(identity: &str, epoch: u64) -> BulkKey {
        key_for(uuid::Uuid::from_u128(1), identity, epoch)
    }

    fn key_for(client_scope: uuid::Uuid, identity: &str, epoch: u64) -> BulkKey {
        BulkKey {
            client_scope,
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
                client_scope: base.client_scope,
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
        assert_eq!(pool.drain_matching(|_| true), vec!["second", "third"]);
    }

    /// The timeout is a wall-clock promise, so nothing in here is allowed to
    /// touch the pool after the release: no take, no release, no second job.
    /// Before the reaper existed this test could only end in a timeout.
    #[test]
    fn an_idle_entry_is_reaped_with_nothing_else_touching_the_pool() {
        let idle_timeout = Duration::from_millis(30);
        let pool = Arc::new(SharedPool::new(idle_timeout, MAX_IDLE));
        let started = Instant::now();
        assert!(pool.release(key("server", 1), "idle").is_empty());

        let reaping = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || pool.reap())
        };
        let reaped = reaping.join().expect("the reaper thread");
        let waited = started.elapsed();
        assert_eq!(reaped, vec!["idle"]);
        // Waited the timeout out rather than spinning it away.
        assert!(waited >= idle_timeout, "reaped after only {waited:?}");
        assert!(pool.idle.lock().unwrap().entries.is_empty());
    }

    /// A reaper with nothing to watch must park rather than spin, and must still
    /// notice the next entry: `release` is what starts its clock.
    #[test]
    fn a_release_wakes_a_reaper_that_had_nothing_to_wait_for() {
        let pool = Arc::new(SharedPool::<&str>::new(Duration::from_millis(20), MAX_IDLE));
        let reaping = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || pool.reap())
        };
        // Released after the reaper is already waiting, which is the waiting it
        // has no deadline for.
        thread::sleep(Duration::from_millis(10));
        pool.release(key("server", 1), "late");
        assert_eq!(reaping.join().expect("the reaper thread"), vec!["late"]);
    }

    /// The round-2 pid race, as a fact rather than as a comment: a cancellation
    /// that has begun is not visible in the bridge's own state, so a bridge that
    /// looks perfect must still not be pooled once the flag is set.
    #[test]
    fn a_cancelled_lease_never_returns_its_bridge_however_healthy_it_looks() {
        assert!(!returnable(true, true, true));
        assert!(returnable(false, true, true));
        assert!(!returnable(false, false, true));
        assert!(!returnable(false, true, false));
    }

    #[test]
    fn stopping_one_same_server_client_leaves_the_other_clients_bridges_pooled() {
        let mut pool = IdlePool::new(IDLE_TIMEOUT, MAX_IDLE);
        let now = Instant::now();
        let stopping = uuid::Uuid::from_u128(10);
        let staying = uuid::Uuid::from_u128(11);
        pool.release(key_for(stopping, "same-server", 1), "theirs", now);
        pool.release(key_for(staying, "same-server", 1), "ours", now);

        assert_eq!(
            pool.drain_matching(|key| key.client_scope == stopping),
            vec!["theirs"]
        );
        // The second live connection keeps the bridge it had warm.
        assert_eq!(
            pool.take(&key_for(staying, "same-server", 1), now).0,
            Some("ours")
        );
    }

    #[test]
    fn reconnect_drains_both_warm_bridges_from_the_previous_epoch() {
        let mut pool = IdlePool::new(IDLE_TIMEOUT, MAX_IDLE);
        let now = Instant::now();
        let scope = uuid::Uuid::from_u128(20);
        pool.release(key_for(scope, "server", 7), "first", now);
        pool.release(key_for(scope, "server", 7), "second", now);

        assert_eq!(
            pool.drain_matching(|key| key.client_scope == scope),
            vec!["first", "second"]
        );
        assert!(pool.entries.is_empty());
    }

    #[test]
    fn cancelled_verifying_request_can_acquire_an_authoritative_bulk_lease() {
        let client = Arc::new(crate::connection::TerminalClient::new());
        client
            .ready
            .store(true, std::sync::atomic::Ordering::Release);
        client
            .terminal_epoch
            .store(7, std::sync::atomic::Ordering::Release);
        *client.server_identity.lock().unwrap() = "server-a".into();
        let binding = BulkBinding::capture(Arc::clone(&client), "server-a".into(), 7).unwrap();
        let cancellation = Arc::new(CancelState::new());
        cancellation.prepare_finalize().unwrap();
        cancellation.cancel();
        let deadline = cancellation.arm_inactivity_deadline();
        let response = tmux_agent_protocol::encode_frame(&tmux_agent_protocol::envelope(
            1,
            0,
            tmux_agent_protocol::v1::envelope::Payload::ServerHello(
                tmux_agent_protocol::v1::ServerHello {
                    server_identity: "server-a".into(),
                    connection_epoch: 7,
                    capabilities: tmux_agent_protocol::HOST_CAPABILITIES,
                    ..Default::default()
                },
            ),
        ))
        .unwrap();
        let escaped = response
            .iter()
            .map(|byte| format!("\\{byte:03o}"))
            .collect::<String>();

        let lease = BulkLease::acquire_with_spawn(
            &ConnectionSpec::Local,
            &binding,
            &cancellation,
            &deadline,
            AcquisitionMode::AuthoritativeReconciliation,
            move |_, cancelled| {
                assert!(
                    !cancelled(),
                    "authoritative establishment ignores request cancellation"
                );
                Command::new("sh")
                    .args([
                        "-c",
                        "printf '%b' \"$1\"; exec sleep 30",
                        "bulk-fixture",
                        &escaped,
                    ])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|error| error.to_string())
            },
        )
        .unwrap();

        assert_ne!(lease.process_id(), 0);
        drop(lease);
        deadline.complete();
    }

    /// A live, writable, settled control client — what a pre-warm requires.
    fn settled_client(identity: &str, epoch: u64) -> Arc<crate::connection::TerminalClient> {
        let client = Arc::new(crate::connection::TerminalClient::new());
        client
            .ready
            .store(true, std::sync::atomic::Ordering::Release);
        client
            .terminal_epoch
            .store(epoch, std::sync::atomic::Ordering::Release);
        *client.server_identity.lock().unwrap() = identity.into();
        client
    }

    /// A helper that answers one ServerHello and then goes quiet, like a real
    /// idle bulk bridge: silent, alive, and holding its stream open.
    fn hello_spawn(
        identity: &str,
        epoch: u64,
    ) -> impl FnOnce(&ConnectionSpec, &dyn Fn() -> bool) -> Result<Child, String> {
        let response = tmux_agent_protocol::encode_frame(&tmux_agent_protocol::envelope(
            1,
            0,
            tmux_agent_protocol::v1::envelope::Payload::ServerHello(
                tmux_agent_protocol::v1::ServerHello {
                    server_identity: identity.into(),
                    connection_epoch: epoch,
                    capabilities: tmux_agent_protocol::HOST_CAPABILITIES,
                    ..Default::default()
                },
            ),
        ))
        .unwrap();
        let escaped = response
            .iter()
            .map(|byte| format!("\\{byte:03o}"))
            .collect::<String>();
        move |_, _| {
            Command::new("sh")
                .args([
                    "-c",
                    "printf '%b' \"$1\"; exec sleep 30",
                    "bulk-fixture",
                    &escaped,
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| error.to_string())
        }
    }

    /// The whole point of the pre-warm, stated as the thing the measurement
    /// showed was false: the first lease of a session reuses a pooled bridge
    /// instead of paying for a spawn and a handshake.
    #[test]
    fn a_prewarmed_connection_hands_the_first_lease_a_reused_bridge() {
        let _guard = super::super::scheduler::engine_test_lock();
        let client = settled_client("prewarm-a", 11);
        let binding = BulkBinding::capture(Arc::clone(&client), "prewarm-a".into(), 11).unwrap();

        assert_eq!(
            prewarm_with_spawn(&ConnectionSpec::Local, &binding, hello_spawn("prewarm-a", 11))
                .unwrap(),
            Prewarm::Warmed,
        );

        let cancellation = Arc::new(CancelState::new());
        let deadline = cancellation.arm_inactivity_deadline();
        let lease = BulkLease::acquire_with_spawn(
            &ConnectionSpec::Local,
            &binding,
            &cancellation,
            &deadline,
            AcquisitionMode::Request,
            |_, _| Err("the first open must not have to spawn anything".into()),
        )
        .expect("the pre-warmed bridge should have satisfied this lease");
        assert!(lease.reused(), "leaseReuse was false after a pre-warm");
        drop(lease);
        deadline.complete();
        close_pooled_bulk_bridges(client.bulk_scope);
    }

    /// The pool floor is one *per connection*, not one per pre-warm call.
    #[test]
    fn a_second_prewarm_does_not_open_a_second_bridge() {
        let _guard = super::super::scheduler::engine_test_lock();
        let client = settled_client("prewarm-b", 12);
        let binding = BulkBinding::capture(Arc::clone(&client), "prewarm-b".into(), 12).unwrap();
        prewarm_with_spawn(&ConnectionSpec::Local, &binding, hello_spawn("prewarm-b", 12)).unwrap();

        assert_eq!(
            prewarm_with_spawn(&ConnectionSpec::Local, &binding, |_, _| Err(
                "a connection that is already warm must not spawn again".into()
            ))
            .unwrap(),
            Prewarm::AlreadyWarm,
        );
        close_pooled_bulk_bridges(client.bulk_scope);
    }

    /// decomposition.md's "Observed once": a connection that is read-only while
    /// it settles is the state a pre-warm must refuse, not the state it
    /// hurries into. Captured while writable and flipped afterwards, because
    /// that is the real race — the binding is taken on the bridge thread and
    /// validated again on the pre-warm thread.
    #[test]
    fn a_connection_that_went_read_only_is_not_prewarmed() {
        let _guard = super::super::scheduler::engine_test_lock();
        let client = settled_client("prewarm-c", 13);
        let binding = BulkBinding::capture(Arc::clone(&client), "prewarm-c".into(), 13).unwrap();
        client
            .read_only
            .store(true, std::sync::atomic::Ordering::Release);

        let error = prewarm_with_spawn(&ConnectionSpec::Local, &binding, |_, _| {
            panic!("a read-only connection must never reach a spawn")
        })
        .expect_err("a read-only connection must not be pre-warmed");
        assert!(error.contains("writable live control connection"), "{error}");
    }

    /// The same refusal for a connection that has not finished settling.
    #[test]
    fn a_connection_that_is_not_ready_yet_is_not_prewarmed() {
        let _guard = super::super::scheduler::engine_test_lock();
        let client = settled_client("prewarm-d", 14);
        let binding = BulkBinding::capture(Arc::clone(&client), "prewarm-d".into(), 14).unwrap();
        client
            .ready
            .store(false, std::sync::atomic::Ordering::Release);

        let error = prewarm_with_spawn(&ConnectionSpec::Local, &binding, |_, _| {
            panic!("a settling connection must never reach a spawn")
        })
        .expect_err("a connection that is not ready must not be pre-warmed");
        assert!(error.contains("writable live control connection"), "{error}");
    }

    /// A pre-warm from a replaced epoch must not leave a bridge that a live
    /// job could pick up, since the key it would be pooled under is stale.
    #[test]
    fn a_prewarm_from_a_replaced_epoch_is_refused() {
        let _guard = super::super::scheduler::engine_test_lock();
        let client = settled_client("prewarm-e", 15);
        let binding = BulkBinding::capture(Arc::clone(&client), "prewarm-e".into(), 15).unwrap();
        client
            .terminal_epoch
            .store(16, std::sync::atomic::Ordering::Release);

        let error = prewarm_with_spawn(&ConnectionSpec::Local, &binding, |_, _| {
            panic!("a stale epoch must never reach a spawn")
        })
        .expect_err("a replaced epoch must not be pre-warmed");
        assert!(error.contains("epoch was replaced"), "{error}");
    }

    #[test]
    fn authoritative_acquisition_cannot_outlive_a_deadline_that_precedes_spawn() {
        let client = Arc::new(crate::connection::TerminalClient::new());
        client
            .ready
            .store(true, std::sync::atomic::Ordering::Release);
        client
            .terminal_epoch
            .store(8, std::sync::atomic::Ordering::Release);
        *client.server_identity.lock().unwrap() = "server-b".into();
        let binding = BulkBinding::capture(client, "server-b".into(), 8).unwrap();
        let cancellation = Arc::new(CancelState::new());
        cancellation.prepare_finalize().unwrap();
        let deadline = cancellation.arm_test_deadline(Duration::from_millis(30));
        std::thread::sleep(Duration::from_millis(80));

        let error = BulkLease::acquire_with_spawn(
            &ConnectionSpec::Local,
            &binding,
            &cancellation,
            &deadline,
            AcquisitionMode::AuthoritativeReconciliation,
            |_, cancelled| {
                assert!(
                    cancelled(),
                    "the authoritative deadline is visible before spawn"
                );
                Command::new("sh")
                    .args(["-c", "exec sleep 30"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|error| error.to_string())
            },
        )
        .err()
        .expect("the late authoritative helper must not become a lease");

        deadline.complete();
        assert!(error.contains("expired"), "{error}");
    }
}
