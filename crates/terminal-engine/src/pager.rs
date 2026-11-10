//! One pane's walk up its own scrollback. Port of `PaneHistoryPager.ts`.
//!
//! The splice is synchronous here: the terminal is rewritten in stream order on
//! the engine thread, so there is no barrier for a splice to wait on, and the
//! "held across the splice" rule has nothing to hold across.

use std::collections::VecDeque;

use serde_json::{Value, json};

use crate::terminal::{HistoryPageAnchor, HistorySpliceOutcome, TerminalFacts};

/// The first page: several screens' worth, inside one frame even on a slow link.
pub const HISTORY_PAGE_LINES: u32 = 300;

/// Pages double up to this. A full 10,000-row scrollback is six pages, and the
/// largest single answer stays near a tenth of a whole-history capture.
pub const HISTORY_MAX_PAGE_LINES: u32 = 4_800;

/// The host's own `MAX_HISTORY_SKIP_LINES`. Reaching it is the end of what this
/// protocol can fetch: past it the host would answer with the same clamped rows.
pub const HISTORY_MAX_SKIP_LINES: usize = 10_000;

/// Answers carry no request identity, so a request the host never answers would
/// misattribute every later answer. The bound makes that failure finite.
pub const HISTORY_MAX_AWAITING: usize = 8;

fn grown_page(lines: u32) -> u32 {
    (lines * 2).min(HISTORY_MAX_PAGE_LINES)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryPageTrigger {
    Prefetch,
    ScrolledToTop,
}

impl HistoryPageTrigger {
    fn name(self) -> &'static str {
        match self {
            Self::Prefetch => "prefetch",
            Self::ScrolledToTop => "scrolledToTop",
        }
    }
}

/// What a retained screen carries about how far up its history it has been
/// fetched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CachedHistoryState {
    pub screen_seeded: bool,
    pub history_exhausted: bool,
    pub history_next_page_lines: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRequest {
    pub serial: u64,
    pub lines: u32,
    pub skip: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrphanReason {
    Reseed,
    ScreenGone,
    GridChanged,
}

impl OrphanReason {
    fn name(self) -> &'static str {
        match self {
            Self::Reseed => "reseed",
            Self::ScreenGone => "screenGone",
            Self::GridChanged => "gridChanged",
        }
    }
}

struct HistoryRequest {
    serial: u64,
    anchor: HistoryPageAnchor,
    lines: u32,
    /// Cleared when the screen this page was asked against is replaced.
    orphaned_by: Option<OrphanReason>,
    trigger: HistoryPageTrigger,
}

