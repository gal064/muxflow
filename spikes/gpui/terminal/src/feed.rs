//! The off-main-thread core: one `alacritty_terminal::Term` per pane, fed by
//! `%output` records parsed from a real `tmux -C` stream. No GPUI in here, so it
//! is unit-testable without a display.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use tmux_control::{ControlDispatcher, ControlParser, DispatchEvent, LayoutNode, parse_layout};

use crate::probe::Probe;

/// Tmux pane id, `%N`.
pub type PaneId = String;

/// One pane's character-grid size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for PaneSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// `Term` replies (cursor position reports and the like) are deliberately
/// dropped: `%output` carries the pane's raw bytes, tmux has already answered
/// those queries itself as the pane's terminal, and answering a second time
/// would inject a duplicate reply into the program's stdin.
#[derive(Clone, Default)]
pub struct Listener;

impl EventListener for Listener {
    fn send_event(&self, _event: Event) {}
}

/// Scrollback lines kept per pane (same as the app's xterm default).
pub const SCROLLBACK: usize = 10_000;

/// A pane's terminal state, shared between the feed thread and the UI.
pub struct Pane {
    pub id: PaneId,
    pub term: FairMutex<Term<Listener>>,
    /// Bumped after every `%output` record is applied, under the term lock.
    pub generation: AtomicU64,
    /// Set by the feed thread after applying output; cleared by the UI on notify.
    pub dirty: AtomicBool,
    pub bytes_fed: AtomicU64,
    pub records_fed: AtomicU64,
}

impl Pane {
    pub fn new(id: PaneId, size: PaneSize) -> Arc<Self> {
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        Arc::new(Self {
            id,
            term: FairMutex::new(Term::new(config, &size, Listener)),
            generation: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            bytes_fed: AtomicU64::new(0),
            records_fed: AtomicU64::new(0),
        })
    }

    pub fn size(&self) -> PaneSize {
        let term = self.term.lock();
        PaneSize {
            cols: term.columns(),
            rows: term.screen_lines(),
        }
    }

    pub(crate) fn apply(&self, parser: &mut Processor, data: &[u8], stats: &FeedStats) -> u64 {
        let started = Instant::now();
        let generation = {
            let mut term = self.term.lock();
            let locked = Instant::now();
            parser.advance(&mut *term, data);
            let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
            stats
                .lock_wait_us
                .fetch_add(locked.duration_since(started).as_micros() as u64, Ordering::Relaxed);
            stats
                .apply_us
                .fetch_add(locked.elapsed().as_micros() as u64, Ordering::Relaxed);
            generation
        };
        self.bytes_fed
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        self.records_fed.fetch_add(1, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Release);
        generation
    }

    fn resize(&self, size: PaneSize) {
        let mut term = self.term.lock();
        if term.columns() != size.cols || term.screen_lines() != size.rows {
            term.resize(size);
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        drop(term);
        self.dirty.store(true, Ordering::Release);
    }
}

/// One pane's rectangle in a tmux window, in cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRect {
    pub pane: PaneId,
    pub left: u16,
    pub top: u16,
    pub cols: u16,
    pub rows: u16,
}

/// Leaf panes of a parsed tmux layout, in layout order.
pub fn layout_panes(node: &LayoutNode, out: &mut Vec<PaneRect>) {
    if let Some(index) = node.pane_index {
        out.push(PaneRect {
            pane: format!("%{index}"),
            left: node.left,
            top: node.top,
            cols: node.width,
            rows: node.height,
        });
    }
    for child in &node.children {
        layout_panes(child, out);
    }
}

/// Where the feed thread's wall time goes. Tells a slow reader (waiting on the
/// term lock or the terminal parser) apart from a slow producer (waiting in read).
#[derive(Default)]
pub struct FeedStats {
    pub read_wait_us: AtomicU64,
    pub lock_wait_us: AtomicU64,
    pub apply_us: AtomicU64,
    pub chunks: AtomicU64,
    pub chunk_bytes: AtomicU64,
}

/// State the feed thread publishes for the UI. Everything a frame needs is
/// reachable through here; the UI never parses.
#[derive(Default)]
pub struct Session {
    pub panes: Mutex<HashMap<PaneId, Arc<Pane>>>,
    pub layouts: Mutex<HashMap<String, Vec<PaneRect>>>,
    pub active_window: Mutex<Option<String>>,
    pub active_pane: Mutex<HashMap<String, PaneId>>,
    /// Latest window-size notification seen, in cells (cols, rows).
    pub topology_dirty: AtomicBool,
    pub pauses: AtomicU64,
    pub resnapshots: AtomicU64,
    pub exited: AtomicBool,
    pub feed_stats: FeedStats,
}

