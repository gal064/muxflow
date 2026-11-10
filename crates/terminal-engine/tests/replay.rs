//! Replays recorded `tmux -C` streams through the engine as host frames and
//! compares each pane with what tmux itself captured. See `fixtures/record.sh`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use terminal_engine::{Command, Engine, EngineOutput, GridSize, Listener, PaneTerminal};
use tmux_control::{ControlParser, ControlRecord, LayoutNode, parse_layout};

const START: GridSize = GridSize {
    columns: 80,
    rows: 24,
};

/// Everything tmux emitted, in order.
enum Record {
    Output { pane: String, data: Vec<u8> },
    Layout(Vec<(String, u16, u16)>),
}

fn leaves(node: &LayoutNode, out: &mut Vec<(String, u16, u16)>) {
    if let Some(index) = node.pane_index {
        out.push((format!("%{index}"), node.width, node.height));
    }
    for child in &node.children {
        leaves(child, out);
    }
}

fn records(path: &Path) -> Vec<Record> {
    let mut parser = ControlParser::new(16 * 1024 * 1024);
    parser.push(&fs::read(path).unwrap());
    parser.finish();
    let mut records = Vec::new();
    while let Some(record) = parser.next_record() {
        match record.unwrap() {
            ControlRecord::Output { pane_id, data } => records.push(Record::Output {
                pane: pane_id,
                data,
            }),
            ControlRecord::Notification { name, arguments } if name == "layout-change" => {
                let layout = arguments.split(' ').nth(1).unwrap();
                let mut panes = Vec::new();
                leaves(&parse_layout(layout).unwrap(), &mut panes);
                records.push(Record::Layout(panes));
            }
            _ => {}
        }
    }
    records
}

fn frame(kind: u8, label: &str, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(label.len() as u16).to_be_bytes());
    bytes.extend_from_slice(label.as_bytes());
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn generation_frame(kind: u8, pane: &str, sequence: u64, generation: u64, data: &[u8]) -> Vec<u8> {
    let mut payload = generation.to_be_bytes().to_vec();
    payload.extend_from_slice(data);
    frame(kind, pane, sequence, &payload)
}

fn row_text(term: &Term<Listener>, line: i32) -> String {
    let row = &term.grid()[Line(line)];
    let mut text = String::new();
    for column in 0..term.columns() {
        let cell = &row[Column(column)];
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        text.push(if cell.c == '\t' { ' ' } else { cell.c });
        text.extend(cell.zerowidth().into_iter().flatten());
    }
    text.trim_end().to_owned()
}

/// Rows with the blanks tmux and a terminal disagree about taken off the end.
fn trimmed(mut rows: Vec<String>) -> Vec<String> {
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

/// tmux and a terminal both keep a tab as a tab in the row, at different cells.
fn expand_tabs(line: &str) -> String {
    let mut expanded = String::new();
    for (column, character) in line.chars().scan(0, |column, character| {
        let at = *column;
        *column += if character == '\t' { 8 - at % 8 } else { 1 };
        Some((at, character))
    }) {
        if character == '\t' {
            expanded.push_str(&" ".repeat(8 - column % 8));
        } else {
            expanded.push(character);
        }
    }
    expanded
}

fn expected(path: &Path) -> Vec<String> {
    trimmed(
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| expand_tabs(line).trim_end().to_owned())
            .collect(),
    )
}

/// A fake host: frames in, every engine request answered.
struct Replay {
    engine: Engine,
    now: Instant,
    sequence: u64,
    generations: HashMap<String, u64>,
    terminals: HashMap<String, Arc<PaneTerminal>>,
    deferred_answers: Vec<u64>,
}

impl Replay {
    fn new() -> Self {
        let mut replay = Self {
            engine: Engine::default(),
            now: Instant::now(),
            sequence: 0,
            generations: HashMap::new(),
            terminals: HashMap::new(),
            deferred_answers: Vec::new(),
        };
        replay.command(Command::SetClient(Some("replay".into())));
        replay.command(Command::Frame(frame(
            10,
            "terminal",
            0,
            &1_u64.to_be_bytes(),
        )));
        replay
    }