type Journal<'a> = &'a mut dyn FnMut(&'static str, Value);

/// `journal` is passed per call rather than stored, so the pager holds no
/// reference to its owner.
pub struct PaneHistoryPager {
    pane_id: String,
    screen_seeded: bool,
    exhausted: bool,
    next_page_lines: u32,
    serial: u64,
    awaiting: VecDeque<HistoryRequest>,
    busy: bool,
}

impl PaneHistoryPager {
    pub fn new(pane_id: &str) -> Self {
        Self {
            pane_id: pane_id.to_owned(),
            screen_seeded: false,
            exhausted: false,
            next_page_lines: HISTORY_PAGE_LINES,
            serial: 0,
            awaiting: VecDeque::new(),
            busy: false,
        }
    }

    pub fn restore(&mut self, state: CachedHistoryState) {
        self.screen_seeded = state.screen_seeded;
        self.exhausted = state.history_exhausted;
        self.next_page_lines = if state.history_next_page_lines == 0 {
            HISTORY_PAGE_LINES
        } else {
            state.history_next_page_lines
        };
    }

    pub fn snapshot(&self) -> CachedHistoryState {
        CachedHistoryState {
            screen_seeded: self.screen_seeded,
            history_exhausted: self.exhausted,
            history_next_page_lines: self.next_page_lines,
        }
    }

    /// A seed replaced the screen: its scrollback is something to fetch again,
    /// from the smallest page, and any page in flight is orphaned.
    pub fn note_screen_seeded(&mut self) {
        self.screen_seeded = true;
        self.exhausted = false;
        self.next_page_lines = HISTORY_PAGE_LINES;
        self.orphan(OrphanReason::Reseed);
    }

    /// The screen went away and what replaces it is not a screen-only seed.
    pub fn note_screen_gone(&mut self) {
        self.screen_seeded = false;
        self.orphan(OrphanReason::ScreenGone);
    }

    /// A reflow moves rows across tmux's history boundary, so the skip a page in
    /// flight quoted no longer names where this buffer begins.
    pub fn note_grid_changed(&mut self) {
        self.orphan(OrphanReason::GridChanged);
    }

    fn orphan(&mut self, reason: OrphanReason) {
        for request in &mut self.awaiting {
            request.orphaned_by = Some(reason);
        }
        self.busy = false;
    }

    /// Asks for the page immediately above what this pane holds. One at a time:
    /// a wheel-up during the prefetch is the same question and costs nothing.
    pub fn request_page(
        &mut self,
        trigger: HistoryPageTrigger,
        facts: TerminalFacts,
        journal: Journal<'_>,
    ) -> Option<PageRequest> {
        if !self.screen_seeded || self.exhausted || self.busy || facts.alternate_screen {
            return None;
        }
        let skip = facts.scrollback_rows;
        // The end of what this protocol can reach. A pane whose history-limit is
        // larger than either ceiling would re-fetch the same clamped rows forever.
        if skip >= HISTORY_MAX_SKIP_LINES || skip >= facts.scrollback_limit {
            self.exhausted = true;
            journal(
                "pane.historyCapped",
                json!({
                    "paneId": self.pane_id,
                    "trigger": trigger.name(),
                    "skip": skip,
                    "rendererLimit": facts.scrollback_limit,
                }),
            );
            return None;
        }
        let lines = self.next_page_lines;
        self.serial += 1;
        let serial = self.serial;
        self.busy = true;
        self.awaiting.push_back(HistoryRequest {
            serial,
            anchor: HistoryPageAnchor {
                skip,
                columns: facts.grid.columns,
                rows: facts.grid.rows,
            },
            lines,
            orphaned_by: None,
            trigger,
        });
        // Dropped from the front: the head is the expectation a lost answer
        // stranded, and everything behind it is legitimate.
        while self.awaiting.len() > HISTORY_MAX_AWAITING {
            let dropped = self
                .awaiting
                .pop_front()
                .map_or(0, |request| request.serial);
            journal(
                "pane.historyExpectationDropped",
                json!({ "paneId": self.pane_id, "serial": dropped }),
            );
        }
        Some(PageRequest {
            serial,
            lines,
            skip,
        })
    }

    /// The request never went out, so no answer will come for it. It leaves the
    /// queue rather than shifting every later answer by one.
    pub fn request_failed(&mut self, serial: u64, error: &str, journal: Journal<'_>) {
        let Some(request) = self
            .awaiting
            .iter()
            .find(|request| request.serial == serial)
        else {
            return;
        };
        let was_current = request.orphaned_by.is_none();
        let trigger = request.trigger.name();
        self.awaiting.retain(|request| request.serial != serial);
        if was_current {
            self.busy = false;
        }
        journal(
            "pane.historyRequestFailed",
            json!({
                "paneId": self.pane_id,
                "trigger": trigger,
                "error": error.chars().take(200).collect::<String>(),
            }),
        );
    }

    /// One page, answering the oldest outstanding request: one page is
    /// outstanding at a time and answers come back in order.
    pub fn receive(
        &mut self,
        data: &[u8],
        history_size: Option<u32>,
        splice: impl FnOnce(&[u8], HistoryPageAnchor) -> HistorySpliceOutcome,
        journal: Journal<'_>,
    ) {
        let Some(request) = self.awaiting.pop_front() else {
            journal("pane.historyUnrequested", json!({ "paneId": self.pane_id }));
            return;
        };
        if request.orphaned_by.is_some() || !self.screen_seeded {
            journal(
                "pane.historySupersededByReseed",
                json!({
                    "paneId": self.pane_id,
                    "serial": request.serial,
                    "reason": request.orphaned_by.unwrap_or(OrphanReason::ScreenGone).name(),
                }),
            );
            return;
        }
        // Never inferred from the answer's own rows: tmux clamps a range past
        // the top and answers one entirely above it with a single row. An absent
        // size leaves the question open, so the next reach-the-top asks again.
        let last_page = history_size.is_some_and(|size| {
            request.anchor.skip as u64 + u64::from(request.lines) >= u64::from(size)
        });
        // tmux answers a range entirely above its history with its top screen row,
        // which is not a page.
        let above_the_top =
            history_size.is_some_and(|size| request.anchor.skip as u64 >= u64::from(size));
        if data.is_empty() || above_the_top {
            self.busy = false;
            self.exhausted = last_page;
            return;
        }
        match splice(data, request.anchor) {
            HistorySpliceOutcome::Applied => {
                self.exhausted = last_page;
                self.next_page_lines = grown_page(request.lines);
            }
            // Every row had already been printed while the page crossed the
            // link. The same ask would get the same answer, so the page grows.
            HistorySpliceOutcome::OverlapExceedsPage => {
                self.next_page_lines = grown_page(request.lines);
            }
            HistorySpliceOutcome::Superseded => {}
        }
        self.busy = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::GridSize;

    #[derive(Clone, Copy)]
    struct FakeRenderer {
        facts: TerminalFacts,
        outcome: HistorySpliceOutcome,
    }

    impl Default for FakeRenderer {
        fn default() -> Self {
            Self {
                facts: TerminalFacts {
                    scrollback_rows: 0,
                    scrollback_limit: 10_000,
                    grid: GridSize {
                        columns: 80,
                        rows: 24,
                    },
                    alternate_screen: false,
                },
                outcome: HistorySpliceOutcome::Applied,
            }
        }
    }

    #[derive(Default)]
    struct Log(Vec<(&'static str, Value)>);

    impl Log {
        fn sink(&mut self) -> impl FnMut(&'static str, Value) + '_ {
            |kind, detail| self.0.push((kind, detail))
        }
    }

    fn page_text(rows: usize) -> Vec<u8> {
        (0..rows)
            .map(|index| format!("row-{index}"))
            .collect::<Vec<_>>()
            .join("\r\n")
            .into_bytes()
    }

    struct Harness {
        renderer: FakeRenderer,
        pager: PaneHistoryPager,
        requests: Vec<(u32, usize)>,
        splices: Vec<(usize, HistoryPageAnchor)>,
        log: Log,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                renderer: FakeRenderer::default(),
                pager: PaneHistoryPager::new("%1"),
                requests: Vec::new(),
                splices: Vec::new(),
                log: Log::default(),
            }
        }

        fn ask(&mut self, trigger: HistoryPageTrigger) -> Option<PageRequest> {
            let request =
                self.pager
                    .request_page(trigger, self.renderer.facts, &mut self.log.sink());
            if let Some(request) = request {
                self.requests.push((request.lines, request.skip));
            }
            request
        }

        fn deliver(&mut self, rows_added: usize, history_size: Option<u32>) {
            let outcome = self.renderer.outcome;
            let splices = &mut self.splices;
            self.pager.receive(
                &page_text(10),
                history_size,
                |data, anchor| {
                    splices.push((data.len(), anchor));
                    outcome
                },
                &mut self.log.sink(),
            );
            self.renderer.facts.scrollback_rows += rows_added;
        }
    }

    use HistoryPageTrigger::{Prefetch, ScrolledToTop};

    #[test]
    fn stops_at_what_it_can_hold_when_tmux_holds_more_history_than_this_side_ever_can() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.ask(Prefetch);
        assert_eq!(h.requests.len(), 1);

        h.deliver(10_000, Some(50_000));

        h.ask(ScrolledToTop);
        h.ask(ScrolledToTop);
        assert_eq!(h.requests.len(), 1);

        h.renderer.facts.scrollback_limit = 50_000;
        h.ask(ScrolledToTop);
        assert_eq!(h.requests.len(), 1);
        assert!(h.pager.snapshot().history_exhausted);
    }

    #[test]
    fn doubles_the_page_as_the_buffer_grows_capping_it_so_a_full_scrollback_is_six_rewrites() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.ask(Prefetch);
        for rows in [300, 600, 1_200, 2_400, 4_800] {
            h.deliver(rows, Some(50_000));
            h.ask(ScrolledToTop);
        }
        assert_eq!(
            h.requests,
            [
                (300, 0),
                (600, 300),
                (1_200, 900),
                (2_400, 2_100),
                (4_800, 4_500),
                (4_800, 9_300)
            ]
        );
    }

    #[test]
    fn reopens_its_latch_when_the_splice_is_refused_so_reaching_the_top_asks_again() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.ask(Prefetch);
        h.renderer.outcome = HistorySpliceOutcome::Superseded;
        h.deliver(0, Some(50_000));
        assert!(!h.pager.snapshot().history_exhausted);
        assert_eq!(
            h.pager.snapshot().history_next_page_lines,
            HISTORY_PAGE_LINES
        );
        h.ask(ScrolledToTop);
        assert_eq!(h.requests.len(), 2);
    }

    #[test]
    fn grows_the_page_when_every_row_of_it_had_already_been_printed_so_the_ask_outruns_the_pane() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.ask(Prefetch);
        h.renderer.outcome = HistorySpliceOutcome::OverlapExceedsPage;
        h.deliver(0, Some(50_000));
        assert!(!h.pager.snapshot().history_exhausted);
        assert_eq!(
            h.pager.snapshot().history_next_page_lines,
            HISTORY_PAGE_LINES * 2
        );
        h.ask(ScrolledToTop);
        assert_eq!(h.requests[1].0, HISTORY_PAGE_LINES * 2);
    }

    #[test]
    fn releases_its_latch_at_once_for_a_reseed() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.ask(Prefetch);
        h.pager.note_screen_seeded();
        h.ask(Prefetch);
        assert_eq!(h.requests.len(), 2);
    }

    #[test]
    fn drops_its_oldest_expectation_rather_than_misattributing_answers_forever() {
        let mut h = Harness::new();
        for _ in 0..=HISTORY_MAX_AWAITING {
            h.pager.note_screen_seeded();
            h.ask(Prefetch);
        }
        assert_eq!(
            h.log.0,
            [(
                "pane.historyExpectationDropped",
                json!({ "paneId": "%1", "serial": 1 })
            )]
        );
        h.deliver(0, Some(2_000));
        assert!(h.splices.is_empty());
        assert_eq!(
            h.log.0.last().unwrap(),
            &(
                "pane.historySupersededByReseed",
                json!({ "paneId": "%1", "serial": 2, "reason": "reseed" })
            )
        );
    }

    #[test]
    fn drops_a_page_the_terminal_reflowed_under_and_asks_again() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.renderer.facts.scrollback_rows = 40;
        h.ask(ScrolledToTop);
        assert_eq!(h.requests, [(HISTORY_PAGE_LINES, 40)]);

        h.pager.note_grid_changed();
        h.renderer.facts.scrollback_rows = 33;

        h.deliver(0, Some(2_000));
        assert!(
            h.splices.is_empty(),
            "a page from before the reflow was spliced"
        );
        assert_eq!(
            h.log.0.last().unwrap(),
            &(
                "pane.historySupersededByReseed",
                json!({ "paneId": "%1", "serial": 1, "reason": "gridChanged" })
            )
        );
        h.ask(ScrolledToTop);
        assert_eq!(h.requests.last().unwrap(), &(HISTORY_PAGE_LINES, 33));
    }

    #[test]
    fn hands_the_splice_the_skip_the_page_was_asked_with_not_the_one_it_holds_now() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.renderer.facts.scrollback_rows = 12;
        h.ask(Prefetch);
        h.renderer.facts.scrollback_rows = 19;
        h.deliver(0, Some(2_000));
        assert_eq!(
            h.splices,
            [(
                page_text(10).len(),
                HistoryPageAnchor {
                    skip: 12,
                    columns: 80,
                    rows: 24
                }
            )]
        );
    }

    #[test]
    fn asks_for_nothing_on_the_alternate_screen() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.renderer.facts.alternate_screen = true;
        assert_eq!(h.ask(ScrolledToTop), None);
    }

    #[test]
    fn asks_for_nothing_until_a_seeded_screen_exists() {
        let mut h = Harness::new();
        assert_eq!(h.ask(Prefetch), None);
    }

    #[test]
    fn a_failed_request_leaves_the_queue_and_reopens_the_latch() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        let request = h.ask(Prefetch).unwrap();
        h.pager
            .request_failed(request.serial, "no route", &mut h.log.sink());
        assert_eq!(h.ask(ScrolledToTop).unwrap().serial, request.serial + 1);
        h.deliver(0, Some(2_000));
        assert_eq!(
            h.splices.len(),
            1,
            "the next answer still matches its own request"
        );
    }

    #[test]
    fn an_empty_answer_still_decides_whether_to_ask_again() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.ask(Prefetch);
        let splices = &mut h.splices;
        h.pager.receive(
            b"",
            Some(100),
            |data, anchor| {
                splices.push((data.len(), anchor));
                HistorySpliceOutcome::Applied
            },
            &mut h.log.sink(),
        );
        assert!(h.splices.is_empty());
        assert!(h.pager.snapshot().history_exhausted);
    }

    #[test]
    fn does_not_splice_the_screen_row_tmux_answers_with_when_it_has_no_history() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.ask(Prefetch);
        h.deliver(0, Some(0));
        assert!(h.splices.is_empty());
        assert!(h.pager.snapshot().history_exhausted);
    }

    #[test]
    fn journals_an_answer_nobody_asked_for() {
        let mut h = Harness::new();
        h.deliver(0, Some(10));
        assert_eq!(h.log.0.last().unwrap().0, "pane.historyUnrequested");
    }

    #[test]
    fn carries_its_paging_state_across_a_hide() {
        let mut h = Harness::new();
        h.pager.note_screen_seeded();
        h.ask(Prefetch);
        h.deliver(300, Some(50_000));
        let carried = h.pager.snapshot();
        let mut restored = PaneHistoryPager::new("%1");
        restored.restore(carried);
        assert_eq!(restored.snapshot(), carried);
        assert_eq!(carried.history_next_page_lines, 600);
    }

    #[test]
    fn reading_zero_as_next_page_means_the_first_size() {
        let mut pager = PaneHistoryPager::new("%1");
        pager.restore(CachedHistoryState {
            screen_seeded: true,
            history_exhausted: false,
            history_next_page_lines: 0,
        });
        assert_eq!(pager.snapshot().history_next_page_lines, HISTORY_PAGE_LINES);
    }
}
