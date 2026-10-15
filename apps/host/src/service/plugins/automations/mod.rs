//! Automations: an agent run on a cron schedule, each run in a new tab of one
//! `automation` workspace. See docs/automations.md.
//!
//! Definitions live in the state directory's `automations.json`, which people
//! and agents edit directly, and the desktop through `dispatch`. The helper
//! runs them from its own timer, whether or not a desktop is connected.

use std::{
    collections::BTreeSet,
    sync::{Mutex, MutexGuard, OnceLock, PoisonError},
    time::Duration,
};

use tmux_agent_protocol::v1;

mod clock;
mod cron;
mod dispatch;
mod launch;
mod scheduler;
mod store;
#[cfg(test)]
mod test_support;
mod track;

pub(super) use dispatch::handle as handle_request;
use scheduler::{Env, State};

const TICK: Duration = Duration::from_secs(15);

static STATE: OnceLock<Mutex<State>> = OnceLock::new();
/// Connections that listed automations, and so want to hear they changed.
static SUBSCRIBERS: Mutex<BTreeSet<u64>> = Mutex::new(BTreeSet::new());

pub(super) fn start() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut owed = false;
        loop {
            interval.tick().await;
            let changed = tokio::task::spawn_blocking(|| with_env(|env| state().tick(env)))
                .await
                .unwrap_or(false);
            // A subscriber whose queue was full hears about it next tick.
            owed = (changed || owed) && !notify();
        }
    })
}

pub(super) fn on_agent_reply(reply: &crate::service::voice::AgentReply) {
    track::queue_reply(reply);
}

/// The scheduler's state. A tick that panicked leaves it as it was; the next
/// one goes on.
fn state() -> MutexGuard<'static, State> {
    STATE
        .get_or_init(|| {
            Mutex::new(State::new(
                store::Paths::under(&crate::paths::state_dir()),
                now_ms(),
            ))
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn subscribe(connection_id: u64) {
    SUBSCRIBERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(connection_id);
}

/// Tells every subscriber to list again. Returns whether each one either got
/// it or has gone.
fn notify() -> bool {
    let event = v1::HostEvent {
        kind: v1::EventKind::Automation.into(),
        automation: Some(v1::AutomationEvent { changed: true }),
        ..Default::default()
    };
    let mut delivered = true;
    SUBSCRIBERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|connection_id| {
            match crate::service::send_control_event_to(*connection_id, event.clone()) {
                crate::service::events::Delivery::Sent => true,
                crate::service::events::Delivery::Full => {
                    delivered = false;
                    true
                }
                crate::service::events::Delivery::Gone => false,
            }
        });
    delivered
}

/// Runs `body` with the real tmux server, clock and agent runtime.
fn with_env<R>(body: impl FnOnce(&Env) -> R) -> R {
    let tmux = crate::service::snapshot::tmux_command;
    let identity = crate::service::snapshot::server_identity;
    body(&Env {
        tmux: &tmux,
        now_ms: &now_ms,
        local: &clock::local_minute,
        server_identity: &identity,
        agents: &|identity| {
            crate::service::agents::AgentRuntime::global()
                .snapshot_for(identity)
                .agents
        },
        embedded_codex: &|| {
            identity() == "tmux:none"
                && crate::service::agents::HookManager::system_default()
                    .and_then(|hooks| hooks.managed_adapters())
                    .is_ok_and(|managed| managed.contains(&"codex"))
        },
        replies: &track::take_replies,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}
