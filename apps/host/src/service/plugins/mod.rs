//! Optional features layered on the daemon.
//!
//! Each plugin is started and fed from here and nowhere else, so removing one
//! is deleting its folder and its lines below. Nothing else in the host names a
//! plugin.

mod automations;

/// Daemon-lifetime tasks, started once beside the hook timer and aborted with
/// it. They run whether or not any client is connected.
pub(crate) fn start() -> Vec<tokio::task::JoinHandle<()>> {
    vec![automations::start()]
}

/// A `Stop` hook's final message, after ingest has accepted it.
///
/// Called from the agent runtime's reply sink while ingest holds its ordering
/// lock, so a plugin may only record the reply here; anything that forks or
/// blocks belongs on its own task.
pub(crate) fn on_agent_reply(reply: &super::voice::AgentReply) {
    automations::on_agent_reply(reply);
}
