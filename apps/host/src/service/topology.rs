use std::future::Future;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use tmux_agent_protocol::v1;
use tokio::sync::{Notify, mpsc};
use tokio::time::sleep;

use super::snapshot::{discover_authoritative, snapshot_from_reconciled_identity};
use super::terminal::TerminalClients;
use super::{SequencerControl, emit_event, reconcile_terminal_clients_if_open};

/// Whether two authoritative snapshots describe the same publishable topology.
///
/// Agent CLIs animate a recognized status marker at the start of the tmux
/// window title. Those frames remain visible to discovery, but changing only
/// the marker does not change the topology consumers act on.
fn same_publishable_topology(
    previous: &tmux_control::TmuxSnapshot,
    current: &tmux_control::TmuxSnapshot,
) -> bool {
    previous.sessions == current.sessions
        && previous.panes == current.panes
        && previous.windows.len() == current.windows.len()
        && previous
            .windows
            .iter()
            .zip(&current.windows)
            .all(|(previous, current)| {
                previous.id == current.id
                    && previous.session_id == current.session_id
                    && previous.index == current.index
                    && equivalent_agent_title(&previous.name, &current.name)
                    && previous.active == current.active
                    && previous.layout == current.layout
                    && previous.zoomed == current.zoomed
                    && previous.pinned == current.pinned
            })
}

pub(super) fn equivalent_agent_title(previous: &str, current: &str) -> bool {
    previous == current
        || matches!(
            (agent_title_suffix(previous), agent_title_suffix(current)),
            (Some(previous), Some(current)) if previous == current
        )
}

/// Returns the meaningful suffix of a title beginning with one or more known
/// Claude Code or Codex status markers.
fn agent_title_suffix(title: &str) -> Option<&str> {
    let mut remaining = title;
    let mut marked = false;

    loop {
        let (after_marker, accepts_variation_selector) =
            if let Some(after) = remaining.strip_prefix("[ . ]") {
                (after, false)
            } else if let Some(after) = remaining.strip_prefix("[ ! ]") {
                (after, false)
            } else {
                let Some(marker) = remaining.chars().next() else {
                    break;
                };
                if !is_agent_status_glyph(marker) {
                    break;
                }
                (&remaining[marker.len_utf8()..], true)
            };

        marked = true;
        remaining = after_marker;
        if accepts_variation_selector
            && (remaining.starts_with('\u{fe0e}') || remaining.starts_with('\u{fe0f}'))
        {
            remaining = &remaining['\u{fe0e}'.len_utf8()..];
        }
        remaining = remaining.trim_start_matches(char::is_whitespace);
    }

    marked.then(|| remaining.trim())
}

fn is_agent_status_glyph(value: char) -> bool {
    matches!(
        value,
        '\u{00b7}'
            | '\u{2713}'..='\u{2718}'
            | '\u{2722}'
            | '\u{2733}'
            | '\u{2736}'
            | '\u{273b}'
            | '\u{273d}'
            | '\u{25d0}'..='\u{25d3}'
            | '\u{2800}'..='\u{28ff}'
    )
}

/// Backstop for a tmux notification the reader never saw.
///
/// tmux notifies on every structural change, and those notifications are what
/// actually drive reconciliation; this timer only exists for the case where one
/// is missed. Running it every two seconds meant a fully idle connection did a
/// tmux discovery and woke every consumer twice a second forever, which is the
/// opposite of "zero periodic round-trips at idle". Thirty seconds is still a
/// backstop and is invisible at rest.
const SAFETY_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// How long a notified pass waits for the rest of its notification group.
///
/// One structural change is several tmux notifications — closing a window
/// emits four — and they land in the same millisecond. Absorbing them into one
/// pass costs every real change this much latency, so it is deliberately far
/// below what anyone can perceive; it is burst absorption, not rate limiting.
const NOTIFICATION_BURST_WINDOW: Duration = Duration::from_millis(10);

