//! What the host does about tmux's `pause-after` flow control.
//!
//! tmux pauses a pane's output when the control client falls behind, and
//! **drops** what the pane produces while paused rather than replaying it. So
//! the only thing that brings a paused pane back is
//! `refresh-client -A '<pane>:continue'`, and a capture written without one
//! re-photographs a screen that then stops moving again — which is precisely
//! the reported symptom: switching to another tab and back refreshes the pane
//! once, and it freezes.
//!
//! Two threads participate and have to agree, which is why this state is shared
//! rather than owned. The control-stream reader learns that a pane was paused
//! (`%pause`), that it came back (`%continue`), and that a resume was rejected
//! (`%error` on the correlated block). The service thread is what writes a seed
//! when the desktop reveals a pane or asks for recovery, and it has to know
//! whether that seed needs a resume in front of it.

use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// How many `refresh-client -A <pane>:continue` writes one flow-control episode
/// is worth: the one issued with the pause, and one retry.
///
/// A retry rather than a loop, because the two ways a resume is rejected have
/// opposite answers. A transient rejection clears on the next write. A
/// deterministic one — the P12-U001 lexer shape, a tmux that does not support
/// the flag — never does, and repeating it would be a command per rejection
/// forever against a pane that is not coming back on its own.
const MAX_RESUME_ATTEMPTS: u8 = 2;

/// What to do about a resume tmux just refused.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RejectedResume {
    /// Write it again. The pane is still paused and a resume is the only thing
    /// that changes that.
    Retry,
    /// Out of attempts. Report the pane stalled and stop resuming it — see
    /// [`FlowControl::reject_resume`] for why stopping is the load-bearing half.
    Stall,
    /// Nothing paused this pane, so this is a correlation failure rather than a
    /// flow-control one and there is nothing here to say about it.
    Ignore,
}

impl RejectedResume {
    /// One word for the daemon log, so the disposition is greppable.
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Retry => "retried",
            Self::Stall => "stalled",
            Self::Ignore => "not-paused",
        }
    }
}

/// The panes tmux has paused, and how many resumes have been spent on each.
#[derive(Default)]
pub(super) struct FlowControl(Mutex<HashMap<String, u8>>);

impl FlowControl {
    /// tmux paused this pane. A new episode gets a new budget, including for a
    /// pane that was previously written off as stalled: what stalled was one
    /// episode's resume, and tmux pausing the pane again is fresh evidence that
    /// the pane is alive and being flow-controlled.
    pub(super) fn paused(&self, pane_id: &str) {
        self.0.lock().unwrap().insert(pane_id.to_owned(), 0);
    }

    /// tmux resumed it, or it is no longer ours to resume.
    pub(super) fn cleared(&self, pane_id: &str) {
        self.0.lock().unwrap().remove(pane_id);
    }

    /// Whether a capture for this pane has to carry a resume in front of it.
    ///
    /// A pane tmux never says `%continue` for stays true here, and every later
    /// capture for it carries a redundant resume. That is the deliberate
    /// direction to be wrong in: a resume for a pane that is not paused is one
    /// extra `refresh-client` on a path that runs on reveals and recoveries,
    /// and a missing one silences the pane for the rest of the session.
    pub(super) fn resume_before_capture(&self, pane_id: &str) -> bool {
        self.0.lock().unwrap().contains_key(pane_id)
    }

    /// Decides what a rejected resume for this pane is worth, and spends it.
    ///
    /// `Stall` **forgets** the pane, and that is not bookkeeping tidiness: it is
    /// what stops the recovery eating itself. A stall asks the desktop for a
    /// seed, the desktop's seed request comes back through
    /// `TerminalAttachment::request_seed`, and while this still called the pane
    /// paused that seed would carry another resume — which the same broken
    /// command would refuse, producing another stall, another seed, forever. The
    /// host has established that it cannot resume this pane; the seed it asked
    /// for is a plain capture, and the loop ends there. A later `%pause` starts
    /// a new episode with a new budget.
    pub(super) fn reject_resume(&self, pane_id: &str) -> RejectedResume {
        let mut paused = self.0.lock().unwrap();
        let Some(spent) = paused.get_mut(pane_id) else {
            return RejectedResume::Ignore;
        };
        if *spent + 1 >= MAX_RESUME_ATTEMPTS {
            paused.remove(pane_id);
            return RejectedResume::Stall;
        }
        *spent += 1;
        RejectedResume::Retry
    }
}

