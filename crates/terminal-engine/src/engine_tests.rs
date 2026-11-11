//! The handshake, against a scripted host. Port of the behavior in
//! `TerminalPaneHostHandshake.test.tsx`.

use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use serde_json::json;

use super::*;
use crate::event::{PaneEvent, PaneResource, PaneResourceState};
use crate::pager::HistoryPageTrigger;
use crate::pane::EngineOutput as Out;
use crate::reading::TerminalReadingState;

const PANE: &str = "%1";
const SIZE: GridSize = GridSize {
    columns: 20,
    rows: 4,
};
const STALE_EPOCH: &str = "terminal_visibility_epoch_rejected: terminal visibility checkpoint belongs to a stale connection epoch";

struct Host {
    engine: Engine,
    now: Instant,
    sequence: u64,
    log: Vec<Out>,
}

impl Host {
    /// An engine on client `c1`, with epoch 1 announced.
    fn connected() -> Self {
        let mut host = Self {
            engine: Engine::default(),
            now: Instant::now(),
            sequence: 0,
            log: Vec::new(),
        };
        host.command(Command::SetClient(Some("c1".into())));
        host.publish(TerminalEvent::GenerationEpoch {
            epoch: 1,
            sequence: 0,
        });
        host.log.clear();
        host
    }

    fn command(&mut self, command: Command) {
        let produced = self.engine.handle(command, self.now);
        self.log.extend(produced);
    }

