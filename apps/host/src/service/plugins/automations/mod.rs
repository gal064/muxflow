//! Automations: an agent run on a cron schedule, each run in a new tab of one
//! `automation` workspace. See docs/automations.md.
//!
//! Definitions live in the state directory's `automations.json`, which people
//! and agents edit directly. The helper runs them from its own timer, whether
//! or not a desktop is connected.

use std::{
    sync::{Mutex, OnceLock, PoisonError},
    time::Duration,
};

mod clock;
mod cron;
mod launch;
mod scheduler;
mod store;
#[cfg(test)]
mod test_support;
mod track;

use scheduler::{Env, State};

const TICK: Duration = Duration::from_secs(15);

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

pub(super) fn start() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let _ = tokio::task::spawn_blocking(tick).await;
        }
    })
}

pub(super) fn on_agent_reply(reply: &crate::service::voice::AgentReply) {
    track::queue_reply(reply);
}

fn tick() {
    let state = STATE.get_or_init(|| {
        Mutex::new(State::new(
            store::Paths::under(&crate::paths::state_dir()),
            now_ms(),
        ))
    });
    let tmux = crate::service::snapshot::tmux_command;
    let identity = crate::service::snapshot::server_identity;
    // A tick that panicked leaves the state as it was; the next one goes on.
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    state.tick(&Env {
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
    });
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}
