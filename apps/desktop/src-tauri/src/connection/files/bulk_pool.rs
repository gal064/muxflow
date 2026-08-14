use std::{
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use tmux_agent_protocol::FrameAccumulator;

use super::super::{ConnectionSpec, transport::spawn_bulk_bridge};
use super::bulk_protocol::BulkProtocolClient;
use super::scheduler::BulkBinding;

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

/// Request ids one lease may use before it would collide with the next lease's.
///
/// Request ids are per *connection*, and `BulkProtocolClient` skips frames whose
/// id does not match the one it is waiting for. Reused ids are therefore not a
/// mismatch that fails loudly — they are a stale response silently accepted as
/// the answer to a new request. So each lease is given its own span of the id
/// space and the ids a job writes are offsets within it. 2^32 requests is four
/// billion chunks; the span is a bound that cannot be reached in practice, and
/// `wire_request_id` refuses rather than wraps if one ever is.
const LEASE_ID_SPAN: u64 = 1 << 32;

/// A job's request id placed in its lease's span of the connection's id space.
pub(super) fn wire_request_id(id_offset: u64, request_id: u64) -> Result<u64, String> {
    if request_id >= LEASE_ID_SPAN {
        return Err(format!(
            "bulk request {request_id} exceeds the {LEASE_ID_SPAN} requests one connection lease may make"
        ));
    }
    Ok(id_offset + request_id)
}

/// What makes two bulk connections interchangeable.
///
/// The handshake binds a bulk connection to one server identity and one control
/// epoch (`BulkProtocolClient::handshake`), and the host builds its per-connection
/// file-transfer state when the connection is made. A pooled connection may
/// therefore only be handed to a job that would have made exactly that
/// handshake — anything else and the reuse would be laundering a stale binding
/// past the check that exists to catch it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BulkKey {
    connection: ConnectionSpec,
    server_identity: String,
    connection_epoch: u64,
}

struct IdleBridge {
    key: BulkKey,
    bridge: Bridge,
    since: Instant,
}