    fn publish(&mut self, event: TerminalEvent) {
        self.engine.publish_event(event, self.now);
        self.engine.pump(self.now);
        self.log.append(&mut self.engine.shared.out);
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    fn seed(&mut self, generation: u64, data: &str) {
        let sequence = self.next_sequence();
        self.publish(TerminalEvent::Pane(PaneEvent::Seed {
            pane_id: PANE.into(),
            generation,
            data: data.as_bytes().to_vec(),
            sequence,
        }));
    }

    fn output(&mut self, generation: u64, data: &str) {
        let sequence = self.next_sequence();
        self.publish(TerminalEvent::Pane(PaneEvent::Output {
            pane_id: PANE.into(),
            generation,
            data: data.as_bytes().to_vec(),
            sequence,
        }));
    }

    fn resource(&mut self, edit: impl FnOnce(&mut PaneResource)) {
        let sequence = self.next_sequence();
        let mut resource = PaneResource {
            pane_id: PANE.into(),
            state: PaneResourceState::Visible,
            requires_seed: false,
            resume_from_renderer: false,
            recovery_reason: String::new(),
            generation: 100 + sequence,
            snapshot_generation: 0,
            tail_through_generation: 0,
            raw_tail: Vec::new(),
            sequence,
        };
        edit(&mut resource);
        self.publish(TerminalEvent::Pane(PaneEvent::Resource(resource)));
    }

    fn mount(&mut self) {
        self.command(Command::Mount {
            pane_id: PANE.into(),
            size: SIZE,
        });
    }

    fn unmount(&mut self) {
        self.command(Command::Unmount {
            pane_id: PANE.into(),
        });
    }

    fn advance(&mut self, by: Duration) {
        let end = self.now + by;
        while let Some(deadline) = self.engine.next_deadline() {
            if deadline > end {
                break;
            }
            self.now = deadline.max(self.now);
            let produced = self.engine.poll(self.now);
            self.log.extend(produced);
        }
        self.now = end;
    }

    fn answer_last_visibility(&mut self, result: Result<(), String>) {
        let token = self
            .visibility()
            .last()
            .expect("a visibility request")
            .token;
        self.command(Command::VisibilityAnswered { token, result });
    }

    fn visibility(&self) -> Vec<Visibility> {
        self.log
            .iter()
            .filter_map(|output| match output {
                Out::SetVisibility {
                    visible,
                    holds_snapshot,
                    checkpoint,
                    token,
                    ..
                } => Some(Visibility {
                    visible: *visible,
                    holds_snapshot: *holds_snapshot,
                    output_generation: checkpoint.output_generation,
                    token: *token,
                }),
                _ => None,
            })
            .collect()
    }

    fn seed_requests(&self) -> usize {
        self.log
            .iter()
            .filter(|output| matches!(output, Out::RequestSeed { .. }))
            .count()
    }

    fn incidents(&self, wanted: &str) -> Vec<serde_json::Value> {
        self.log
            .iter()
            .filter_map(|output| match output {
                Out::Incident { kind, detail } if *kind == wanted => Some(detail.clone()),
                _ => None,
            })
            .collect()
    }

    /// The banner as the pane last set it; `None` once cleared or never set.
    fn renderer_diagnostic(&self) -> Option<String> {
        self.log.iter().rev().find_map(|output| match output {
            Out::RendererDiagnostic { message, .. } => Some(message.clone()),
            _ => None,
        })?
    }

    fn reading_states(&self) -> Vec<TerminalReadingState> {
        self.log
            .iter()
            .filter_map(|output| match output {
                Out::ReadingChanged { state, .. } => Some(*state),
                _ => None,
            })
            .collect()
    }

    fn display_offset(&self) -> usize {
        let terminal = self.engine.pane_terminal(PANE).expect("mounted");
        terminal.lock().grid().display_offset()
    }

    /// A shown pane with history, scrolled up one row and reported as read.
    fn reading_pane(&mut self) {
        self.shown_pane(1, "a\r\nb\r\nc\r\nd\r\ne\r\nf");
        self.engine
            .pane_terminal(PANE)
            .unwrap()
            .lock()
            .scroll_display(Scroll::Delta(1));
        self.command(Command::Viewport {
            pane_id: PANE.into(),
            at_bottom: false,
        });
        self.log.clear();
    }

    fn resume_kept_screen(&mut self, tail: &str) {
        self.resource(|r| {
            r.resume_from_renderer = true;
            r.snapshot_generation = 1;
            r.tail_through_generation = 3;
            r.raw_tail = tail.as_bytes().to_vec();
        });
    }

    fn revealed(&self) -> bool {
        self.log
            .iter()
            .any(|output| matches!(output, Out::Revealed { .. }))
    }

    fn screen(&self) -> String {
        let terminal = self.engine.pane_terminal(PANE).expect("mounted");
        let term = terminal.lock();
        (0..term.screen_lines() as i32)
            .map(|line| {
                let row = &term.grid()[Line(line)];
                (0..term.columns())
                    .map(|column| row[Column(column)].c)
                    .collect::<String>()
            })
            .map(|text| text.trim_end().to_owned())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("|")
    }

    /// A pane that has been mounted, seeded and shown, with nothing outstanding.
    fn shown_pane(&mut self, generation: u64, data: &str) {
        self.mount();
        self.answer_last_visibility(Ok(()));
        self.seed(generation, data);
        self.log.clear();
    }

    /// Hides a shown pane, has the host accept it, and mounts it again.
    fn hide_and_remount(&mut self) {
        self.unmount();
        self.answer_last_visibility(Ok(()));
        self.log.clear();
        self.mount();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Visibility {
    visible: bool,
    holds_snapshot: bool,
    output_generation: u64,
    token: u64,
}

#[test]
fn shows_a_pane_the_content_tmux_was_already_holding_and_reveals_it() {
    let mut host = Host::connected();
    host.mount();
    assert!(host.visibility()[0].visible);
    host.seed(1, "BEFORE-CONNECT");
    assert_eq!(host.screen(), "BEFORE-CONNECT");
    assert!(host.revealed());
}

#[test]
fn keeps_live_output_flowing_onto_the_screen_the_handshake_brought() {
    let mut host = Host::connected();
    host.shown_pane(1, "HISTORY");
    host.output(2, "|LIVE");
    assert_eq!(host.screen(), "HISTORY|LIVE");
}

#[test]
fn defers_output_until_the_reveal_is_answered_and_then_drops_what_the_tail_covered() {
    let mut host = Host::connected();
    host.shown_pane(5, "base");
    host.hide_and_remount();
    host.output(6, "-covered");
    host.output(7, "-new");
    assert_eq!(host.screen(), "base");
    host.resource(|r| {
        r.resume_from_renderer = true;
        r.snapshot_generation = 5;
        r.tail_through_generation = 6;
        r.raw_tail = b"-covered".to_vec();
    });
    assert_eq!(host.screen(), "base-covered-new");
}

#[test]
fn asks_for_a_seed_when_the_host_answers_with_unspecified_and_no_material() {
    let mut host = Host::connected();
    host.mount();
    host.resource(|r| r.state = PaneResourceState::Unspecified);
    assert_eq!(host.seed_requests(), 1);
    assert_eq!(
        host.incidents("pane.revealDeadEnd"),
        [json!({ "paneId": PANE, "state": "unspecified" })]
    );
    host.seed(200, "TMUX HAS THIS");
    assert_eq!(host.screen(), "TMUX HAS THIS");
}

#[test]
fn ignores_its_own_hide_echoed_back_rather_than_blanking_the_pane() {
    let mut host = Host::connected();
    host.shown_pane(1, "SHOWING");
    host.resource(|r| r.state = PaneResourceState::HiddenBuffered);
    assert_eq!(host.screen(), "SHOWING");
    assert_eq!(host.seed_requests(), 0);
    assert_eq!(host.incidents("pane.hideEchoAfterReady").len(), 1);
}

#[test]
fn asks_for_a_seed_on_the_spot_when_the_echo_reaches_a_pane_with_nothing_on_it() {
    let mut host = Host::connected();
    host.mount();
    host.resource(|r| r.state = PaneResourceState::HiddenBuffered);
    assert_eq!(host.seed_requests(), 1);
    assert_eq!(
        host.incidents("pane.revealDeadEnd"),
        Vec::<serde_json::Value>::new()
    );
}

#[test]
fn still_bounds_the_wait_when_neither_the_reveal_nor_the_seed_is_answered() {
    let mut host = Host::connected();
    host.mount();
    host.resource(|r| r.state = PaneResourceState::HiddenBuffered);
    let before = host.visibility().len();
    host.advance(Duration::from_millis(2_001));
    assert_eq!(host.seed_requests(), 2);
    assert_eq!(
        host.visibility().len(),
        before + 1,
        "the retry re-asserts visibility"
    );
    assert_eq!(
        host.incidents("pane.degraded"),
        [json!({ "paneId": PANE, "reason": "paneAwaitingSeed", "attempt": 0 })]
    );
    assert!(
        host.renderer_diagnostic()
            .is_some_and(|message| message.ends_with("; retrying…")),
        "the banner says it is retrying"
    );
}

#[test]
fn keeps_painting_output_after_a_dead_end_answer_is_repaired() {
    let mut host = Host::connected();
    host.mount();
    host.resource(|r| r.state = PaneResourceState::Unspecified);
    host.seed(200, "BACK");
    host.output(201, "+MORE");
    assert_eq!(host.screen(), "BACK+MORE");
}

#[test]
fn writes_only_the_tail_when_the_host_hands_back_the_screen_kept_here() {
    let mut host = Host::connected();
    host.shown_pane(5, "kept");
    host.hide_and_remount();
    assert_eq!(host.screen(), "kept");
    assert_eq!(
        host.visibility()[0],
        Visibility {
            visible: true,
            holds_snapshot: true,
            output_generation: 5,
            token: host.visibility()[0].token,
        }
    );
    host.resource(|r| {
        r.resume_from_renderer = true;
        r.snapshot_generation = 5;
        r.tail_through_generation = 8;
        r.raw_tail = b"-tail".to_vec();
    });
    assert_eq!(host.screen(), "kept-tail");
    assert_eq!(host.seed_requests(), 0);
}

#[test]
fn still_acknowledges_and_reveals_when_the_resume_has_no_tail() {
    let mut host = Host::connected();
    host.shown_pane(5, "kept");
    host.hide_and_remount();
    assert!(host.revealed(), "the kept screen is content");
    host.resource(|r| {
        r.resume_from_renderer = true;
        r.snapshot_generation = 5;
        r.tail_through_generation = 5;
    });
    assert_eq!(host.screen(), "kept");
    host.output(500, "-live");
    assert_eq!(host.screen(), "kept-live");
}

#[test]
fn refuses_a_tail_for_a_screen_this_side_is_not_holding() {
    let mut host = Host::connected();
    host.shown_pane(5, "kept");
    host.hide_and_remount();
    host.resource(|r| {
        r.resume_from_renderer = true;
        r.snapshot_generation = 4;
        r.tail_through_generation = 8;
        r.raw_tail = b"-tail".to_vec();
    });
    assert_eq!(host.screen(), "");
    assert_eq!(host.seed_requests(), 1);
}

#[test]
fn takes_the_checkpoint_free_seed_path_instead_of_replaying_a_stale_epoch() {
    let mut host = Host::connected();
    host.mount();
    host.answer_last_visibility(Err(STALE_EPOCH.to_owned()));
    assert_eq!(host.seed_requests(), 1);
    assert_eq!(host.incidents("pane.revealRebuilt").len(), 1);
    let sent = host.visibility().len();
    host.advance(Duration::from_millis(1_000));
    assert_eq!(
        host.visibility().len(),
        sent,
        "no retry of a refused checkpoint"
    );
}

#[test]
fn still_retries_a_transport_that_is_only_coming_up() {
    let mut host = Host::connected();
    host.mount();
    host.answer_last_visibility(Err("host bridge is disconnected".to_owned()));
    assert_eq!(host.seed_requests(), 0);
    assert_eq!(host.incidents("pane.revealRetry").len(), 1);
    host.advance(Duration::from_millis(251));
    assert_eq!(host.visibility().len(), 2);
    host.answer_last_visibility(Ok(()));
    assert_eq!(host.incidents("pane.revealRetry").len(), 1);
}

#[test]
fn degrades_when_the_reveal_is_refused_outright_and_keeps_asking_on_the_watchdog() {
    let mut host = Host::connected();
    host.mount();
    host.answer_last_visibility(Err("terminal visibility conflict".to_owned()));
    assert_eq!(host.incidents("pane.revealFailed").len(), 1);
    assert_eq!(host.seed_requests(), 1);
    host.advance(Duration::from_millis(2_001));
    assert_eq!(host.visibility().len(), 2);
}

#[test]
fn reseeds_a_pane_whose_reveal_produced_nothing_and_stops_once_it_paints() {
    let mut host = Host::connected();
    host.mount();
    host.answer_last_visibility(Ok(()));
    host.advance(Duration::from_millis(3_999));
    assert_eq!(host.seed_requests(), 0);
    host.advance(Duration::from_millis(2));
    assert_eq!(
        host.incidents("pane.revealVoid"),
        [json!({ "paneId": PANE, "msWaited": 4_000, "attempt": 0 })]
    );
    assert_eq!(host.seed_requests(), 1);
    host.seed(1, "HERE");
    host.advance(Duration::from_secs(20));
    assert_eq!(host.seed_requests(), 1);
}

#[test]
fn gives_up_after_its_bounded_attempts_when_nothing_ever_answers() {
    let mut host = Host::connected();
    host.mount();
    host.answer_last_visibility(Ok(()));
    host.advance(Duration::from_secs(60));
    let attempts: Vec<_> = host
        .incidents("pane.revealVoid")
        .iter()
        .map(|detail| detail["attempt"].as_u64().unwrap())
        .collect();
    assert_eq!(attempts, [0, 1, 2]);
    assert_eq!(host.seed_requests(), 3);
}

#[test]
fn says_nothing_about_an_ordinary_handshake() {
    let mut host = Host::connected();
    host.mount();
    host.seed(1, "first");
    host.answer_last_visibility(Ok(()));
    host.advance(Duration::from_secs(20));
    assert_eq!(host.seed_requests(), 0);
    assert!(
        !host
            .log
            .iter()
            .any(|output| matches!(output, Out::Incident { kind, .. } if *kind != "link.epoch"))
    );
}

#[test]
fn a_reveal_the_host_answers_over_the_screen_already_held_is_not_void() {
    let mut host = Host::connected();
    host.shown_pane(1, "IDLE PROMPT");
    host.hide_and_remount();
    host.resource(|_| {});
    host.answer_last_visibility(Ok(()));
    host.advance(Duration::from_secs(20));
    assert_eq!(host.incidents("pane.revealVoid").len(), 0);
    assert_eq!(host.seed_requests(), 0);
    assert_eq!(host.screen(), "IDLE PROMPT");
}
#[test]
fn takes_its_timers_with_it_when_the_pane_unmounts_first() {
    let mut host = Host::connected();
    host.mount();
    host.answer_last_visibility(Ok(()));
    host.unmount();
    assert_eq!(host.engine.next_deadline(), None);
    host.advance(Duration::from_secs(10));
    assert_eq!(host.incidents("pane.revealVoid").len(), 0);
}

#[test]
fn hides_with_the_applied_generation_and_says_it_kept_the_screen() {
    let mut host = Host::connected();
    host.shown_pane(5, "kept");
    host.output(6, "+more");
    host.unmount();
    let hide = *host.visibility().last().unwrap();
    assert!(!hide.visible);
    assert!(hide.holds_snapshot);
    assert_eq!(hide.output_generation, 6);
    assert_eq!(host.engine.retained_screen_count(), 1);
}

#[test]
fn a_reveal_waits_for_the_hide_still_outstanding_at_the_host() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    host.unmount();
    let hides = host.visibility().len();
    host.mount();
    assert_eq!(
        host.visibility().len(),
        hides,
        "no reveal behind an unanswered hide"
    );
    host.answer_last_visibility(Ok(()));
    assert_eq!(host.visibility().len(), hides + 1);
    assert!(host.visibility().last().unwrap().visible);
}

#[test]
fn a_reveal_stops_waiting_for_a_hide_that_never_answers() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    host.unmount();
    let hide = *host.visibility().last().unwrap();
    host.mount();
    host.advance(Duration::from_millis(2_001));
    assert_eq!(host.incidents("terminal.pane.handoffTimeouts").len(), 1);
    assert!(host.visibility().last().unwrap().visible);
    // A late refusal is not the reveal's, but the cutoff it rejects still stands.
    let reveals = host.visibility().len();
    host.command(Command::VisibilityAnswered {
        token: hide.token,
        result: Err("late".into()),
    });
    assert_eq!(host.visibility().len(), reveals);
    assert_eq!(host.seed_requests(), 1);
}

#[test]
fn reseeds_a_pane_whose_hide_checkpoint_was_refused() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    host.unmount();
    host.answer_last_visibility(Err("refused".into()));
    assert_eq!(host.seed_requests(), 1);
}