impl Session {
    /// Fetch a pane, creating it with a default size if output arrives before its layout.
    pub fn pane(&self, id: &str) -> Arc<Pane> {
        let mut panes = self.panes.lock().unwrap();
        panes
            .entry(id.to_string())
            .or_insert_with(|| {
                Pane::new(
                    id.to_string(),
                    PaneSize {
                        cols: 80,
                        rows: 24,
                    },
                )
            })
            .clone()
    }

    pub fn panes_in_window(&self, window: &str) -> Vec<PaneRect> {
        self.layouts
            .lock()
            .unwrap()
            .get(window)
            .cloned()
            .unwrap_or_default()
    }

    pub fn apply_layout(&self, window: &str, rects: Vec<PaneRect>) {
        for rect in &rects {
            self.pane(&rect.pane).resize(PaneSize {
                cols: rect.cols as usize,
                rows: rect.rows as usize,
            });
        }
        self.layouts
            .lock()
            .unwrap()
            .insert(window.to_string(), rects);
        self.topology_dirty.store(true, Ordering::Release);
    }
}

/// The parse-and-apply step, separated from the reader thread so tests can
/// drive it with byte fixtures.
pub struct Feed {
    parser: ControlParser,
    dispatcher: ControlDispatcher,
    processors: HashMap<PaneId, Processor>,
    pub session: Arc<Session>,
    probe: Arc<Probe>,
}

/// What the reader thread must do after a chunk: send tmux commands or wake UI.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FeedEffects {
    pub resume_panes: Vec<PaneId>,
    pub woke_ui: bool,
}

impl Feed {
    pub fn new(session: Arc<Session>, probe: Arc<Probe>) -> Self {
        Self {
            parser: ControlParser::default(),
            dispatcher: ControlDispatcher::default(),
            processors: HashMap::new(),
            session,
            probe,
        }
    }

    /// Feed one chunk of raw control-mode bytes.
    pub fn push(&mut self, bytes: &[u8]) -> FeedEffects {
        let mut effects = FeedEffects::default();
        self.parser.push(bytes);
        while let Some(record) = self.parser.next_record() {
            for event in self.dispatcher.handle(record) {
                self.handle(event, &mut effects);
            }
        }
        effects
    }

    fn handle(&mut self, event: DispatchEvent, effects: &mut FeedEffects) {
        match event {
            DispatchEvent::Output { pane_id, data } => {
                // Probe timestamps sit before the term write, matching where
                // `echoLagProbe.noteOutput` sits relative to `xterm.write`.
                self.probe.on_output(&pane_id, &data);
                let pane = self.session.pane(&pane_id);
                let parser = self.processors.entry(pane_id.clone()).or_default();
                let generation = pane.apply(parser, &data, &self.session.feed_stats);
                self.probe.after_feed(&pane_id, generation);
                effects.woke_ui = true;
            }
            DispatchEvent::Notification { name, arguments } => {
                self.notification(&name, &arguments, effects)
            }
            DispatchEvent::Exit { .. } => {
                self.session.exited.store(true, Ordering::Release);
                effects.woke_ui = true;
            }
            DispatchEvent::ResnapshotRequired { reason } => {
                eprintln!("feed: resnapshot required: {reason}");
                self.session.resnapshots.fetch_add(1, Ordering::Relaxed);
            }
            DispatchEvent::CommandCompleted { .. } => {}
        }
    }

