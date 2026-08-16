use super::*;
use crate::service::terminal::FlowControl;

/// Everything `handle` writes to, so a test can read back both what the
/// desktop was told and what tmux was asked for.
struct Harness {
    flow: Arc<FlowControl>,
    writes: std_mpsc::Receiver<super::super::ControlWrite>,
    writer: std_mpsc::Sender<super::super::ControlWrite>,
    events: mpsc::Receiver<SequencerControl>,
    sender: mpsc::Sender<SequencerControl>,
    overflowed: AtomicBool,
    resources: Arc<Mutex<PaneResourceStore>>,
    generation: Arc<AtomicU64>,
    stopped: AtomicBool,
    input_completion_tx: std_mpsc::Sender<InputCompletion>,
    input_completions: std_mpsc::Receiver<InputCompletion>,
}

impl Harness {
    fn new(pane_ids: &[String]) -> (StreamState, Self) {
        let flow = Arc::new(FlowControl::default());
        let (writer, writes) = std_mpsc::channel();
        let (sender, events) = mpsc::channel(64);
        let (input_completion_tx, input_completions) = std_mpsc::channel();
        let state = StreamState::new(pane_ids, Arc::clone(&flow));
        (
            state,
            Self {
                flow,
                writes,
                writer,
                events,
                sender,
                overflowed: AtomicBool::new(false),
                resources: Arc::new(Mutex::new(PaneResourceStore::with_total_limit(
                    8, 1024, 4096,
                ))),
                generation: Arc::new(AtomicU64::new(0)),
                stopped: AtomicBool::new(false),
                input_completion_tx,
                input_completions,
            },
        )
    }

    fn runtime(&self) -> StreamRuntime<'_> {
        StreamRuntime {
            writer: &self.writer,
            sender: &self.sender,
            overflowed: &self.overflowed,
            resources: &self.resources,
            terminal_generation: &self.generation,
            stopped: &self.stopped,
            input_completion: &self.input_completion_tx,
        }
    }

    /// Every capture tmux was asked for, as `(pane, resumed first)`.
    fn writes(&self) -> Vec<(String, bool)> {
        self.writes
            .try_iter()
            .map(|write| (write.pane_id, write.resume_first))
            .collect()
    }

    fn events(&mut self) -> Vec<v1::HostEvent> {
        let mut drained = Vec::new();
        while let Ok(SequencerControl::OrderedEvent(event)) = self.events.try_recv() {
            drained.push(event);
        }
        drained
    }
}

fn kinds(events: &[v1::HostEvent]) -> Vec<(v1::EventKind, String)> {
    events
        .iter()
        .map(|event| {
            (
                v1::EventKind::try_from(event.kind).unwrap_or_default(),
                event.scope.clone(),
            )
        })
        .collect()
}

const TAG: CommandTag = CommandTag {
    timestamp: 1,
    number: 7,
    flags: 1,
};

/// Drives one rejected `refresh-client -A` through the correlation the
/// reader really uses: an untargeted marker block naming the pane, then the
/// block that fails.
fn reject_one_resume(state: &mut StreamState, harness: &Harness, pane_id: &str) {
    state.expected_resume = Some(pane_id.to_owned());
    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    state.handle(
        ControlRecord::CommandOutput(b"parse error: syntax error".to_vec()),
        harness.runtime(),
    );
    state.handle(
        ControlRecord::Error {
            tag: TAG,
            arguments: "1 7 1".into(),
        },
        harness.runtime(),
    );
}