#[test]
fn does_not_keep_a_screen_the_user_was_reading_past_new_output() {
    let mut host = Host::connected();
    host.shown_pane(1, "base");
    host.command(Command::Viewport {
        pane_id: PANE.into(),
        at_bottom: false,
    });
    host.output(2, "+skipped");
    assert_eq!(
        host.screen(),
        "base",
        "output does not touch a screen being read"
    );
    assert!(host.log.iter().any(|output| matches!(
        output,
        Out::ReadingChanged {
            state: TerminalReadingState::Outdated,
            ..
        }
    )));
    host.unmount();
    let hide = *host.visibility().last().unwrap();
    assert!(!hide.holds_snapshot);
    assert_eq!(host.engine.retained_screen_count(), 0);
}

#[test]
fn asks_for_one_authoritative_seed_on_the_way_back_to_live_and_ignores_a_late_answer() {
    let mut host = Host::connected();
    host.shown_pane(1, "base");
    host.command(Command::Viewport {
        pane_id: PANE.into(),
        at_bottom: false,
    });
    host.output(2, "+skipped");
    host.command(Command::ReturnToLive {
        pane_id: PANE.into(),
        reason: "typed".into(),
    });
    assert_eq!(host.seed_requests(), 1);
    assert_eq!(
        host.screen(),
        "base",
        "the old screen stays up until the seed lands"
    );
    host.resource(|r| r.state = PaneResourceState::Unspecified);
    assert_eq!(
        host.seed_requests(),
        1,
        "a late answer does not ask a second time"
    );
    host.seed(500, "fresh");
    assert_eq!(host.screen(), "fresh");
}