/// Floor on the spacing between the starts of two discovery passes.
///
/// Nothing about the notification source bounds its rate: an agent CLI that
/// animates its window title renames the window at its redraw rate, and tmux
/// reports every rename as a topology notification. Each pass costs a tmux
/// discovery and pushes a full snapshot to the desktop, which re-runs every
/// generation-keyed effect it has. Measuring the gap from the previous pass's
/// start rather than its end means the first change after a quiet period waits
/// only the burst window, while any sustained stream — whatever period it
/// arrives at — is capped at four passes a second.
const MIN_PASS_INTERVAL: Duration = Duration::from_millis(250);

/// How often a wait re-checks `closed`, so a teardown leaves it promptly
/// instead of after the full inter-pass gap.
const CLOSE_CHECK_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Default)]
pub(super) struct TopologySignal {
    epoch: Arc<AtomicU64>,
    acknowledged_epoch: Arc<AtomicU64>,
    notify: Arc<Notify>,
}

impl TopologySignal {
    pub(super) fn mark_dirty(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.notify.notify_one();
    }

    /// Wakes the actor without dirtying the topology, so a teardown that has
    /// just set `closed` is observed now rather than after the safety
    /// interval. Left asleep, the actor keeps its event-sender clone alive for
    /// up to thirty seconds, which turns every clean disconnect into a
    /// forced writer abort.
    pub(super) fn wake(&self) {
        self.notify.notify_one();
    }

    pub(super) fn observe_event(&self, message: &SequencerControl) {
        if matches!(
            message,
            SequencerControl::OrderedEvent(event)
                if v1::EventKind::try_from(event.kind).unwrap_or_default()
                    == v1::EventKind::TopologyDirty
        ) {
            self.mark_dirty();
        }
    }

    pub(super) fn current_epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(super) fn acknowledge_through(&self, epoch: u64) {
        self.acknowledged_epoch.fetch_max(epoch, Ordering::AcqRel);
    }

    fn acknowledges(&self, epoch: u64) -> bool {
        self.acknowledged_epoch.load(Ordering::Acquire) >= epoch
    }
}

pub(super) struct TopologyActor {
    pub closed: Arc<AtomicBool>,
    pub subscribed: Arc<AtomicBool>,
    pub generation: Arc<AtomicU64>,
    pub overflowed: Arc<AtomicBool>,
    pub lock: Arc<tokio::sync::Mutex<()>>,
    pub baseline: Arc<Mutex<Option<(tmux_control::TmuxSnapshot, String)>>>,
    pub terminal: Arc<Mutex<TerminalClients>>,
    pub sender: mpsc::Sender<SequencerControl>,
    pub signal: TopologySignal,
}

impl TopologyActor {
    pub(super) fn spawn(self) {
        tokio::spawn(self.run(|| async {
            match tokio::task::spawn_blocking(discover_authoritative).await {
                Ok(discovered) => discovered,
                Err(error) => Err(error.into()),
            }
        }));
    }

