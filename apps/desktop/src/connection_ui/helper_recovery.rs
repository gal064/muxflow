//! Keeps the helper on an SSH host aligned with the one this desktop ships.
//! Port of `app/useMissingHelperRecovery.ts`, without the renderer plumbing.
//!
//! A failed handshake may mean the helper is absent or incompatible, and a
//! successful one proves only the protocol contract, so both ask the
//! digest-aware probe. Nothing is decided on a probe that could not complete:
//! an unreachable host fails the probe too, and the original connection error
//! is the honest thing to leave on screen for that.

use desktop_core::connection::ConnectionSpec;

use super::{
    connection_reducer::ConnectionPhase,
    helper_upgrade::{
        HelperUpgradeAction, HelperUpgradeState, RemoteHelperProbe, helper_upgrade_reducer,
    },
    host_profiles::helper_connection_key,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reason {
    Connected,
    HandshakeFailure,
    Manual,
}

#[derive(Clone, Debug)]
struct ActiveProbe {
    connection_key: String,
    generation: u64,
    key: String,
    request_upgrade: bool,
    survives_transport_drop: bool,
}

/// One probe to run, identified so a stale answer can be recognised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeRequest {
    pub generation: u64,
    pub connection: ConnectionSpec,
}

#[derive(Debug, Default)]
pub struct HelperRecovery {
    scope_key: String,
    connection_key: String,
    attempted_connected: Option<String>,
    attempted_handshake: Option<String>,
    generation: u64,
    active: Option<ActiveProbe>,
    settled_key: Option<String>,
}

impl HelperRecovery {
    /// The active connection or its epoch changed: drop everything scoped to
    /// the old one, including the helper state a dialog might be showing.
    pub fn set_scope(
        &mut self,
        connection: &ConnectionSpec,
        epoch: u64,
        state: &mut HelperUpgradeState,
    ) {
        let key = format!("{}:{epoch}", helper_connection_key(connection));
        if key == self.scope_key {
            return;
        }
        self.scope_key = key;
        self.connection_key = helper_connection_key(connection);
        self.attempted_connected = None;
        self.attempted_handshake = None;
        self.settled_key = None;
        if self.active.take().is_some() {
            self.generation += 1;
        }
        helper_upgrade_reducer(state, HelperUpgradeAction::Reset);
    }

    pub fn handshake_failed(
        &mut self,
        connection: &ConnectionSpec,
        state: &mut HelperUpgradeState,
    ) -> Option<ProbeRequest> {
        self.probe(connection, Reason::HandshakeFailure, true, state)
    }

    /// The Settings button: informational, it never opens the upgrade prompt.
    pub fn probe_manually(
        &mut self,
        connection: &ConnectionSpec,
        state: &mut HelperUpgradeState,
    ) -> Option<ProbeRequest> {
        self.probe(connection, Reason::Manual, false, state)
    }

    pub fn connection_state_changed(
        &mut self,
        connection: &ConnectionSpec,
        phase: ConnectionPhase,
        state: &mut HelperUpgradeState,
    ) -> Option<ProbeRequest> {
        if matches!(connection, ConnectionSpec::Local) {
            return None;
        }
        if phase == ConnectionPhase::Connected {
            return self.probe(connection, Reason::Connected, true, state);
        }
        if phase == ConnectionPhase::Resyncing {
            return None;
        }
        if self.connection_key != helper_connection_key(connection) {
            return None;
        }
        let key = self.scope_key.clone();
        if self.attempted_connected.as_deref() == Some(key.as_str()) {
            self.attempted_connected = None;
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.key == key && !active.survives_transport_drop)
        {
            let active = self.active.take().expect("checked above");
            self.generation += 1;
            helper_upgrade_reducer(
                state,
                HelperUpgradeAction::AbandonProbe {
                    connection_key: active.connection_key,
                },
            );
        }
        if self.settled_key.as_deref() == Some(key.as_str()) {
            self.settled_key = None;
            helper_upgrade_reducer(
                state,
                HelperUpgradeAction::InvalidateConnectionProbe {
                    connection_key: self.connection_key.clone(),
                },
            );
        }
        None
    }

    /// A probe's answer. Ignored unless it answers the request still in flight:
    /// a host switch or a reconnect makes it about a machine this app has left.
    pub fn settle(
        &mut self,
        generation: u64,
        result: Result<RemoteHelperProbe, String>,
        state: &mut HelperUpgradeState,
    ) {
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.generation != generation)
        {
            return;
        }
        let request = self.active.take().expect("checked above");
        self.settled_key = (!request.survives_transport_drop).then(|| request.key.clone());
        match result {
            Ok(probe) => {
                let upgrade = request.request_upgrade && (!probe.installed || !probe.compatible);
                helper_upgrade_reducer(
                    state,
                    HelperUpgradeAction::ProbeSucceeded {
                        connection_key: request.connection_key,
                        probe,
                    },
                );
                if upgrade {
                    helper_upgrade_reducer(state, HelperUpgradeAction::RequestUpgrade);
                }
            }
            // No connection detail here. On a failed handshake its original
            // error is more useful, and on a live connection this secondary
            // check must not make a healthy terminal read as disconnected.
            Err(message) => helper_upgrade_reducer(
                state,
                HelperUpgradeAction::ProbeFailed {
                    connection_key: request.connection_key,
                    message,
                },
            ),
        }
    }

    fn probe(
        &mut self,
        connection: &ConnectionSpec,
        reason: Reason,
        request_upgrade: bool,
        state: &mut HelperUpgradeState,
    ) -> Option<ProbeRequest> {
        if matches!(connection, ConnectionSpec::Local) {
            return None;
        }
        let connection_key = helper_connection_key(connection);
        if self.connection_key != helper_connection_key(connection) {
            return None;
        }
        let key = self.scope_key.clone();
        // The bridge supervisor retries in a loop: one question per connection,
        // not per attempt. An explicit reconnect gets a new scope, and a native
        // in-place reconnect is re-armed by its connection-state events.
        let attempted = match reason {
            Reason::Connected => Some(&mut self.attempted_connected),
            Reason::HandshakeFailure => Some(&mut self.attempted_handshake),
            Reason::Manual => None,
        };
        if let Some(attempted) = attempted {
            if attempted.as_deref() == Some(key.as_str()) {
                return None;
            }
            *attempted = Some(key.clone());
        }
        if let Some(active) = self.active.as_mut().filter(|active| active.key == key) {
            let transport_supersedes_recovery =
                reason == Reason::Connected && active.survives_transport_drop;
            let recovery_supersedes_transport =
                reason == Reason::HandshakeFailure && !active.survives_transport_drop;
            if transport_supersedes_recovery || recovery_supersedes_transport {
                // Failed and settled transports are different observations:
                // whichever arrived second gets its own probe.
                self.active = None;
                self.generation += 1;
            } else {
                // One round trip answers every caller. Keep the strongest
                // intent; a manual caller may strengthen intent, never age.
                active.request_upgrade |= request_upgrade;
                active.survives_transport_drop &= reason != Reason::Connected;
                return None;
            }
        }
        self.generation += 1;
        self.active = Some(ActiveProbe {
            connection_key: connection_key.clone(),
            generation: self.generation,
            key,
            request_upgrade,
            survives_transport_drop: reason != Reason::Connected,
        });
        self.settled_key = None;
        helper_upgrade_reducer(state, HelperUpgradeAction::Probe { connection_key });
        Some(ProbeRequest {
            generation: self.generation,
            connection: connection.clone(),
        })
    }
}