#[test]
fn scrolls_locally_when_returning_to_live_from_a_screen_that_is_still_current() {
    let mut host = Host::connected();
    host.shown_pane(1, "a\r\nb\r\nc\r\nd\r\ne\r\nf");
    host.engine
        .pane_terminal(PANE)
        .unwrap()
        .lock()
        .scroll_display(Scroll::Delta(1));
    host.command(Command::Viewport {
        pane_id: PANE.into(),
        at_bottom: false,
    });
    host.command(Command::ReturnToLive {
        pane_id: PANE.into(),
        reason: "typed".into(),
    });
    assert_eq!(host.seed_requests(), 0);
    assert_eq!(
        host.engine
            .pane_terminal(PANE)
            .unwrap()
            .lock()
            .grid()
            .display_offset(),
        0
    );
}

#[test]
fn prefetches_a_page_after_a_seed_and_splices_the_answer_above_the_screen() {
    let mut host = Host::connected();
    host.mount();
    host.seed(1, "screen");
    let request = host.log.iter().find_map(|output| match output {
        Out::RequestHistory { lines, skip, .. } => Some((*lines, *skip)),
        _ => None,
    });
    assert_eq!(request, Some((300, 0)));
    let sequence = host.next_sequence();
    host.publish(TerminalEvent::Pane(PaneEvent::History {
        pane_id: PANE.into(),
        data: b"older\r\nolder2".to_vec(),
        history_size: Some(2),
        sequence,
    }));
    let terminal = host.engine.pane_terminal(PANE).unwrap();
    assert_eq!(terminal.facts().scrollback_rows, 2);
    assert_eq!(host.screen(), "screen");
    host.command(Command::RequestPage {
        pane_id: PANE.into(),
        trigger: HistoryPageTrigger::ScrolledToTop,
    });
    let requests = host
        .log
        .iter()
        .filter(|o| matches!(o, Out::RequestHistory { .. }))
        .count();
    assert_eq!(requests, 1, "tmux said that was the top");
}

