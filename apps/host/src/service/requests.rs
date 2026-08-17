use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use tmux_agent_protocol::v1;
use tokio::{sync::mpsc, time::sleep};

use super::active_root::resolve_cached;
use super::filesystem::{FileService, root_token, validate_root_token};
use super::snapshot::discover_authoritative;
use super::terminal::VisibilityChange;
use super::{
    EVENT_QUEUE, SequencerControl, TerminalClients, broadcast_control_event, emit_event,
    lock_topology_generation, reconcile_internal_tmux_change, reconcile_terminal_clients,
    response_error, response_ok, response_snapshot, same_action_topology, send_response,
    send_snapshot_response, snapshot_from_identity, testing_enabled, tmux_actions,
};

mod active_root_dispatch;
mod agent_dispatch;
mod dispatcher;
mod filesystem_dispatch;
pub(super) mod git_dispatch;
pub(super) mod operation_policy;
mod tmux_action_dispatch;
pub(super) use dispatcher::handle_request;

pub(super) struct RequestContext {
    pub(super) control_tx: mpsc::Sender<SequencerControl>,
    pub(super) event_tx: mpsc::Sender<SequencerControl>,
    pub(super) generation: Arc<AtomicU64>,
    pub(super) overflowed: Arc<AtomicBool>,
    pub(super) subscribed: Arc<AtomicBool>,
    pub(super) pending: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    pub(super) terminal: Arc<Mutex<TerminalClients>>,
    pub(super) topology_lock: Arc<tokio::sync::Mutex<()>>,
    pub(super) topology_baseline: Arc<Mutex<Option<(tmux_control::TmuxSnapshot, String)>>>,
    pub(super) topology_signal: super::topology::TopologySignal,
    pub(super) files: Arc<FileService>,
    pub(super) git: Arc<super::git::GitService>,
    pub(super) bulk_connection: bool,
    /// Whether this connection may open the independent bulk lane at all. A
    /// read-only host refuses one, so its Git diff bodies cannot be deferred.
    pub(super) bulk_available: bool,
    pub(super) connection_epoch: u64,
    pub(super) closed: Arc<AtomicBool>,
}

fn git_response(operation_id: &str, update: impl FnOnce(&mut v1::GitResponse)) -> v1::Response {
    let mut git = v1::GitResponse {
        operation_id: operation_id.to_owned(),
        ..Default::default()
    };
    update(&mut git);
    v1::Response {
        ok: true,
        git: Some(git),
        ..Default::default()
    }
}

fn file_response(
    operation_id: &str,
    update: impl FnOnce(&mut v1::FileServiceResponse),
) -> v1::Response {
    let mut file = v1::FileServiceResponse {
        operation_id: operation_id.to_owned(),
        ..Default::default()
    };
    update(&mut file);
    v1::Response {
        ok: true,
        file: Some(file),
        ..Default::default()
    }
}

struct SnapshotRequest {
    request_id: u64,
    subscribe_after: bool,
    sender: mpsc::Sender<SequencerControl>,
    generation: Arc<AtomicU64>,
    cancellation: Arc<AtomicBool>,
    pending: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    subscribed: Arc<AtomicBool>,
    topology_lock: Arc<tokio::sync::Mutex<()>>,
    topology_baseline: Arc<Mutex<Option<(tmux_control::TmuxSnapshot, String)>>>,
    terminal: Arc<Mutex<TerminalClients>>,
    event_sender: mpsc::Sender<SequencerControl>,
    overflowed: Arc<AtomicBool>,
}

fn spawn_snapshot_request(request: SnapshotRequest) {
    let SnapshotRequest {
        request_id,
        subscribe_after,
        sender,
        generation,
        cancellation,
        pending,
        subscribed,
        topology_lock,
        topology_baseline,
        terminal,
        event_sender,
        overflowed,
    } = request;
    tokio::spawn(async move {
        let _topology_guard = topology_lock.lock().await;
        if cancellation.load(Ordering::Acquire) {
            send_response(
                &sender,
                request_id,
                response_error("cancelled", "request was cancelled"),
            )
            .await;
            pending.lock().unwrap().remove(&request_id);
            return;
        }
        let result = tokio::task::spawn_blocking(discover_authoritative).await;
        if cancellation.load(Ordering::Acquire) {
            send_response(
                &sender,
                request_id,
                response_error("cancelled", "request was cancelled"),
            )
            .await;
        } else {
            match result {
                Ok(Ok((value, identity))) => {
                    let generation = generation.fetch_add(1, Ordering::AcqRel) + 1;
                    *topology_baseline.lock().unwrap() = Some((value.clone(), identity.clone()));
                    send_snapshot_response(
                        &sender,
                        request_id,
                        response_snapshot(snapshot_from_identity(
                            value.clone(),
                            generation,
                            identity,
                        )),
                    )
                    .await;
                    reconcile_terminal_clients(&terminal, &value, &event_sender, &overflowed);
                    if subscribe_after {
                        subscribed.store(true, Ordering::Release);
                    }
                }
                Ok(Err(error)) => {
                    send_response(
                        &sender,
                        request_id,
                        response_error("tmux_unavailable", &error.to_string()),
                    )
                    .await;
                }
                Err(error) => {
                    send_response(
                        &sender,
                        request_id,
                        response_error("snapshot_task_failed", &error.to_string()),
                    )
                    .await;
                }
            }
        }
        pending.lock().unwrap().remove(&request_id);
    });
}