    fn command(&mut self, command: Command) {
        let produced = self.engine.handle(command, self.now);
        for output in produced {
            match output {
                EngineOutput::Mounted { pane_id, terminal } => {
                    self.terminals.insert(pane_id, terminal);
                }
                EngineOutput::SetVisibility { token, .. } => self.deferred_answers.push(token),
                EngineOutput::RequestSeed {
                    pane_id, reason, ..
                } => {
                    panic!("{pane_id} asked for a seed: {reason}");
                }
                _ => {}
            }
        }
        for token in std::mem::take(&mut self.deferred_answers) {
            self.command(Command::VisibilityAnswered {
                token,
                result: Ok(()),
            });
        }
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    fn next_generation(&mut self, pane: &str) -> u64 {
        let generation = self.generations.entry(pane.to_owned()).or_default();
        *generation += 1;
        *generation
    }

    fn mount(&mut self, pane: &str, size: GridSize) {
        self.command(Command::Mount {
            pane_id: pane.to_owned(),
            size,
        });
    }

    fn seed_empty(&mut self, pane: &str) {
        let sequence = self.next_sequence();
        let generation = self.next_generation(pane);
        self.command(Command::Frame(generation_frame(
            1, pane, sequence, generation, b"",
        )));
    }

    fn output(&mut self, pane: &str, data: &[u8]) {
        let sequence = self.next_sequence();
        let generation = self.next_generation(pane);
        self.command(Command::Frame(generation_frame(
            2, pane, sequence, generation, data,
        )));
    }

    fn layout(&mut self, panes: &[(String, u16, u16)]) {
        let sequence = self.next_sequence();
        let label = format!("layout-change {}", serde_json::to_string(panes).unwrap());
        self.command(Command::Frame(frame(3, &label, sequence, &[])));
    }

    /// What a host sends a pane it has already been showing.
    fn visible_answer(&mut self, pane: &str) {
        let sequence = self.next_sequence();
        let generation = self.next_generation(pane);
        let mut resource = vec![1_u8, 0];
        resource.extend_from_slice(&generation.to_be_bytes());
        resource.extend_from_slice(&[0; 24]);
        self.command(Command::Frame(frame(9, pane, sequence, &resource)));
    }

    fn settle(&mut self) {
        self.now += Duration::from_secs(1);
        let produced = self.engine.poll(self.now);
        assert!(
            !produced
                .iter()
                .any(|o| matches!(o, EngineOutput::RequestSeed { .. }))
        );
    }

    fn screen(&self, pane: &str) -> Vec<String> {
        let term = self.terminals[pane].lock();
        trimmed(
            (0..term.screen_lines() as i32)
                .map(|line| row_text(&term, line))
                .collect(),
        )
    }

    fn history(&self, pane: &str) -> Vec<String> {
        let term = self.terminals[pane].lock();
        let oldest = -(term.grid().history_size() as i32);
        trimmed((oldest..0).map(|line| row_text(&term, line)).collect())
    }
}

struct Fixture {
    name: String,
    control: PathBuf,
    panes: BTreeMap<String, PathBuf>,
}

fn fixtures() -> Vec<Fixture> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension()? == "control")
                .then(|| path.file_stem().unwrap().to_str().unwrap().to_owned())
        })
        .collect();
    names.sort();
    assert!(names.len() >= 8, "fixtures are missing: {names:?}");
    names
        .into_iter()
        .map(|name| {
            let prefix = format!("{name}.pane-");
            let panes = fs::read_dir(&dir)
                .unwrap()
                .filter_map(|entry| {
                    let path = entry.unwrap().path();
                    let file = path.file_name().unwrap().to_str().unwrap().to_owned();
                    let id = file
                        .strip_prefix(&prefix)?
                        .strip_suffix(".screen")?
                        .to_owned();
                    Some((format!("%{id}"), path))
                })
                .collect();
            Fixture {
                control: dir.join(format!("{name}.control")),
                name,
                panes,
            }
        })
        .collect()
}