#[test]
fn a_kept_screen_keeps_its_place_in_the_history_walk() {
    let mut host = Host::connected();
    host.mount();
    host.answer_last_visibility(Ok(()));
    host.seed(1, "screen");
    let sequence = host.next_sequence();
    host.publish(TerminalEvent::Pane(PaneEvent::History {
        pane_id: PANE.into(),
        data: b"older".to_vec(),
        history_size: Some(9_000),
        sequence,
    }));
    host.hide_and_remount();
    host.command(Command::RequestPage {
        pane_id: PANE.into(),
        trigger: HistoryPageTrigger::Prefetch,
    });
    let request = host.log.iter().find_map(|output| match output {
        Out::RequestHistory { lines, skip, .. } => Some((*lines, *skip)),
        _ => None,
    });
    assert_eq!(request, Some((600, 1)));
}

#[test]
fn applies_a_grid_change_at_its_place_in_the_stream() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    let sequence = host.next_sequence();
    let size = GridSize {
        columns: 40,
        rows: 12,
    };
    host.publish(TerminalEvent::TopologyDirty {
        grids: Some(vec![crate::event::PaneGrid {
            pane_id: PANE.into(),
            size,
        }]),
        sequence,
    });
    assert_eq!(host.engine.pane_terminal(PANE).unwrap().facts().grid, size);
    assert_eq!(host.seed_requests(), 0, "a resize is not a reset");
}