/// The user's report, end to end: a pane freezes, and switching tabs and
/// back refreshes it once before it freezes again.
///
/// tmux drops a paused pane's output rather than replaying it, so a capture
/// written *without* the resume re-photographs the screen and leaves the
/// pane paused — which is precisely what a reveal used to write. Every
/// capture for a pane the host believes paused now carries the resume, so
/// the reseed the desktop already performs is a real recovery.
#[test]
fn a_paused_pane_is_resumed_by_the_capture_that_recovers_it() {
    let (mut state, mut harness) = Harness::new(&["%1".into(), "%2".into()]);
    state.handle(
        ControlRecord::Notification {
            name: "pause".into(),
            arguments: "%1".into(),
        },
        harness.runtime(),
    );
    assert!(harness.flow.resume_before_capture("%1"));
    assert!(!harness.flow.resume_before_capture("%2"));
    assert_eq!(harness.writes(), vec![("%1".to_owned(), true)]);
    assert_eq!(
        kinds(&harness.events()),
        vec![(v1::EventKind::TerminalFlowPaused, "%1".to_owned())]
    );

    // The half the service thread owns: a reveal or a recovery seed asks
    // this same question before it writes.
    assert!(harness.flow.resume_before_capture("%1"));

    // tmux acknowledges the resume the only way it ever does.
    state.handle(
        ControlRecord::Notification {
            name: "continue".into(),
            arguments: "%1".into(),
        },
        harness.runtime(),
    );
    assert!(!harness.flow.resume_before_capture("%1"));
}

/// A rejected resume used to be reported and then abandoned, and the reseed
/// its event asked for could not resume the pane. It is retried once, and
/// then reported stalled rather than retried at forever.
#[test]
fn a_rejected_resume_is_retried_once_and_then_reported_stalled() {
    let (mut state, mut harness) = Harness::new(&["%5".into()]);
    state.handle(
        ControlRecord::Notification {
            name: "pause".into(),
            arguments: "%5".into(),
        },
        harness.runtime(),
    );
    assert_eq!(harness.writes(), vec![("%5".to_owned(), true)]);
    harness.events();

    reject_one_resume(&mut state, &harness, "%5");
    assert_eq!(
        harness.writes(),
        vec![("%5".to_owned(), true)],
        "the retry must carry the resume, not merely re-photograph the pane"
    );
    assert_eq!(
        harness.events(),
        Vec::new(),
        "the host is still handling it; the desktop has nothing to do and is told nothing"
    );

    reject_one_resume(&mut state, &harness, "%5");
    assert_eq!(
        harness.writes(),
        Vec::new(),
        "the budget is spent; a deterministic rejection must not loop"
    );
    let events = harness.events();
    assert_eq!(
        kinds(&events),
        vec![(v1::EventKind::TerminalFlowStalled, "%5".to_owned())],
        "exactly one event, because both this and a resnapshot make the desktop \
             ask for a seed and two seeds per rejection is exponential"
    );
    assert!(
        events[0].detail.contains("parse error: syntax error"),
        "the reason tmux printed is the whole value of the report: {}",
        events[0].detail
    );
    assert!(
        !harness.flow.resume_before_capture("%5"),
        "the seed this stall asks for must be a plain capture, or its own rejection \
             produces the next stall, which asks for the next seed, forever"
    );

    // A fresh flow-control episode is a fresh budget: this pane must not be
    // written off for the rest of the connection.
    state.handle(
        ControlRecord::Notification {
            name: "pause".into(),
            arguments: "%5".into(),
        },
        harness.runtime(),
    );
    harness.writes();
    harness.events();
    reject_one_resume(&mut state, &harness, "%5");
    assert_eq!(harness.writes(), vec![("%5".to_owned(), true)]);
}

/// A `%error` on a resume for a pane nothing paused is a correlation
/// failure, not a flow-control one: it is reported the way every other
/// rejected command is, and nothing about flow control is claimed.
#[test]
fn a_resume_rejection_for_a_pane_that_is_not_paused_is_not_flow_control() {
    let (mut state, mut harness) = Harness::new(&["%5".into()]);
    reject_one_resume(&mut state, &harness, "%5");
    assert_eq!(harness.writes(), Vec::new());
    assert_eq!(
        kinds(&harness.events()),
        vec![(v1::EventKind::TerminalResnapshotRequired, "%5".to_owned())],
    );
}

