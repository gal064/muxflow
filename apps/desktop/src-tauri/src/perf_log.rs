//! Opt-in Phase 12/14 measurement façade.
//!
//! Persistence, bridge admission, and native operation accounting deliberately
//! live in separate modules so adding a counter cannot grow a second runtime
//! coordinator inside the log sink.
//!
//! Compile-out scheme: the real implementation is compiled whenever
//! `debug_assertions` is on (plain `cargo build` / `cargo test`) or the
//! non-default `perf-log` cargo feature is enabled. A plain release build
//! (`cargo build --release`, `tauri build`) therefore contains only the inert
//! stubs in `stub.rs`; a measurement campaign re-enables the real code with
//! `cargo build --release --features perf-log`. Call sites stay unconditional
//! because the stubs keep identical module paths and signatures.

#[cfg(any(debug_assertions, feature = "perf-log"))]
pub(crate) mod bridge;
#[cfg(any(debug_assertions, feature = "perf-log"))]
pub(crate) mod operations;
#[cfg(any(debug_assertions, feature = "perf-log"))]
pub(crate) mod sink;

#[cfg(not(any(debug_assertions, feature = "perf-log")))]
mod stub;
#[cfg(not(any(debug_assertions, feature = "perf-log")))]
pub(crate) use stub::{bridge, operations, sink};

pub(crate) use bridge::{quiesce_bridge_measurement, send_bridge_frame};
pub(crate) use operations::{
    RemoteOperation, TransferAdmission, record_remote_operation, record_transfer_admission,
    record_transfer_completion, record_transfer_state,
};

#[cfg(test)]
pub(crate) use operations::{reset_transfer_measurements, transfer_measurements_for_test};