#[test]
fn a_new_epoch_drops_every_kept_screen_and_reveals_again() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    host.unmount();
    host.answer_last_visibility(Ok(()));
    host.command(Command::Mount {
        pane_id: "%2".into(),
        size: SIZE,
    });
    assert_eq!(host.engine.retained_screen_count(), 1);
    let before = host.visibility().len();
    host.publish(TerminalEvent::GenerationEpoch {
        epoch: 2,
        sequence: 0,
    });
    assert_eq!(host.engine.retained_screen_count(), 0);
    assert_eq!(host.visibility().len(), before + 1);
}

#[test]
fn a_seed_the_hub_requires_invalidates_a_kept_screen() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    host.unmount();
    host.answer_last_visibility(Ok(()));
    let sequence = host.next_sequence();
    host.publish(TerminalEvent::Pane(PaneEvent::Seed {
        pane_id: PANE.into(),
        generation: 1,
        data: b"stale".to_vec(),
        sequence,
    }));
    assert_eq!(host.engine.retained_screen_count(), 0);
    assert_eq!(host.seed_requests(), 1);
}

#[test]
fn a_flow_stall_drops_the_kept_screen_and_tells_the_app_once() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    host.unmount();
    host.answer_last_visibility(Ok(()));
    let sequence = host.next_sequence();
    host.publish(TerminalEvent::FlowStalled {
        pane_id: PANE.into(),
        sequence,
    });
    assert_eq!(host.engine.retained_screen_count(), 0);
    assert_eq!(
        host.log
            .iter()
            .filter(|o| matches!(o, Out::FlowStalled { .. }))
            .count(),
        1
    );
    host.publish(TerminalEvent::FlowStalled {
        pane_id: PANE.into(),
        sequence,
    });
    assert_eq!(
        host.log
            .iter()
            .filter(|o| matches!(o, Out::FlowStalled { .. }))
            .count(),
        1
    );
}

#[test]
fn a_new_client_is_revealed_to_and_an_old_answer_is_not_believed() {
    let mut host = Host::connected();
    host.mount();
    let old = host.visibility()[0].token;
    host.command(Command::SetClient(Some("c2".into())));
    assert_eq!(host.visibility().len(), 2);
    host.command(Command::VisibilityAnswered {
        token: old,
        result: Err("terminal visibility conflict".into()),
    });
    assert_eq!(host.incidents("pane.revealFailed").len(), 0);
}

#[test]
fn acknowledges_a_frame_only_after_applying_it_and_acknowledges_one_it_cannot_decode() {
    let mut host = Host::connected();
    host.mount();
    host.seed(1, "x");
    host.log.clear();
    let mut frame = vec![2_u8];
    frame.extend_from_slice(&(PANE.len() as u16).to_be_bytes());
    frame.extend_from_slice(PANE.as_bytes());
    frame.extend_from_slice(&2_u64.to_be_bytes());
    frame.extend_from_slice(&2_u64.to_be_bytes());
    frame.extend_from_slice(b"y");
    host.engine
        .shared
        .hub
        .mark_rendered(PANE, 1, Some(1), &mut Vec::new());
    let length = frame.len();
    host.command(Command::Frame(frame));
    assert!(matches!(
        host.log.last(),
        Some(Out::FrameConsumed {
            byte_length,
            epoch: Some(1)
        }) if *byte_length == length
    ));

    host.log.clear();
    host.command(Command::Frame(vec![1, 2]));
    assert_eq!(host.incidents("link.decodeFailure").len(), 1);
    assert!(matches!(
        host.log.last(),
        Some(Out::FrameConsumed { byte_length: 2, .. })
    ));
}

#[test]
fn raises_dirty_on_the_edge_and_once_more_after_the_ui_took_the_flag() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    let terminal = host.engine.pane_terminal(PANE).unwrap().clone();
    assert!(terminal.take_dirty());
    host.output(2, "a");
    host.output(3, "b");
    assert_eq!(
        host.log
            .iter()
            .filter(|o| matches!(o, Out::Dirty { .. }))
            .count(),
        1
    );
    assert!(terminal.take_dirty());
    host.output(4, "c");
    assert_eq!(
        host.log
            .iter()
            .filter(|o| matches!(o, Out::Dirty { .. }))
            .count(),
        2
    );
}

#[test]
fn lands_a_synchronized_update_when_it_times_out() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    host.output(2, "\x1b[?2026hheld");
    assert_eq!(host.screen(), "x");
    host.advance(Duration::from_secs(1));
    assert_eq!(host.screen(), "xheld");
}