/// A pane this client no longer owns is not one it can resume, and leaving
/// it recorded would hand the next pane to take its id a pause that was
/// never its own.
#[test]
fn losing_a_pane_forgets_that_it_was_paused() {
    let (mut state, harness) = Harness::new(&["%1".into()]);
    state.handle(
        ControlRecord::Notification {
            name: "pause".into(),
            arguments: "%1".into(),
        },
        harness.runtime(),
    );
    assert!(harness.flow.resume_before_capture("%1"));
    state.apply_control(StreamControl::Membership {
        pane_ids: Vec::new(),
    });
    assert!(!harness.flow.resume_before_capture("%1"));
}

/// A malformed `%pause` must not enter a pane id nothing can ever clear.
#[test]
fn a_pause_naming_nothing_valid_records_nothing() {
    let (mut state, mut harness) = Harness::new(&["%1".into()]);
    for arguments in ["", "1", "pane-1", "%1;rm"] {
        state.handle(
            ControlRecord::Notification {
                name: "pause".into(),
                arguments: arguments.into(),
            },
            harness.runtime(),
        );
    }
    assert_eq!(harness.writes(), Vec::new());
    assert_eq!(harness.events(), Vec::new());
}

#[test]
fn pane_close_prunes_capture_state_without_disturbing_sibling() {
    let mut state = StreamState::new(
        &["%1".into(), "%2".into()],
        Arc::new(crate::service::terminal::FlowControl::default()),
    );
    state.command_block = CommandBlock::CapturePrimary {
        tag: CommandTag {
            timestamp: 1,
            number: 2,
            flags: 1,
        },
        pane_id: "%1".into(),
        lines: vec![b"stale".to_vec()],
    };
    state.pending_alternate = Some(("%1".into(), Vec::new(), 1));
    state.apply_control(StreamControl::Membership {
        pane_ids: vec!["%2".into()],
    });
    assert!(!state.pane_states.contains_key("%1"));
    assert!(state.pane_states.contains_key("%2"));
    assert!(matches!(state.command_block, CommandBlock::None));
    assert!(state.pending_alternate.is_none());
}

#[test]
fn an_in_band_input_block_is_correlated_to_the_pane_that_was_typed_into() {
    // Input correlation is consumed by the block that follows its marker,
    // so an error inside that block recovers only the pane typed into.
    let mut state = StreamState::new(
        &["%1".into(), "%2".into()],
        Arc::new(crate::service::terminal::FlowControl::default()),
    );
    state.expected_input = Some((7, "%2".into()));
    let block = state.start_block(CommandTag {
        timestamp: 1,
        number: 2,
        flags: 1,
    });
    assert!(
        matches!(block, CommandBlock::Input { input_id: 7, ref pane_id, .. } if pane_id == "%2")
    );
    state.command_block = block;
    assert_eq!(state.active_scope(), "%2");
}

