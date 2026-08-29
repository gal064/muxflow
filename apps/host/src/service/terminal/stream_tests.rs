use super::*;
use crate::service::terminal::FlowControl;

#[test]
fn adjacent_output_is_coalesced_without_crossing_panes_or_the_byte_bound() {
    let mut pending = PendingOutput::default();
    assert!(pending.push("%1".into(), vec![1; 32 * 1024]).is_empty());
    assert!(pending.push("%1".into(), vec![2; 32 * 1024]).is_empty());
    let flushed = pending.push("%1".into(), vec![3]).pop().unwrap();
    let ControlRecord::Output { pane_id, data } = flushed else {
        panic!("expected coalesced output");
    };
    assert_eq!(pane_id, "%1");
    assert_eq!(data.len(), 64 * 1024);
    assert_eq!(data[0], 1);
    assert_eq!(data[32 * 1024], 2);

    let flushed = pending.push("%2".into(), vec![4]).pop().unwrap();
    let ControlRecord::Output { pane_id, data } = flushed else {
        panic!("expected pane-bound output");
    };
    assert_eq!(pane_id, "%1");
    assert_eq!(data, [3]);
    let ControlRecord::Output { pane_id, data } = pending.take().unwrap() else {
        panic!("expected final pane output");
    };
    assert_eq!(pane_id, "%2");
    assert_eq!(data, [4]);

    let mut pending = PendingOutput::default();
    let flushed = pending.push("%3".into(), vec![5; MAX_COALESCED_OUTPUT_BYTES + 1]);
    assert_eq!(flushed.len(), 1);
    let ControlRecord::Output { data, .. } = &flushed[0] else {
        panic!("expected bounded oversized output");
    };
    assert_eq!(data.len(), MAX_COALESCED_OUTPUT_BYTES);
    let ControlRecord::Output { data, .. } = pending.take().unwrap() else {
        panic!("expected oversized output remainder");
    };
    assert_eq!(data, [5]);
}

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
    capture_in_flight: Mutex<HashSet<String>>,
    output_credit: Arc<super::OutputCredit>,
    emission_order: Arc<Mutex<()>>,
    topology_trigger: TopologyOutputTrigger,
}