#[test]
fn runs_on_its_own_thread_and_stops_when_dropped() {
    let (sink, outputs) = mpsc::channel();
    let handle = spawn(HubLimits::default(), sink);
    assert!(handle.send(Command::Mount {
        pane_id: PANE.into(),
        size: SIZE,
    }));
    let output = outputs.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(output, Out::Mounted { .. }));
    drop(handle);
    assert!(outputs.recv_timeout(Duration::from_secs(5)).is_err());
}

#[test]
fn a_later_ui_size_does_not_move_the_parser_off_the_grid_the_stream_set() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    let stream = GridSize {
        columns: 100,
        rows: 30,
    };
    let sequence = host.next_sequence();
    host.publish(TerminalEvent::TopologyDirty {
        grids: Some(vec![crate::event::PaneGrid {
            pane_id: PANE.into(),
            size: stream,
        }]),
        sequence,
    });
    host.command(Command::SetGrid {
        pane_id: PANE.into(),
        size: SIZE,
    });
    assert_eq!(
        host.engine.pane_terminal(PANE).unwrap().facts().grid,
        stream
    );
}

#[test]
fn tells_the_ui_about_the_terminal_before_anything_it_would_do_with_one() {
    let mut host = Host::connected();
    host.shown_pane(5, "kept");
    host.hide_and_remount();
    let mounted = host
        .log
        .iter()
        .position(|o| matches!(o, Out::Mounted { .. }));
    let revealed = host
        .log
        .iter()
        .position(|o| matches!(o, Out::Revealed { .. }));
    assert!(mounted.unwrap() < revealed.unwrap());
}

#[test]
fn wakes_the_ui_for_a_seed_and_for_what_follows_it() {
    let mut host = Host::connected();
    host.mount();
    host.seed(1, "x");
    assert_eq!(
        host.log
            .iter()
            .filter(|o| matches!(o, Out::Dirty { .. }))
            .count(),
        1
    );
}

#[test]
fn keeps_at_most_the_screen_budget_of_hidden_panes() {
    let mut host = Host::connected();
    for index in 0..=MAX_RETAINED_SCREENS as u64 {
        let pane = format!("%{index}");
        host.command(Command::Mount {
            pane_id: pane.clone(),
            size: SIZE,
        });
        let sequence = host.next_sequence();
        host.publish(TerminalEvent::Pane(PaneEvent::Seed {
            pane_id: pane.clone(),
            generation: 1,
            data: b"x".to_vec(),
            sequence,
        }));
        host.command(Command::Unmount { pane_id: pane });
    }
    assert_eq!(host.engine.retained_screen_count(), MAX_RETAINED_SCREENS);
}

#[test]
fn drops_a_page_the_grid_changed_under_at_the_engine_level() {
    let mut host = Host::connected();
    host.mount();
    host.seed(1, "screen");
    host.command(Command::SetGrid {
        pane_id: PANE.into(),
        size: GridSize {
            columns: 30,
            rows: 6,
        },
    });
    let sequence = host.next_sequence();
    host.publish(TerminalEvent::Pane(PaneEvent::History {
        pane_id: PANE.into(),
        data: b"older".to_vec(),
        history_size: Some(9_000),
        sequence,
    }));
    assert_eq!(
        host.engine
            .pane_terminal(PANE)
            .unwrap()
            .facts()
            .scrollback_rows,
        0
    );
    assert_eq!(host.incidents("pane.historySupersededByReseed").len(), 1);
}

#[test]
fn a_hide_applies_what_a_synchronized_update_was_holding() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    host.output(2, "\x1b[?2026hheld");
    assert_eq!(host.screen(), "x");
    host.hide_and_remount();
    assert_eq!(host.screen(), "xheld");
}

#[test]
fn charges_an_alternate_screen_pane_for_the_history_it_may_be_hiding() {
    let mut host = Host::connected();
    host.shown_pane(1, "x");
    host.output(2, "\x1b[?1049h");
    host.unmount();
    assert_eq!(
        host.engine.retained_bytes,
        (SCROLLBACK_LIMIT + SIZE.rows) * SIZE.columns * size_of::<Cell>()
    );
}

