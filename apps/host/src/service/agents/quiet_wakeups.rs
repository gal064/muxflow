/// EXPERIMENT: stay quiet on background wakeup turns. We are using this and
/// evaluating whether it works, so it may be changed or removed.
///
/// Why this exists: Claude Code fires `Stop` at the end of every turn, including
/// turns started by a background shell or Monitor reporting in. Treating each of
/// those as finished sent a "done" notification on every wakeup, so one long job
/// produced a stream of false ones.
///
/// The rule: a turn the user started always notifies when it ends. A turn started
/// by a background wakeup stays Working, with no notification, while background
/// work is still running. The wakeup turn that ends with nothing left running
/// sends the real "done".
///
/// Why it looks at who started the turn: a running background shell can be a job
/// the agent is waiting for or a dev server it forgot to stop, and Claude's hook
/// data does not tell them apart. Holding on every shell would lose notifications
/// to forgotten servers, and guessing from the command text is fragile.
///
/// Known costs to weigh while evaluating:
/// - A turn that launches long work notifies once early, then again at the real
///   end.
/// - If the last wakeup turn leaves only a forgotten server running, nothing
///   notifies and the agent shows Working until the 24-hour stale limit.
///
/// How to evaluate: every notification held back is recorded in the lifecycle
/// diagnostics with the reason `quiet_wakeup_held`. Compare those against real
/// completions you missed.
///
/// `ENABLED` is the off switch. Set it to `false` and Muxflow behaves as it did
/// before this experiment. Callers must go through the functions below and not
/// read their inputs directly, so that this stays the only line to change.
const ENABLED: bool = true;

pub(super) fn stay_working(turn_started_by_wakeup: bool, background_work_left: bool) -> bool {
    ENABLED && turn_started_by_wakeup && background_work_left
}

/// Claude's routine idle notice is not the agent asking anything, and it can
/// arrive while a wakeup turn is still open.
pub(super) fn ignore_idle_notice(turn_started_by_wakeup: bool) -> bool {
    ENABLED && turn_started_by_wakeup
}

#[cfg(test)]
mod tests {
    use super::stay_working;

    #[test]
    fn only_a_wakeup_turn_with_work_left_stays_working() {
        assert!(stay_working(true, true));
        assert!(!stay_working(true, false));
        assert!(!stay_working(false, true));
        assert!(!stay_working(false, false));
    }
}