/// The handles a live bulk bridge is: the process, its pipes, and the frame
/// decoder that may still be holding bytes read past the last response.
struct Bridge {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    decoder: FrameAccumulator,
    /// The first request id the next lease on this bridge may use.
    next_id_offset: u64,
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
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn idle() -> &'static Mutex<Vec<IdleBridge>> {
    static IDLE: OnceLock<Mutex<Vec<IdleBridge>>> = OnceLock::new();
    IDLE.get_or_init(|| Mutex::new(Vec::new()))
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
/// A lease reuses all of it. What it does *not* do is reuse a connection whose
/// stream position is in doubt — see `Bridge::clean`.
pub(super) struct BulkLease {
    key: BulkKey,
    bridge: Option<Bridge>,
    id_offset: u64,
}

impl BulkLease {
    pub(super) fn acquire(
        connection: &ConnectionSpec,
        binding: &BulkBinding,
    ) -> Result<Self, String> {
        let key = BulkKey {
            connection: connection.clone(),
            server_identity: binding.expected_server_identity.clone(),
            connection_epoch: binding.connection_epoch,
        };
        if let Some(bridge) = take_idle(&key) {
            // A reused bridge is still only usable while the control connection
            // it was bound to is the live one, and that is exactly what the
            // handshake checked when it was made. Re-checking the binding here
            // keeps the guarantee without re-paying for the handshake.
            binding.validate()?;
            let id_offset = bridge.next_id_offset;
            return Ok(Self {
                key,
                bridge: Some(bridge),
                id_offset,
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
            next_id_offset: 0,
            clean: true,
        };
        // The handshake is part of establishing the bridge, not part of the job:
        // a reused bridge has already made it, which is one of the round trips
        // this pool exists to stop paying.
        if let Err(error) =
            BulkProtocolClient::handshake(&mut bridge.stdin, &mut bridge.reader, binding)
        {
            bridge.kill();
            return Err(error);
        }
        Ok(Self {
            key,
            bridge: Some(bridge),
            id_offset: 0,
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
        let offset = self.id_offset;
        let bridge = self.bridge.as_mut().expect("a lease holds its bridge");
        BulkProtocolClient::resumed(
            &mut bridge.stdin,
            &mut bridge.reader,
            &mut bridge.decoder,
            offset,
            &mut bridge.clean,
        )
    }
}

impl Drop for BulkLease {
    fn drop(&mut self) {
        let Some(mut bridge) = self.bridge.take() else {
            return;
        };
        if !bridge.clean {
            bridge.kill();
            return;
        }
        bridge.next_id_offset = self.id_offset + LEASE_ID_SPAN;
        let mut pool = idle().lock().unwrap();
        expire(&mut pool);
        pool.push(IdleBridge {
            key: self.key.clone(),
            bridge,
            since: Instant::now(),
        });
        // Oldest first, so the bridge dropped here is the one most likely to
        // still be warm on the other side.
        while pool.len() > MAX_IDLE {
            pool.remove(0).bridge.kill();
        }
    }
}

fn take_idle(key: &BulkKey) -> Option<Bridge> {
    let mut pool = idle().lock().unwrap();
    expire(&mut pool);
    let index = pool.iter().position(|entry| &entry.key == key)?;
    Some(pool.remove(index).bridge)
}

/// Closes connections nobody came back for.
///
/// Called on every acquire and every release rather than from a timer thread:
/// the pool only ever holds entries because a job put them there, and a process
/// that has stopped doing file operations is exactly the one whose next event
/// would otherwise be an expiry tick with nothing to do.
fn expire(pool: &mut Vec<IdleBridge>) {
    let now = Instant::now();
    let mut index = 0;
    while index < pool.len() {
        if now.duration_since(pool[index].since) >= IDLE_TIMEOUT {
            pool.remove(index).bridge.kill();
        } else {
            index += 1;
        }
    }
}

/// Drops every pooled connection. The control connection being replaced makes
/// every bulk binding stale, so the bridges bound to it are no longer reusable
/// by anyone and are closed rather than waiting out their idle timeout.
pub(crate) fn close_pooled_bulk_bridges() {
    let mut pool = idle().lock().unwrap();
    for entry in pool.drain(..) {
        entry.bridge.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bridge_is_only_interchangeable_within_one_identity_and_epoch() {
        let base = BulkKey {
            connection: ConnectionSpec::Local,
            server_identity: "server".into(),
            connection_epoch: 3,
        };
        assert_eq!(base, base.clone());
        assert_ne!(
            base,
            BulkKey {
                server_identity: "other".into(),
                ..base.clone()
            }
        );
        assert_ne!(
            base,
            BulkKey {
                connection_epoch: 4,
                ..base.clone()
            }
        );
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
    }

    #[test]
    fn expiry_closes_only_what_has_been_idle_too_long() {
        let mut pool = Vec::new();
        // Nothing to close, and no panic on an empty pool: `expire` runs on
        // every acquire, including the very first one.
        expire(&mut pool);
        assert!(pool.is_empty());
    }

    /// The subtle half of reuse. `BulkProtocolClient` *skips* frames whose id
    /// does not match the one it is waiting for, so two jobs sharing a
    /// connection and both starting at request 2 would let a stale response be
    /// read as the answer to a new request. Leases must not overlap.
    #[test]
    fn one_lease_can_never_reach_the_next_lease_s_request_ids() {
        let first = 0;
        let second = first + LEASE_ID_SPAN;
        assert_eq!(wire_request_id(first, 2).unwrap(), 2);
        assert_eq!(wire_request_id(second, 2).unwrap(), LEASE_ID_SPAN + 2);
        assert!(wire_request_id(first, LEASE_ID_SPAN - 1).unwrap() < second);
        // And a job that somehow made four billion requests is refused rather
        // than quietly wrapping into the next lease's ids.
        assert!(
            wire_request_id(first, LEASE_ID_SPAN)
                .unwrap_err()
                .contains("one connection lease may make")
        );
    }
}