    /// The reconciliation loop, over a caller-supplied discovery pass.
    ///
    /// Only the tests substitute the pass; they need one that cannot fork tmux
    /// and that can be counted.
    async fn run<Discover, Pass>(self, discover: Discover)
    where
        Discover: Fn() -> Pass + Send + 'static,
        Pass: Future<Output = anyhow::Result<(tmux_control::TmuxSnapshot, String)>> + Send,
    {
        let mut discovery_failed = false;
        let mut last_reconciled_epoch = 0;
        let mut last_pass_started: Option<Instant> = None;
        while !self.closed.load(Ordering::Acquire) {
            let notified = tokio::select! {
                _ = self.signal.notify.notified() => true,
                _ = sleep(SAFETY_RECONCILE_INTERVAL) => false,
            };
            // A teardown wake must not be answered with one last discovery
            // pass; the connection this actor serves is already gone.
            if self.closed.load(Ordering::Acquire) {
                break;
            }
            if !self.subscribed.load(Ordering::Acquire) {
                continue;
            }
            if notified && self.signal.epoch.load(Ordering::Acquire) == last_reconciled_epoch {
                continue;
            }
            if notified {
                let ready_at = (Instant::now() + NOTIFICATION_BURST_WINDOW)
                    .max(pass_gap_deadline(last_pass_started));
                if !self.wait_until(ready_at).await {
                    break;
                }
            }
            if self.overflowed.swap(false, Ordering::AcqRel) {
                emit_event(
                    &self.sender,
                    &self.overflowed,
                    v1::HostEvent {
                        kind: v1::EventKind::ResyncRequired.into(),
                        scope: "full".into(),
                        detail: "event queue overflow".into(),
                        ..Default::default()
                    },
                );
            }

            loop {
                let observed_epoch = self.signal.epoch.load(Ordering::Acquire);
                let guard = self.lock.lock().await;
                if notified && self.signal.acknowledges(observed_epoch) {
                    drop(guard);
                    if self.signal.epoch.load(Ordering::Acquire) == observed_epoch {
                        last_reconciled_epoch = observed_epoch;
                        break;
                    }
                    continue;
                }
                last_pass_started = Some(Instant::now());
                let discovered = discover().await;
                match discovered {
                    Ok((current, identity)) => {
                        discovery_failed = false;
                        if !self
                            .reconcile_observation(current, identity, notified)
                            .await
                        {
                            // Agent persistence rolled its observation back.
                            // Leave this epoch unreconciled and wait for the
                            // next notification or safety pass to retry; a
                            // tight retry loop would turn an unwritable store
                            // into a discovery storm.
                            drop(guard);
                            break;
                        }
                    }
                    Err(_) if !discovery_failed => {
                        discovery_failed = true;
                        emit_event(
                            &self.sender,
                            &self.overflowed,
                            v1::HostEvent {
                                kind: v1::EventKind::ResyncRequired.into(),
                                scope: "full".into(),
                                detail: "tmux discovery failed".into(),
                                ..Default::default()
                            },
                        );
                    }
                    Err(_) => {}
                }
                drop(guard);

                if self.signal.epoch.load(Ordering::Acquire) == observed_epoch {
                    // This pass reconciled exactly the epoch captured at
                    // its start. A dirty notification may arrive between
                    // the comparison above and here; recording a newer
                    // load would incorrectly consume that notification.
                    last_reconciled_epoch = observed_epoch;
                    break;
                }
                // More dirtiness landed while this pass ran, so the follow-up
                // waits out the pass-rate floor rather than starting at
                // whatever rate tmux can answer.
                if !self.wait_until(pass_gap_deadline(last_pass_started)).await {
                    break;
                }
            }
        }
    }

    async fn reconcile_observation(
        &self,
        current: tmux_control::TmuxSnapshot,
        identity: String,
        notified: bool,
    ) -> bool {
        let topology_changed =
            self.baseline
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|(value, value_identity)| {
                    value_identity != &identity || !same_publishable_topology(value, &current)
                });
        // Agent detection is an observation of the pane's live process tree,
        // not of whether this tmux snapshot needs publishing. Keep running it
        // for equivalent title frames: a shell-hosted agent can start or stop
        // without changing pane_current_command, and suppressing that frame
        // must not suppress the resulting agent-state refresh.
        let agent_changed = match crate::service::agents::AgentRuntime::global()
            .reconcile_topology(&current, &identity)
        {
            Ok(changed) => changed,
            Err(_) => {
                // Persistence rollback means the process observation was not
                // reconciled. Never answer that failure with the ordinary
                // unchanged acknowledgement: it would tell the desktop its
                // current agent protection is authoritative when it is not.
                reconcile_terminal_clients_if_open(
                    &self.closed,
                    &self.terminal,
                    &current,
                    &self.sender,
                    &self.overflowed,
                );
                return false;
            }
        };
        let changed = topology_changed || agent_changed;
        if changed {
            *self.baseline.lock().unwrap() = Some((current.clone(), identity.clone()));
            let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
            let _ = self
                .sender
                .send(SequencerControl::OrderedEvent(v1::HostEvent {
                    kind: v1::EventKind::TopologySnapshot.into(),
                    scope: "topology".into(),
                    snapshot: Some(snapshot_from_reconciled_identity(
                        current.clone(),
                        generation,
                        identity,
                    )),
                    ..Default::default()
                }))
                .await;
        } else if notified {
            // A tmux notification can describe a transient change that has
            // already settled back to the authoritative baseline, or one
            // animation frame replacing another without changing the title's
            // meaningful suffix. Close the frontend's reconciliation state
            // even when no generation change is needed.
            //
            // Nothing moved, so nothing is described: the acknowledgement
            // carries the generation it reconciled and no snapshot at all.
            let generation = self.generation.load(Ordering::Acquire);
            let _ = self
                .sender
                .send(SequencerControl::OrderedEvent(v1::HostEvent {
                    kind: v1::EventKind::TopologySnapshot.into(),
                    scope: "topology".into(),
                    detail: "topology reconciliation completed".into(),
                    topology_generation: generation,
                    ..Default::default()
                }))
                .await;
        }
        reconcile_terminal_clients_if_open(
            &self.closed,
            &self.terminal,
            &current,
            &self.sender,
            &self.overflowed,
        );
        true
    }

    /// Sleeps until `deadline`. Returns false if the connection closed, which
    /// a teardown wake makes visible within [`CLOSE_CHECK_INTERVAL`] rather
    /// than after the whole wait.
    async fn wait_until(&self, deadline: Instant) -> bool {
        loop {
            if self.closed.load(Ordering::Acquire) {
                return false;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return true;
            }
            sleep(remaining.min(CLOSE_CHECK_INTERVAL)).await;
        }
    }
}