    fn notification(&mut self, name: &str, arguments: &str, effects: &mut FeedEffects) {
        let mut fields = arguments.split(' ');
        match name {
            "layout-change" => {
                let (Some(window), Some(layout)) = (fields.next(), fields.next()) else {
                    return;
                };
                let Ok(tree) = parse_layout(layout) else {
                    eprintln!("feed: unparsable layout {layout}");
                    return;
                };
                let mut rects = Vec::new();
                layout_panes(&tree, &mut rects);
                self.session.apply_layout(window, rects);
                effects.woke_ui = true;
            }
            "session-window-changed" => {
                if let Some(window) = fields.nth(1) {
                    *self.session.active_window.lock().unwrap() = Some(window.to_string());
                    self.session
                        .topology_dirty
                        .store(true, Ordering::Release);
                    effects.woke_ui = true;
                }
            }
            "window-pane-changed" => {
                if let (Some(window), Some(pane)) = (fields.next(), fields.next()) {
                    self.session
                        .active_pane
                        .lock()
                        .unwrap()
                        .insert(window.to_string(), pane.to_string());
                    self.session
                        .topology_dirty
                        .store(true, Ordering::Release);
                    effects.woke_ui = true;
                }
            }
            "pause" => {
                if let Some(pane) = fields.next() {
                    self.session.pauses.fetch_add(1, Ordering::Relaxed);
                    self.probe.on_pause(pane);
                    effects.resume_panes.push(pane.to_string());
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::TermDamage;

    fn feed() -> (Feed, Arc<Session>) {
        let session = Arc::new(Session::default());
        (Feed::new(session.clone(), Arc::new(Probe::default())), session)
    }

    fn cell_text(pane: &Pane, line: i32, cols: usize) -> String {
        let term = pane.term.lock();
        (0..cols)
            .map(|c| term.grid()[Line(line)][Column(c)].c)
            .collect()
    }

    #[test]
    fn output_records_reach_one_term_per_pane_with_octal_decoding() {
        let (mut feed, session) = feed();
        feed.push(b"%output %1 hi\\033[1mB\n%output %2 other\n");
        let one = session.pane("%1");
        let two = session.pane("%2");
        assert_eq!(cell_text(&one, 0, 3), "hiB");
        assert_eq!(cell_text(&two, 0, 5), "other");
        assert_eq!(one.generation.load(Ordering::Acquire), 1);
        assert!(two.dirty.load(Ordering::Acquire));
    }

    #[test]
    fn partial_records_across_chunks_do_not_apply_early() {
        let (mut feed, session) = feed();
        feed.push(b"%output %1 ab");
        assert_eq!(session.pane("%1").generation.load(Ordering::Acquire), 0);
        feed.push(b"c\\033[32md\n");
        assert_eq!(session.pane("%1").generation.load(Ordering::Acquire), 1);
        assert_eq!(cell_text(&session.pane("%1"), 0, 4), "abcd");
    }

    #[test]
    fn layout_change_resizes_the_panes_it_names() {
        let (mut feed, session) = feed();
        feed.push(b"%layout-change @1 b25d,100x30,0,0,1 b25d,100x30,0,0,1 *\n");
        let pane = session.pane("%1");
        assert_eq!(pane.size(), PaneSize { cols: 100, rows: 30 });
        let rects = session.panes_in_window("@1");
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].cols, 100);
    }

    #[test]
    fn session_window_and_pane_notifications_update_topology() {
        let (mut feed, session) = feed();
        feed.push(b"%session-window-changed $0 @5\n%window-pane-changed @5 %9\n");
        assert_eq!(
            session.active_window.lock().unwrap().as_deref(),
            Some("@5")
        );
        assert_eq!(
            session.active_pane.lock().unwrap().get("@5").cloned(),
            Some("%9".to_string())
        );
    }

    #[test]
    fn pause_records_ask_for_a_resume_of_that_pane() {
        let (mut feed, session) = feed();
        let effects = feed.push(b"%pause %3\n");
        assert_eq!(effects.resume_panes, vec!["%3".to_string()]);
        assert_eq!(session.pauses.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn partial_output_damages_only_touched_rows() {
        let (mut feed, session) = feed();
        let pane = session.pane("%1");
        pane.term.lock().reset_damage(); // A new Term starts fully damaged.
        feed.push(b"%output %1 \\033[1;1Hfirst\\033[3;1Hthird\n");
        {
            let mut term = pane.term.lock();
            let lines: Vec<usize> = match term.damage() {
                TermDamage::Full => panic!("cursor moves are not full damage"),
                TermDamage::Partial(iter) => iter.map(|bounds| bounds.line).collect(),
            };
            assert!(lines.contains(&0));
            assert!(lines.contains(&2));
            term.reset_damage();
        }
        feed.push(b"%output %1 \\033[5;1Hfifth\n");
        let mut term = pane.term.lock();
        let lines: Vec<usize> = match term.damage() {
            TermDamage::Full => panic!("single line write is not full damage"),
            TermDamage::Partial(iter) => iter.map(|bounds| bounds.line).collect(),
        };
        assert!(lines.contains(&4));
        assert!(!lines.contains(&0));
    }

    #[test]
    fn scroll_is_full_damage_so_row_cache_cannot_help_in_a_flood() {
        let (mut feed, session) = feed();
        // A new Term is fully damaged; clear that so only the scroll can set it.
        session.pane("%1").term.lock().reset_damage();
        // Fill past the bottom of a 24-row screen to force a scroll.
        let mut input = Vec::new();
        for i in 0..30 {
            input.extend_from_slice(format!("%output %1 line{i}\\015\\012\n").as_bytes());
        }
        feed.push(&input);
        let pane = session.pane("%1");
        let mut term = pane.term.lock();
        assert!(matches!(term.damage(), TermDamage::Full));
        term.reset_damage();
        assert_eq!(
            term.grid()[Point::new(Line(0), Column(0))].c,
            'l',
            "scrolled grid still has text"
        );
    }

    #[test]
    fn extended_output_feeds_like_output() {
        let (mut feed, session) = feed();
        feed.push(b"%extended-output %2 12 : hello\n");
        assert_eq!(cell_text(&session.pane("%2"), 0, 5), "hello");
    }
}
