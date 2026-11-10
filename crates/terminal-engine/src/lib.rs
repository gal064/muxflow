//! One `alacritty_terminal` per pane, and the Rust port of the desktop's
//! terminal event path: `TerminalEventHub` (generations, seeds, backlog, pane
//! health), `PaneHistoryPager`, and the pane handshake around them.
//!
//! No UI toolkit, and no clock or timer of its own: every state machine takes
//! `now`, and [`Engine`] is single-threaded. [`spawn`] is the thin thread that
//! gives it an inbox and a deadline, so all of this runs in tests with no display.

mod deferred;
mod engine;
mod event;
mod history_page;
mod hub;
mod pager;
mod pane;
mod reading;
mod reveal;
mod reveal_retry;
mod terminal;
mod watchdog;

pub use deferred::{DeferredOutput, DeferredOutputAdmission, DeferredTerminalOutputQueue};
pub use engine::{Command, Engine, EngineHandle, spawn};
pub use event::{
    GridSize, PaneEvent, PaneGrid, PaneResource, PaneResourceState, TerminalEvent, decode_frame,
};
pub use history_page::{compose_history_page, split_history_rows, visible_width};
pub use hub::{
    Admission, HubLimits, HubOutput, PaneHealth, TerminalEventHub, VisibilityCheckpoint,
};
pub use pager::{CachedHistoryState, HistoryPageTrigger, PageRequest, PaneHistoryPager};
pub use pane::EngineOutput;
pub use reading::TerminalReadingState;
pub use reveal::{
    PaneRecoveryPlan, PaneRevealEffect, PaneRevealState, output_after_recovery, reduce_pane_reveal,
};
pub use reveal_retry::{RevealFailureAction, reveal_failure_action};
pub use terminal::{
    GridOutcome, HistoryPageAnchor, HistorySpliceOutcome, Listener, PaneTerminal, SCROLLBACK_LIMIT,
    TerminalFacts,
};
pub use watchdog::{PaneDegradedReason, PaneDegradedWatchdog, WatchdogRetry};