#[test]
fn an_in_band_input_completion_is_reported_only_after_tmux_ends_its_block() {
    let (mut state, harness) = Harness::new(&["%1".into()]);
    state.expected_input = Some((8, "%1".into()));
    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    assert!(harness.input_completions.try_recv().is_err());
    state.handle(
        ControlRecord::End {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    assert_eq!(harness.input_completions.try_recv().unwrap(), (8, Ok(())));
}

#[test]
fn a_rejected_in_band_input_reports_its_tmux_error_to_the_barrier_lane() {
    let (mut state, harness) = Harness::new(&["%1".into()]);
    state.expected_input = Some((9, "%1".into()));
    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    state.handle(
        ControlRecord::CommandOutput(b"can't find pane: %1".to_vec()),
        harness.runtime(),
    );
    state.handle(
        ControlRecord::Error {
            tag: TAG,
            arguments: "1 7 1".into(),
        },
        harness.runtime(),
    );
    let (input_id, result) = harness.input_completions.try_recv().unwrap();
    assert_eq!(input_id, 9);
    let error = result.unwrap_err();
    assert!(error.contains("terminal input for %1 was rejected by tmux"));
    assert!(error.contains("can't find pane"));
}

#[test]
fn capture_marker_carries_pane_scope() {
    let mut block = CommandBlock::Unknown {
        tag: CommandTag {
            timestamp: 1,
            number: 1,
            flags: 1,
        },
        pane_id: None,
        lines: Vec::new(),
    };
    if let CommandBlock::Unknown { pane_id, .. } = &mut block {
        *pane_id = marker_pane(b"__ADE_CAPTURE__:2");
    }
    let mut state = StreamState::new(
        &["%2".into()],
        Arc::new(crate::service::terminal::FlowControl::default()),
    );
    state.command_block = block;
    assert_eq!(state.active_scope(), "%2");
}

/// P12-U001's compounding bugs: a rejected `refresh-client -A` reported
/// only the `%error` header's numbers (so the actual "parse error" was
/// invisible in every log) and was attributed to the connection, which is
/// what turned each one into a full resync — the P12-Q005 signature.
#[test]
fn a_rejected_resume_names_its_pane_and_carries_the_reason_tmux_printed() {
    let tag = CommandTag {
        timestamp: 1786682005,
        number: 425,
        flags: 1,
    };
    let mut state = StreamState::new(
        &["%1".into(), "%5".into()],
        Arc::new(crate::service::terminal::FlowControl::default()),
    );
    state.expected_resume = Some("%5".into());
    state.command_block = state.start_block(tag);
    assert!(matches!(
        state.command_block,
        CommandBlock::Resume { ref pane_id, .. } if pane_id == "%5"
    ));
    assert_eq!(state.active_scope(), "%5");

    let mut runtime_lines = Vec::new();
    if let CommandBlock::Resume { lines, .. } = &mut state.command_block {
        lines.push(b"parse error: syntax error".to_vec());
        runtime_lines.clone_from(lines);
    }
    let detail = error_reason("1786682005 425 1", &runtime_lines);
    assert_eq!(detail, "1786682005 425 1: parse error: syntax error");
    assert_eq!(error_reason("1786682005 425 1", &[]), "1786682005 425 1");
}

/// tmux emits `%continue` only when a pane really was paused and says
/// nothing otherwise, so neither the notification nor a clean `%end` on the
/// resume is an acknowledgement. Only the capture written with it restores
/// the pane — output produced while paused is dropped, never replayed.
#[test]
fn a_clean_resume_block_is_not_treated_as_an_acknowledgement() {
    let tag = CommandTag {
        timestamp: 1,
        number: 7,
        flags: 1,
    };
    let mut state = StreamState::new(
        &["%5".into()],
        Arc::new(crate::service::terminal::FlowControl::default()),
    );
    state.expected_resume = Some("%5".into());
    state.command_block = state.start_block(tag);
    let (sender, _receiver) = mpsc::channel(8);
    let overflowed = AtomicBool::new(false);
    let resources = Arc::new(Mutex::new(PaneResourceStore::with_total_limit(
        4, 1024, 4096,
    )));
    let generation = Arc::new(AtomicU64::new(0));
    let (writer, _writes) = std_mpsc::channel();
    let (input_completion, _input_completions) = std_mpsc::channel();
    let stopped = AtomicBool::new(false);
    state.finish_block(
        tag,
        StreamRuntime {
            writer: &writer,
            sender: &sender,
            overflowed: &overflowed,
            resources: &resources,
            terminal_generation: &generation,
            stopped: &stopped,
            input_completion: &input_completion,
        },
    );
    assert!(matches!(state.command_block, CommandBlock::None));
    assert!(
        matches!(
            state.pane_states.get("%5"),
            Some(PaneSeedState::Pending { .. })
        ),
        "a resume must not mark the pane live; its capture does that"
    );
}