fn check(replay: &Replay, fixture: &Fixture, compare_history: bool, how: &str) {
    for (pane, screen) in &fixture.panes {
        assert_eq!(
            replay.screen(pane),
            expected(screen),
            "{} {pane} screen {how}",
            fixture.name
        );
        if compare_history {
            let history = screen.with_extension("history");
            assert_eq!(
                replay.history(pane),
                expected(&history),
                "{} {pane} history {how}",
                fixture.name
            );
        }
    }
}

fn start(fixture: &Fixture) -> Replay {
    let mut replay = Replay::new();
    for pane in fixture.panes.keys() {
        replay.mount(pane, START);
        replay.seed_empty(pane);
    }
    replay
}

/// A reflow is where tmux and a terminal legitimately disagree about which rows
/// are history, which is why the app re-captures on a resize. A terminal on the
/// alternate screen has no history to compare at all.
fn history_is_comparable(fixture: &Fixture) -> bool {
    !["resize", "split", "alt_open"].contains(&fixture.name.as_str())
}

#[test]
fn every_recording_reaches_the_screen_tmux_captured() {
    for fixture in fixtures() {
        let mut replay = start(&fixture);
        for record in records(&fixture.control) {
            match record {
                Record::Output { pane, data } => {
                    if fixture.panes.contains_key(&pane) {
                        replay.output(&pane, &data);
                    }
                }
                Record::Layout(panes) => replay.layout(&panes),
            }
        }
        replay.settle();
        check(
            &replay,
            &fixture,
            history_is_comparable(&fixture),
            "as recorded",
        );
    }
}

#[test]
fn the_screen_does_not_depend_on_how_the_stream_was_cut_into_frames() {
    for fixture in fixtures() {
        let mut replay = start(&fixture);
        for record in records(&fixture.control) {
            match record {
                Record::Output { pane, data } => {
                    if fixture.panes.contains_key(&pane) {
                        for byte in data {
                            replay.output(&pane, &[byte]);
                        }
                    }
                }
                Record::Layout(panes) => replay.layout(&panes),
            }
        }
        replay.settle();
        check(
            &replay,
            &fixture,
            history_is_comparable(&fixture),
            "one byte per frame",
        );
    }
}

#[test]
fn a_pane_hidden_midway_catches_up_from_its_backlog() {
    for fixture in fixtures() {
        let mut replay = start(&fixture);
        let records = records(&fixture.control);
        let hide_at = records.len() / 2;
        let hidden: Vec<String> = fixture.panes.keys().cloned().collect();
        let mut sizes: HashMap<String, GridSize> = HashMap::new();
        for (index, record) in records.into_iter().enumerate() {
            if index == hide_at {
                for pane in &hidden {
                    replay.command(Command::Unmount {
                        pane_id: pane.clone(),
                    });
                }
            }
            match record {
                Record::Output { pane, data } => {
                    if fixture.panes.contains_key(&pane) {
                        replay.output(&pane, &data);
                    }
                }
                Record::Layout(panes) => {
                    for (pane, columns, rows) in &panes {
                        let size = GridSize {
                            columns: usize::from(*columns),
                            rows: usize::from(*rows),
                        };
                        sizes.insert(pane.clone(), size);
                    }
                    replay.layout(&panes);
                }
            }
        }
        for pane in &hidden {
            // The UI mounts at the current layout; a hidden pane hears no grids.
            replay.mount(pane, sizes.get(pane).copied().unwrap_or(START));
            replay.visible_answer(pane);
        }
        replay.settle();
        check(&replay, &fixture, false, "after a hide");
    }
}