/// The one command that takes a pane out of tmux's flow-control pause.
///
/// The quotes are load-bearing. tmux's command lexer (`cmd-parse.y` `yylex`)
/// treats an unquoted word beginning with `%` as a `%if`-style conditional
/// directive unless the rest of the word is digits or `%`; `%5:continue`
/// contains `:`, so the unquoted form is a `parse error: syntax error` and the
/// pane stays paused (P12-U001).
/// `resume_command_quotes_the_pause_argument_tmux_lexer_rejects` pins the
/// byte-exact form.
///
/// Pure, and deliberately so: `rejected` is passed in rather than read here,
/// because a formatter that consumed a global counter would return different
/// bytes on identical calls, and the first refactor that logged the command
/// before writing it would silently spend the injection on the log.
/// [`take_injected_rejection`] is called once, at the write.
pub(super) fn resume_command(pane_id: &str, rejected: bool) -> String {
    if rejected {
        return format!("refresh-client -A {pane_id}:continue");
    }
    format!("refresh-client -A '{pane_id}:continue'")
}

/// Consumes one injected rejection, if any are configured and any are left.
///
/// `ADE_TEST_REJECT_FLOW_RESUME=<n>` writes the unquoted form back for the first
/// `n` resumes a process sends, which is what gives the pause lane a
/// reproducible rejected resume — the half of this mechanism a healthy tmux
/// never exercises. A count rather than a switch, because a permanently broken
/// resume is not something this code can recover a pane from: nothing can, if
/// the only command that resumes it never parses.
///
/// Gated on `ADE_PHASE1_TESTING` like every other fault injection in this crate,
/// and read once — a daemon does not change its mind about being a test daemon.
pub(super) fn take_injected_rejection() -> bool {
    static REMAINING: std::sync::OnceLock<AtomicU64> = std::sync::OnceLock::new();
    REMAINING
        .get_or_init(|| {
            AtomicU64::new(if std::env::var_os("ADE_PHASE1_TESTING").is_some() {
                std::env::var("ADE_TEST_REJECT_FLOW_RESUME")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0)
            } else {
                0
            })
        })
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| {
            (left > 0).then(|| left - 1)
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget is what separates a transient rejection from a permanent one.
    #[test]
    fn a_rejection_buys_one_retry_and_then_a_stall() {
        let flow = FlowControl::default();
        flow.paused("%1");
        assert_eq!(flow.reject_resume("%1"), RejectedResume::Retry);
        assert_eq!(flow.reject_resume("%1"), RejectedResume::Stall);
    }

    /// The stall has to stop this pane's captures carrying a resume, or the
    /// seed it asks for produces the next rejection, which asks for the next
    /// seed, and the recovery never terminates.
    #[test]
    fn a_stalled_pane_stops_asking_for_resumes_it_cannot_get() {
        let flow = FlowControl::default();
        flow.paused("%1");
        assert!(flow.resume_before_capture("%1"));
        flow.reject_resume("%1");
        assert!(flow.resume_before_capture("%1"), "a retry still needs one");
        flow.reject_resume("%1");
        assert!(
            !flow.resume_before_capture("%1"),
            "a stalled pane's seed must be a plain capture, or the loop feeds itself"
        );
        assert_eq!(
            flow.reject_resume("%1"),
            RejectedResume::Ignore,
            "and nothing further is owed about it"
        );
    }

    /// A pane written off in one episode is not written off for the connection:
    /// tmux pausing it again is fresh evidence that it is alive.
    #[test]
    fn a_new_pause_is_a_new_budget() {
        let flow = FlowControl::default();
        flow.paused("%1");
        flow.reject_resume("%1");
        flow.reject_resume("%1");
        flow.paused("%1");
        assert!(flow.resume_before_capture("%1"));
        assert_eq!(flow.reject_resume("%1"), RejectedResume::Retry);
    }

    #[test]
    fn a_resume_rejected_for_a_pane_nothing_paused_is_not_flow_control() {
        let flow = FlowControl::default();
        assert_eq!(flow.reject_resume("%9"), RejectedResume::Ignore);
        assert!(!flow.resume_before_capture("%9"));
    }

    #[test]
    fn continue_and_pane_loss_both_end_an_episode() {
        let flow = FlowControl::default();
        flow.paused("%1");
        flow.cleared("%1");
        assert!(!flow.resume_before_capture("%1"));
        assert_eq!(flow.reject_resume("%1"), RejectedResume::Ignore);
    }

    /// The quoted form is the only one a daemon writes unless a test asks
    /// otherwise, and the unquoted one is the exact P12-U001 shape.
    #[test]
    fn the_resume_command_quotes_what_tmux_would_otherwise_read_as_a_directive() {
        assert_eq!(
            resume_command("%5", false),
            "refresh-client -A '%5:continue'"
        );
        assert_eq!(resume_command("%5", true), "refresh-client -A %5:continue");
    }
}
