//! Opt-in Phase 12/14 measurement façade.
//!
//! Persistence, bridge admission, and native operation accounting deliberately
//! live in separate modules so adding a counter cannot grow a second runtime
//! coordinator inside the log sink.

pub(crate) mod bridge;
pub(crate) mod operations;
pub(crate) mod sink;

pub(crate) use bridge::{quiesce_bridge_measurement, send_bridge_frame};
pub(crate) use operations::{
    RemoteOperation, TransferAdmission, record_remote_operation, record_transfer_admission,
    record_transfer_completion, record_transfer_state,
};

#[cfg(test)]
pub(crate) use operations::{reset_transfer_measurements, transfer_measurements_for_test};