impl Harness {
    fn new(pane_ids: &[String]) -> (StreamState, Self) {
        let flow = Arc::new(FlowControl::default());
        let (writer, writes) = std_mpsc::channel();
        let (sender, events) = mpsc::channel(64);
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
                capture_in_flight: Mutex::new(HashSet::new()),
                output_credit: Arc::new(super::OutputCredit::negotiated(false)),
                emission_order: Arc::new(Mutex::new(())),
                topology_trigger: TopologyOutputTrigger::default(),
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
            capture_in_flight: &self.capture_in_flight,
            output_credit: &self.output_credit,
            emission_order: &self.emission_order,
            topology_trigger: &self.topology_trigger,
            read_started: std::time::Instant::now(),
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
    state.apply_control(
        StreamControl::Membership {
            pane_ids: Vec::new(),
        },
        &harness.capture_in_flight,
    );
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
    state.expected_capture = Some("%1".into());
    state.expected_resume = Some("%1".into());
    state.pending_alternate = Some(("%1".into(), Vec::new(), 1));
    state.pending_metadata = Some(PendingCaptureMetadata {
        pane_id: "%1".into(),
        visible_lines: Vec::new(),
        saved_normal_lines: Vec::new(),
        visible_boundary: 1,
    });
    state.apply_control(
        StreamControl::Membership {
            pane_ids: vec!["%2".into()],
        },
        &Mutex::new(HashSet::new()),
    );
    assert!(!state.pane_states.contains_key("%1"));
    assert!(state.pane_states.contains_key("%2"));
    assert!(matches!(state.command_block, CommandBlock::Draining { .. }));
    assert!(state.expected_capture.is_none());
    assert!(state.expected_resume.is_none());
    assert!(state.pending_alternate.is_none());
    assert!(state.pending_metadata.is_none());
}

#[test]
fn pane_close_drains_every_in_flight_block_until_its_tmux_fence() {
    let tag = CommandTag {
        timestamp: 1,
        number: 2,
        flags: 1,
    };
    let blocks = [
        CommandBlock::Unknown {
            tag,
            pane_id: Some("%1".into()),
            lines: Vec::new(),
        },
        CommandBlock::Resume {
            tag,
            pane_id: "%1".into(),
            lines: Vec::new(),
        },
        CommandBlock::CapturePrimary {
            tag,
            pane_id: "%1".into(),
            lines: Vec::new(),
        },
        CommandBlock::CaptureAlternate {
            tag,
            pane_id: "%1".into(),
            visible_lines: Vec::new(),
            visible_boundary: 1,
            lines: Vec::new(),
        },
        CommandBlock::CaptureMetadata {
            tag,
            pane_id: "%1".into(),
            visible_lines: Vec::new(),
            saved_normal_lines: Vec::new(),
            visible_boundary: 1,
            lines: Vec::new(),
        },
    ];

    for block in blocks {
        let mut state = StreamState::new(&["%1".into()], Arc::new(FlowControl::default()));
        state.command_block = block;
        state.apply_control(
            StreamControl::Membership {
                pane_ids: Vec::new(),
            },
            &Mutex::new(HashSet::new()),
        );
        assert!(state.active_tag_matches(tag));
        assert!(matches!(state.command_block, CommandBlock::Draining { .. }));
        assert!(!state.pane_states.contains_key("%1"));
    }
}

#[test]
fn pane_close_during_capture_keeps_parser_and_stream_correlation_aligned() {
    let (mut state, mut harness) = Harness::new(&["%1".into(), "%2".into()]);
    state.pane_states.insert("%2".into(), PaneSeedState::Live);
    state.expected_capture = Some("%1".into());

    let mut parser = ControlParser::default();
    parser.push(b"%begin 1 2 1\n");
    state.handle(parser.next_record().unwrap().unwrap(), harness.runtime());

    state.apply_control(
        StreamControl::Membership {
            pane_ids: vec!["%2".into()],
        },
        &Mutex::new(HashSet::new()),
    );
    parser.push(b"captured row\n%end 1 2 1\n");
    while let Some(record) = parser.next_record() {
        state.handle(record.unwrap(), harness.runtime());
    }

    assert_eq!(harness.events(), Vec::new());
    assert!(!state.pane_states.contains_key("%1"));
    assert!(matches!(
        state.pane_states.get("%2"),
        Some(PaneSeedState::Live)
    ));
}

#[test]
fn pane_close_during_rejected_command_drains_the_stale_error() {
    let (mut state, mut harness) = Harness::new(&["%1".into(), "%2".into()]);
    let tag = CommandTag {
        timestamp: 1,
        number: 2,
        flags: 1,
    };
    state.command_block = CommandBlock::Resume {
        tag,
        pane_id: "%1".into(),
        lines: vec![b"pane disappeared".to_vec()],
    };
    state.apply_control(
        StreamControl::Membership {
            pane_ids: vec!["%2".into()],
        },
        &Mutex::new(HashSet::new()),
    );
    state.handle(
        ControlRecord::Error {
            tag,
            arguments: "1 2 1".into(),
        },
        harness.runtime(),
    );

    assert_eq!(harness.events(), Vec::new());
}

#[test]
fn pane_remove_and_readd_before_fence_cannot_publish_the_old_capture() {
    let (mut state, mut harness) = Harness::new(&["%1".into()]);
    let tag = CommandTag {
        timestamp: 1,
        number: 2,
        flags: 1,
    };
    state.command_block = CommandBlock::CaptureMetadata {
        tag,
        pane_id: "%1".into(),
        visible_lines: vec![b"obsolete".to_vec()],
        saved_normal_lines: Vec::new(),
        visible_boundary: 0,
        lines: vec![b"1,1,0,0,0,0,0,0,0,0,0".to_vec()],
    };

    state.apply_control(
        StreamControl::Membership {
            pane_ids: Vec::new(),
        },
        &harness.capture_in_flight,
    );
    state.apply_control(
        StreamControl::Membership {
            pane_ids: vec!["%1".into()],
        },
        &harness.capture_in_flight,
    );
    state.handle(
        ControlRecord::End {
            tag,
            arguments: "1 2 1".into(),
        },
        harness.runtime(),
    );

    assert_eq!(harness.events(), Vec::new());
    assert!(matches!(
        state.pane_states.get("%1"),
        Some(PaneSeedState::Pending { .. })
    ));
    assert!(matches!(state.command_block, CommandBlock::None));
}

#[test]
fn parser_error_inside_capture_drains_rows_until_the_matching_fence() {
    let (mut state, mut harness) = Harness::new(&["%1".into()]);
    state.expected_capture = Some("%1".into());
    let mut parser = ControlParser::new(20);
    parser.push(b"%begin 1 2 1\n123456789012345678901\nremaining row\n%end 1 2 1\n");

    while let Some(record) = parser.next_record() {
        match record {
            Ok(record) => state.handle(record, harness.runtime()),
            Err(_) => state.resnapshot_all(&harness.writer, &harness.resources, &harness.stopped),
        }
    }

    assert_eq!(harness.events(), Vec::new());
    assert_eq!(harness.writes(), vec![("%1".into(), false)]);
    assert!(matches!(state.command_block, CommandBlock::None));
    assert!(matches!(
        state.pane_states.get("%1"),
        Some(PaneSeedState::Pending { .. })
    ));
}

/// A pane's capture leaves membership with the pane.
///
/// Only the metadata block clears a ledger entry, and a capture whose marker is
/// filtered away for leaving membership never reaches one. Left behind, that
/// entry outlives the pane: the id is re-added later, every seed it asks for is
/// coalesced against a photograph nobody is taking, and the pane stays blank
/// for as long as the control client lives.
#[test]
fn a_pane_leaving_membership_takes_its_capture_out_of_the_ledger() {
    let (mut state, _harness) = Harness::new(&["%1".into(), "%2".into()]);
    let ledger = Mutex::new(HashSet::from(["%1".to_owned(), "%2".to_owned()]));

    state.apply_control(
        StreamControl::Membership {
            pane_ids: vec!["%2".into()],
        },
        &ledger,
    );

    assert_eq!(
        ledger.lock().unwrap().iter().cloned().collect::<Vec<_>>(),
        vec!["%2".to_owned()]
    );
}

/// Every exit from a command block frees the capture ledger, the drained one
/// included.
///
/// A block is drained because the capture inside it was abandoned — a
/// membership change, a resnapshot — so nothing is going to answer it. The
/// ledger's one job is to suppress a second photograph while one is coming, and
/// an entry nobody will ever clear suppresses every seed this pane asks for for
/// as long as the client lives.
#[test]
fn a_drained_block_that_errors_frees_the_capture_ledger() {
    let (mut state, harness) = Harness::new(&["%1".into()]);
    let tag = CommandTag {
        timestamp: 1,
        number: 1,
        flags: 1,
    };
    state.command_block = CommandBlock::Draining { tag };
    harness
        .capture_in_flight
        .lock()
        .unwrap()
        .insert("%1".into());

    state.handle(
        ControlRecord::Error {
            tag,
            arguments: "1 2 1".into(),
        },
        harness.runtime(),
    );

    assert!(
        harness.capture_in_flight.lock().unwrap().is_empty(),
        "a drained capture stayed in the ledger and would silence the seed that replaces it"
    );
    assert!(matches!(state.command_block, CommandBlock::None));
}

/// A resnapshot photographs the panes somebody is looking at.
///
/// A hidden pane's screen is discarded on the way out — only a visible pane's
/// seed is emitted — so capturing it is work tmux does for nobody, and its
/// reveal takes a fresh photograph regardless. What it must not do is answer
/// that reveal with the tail it was holding when the stream lost bytes, so the
/// pane is marked as owing a seed here instead of being captured.
#[test]
fn a_resnapshot_photographs_the_visible_panes_and_indebts_the_hidden_ones() {
    let (mut state, harness) = Harness::new(&["%1".into(), "%2".into()]);
    {
        let mut resources = harness.resources.lock().unwrap();
        resources.set_visible("%1", true, 1);
        resources.set_visible("%2", false, 1);
    }

    state.resnapshot_all(&harness.writer, &harness.resources, &harness.stopped);

    assert_eq!(harness.writes(), vec![("%1".into(), false)]);
    assert!(matches!(
        state.pane_states.get("%1"),
        Some(PaneSeedState::Pending { .. })
    ));
    assert!(matches!(
        state.pane_states.get("%2"),
        Some(PaneSeedState::Live)
    ));
    let resources = harness.resources.lock().unwrap();
    assert!(resources.get("%2").unwrap().requires_seed);
    assert!(!resources.get("%1").unwrap().requires_seed);
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
    let stopped = Arc::new(AtomicBool::new(false));
    let output_credit = super::OutputCredit::negotiated(false);
    let emission_order = Arc::new(Mutex::new(()));
    let topology_trigger = TopologyOutputTrigger::default();
    state.finish_block(
        tag,
        StreamRuntime {
            writer: &writer,
            sender: &sender,
            overflowed: &overflowed,
            resources: &resources,
            terminal_generation: &generation,
            stopped: &stopped,
            capture_in_flight: &Mutex::new(HashSet::new()),
            output_credit: &output_credit,
            emission_order: &emission_order,
            topology_trigger: &topology_trigger,
            read_started: std::time::Instant::now(),
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

#[test]
fn terminal_output_waits_for_bounded_sequencer_capacity_without_marking_overflow() {
    let (sender, mut receiver) = mpsc::channel(1);
    sender
        .try_send(SequencerControl::OrderedEvent(v1::HostEvent::default()))
        .unwrap();
    let overflowed = Arc::new(AtomicBool::new(false));
    let thread_overflowed = Arc::clone(&overflowed);
    let emitted = std::thread::spawn(move || {
        let output_credit = super::OutputCredit::negotiated(false);
        super::stream_helpers::emit_terminal(
            &sender,
            &thread_overflowed,
            v1::EventKind::TerminalOutput,
            "%1".into(),
            b"exact".to_vec(),
            1,
            &AtomicBool::new(false),
            &output_credit,
        );
    });
    std::thread::sleep(std::time::Duration::from_millis(10));
    assert!(
        !emitted.is_finished(),
        "a full bounded queue must apply backpressure"
    );

    receiver.blocking_recv().unwrap();
    emitted.join().unwrap();
    assert!(!overflowed.load(Ordering::Acquire));
    let SequencerControl::OrderedEvent(event) = receiver.blocking_recv().unwrap() else {
        panic!("terminal output was not queued");
    };
    assert_eq!(event.terminal.unwrap().data, b"exact");
}

/// The backpressure above must have an exit: connection teardown aborts the
/// sequencer writer, which drops the receiver, and that is the only thing that
/// can release a reader already parked in the channel send. A parked reader
/// that outlives the receiver held its connection task — and the remote
/// bridge process — alive indefinitely.
#[test]
fn dropping_the_sequencer_receiver_releases_a_parked_terminal_emitter() {
    let (sender, receiver) = mpsc::channel(1);
    sender
        .try_send(SequencerControl::OrderedEvent(v1::HostEvent::default()))
        .unwrap();
    let overflowed = Arc::new(AtomicBool::new(false));
    let thread_overflowed = Arc::clone(&overflowed);
    let emitted = std::thread::spawn(move || {
        let output_credit = super::OutputCredit::negotiated(false);
        super::stream_helpers::emit_terminal(
            &sender,
            &thread_overflowed,
            v1::EventKind::TerminalOutput,
            "%1".into(),
            b"parked".to_vec(),
            1,
            &AtomicBool::new(false),
            &output_credit,
        );
    });
    std::thread::sleep(std::time::Duration::from_millis(10));
    assert!(
        !emitted.is_finished(),
        "a full bounded queue must apply backpressure"
    );

    drop(receiver);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !emitted.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "a parked emitter must return once the receiver is gone"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    emitted.join().unwrap();
    assert!(overflowed.load(Ordering::Acquire));
}

/// The scrollback a screen-only seed leaves behind, fetched on demand.
///
/// The whole point of the block is what it does *not* do: the pane stays
/// exactly as pending or as live as it was, the generation counter does not
/// move, and no seed is built. It is an answer to a question, delivered beside
/// the output stream rather than inside it.
#[test]
fn a_history_block_answers_with_the_scrollback_and_moves_nothing_else() {
    let (mut state, mut harness) = Harness::new(&["%1".into()]);
    let generation_before = harness.generation.load(Ordering::Acquire);

    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    state.handle(
        ControlRecord::CommandOutput(b"__ADE_HISTORY__:300:1".to_vec()),
        harness.runtime(),
    );
    state.handle(
        ControlRecord::End {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    assert_eq!(state.expected_history.as_deref(), Some("%1"));

    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    for line in [b"older".to_vec(), b"newer".to_vec()] {
        state.handle(ControlRecord::CommandOutput(line), harness.runtime());
    }
    state.handle(
        ControlRecord::End {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    // Nothing yet: the rows wait for the size probe, so the renderer gets one
    // answer that says both what is above its screen and whether that is all.
    assert!(harness.events().is_empty());

    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    state.handle(
        ControlRecord::CommandOutput(b"__ADE_HISTORY_META__:1200".to_vec()),
        harness.runtime(),
    );
    state.handle(
        ControlRecord::End {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );

    let events = harness.events();
    assert_eq!(
        kinds(&events),
        vec![(v1::EventKind::TerminalHistory, String::new())]
    );
    let terminal = events[0].terminal.as_ref().expect("history carries bytes");
    assert_eq!(terminal.pane_id, "%1");
    assert_eq!(terminal.data, b"older\r\nnewer");
    // The whole point of the third block: 1,200 lines above the screen, which
    // is how the renderer knows a 300-line page from 40 above the display has
    // more behind it.
    assert_eq!(terminal.history_size, 1_200);
    assert!(terminal.history_size_known);
    // Not part of the output stream: it claims no place in the generation
    // ordering, so a renderer's monotonic gate can never discard output because
    // a history answer went past it.
    assert_eq!(terminal.generation, 0);
    assert_eq!(
        harness.generation.load(Ordering::Acquire),
        generation_before
    );
    assert!(matches!(
        state.pane_states.get("%1"),
        Some(PaneSeedState::Pending { .. })
    ));
    assert!(state.expected_history.is_none());
    assert!(state.pending_history_meta.is_none());
    assert!(harness.writes().is_empty());
}

/// The size probe is targeted, so a pane that goes away mid-request rejects it
/// — and the rows tmux already handed over are still the page that was asked
/// for. They are delivered without a size, which the renderer reads as "ask
/// again", never as the top of the history. Dropping them instead would leave
/// the pane waiting on a request nothing will ever answer.
#[test]
fn a_history_whose_size_probe_is_rejected_is_still_answered_without_one() {
    let (mut state, mut harness) = Harness::new(&["%1".into()]);
    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    state.handle(
        ControlRecord::CommandOutput(b"__ADE_HISTORY__:300:1".to_vec()),
        harness.runtime(),
    );
    state.handle(
        ControlRecord::End {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    state.handle(
        ControlRecord::CommandOutput(b"older".to_vec()),
        harness.runtime(),
    );
    state.handle(
        ControlRecord::End {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );

    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    state.handle(
        ControlRecord::Error {
            tag: TAG,
            arguments: "1 7 1".into(),
        },
        harness.runtime(),
    );

    let events = harness.events();
    let history = events
        .iter()
        .find(|event| event.kind == i32::from(v1::EventKind::TerminalHistory))
        .expect("the captured page is still answered");
    let terminal = history.terminal.as_ref().expect("history carries bytes");
    assert_eq!(terminal.data, b"older");
    assert!(!terminal.history_size_known);
    assert_eq!(terminal.history_size, 0);
    // And the sequence is over: nothing is left to be mistaken for the missing
    // half of it.
    assert!(state.pending_history_meta.is_none());
    assert!(state.expected_history.is_none());
}

/// A probe that answered something this host cannot read is the same answer as
/// one that did not answer at all: a page to ask about again, never the end.
#[test]
fn a_history_size_that_is_not_a_number_is_no_size_at_all() {
    assert_eq!(
        history_size_marker(b"__ADE_HISTORY_META__:1200"),
        Some(1_200)
    );
    assert_eq!(history_size_marker(b"__ADE_HISTORY_META__:0"), Some(0));
    assert_eq!(history_size_marker(b"__ADE_HISTORY_META__:"), None);
    assert_eq!(
        history_size_marker(b"__ADE_HISTORY_META__:#{history_size}"),
        None
    );
    // Not the leading marker, which names a pane rather than a size.
    assert_eq!(history_size_marker(b"__ADE_HISTORY__:300:1"), None);
}

/// A history marker for a pane this client does not own is addressed to
/// nobody, and the block after it must not be read as one.
#[test]
fn a_history_marker_for_an_unowned_pane_correlates_to_nothing() {
    let (mut state, harness) = Harness::new(&["%1".into()]);
    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    state.handle(
        ControlRecord::CommandOutput(b"__ADE_HISTORY__:2000:9".to_vec()),
        harness.runtime(),
    );
    state.handle(
        ControlRecord::End {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    assert!(state.expected_history.is_none());
}

/// Drives one untargeted marker block — the shape every correlation starts as.
fn marker_block(state: &mut StreamState, harness: &Harness, marker: &[u8]) {
    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    state.handle(
        ControlRecord::CommandOutput(marker.to_vec()),
        harness.runtime(),
    );
    state.handle(
        ControlRecord::End {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
}

/// A recovery releases *every* correlation slot, not the two it happens to
/// name.
///
/// `start_block` reads `expected_history` before `expected_capture`, so a
/// history slot outliving a resnapshot is spent on the recovery's own capture:
/// the marker line goes out to the desktop as scrollback, the screen behind it
/// falls through as an unrecognised block, and the pane sits Pending with
/// nothing left coming to seed it.
#[test]
fn a_resnapshot_releases_a_pending_history_so_its_own_capture_is_read_as_one() {
    let (mut state, mut harness) = Harness::new(&["%1".into()]);
    marker_block(&mut state, &harness, b"__ADE_HISTORY__:2000:1");
    assert_eq!(state.expected_history.as_deref(), Some("%1"));

    state.resnapshot_all(&harness.writer, &harness.resources, &harness.stopped);
    assert!(state.expected_history.is_none());
    assert_eq!(harness.writes(), vec![("%1".to_owned(), false)]);

    marker_block(&mut state, &harness, b"__ADE_CAPTURE__:1");
    assert_eq!(state.expected_capture.as_deref(), Some("%1"));
    assert!(state.expected_history.is_none());
    state.handle(
        ControlRecord::Begin {
            tag: TAG,
            arguments: String::new(),
        },
        harness.runtime(),
    );
    assert!(matches!(
        state.command_block,
        CommandBlock::CapturePrimary { .. }
    ));
    // Nothing was published as the answer to a question nobody asked.
    assert_eq!(harness.events(), Vec::new());
}