/// The earliest the next discovery pass may start, given when the previous one
/// did. A first pass after a quiet period has no previous start to wait out.
fn pass_gap_deadline(previous_pass: Option<Instant>) -> Instant {
    previous_pass.map_or_else(Instant::now, |started| started + MIN_PASS_INTERVAL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::OutputCredit;
    use crate::service::terminal::TerminalClients;
    use crate::service::topology_output_trigger::TopologyOutputTrigger;

    /// A subscribed actor whose discovery pass counts itself, costs about what
    /// a tmux fork costs, and answers with a different identity every time —
    /// the way a window whose title animates makes every snapshot differ from
    /// the one before it.
    struct CountingActor {
        signal: TopologySignal,
        closed: Arc<AtomicBool>,
        passes: Arc<AtomicU64>,
        actor: tokio::task::JoinHandle<()>,
        /// Kept alive: a dropped receiver would make every snapshot send fail
        /// and change what the loop costs.
        _events: mpsc::Receiver<SequencerControl>,
    }

    impl CountingActor {
        fn spawn() -> Self {
            let signal = TopologySignal::default();
            let closed = Arc::new(AtomicBool::new(false));
            let passes = Arc::new(AtomicU64::new(0));
            let (sender, events) = mpsc::channel(4096);
            let actor = TopologyActor {
                closed: Arc::clone(&closed),
                subscribed: Arc::new(AtomicBool::new(true)),
                generation: Arc::new(AtomicU64::new(0)),
                overflowed: Arc::new(AtomicBool::new(false)),
                lock: Arc::new(tokio::sync::Mutex::new(())),
                baseline: Arc::new(Mutex::new(None)),
                terminal: Arc::new(Mutex::new(TerminalClients::new(
                    Arc::new(OutputCredit::negotiated(false)),
                    TopologyOutputTrigger::default(),
                ))),
                sender,
                signal: signal.clone(),
            };
            let counter = Arc::clone(&passes);
            let actor = tokio::spawn(actor.run(move || {
                let counter = Arc::clone(&counter);
                async move {
                    let pass = counter.fetch_add(1, Ordering::AcqRel);
                    sleep(Duration::from_millis(1)).await;
                    Ok((
                        tmux_control::TmuxSnapshot::default(),
                        format!("tmux:{pass}"),
                    ))
                }
            }));
            Self {
                signal,
                closed,
                passes,
                actor,
                _events: events,
            }
        }

        fn passes(&self) -> u64 {
            self.passes.load(Ordering::Acquire)
        }

        async fn shutdown(self) {
            self.closed.store(true, Ordering::Release);
            self.signal.wake();
            tokio::time::timeout(Duration::from_secs(5), self.actor)
                .await
                .expect("the actor must observe its closed flag")
                .unwrap();
        }
    }

    /// Renames the window every `period` for `burst`, then waits for the last
    /// pass the storm earned, and reports how many passes the whole run cost
    /// against what [`MIN_PASS_INTERVAL`] permits over the same span.
    async fn storm_passes_against_ceiling(period: Duration, burst: Duration) -> (u64, u128) {
        let harness = CountingActor::spawn();
        let started = Instant::now();
        while started.elapsed() < burst {
            harness.signal.mark_dirty();
            sleep(period).await;
        }
        sleep(MIN_PASS_INTERVAL * 2).await;

        let passes = harness.passes();
        // Two passes of slack: the storm's first pass owes nothing to the rate
        // floor, and a real-time run drifts by a fraction of a gap.
        let ceiling = started.elapsed().as_millis() / MIN_PASS_INTERVAL.as_millis() + 2;
        harness.shutdown().await;
        (passes, ceiling)
    }

    /// An agent animating a status glyph in its window title renames the window
    /// at the CLI's redraw rate — 100ms is a typical frame period — and every
    /// rename is a topology notification. Each pass forks tmux and pushes a
    /// snapshot the desktop re-renders from, so the pass rate has to follow
    /// [`MIN_PASS_INTERVAL`] and not the frame rate. Under a settle-only
    /// design this burst produced one pass per rename, because every frame
    /// arrived after the settle had already elapsed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_animated_title_costs_passes_at_the_pass_rate_not_the_frame_rate() {
        let (passes, ceiling) =
            storm_passes_against_ceiling(Duration::from_millis(100), Duration::from_secs(1)).await;
        assert!(
            passes >= 2,
            "a sustained rename storm must keep reconciling, saw {passes} passes"
        );
        assert!(
            u128::from(passes) <= ceiling,
            "a 100ms rename storm produced {passes} discovery passes, over the {ceiling} the rate floor allows"
        );
    }

    /// The same floor has to hold when notifications arrive far faster than a
    /// pass can answer them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_notification_flood_is_bounded_by_the_same_pass_rate() {
        let (passes, ceiling) =
            storm_passes_against_ceiling(Duration::from_millis(5), Duration::from_millis(600))
                .await;
        assert!(
            passes >= 2,
            "a sustained rename storm must keep reconciling, saw {passes} passes"
        );
        assert!(
            u128::from(passes) <= ceiling,
            "a 5ms rename flood produced {passes} discovery passes, over the {ceiling} the rate floor allows"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_rename_costs_exactly_one_prompt_pass() {
        let harness = CountingActor::spawn();
        let started = Instant::now();
        harness.signal.mark_dirty();
        while harness.passes() == 0 {
            // A change arriving out of quiet owes nothing to the rate floor:
            // it waits the burst window and nothing else.
            assert!(
                started.elapsed() < MIN_PASS_INTERVAL / 2,
                "a lone rename waited {:?} for its discovery pass",
                started.elapsed()
            );
            sleep(Duration::from_millis(1)).await;
        }

        sleep(MIN_PASS_INTERVAL * 3).await;
        assert_eq!(
            harness.passes(),
            1,
            "coalescing must not turn one rename into a repeating pass"
        );
        harness.shutdown().await;
    }

    /// A pass decides on its follow-up from a single comparison against the
    /// epoch it started from, so any number of marks landing while it runs
    /// costs exactly one more pass rather than one pass each.
    #[test]
    fn concurrent_dirty_coalesces_to_exactly_one_follow_up() {
        let signal = TopologySignal::default();
        signal.mark_dirty();
        let observed_epoch = signal.current_epoch();
        for _ in 0..100 {
            signal.mark_dirty();
        }
        assert_ne!(signal.current_epoch(), observed_epoch);

        // The follow-up observes all hundred at once, and finding nothing
        // newer at its end ends the run.
        let follow_up_epoch = signal.current_epoch();
        assert_eq!(follow_up_epoch, 101);
        assert_eq!(signal.current_epoch(), follow_up_epoch);
    }

    #[test]
    fn action_acknowledgement_consumes_only_covered_dirty_epochs() {
        let signal = TopologySignal::default();
        signal.mark_dirty();
        let covered = signal.current_epoch();
        signal.acknowledge_through(covered);
        assert!(signal.acknowledges(covered));

        signal.mark_dirty();
        let later = signal.current_epoch();
        assert!(!signal.acknowledges(later));
    }

    /// The actor's only reason to skip a discovery pass for an epoch it was
    /// notified about is [`TopologySignal::acknowledges`], and the sole writer
    /// of that state is a frontend tmux action acknowledging the dirtiness its
    /// own authoritative postcheck already covered. Output-driven dirtiness
    /// raises the epoch past anything such an action can have acknowledged, so
    /// it always reaches discovery — which is what makes the output trigger
    /// safe to point at the same `mark_dirty`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_driven_dirty_is_never_consumed_by_an_action_acknowledgement() {
        let signal = TopologySignal::default();
        // An action reconciled and acknowledged everything dirty so far.
        signal.mark_dirty();
        signal.acknowledge_through(signal.current_epoch());
        assert!(signal.acknowledges(signal.current_epoch()));

        // A quiet pane then prints the prompt that followed a `cd`.
        crate::service::topology_output_trigger::TopologyOutputTrigger::new(
            signal.clone(),
            tokio::runtime::Handle::current(),
        )
        .note_output();

        let observed_epoch = signal.current_epoch();
        assert_eq!(observed_epoch, 2);
        assert!(
            !signal.acknowledges(observed_epoch),
            "the actor would have skipped its discovery pass for this epoch"
        );
    }

    #[test]
    fn later_dirty_between_final_check_and_completion_is_not_consumed() {
        let signal = TopologySignal::default();
        signal.mark_dirty();
        let observed_epoch = signal.current_epoch();

        // Model the final comparison as having seen no later dirtiness, then
        // inject one in the exact window before the completed pass records
        // what it reconciled.
        assert_eq!(signal.current_epoch(), observed_epoch);
        signal.mark_dirty();
        let last_reconciled_epoch = observed_epoch;

        assert_ne!(signal.current_epoch(), last_reconciled_epoch);
    }

    /// A server whose every discovery pass answers with the same tree, so the
    /// pass after the first is the unchanged-but-notified one.
    fn one_session_server() -> tmux_control::TmuxSnapshot {
        tmux_control::TmuxSnapshot {
            sessions: vec![tmux_control::Session {
                id: "$1".into(),
                name: "work".into(),
                window_count: 1,
                attached_clients: 1,
                order: 0,
                pinned: false,
            }],
            windows: Vec::new(),
            panes: Vec::new(),
        }
    }

    fn titled_server(title: &str) -> tmux_control::TmuxSnapshot {
        tmux_control::TmuxSnapshot {
            sessions: one_session_server().sessions,
            windows: vec![tmux_control::Window {
                id: "@1".into(),
                session_id: "$1".into(),
                index: 0,
                name: title.into(),
                active: true,
                layout: "layout".into(),
                zoomed: false,
                pinned: false,
            }],
            panes: vec![tmux_control::Pane {
                id: "%1".into(),
                session_id: "$1".into(),
                window_id: "@1".into(),
                index: 0,
                active: true,
                width: 80,
                height: 24,
                left: 0,
                top: 0,
                current_path: "/work".into(),
                current_command: "bash".into(),
                pane_pid: 42,
                start_command: "bash".into(),
            }],
        }
    }

    fn observation_actor() -> (
        TopologyActor,
        Arc<AtomicU64>,
        mpsc::Receiver<SequencerControl>,
    ) {
        let generation = Arc::new(AtomicU64::new(0));
        let (sender, events) = mpsc::channel(64);
        (
            TopologyActor {
                closed: Arc::new(AtomicBool::new(false)),
                subscribed: Arc::new(AtomicBool::new(true)),
                generation: Arc::clone(&generation),
                overflowed: Arc::new(AtomicBool::new(false)),
                lock: Arc::new(tokio::sync::Mutex::new(())),
                baseline: Arc::new(Mutex::new(None)),
                terminal: Arc::new(Mutex::new(TerminalClients::new(
                    Arc::new(OutputCredit::negotiated(false)),
                    TopologyOutputTrigger::default(),
                ))),
                sender,
                signal: TopologySignal::default(),
            },
            generation,
            events,
        )
    }

    #[test]
    fn agent_title_parser_matches_the_desktop_marker_set() {
        for marker in [
            "·", "✢", "✳", "✶", "✻", "✽", "◐", "◓", "◑", "◒", "⠀", "⠋", "⣿", "✓", "✔", "✕", "✘",
            "[ . ]", "[ ! ]",
        ] {
            assert_eq!(
                agent_title_suffix(&format!("{marker} Fix tests")),
                Some("Fix tests"),
                "marker {marker:?}"
            );
        }
        assert_eq!(agent_title_suffix("✳️ Fix tests"), Some("Fix tests"));
        assert_eq!(agent_title_suffix("✳︎ Fix tests"), Some("Fix tests"));
        assert_eq!(
            agent_title_suffix("✳ ✶ ⠋ [ ! ] Fix tests"),
            Some("Fix tests")
        );
        assert_eq!(agent_title_suffix("⠋"), Some(""));
        assert_eq!(agent_title_suffix("[ . ]   "), Some(""));

        assert_eq!(agent_title_suffix("🚀 deploy"), None);
        assert_eq!(agent_title_suffix("Fix tests"), None);
        assert_eq!(agent_title_suffix("release ✳ notes"), None);
        assert_eq!(agent_title_suffix("Action [ ! ] required"), None);
    }

    #[test]
    fn agent_title_equivalence_requires_both_titles_to_be_marked() {
        assert!(equivalent_agent_title("✳ Fix tests", "⠋ Fix tests"));
        assert!(equivalent_agent_title("✳", "[ . ]"));
        assert!(!equivalent_agent_title("Fix tests", "✳ Fix tests"));
        assert!(!equivalent_agent_title("✳ Fix tests", "⠋ Ship tests"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_title_frames_do_not_publish_but_real_changes_still_do() {
        let (actor, generation, mut events) = observation_actor();

        actor
            .reconcile_observation(titled_server("Fix tests"), "tmux:stable".into(), true)
            .await;
        let initial = next_topology_snapshot_event(&mut events).await;
        assert_eq!(initial.snapshot.unwrap().generation, 1);

        actor
            .reconcile_observation(titled_server("✳ Fix tests"), "tmux:stable".into(), true)
            .await;
        let entered = next_topology_snapshot_event(&mut events).await;
        let entered_snapshot = entered
            .snapshot
            .expect("entering the marked agent state must publish");
        assert_eq!(entered_snapshot.generation, 2);
        assert_eq!(entered_snapshot.windows[0].name, "✳ Fix tests");

        let frames = [
            "·", "✢", "✳", "✶", "✻", "✽", "◐", "◓", "◑", "◒", "⠦", "⠋", "⣷", "✓", "✗", "[ . ]",
            "[ ! ]",
        ];
        for index in 0..48 {
            let title = format!("{} Fix tests", frames[index % frames.len()]);
            actor
                .reconcile_observation(titled_server(&title), "tmux:stable".into(), true)
                .await;
            let animation = next_topology_snapshot_event(&mut events).await;
            assert!(
                animation.snapshot.is_none(),
                "animation frame {index} published a full snapshot"
            );
            assert_eq!(animation.topology_generation, 2);
            assert_eq!(generation.load(Ordering::Acquire), 2);
        }

        actor
            .reconcile_observation(titled_server("✓ Ship tests"), "tmux:stable".into(), true)
            .await;
        let renamed = next_topology_snapshot_event(&mut events).await;
        let renamed_snapshot = renamed
            .snapshot
            .expect("a meaningful title suffix change must publish");
        assert_eq!(renamed_snapshot.generation, 3);
        assert_eq!(renamed_snapshot.windows[0].name, "✓ Ship tests");

        actor
            .reconcile_observation(titled_server("Ship tests"), "tmux:stable".into(), true)
            .await;
        let left = next_topology_snapshot_event(&mut events).await;
        assert_eq!(
            left.snapshot
                .expect("leaving the marked agent state must publish")
                .generation,
            4
        );

        let mut structurally_changed = titled_server("Ship tests");
        structurally_changed.panes[0].width = 120;
        actor
            .reconcile_observation(structurally_changed, "tmux:stable".into(), true)
            .await;
        let structural = next_topology_snapshot_event(&mut events).await;
        assert_eq!(
            structural
                .snapshot
                .expect("a structural pane change must publish")
                .generation,
            5
        );

        let mut window_changed = titled_server("Ship tests");
        window_changed.panes[0].width = 120;
        window_changed.windows[0].zoomed = true;
        actor
            .reconcile_observation(window_changed, "tmux:stable".into(), true)
            .await;
        let window_structural = next_topology_snapshot_event(&mut events).await;
        assert_eq!(
            window_structural
                .snapshot
                .expect("a structural window change must publish")
                .generation,
            6
        );
        assert_eq!(generation.load(Ordering::Acquire), 6);
    }

    async fn next_topology_snapshot_event(
        events: &mut mpsc::Receiver<SequencerControl>,
    ) -> v1::HostEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let control = tokio::time::timeout(remaining, events.recv())
                .await
                .expect("a topology event must arrive")
                .expect("the actor must keep its sender");
            if let SequencerControl::OrderedEvent(event) = control
                && event.kind == i32::from(v1::EventKind::TopologySnapshot)
            {
                return event;
            }
        }
    }

    /// A notified pass that finds the world unchanged says so with the
    /// generation it reconciled and nothing else.
    ///
    /// It used to answer with the whole server — a second copy of a tree the
    /// desktop already holds byte for byte, 7–39 KB of it on a busy one, sent
    /// several times per window switch because a switch is several
    /// notifications. The desktop only ever used this event to close its
    /// reconciliation state, which the generation alone does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unchanged_notified_pass_acknowledges_the_generation_and_sends_no_server() {
        let signal = TopologySignal::default();
        let closed = Arc::new(AtomicBool::new(false));
        let (sender, mut events) = mpsc::channel(64);
        let actor = TopologyActor {
            closed: Arc::clone(&closed),
            subscribed: Arc::new(AtomicBool::new(true)),
            generation: Arc::new(AtomicU64::new(0)),
            overflowed: Arc::new(AtomicBool::new(false)),
            lock: Arc::new(tokio::sync::Mutex::new(())),
            baseline: Arc::new(Mutex::new(None)),
            terminal: Arc::new(Mutex::new(TerminalClients::new(
                Arc::new(OutputCredit::negotiated(false)),
                TopologyOutputTrigger::default(),
            ))),
            sender,
            signal: signal.clone(),
        };
        let running =
            tokio::spawn(actor.run(|| async { Ok((one_session_server(), "tmux:stable".into())) }));

        // The first pass has no baseline to compare against, so it changed.
        signal.mark_dirty();
        let described = next_topology_snapshot_event(&mut events).await;
        let snapshot = described
            .snapshot
            .expect("the pass that changed the world must describe it");
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.generation, 1);
        assert_eq!(snapshot.server_identity, "tmux:stable");

        // The second finds the same tree behind the same identity.
        signal.mark_dirty();
        let acknowledged = next_topology_snapshot_event(&mut events).await;
        assert!(
            acknowledged.snapshot.is_none(),
            "an unchanged pass resent the whole server to say nothing moved"
        );
        assert_eq!(acknowledged.detail, "topology reconciliation completed");
        assert_eq!(
            acknowledged.topology_generation, 1,
            "the acknowledgement must carry the generation it reconciled"
        );

        closed.store(true, Ordering::Release);
        signal.wake();
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .expect("the actor must observe its closed flag")
            .unwrap();
    }
}