#[test]
fn hands_up_an_osc52_write_cut_across_outputs_and_ignores_a_read() {
    let mut host = Host::connected();
    host.shown_pane(1, "SHELL");
    // "copied" in base64, split across two outputs as tmux may cut it; then a
    // read request, which tmux answered already and nothing here may answer.
    host.output(2, "\x1b]52;c;Y29w");
    host.output(3, "aWVk\x07\x1b]52;c;?\x07");
    let writes: Vec<_> = host
        .log
        .iter()
        .filter_map(|output| match output {
            Out::ClipboardWrite { pane_id, text } => Some((pane_id.clone(), text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(writes, [(PANE.to_owned(), "copied".to_owned())]);
}

#[test]
fn hands_up_an_osc52_write_a_timed_out_synchronized_update_was_holding() {
    let mut host = Host::connected();
    host.shown_pane(1, "SHELL");
    // The update never ends: the timeout lands it, write and all.
    host.output(2, "\x1b[?2026h\x1b]52;c;Y29waWVk\x07");
    host.advance(Duration::from_secs(1));
    let writes: Vec<_> = host
        .log
        .iter()
        .filter_map(|output| match output {
            Out::ClipboardWrite { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(writes, ["copied"]);
}

#[test]
fn reveals_a_pane_whose_only_content_ever_arrives_as_output() {
    let mut host = Host::connected();
    host.mount();
    host.answer_last_visibility(Ok(()));
    // The host's photograph of a pane that has printed nothing yet.
    host.seed(1, "");
    host.output(2, "FIRST OUTPUT");
    assert_eq!(host.screen(), "FIRST OUTPUT");
    assert!(host.revealed());
}

#[test]
fn says_it_is_waiting_for_a_seed_and_shows_nothing_until_the_seed_arrives() {
    let mut host = Host::connected();
    host.mount();
    host.resource(|r| {
        r.state = PaneResourceState::Released;
        r.recovery_reason = "Renderer state was released".into();
    });
    assert!(!host.revealed(), "the deliberate blank is not content");
    assert_eq!(host.screen(), "");
    assert_eq!(
        host.renderer_diagnostic().as_deref(),
        Some("Renderer state was released; waiting for a fresh terminal seed…")
    );
    host.seed(500, "back");
    assert!(host.revealed());
    assert_eq!(
        host.renderer_diagnostic(),
        None,
        "the seed clears the banner"
    );
}

/// The old `restoredViewport`: the reader comes back to the rows they left,
/// and the kept screen is still the one being read, so neither output nor the
/// reveal's tail is written over it. Typing asks for one seed.
#[test]
fn a_kept_screen_comes_back_where_the_reader_left_it_and_nothing_writes_over_it() {
    let mut host = Host::connected();
    host.reading_pane();
    let read = host.screen();
    host.hide_and_remount();
    assert_eq!(host.display_offset(), 1);
    host.output(2, "deferred");
    host.resume_kept_screen("tail");
    assert_eq!(host.screen(), read);
    assert_eq!(host.display_offset(), 1);
    assert_eq!(host.reading_states(), [TerminalReadingState::Outdated]);
    assert_eq!(host.seed_requests(), 0);
    host.command(Command::ReturnToLive {
        pane_id: PANE.into(),
        reason: "typed".into(),
    });
    assert_eq!(host.seed_requests(), 1);
}

#[test]
fn keeps_one_seed_and_the_old_screen_when_going_live_outruns_the_resume() {
    let mut host = Host::connected();
    host.reading_pane();
    let read = host.screen();
    host.hide_and_remount();
    host.output(2, "deferred");
    host.command(Command::ReturnToLive {
        pane_id: PANE.into(),
        reason: "typed".into(),
    });
    host.resume_kept_screen("late-tail");
    assert_eq!(host.seed_requests(), 1);
    assert_eq!(host.screen(), read, "the old screen stays until the seed");
    host.seed(500, "fresh");
    assert_eq!(host.screen(), "fresh");
}

#[test]
fn output_held_for_the_reveal_is_not_written_once_the_reader_scrolls_away() {
    let mut host = Host::connected();
    host.shown_pane(1, "base");
    host.hide_and_remount();
    host.output(2, "queued");
    host.command(Command::Viewport {
        pane_id: PANE.into(),
        at_bottom: false,
    });
    host.resource(|r| {
        r.snapshot_generation = 1;
        r.tail_through_generation = 2;
        r.generation = 2;
    });
    assert_eq!(host.screen(), "base");
    assert_eq!(
        host.reading_states().last(),
        Some(&TerminalReadingState::Outdated)
    );
}

#[test]
fn drops_a_page_in_flight_when_output_makes_the_view_outdated() {
    let mut host = Host::connected();
    host.mount();
    host.seed(1, "screen");
    host.command(Command::Viewport {
        pane_id: PANE.into(),
        at_bottom: false,
    });
    host.output(2, "+live");
    let sequence = host.next_sequence();
    host.publish(TerminalEvent::Pane(PaneEvent::History {
        pane_id: PANE.into(),
        data: b"earlier output".to_vec(),
        history_size: Some(2_000),
        sequence,
    }));
    let terminal = host.engine.pane_terminal(PANE).unwrap();
    assert_eq!(terminal.facts().scrollback_rows, 0);
    let superseded = host.incidents("pane.historySupersededByReseed");
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0]["reason"], "screenGone");
}
