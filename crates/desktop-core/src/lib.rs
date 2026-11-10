//! Muxflow's desktop backend: connections, transfers, profiles, app state,
//! notifications, incidents, update check and the perf-log sink. No UI toolkit;
//! the embedding application reaches it through the traits in [`platform`].

pub mod app_state;
pub mod connection;
pub mod external_links;
pub mod incidents;
pub mod notifications;
pub mod perf_log;
pub mod platform;
pub mod power_events;
pub mod update_check;
