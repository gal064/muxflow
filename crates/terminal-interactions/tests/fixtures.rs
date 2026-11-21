//! Runs the fixtures that `packages/terminal-interactions` also runs, so the
//! Rust port and the TypeScript package the mobile app uses cannot drift apart.

use serde::Deserialize;
use serde_json::{Value, json};
use terminal_interactions::{
    Buffer, BufferLine, BufferRange, Cell, Position, TerminalSelectionSnapshot,
    capture_terminal_selection, clean_wrapped_command_selection, decode_topology_notification,
    is_explicit_terminal_file_path, resolve_terminal_file_path, terminal_file_link_cell_range,
    terminal_file_links, terminal_links_for_buffer_line, terminal_web_links,
};

macro_rules! fixture {
    ($name:literal) => {
        serde_json::from_str::<Value>(include_str!(concat!(
            "../../../packages/terminal-interactions/fixtures/",
            $name
        )))
        .expect("fixture is valid JSON")
    };
}

struct GridCell {
    chars: String,
    width: usize,
    attribute_default: bool,
}

struct GridLine {
    cells: Vec<GridCell>,
    wrapped: bool,
}

impl BufferLine for GridLine {
    fn is_wrapped(&self) -> bool {
        self.wrapped
    }
    fn columns(&self) -> usize {
        self.cells.len()
    }
    fn cell(&self, index: usize) -> Option<Cell<'_>> {
        self.cells.get(index).map(|cell| Cell {
            chars: &cell.chars,
            width: cell.width,
            attribute_default: cell.attribute_default,
        })
    }
}

struct Grid(Vec<GridLine>);

impl Buffer for Grid {
    fn line_count(&self) -> usize {
        self.0.len()
    }
    fn line(&self, row: usize) -> Option<&dyn BufferLine> {
        self.0.get(row).map(|line| line as &dyn BufferLine)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RowSpec {
    text: String,
    wrapped: bool,
    #[serde(default)]
    wide: Vec<usize>,
    #[serde(default)]
    styled: bool,
}

/// Rebuilds the cells xterm produced: one cell per character, a second empty
/// cell after each wide glyph, and never-written cells out to the last column.
fn grid(columns: usize, buffer_rows: usize, rows: &Value) -> Grid {
    let specs: Vec<RowSpec> = serde_json::from_value(rows.clone()).expect("rows");
    let mut lines: Vec<GridLine> = specs
        .iter()
        .map(|spec| {
            let mut cells = Vec::new();
            for (index, character) in spec.text.chars().enumerate() {
                let attribute_default = !(spec.styled && !character.is_whitespace());
                let width = if spec.wide.contains(&index) { 2 } else { 1 };
                cells.push(GridCell {
                    chars: character.to_string(),
                    width,
                    attribute_default,
                });
                if width == 2 {
                    cells.push(GridCell {
                        chars: String::new(),
                        width: 0,
                        attribute_default,
                    });
                }
            }
            assert!(
                cells.len() <= columns,
                "row wider than the terminal: {}",
                spec.text
            );
            while cells.len() < columns {
                cells.push(GridCell {
                    chars: String::new(),
                    width: 1,
                    attribute_default: true,
                });
            }
            GridLine {
                cells,
                wrapped: spec.wrapped,
            }
        })
        .collect();
    while lines.len() < buffer_rows {
        let cells = (0..columns)
            .map(|_| GridCell {
                chars: String::new(),
                width: 1,
                attribute_default: true,
            })
            .collect();
        lines.push(GridLine {
            cells,
            wrapped: false,
        });
    }
    Grid(lines)
}

fn cases(value: &Value) -> &Vec<Value> {
    value.as_array().expect("array of cases")
}

fn usize_field(value: &Value, key: &str) -> usize {
    value[key]
        .as_u64()
        .unwrap_or_else(|| panic!("{key} is a number")) as usize
}

/// Fixtures record UTF-16 offsets, as JavaScript reports them. The Rust API
/// reports byte offsets.
fn utf16_to_byte(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units == offset {
            return byte;
        }
        units += character.len_utf16();
    }
    // An offset past the end maps to the same distance past the end in bytes.
    text.len() + (offset - units)
}

fn expected_links(line: &str, links: &Value) -> Value {
    Value::Array(
        cases(links)
            .iter()
            .map(|link| {
                json!({
                    "text": link["text"],
                    "start": utf16_to_byte(line, usize_field(link, "start")),
                    "end": utf16_to_byte(line, usize_field(link, "end")),
                })
            })
            .collect(),
    )
}

#[test]
fn web_links() {
    for case in cases(&fixture!("web-links.json")) {
        let line = case["line"].as_str().unwrap();
        let actual = serde_json::to_value(terminal_web_links(line)).unwrap();
        assert_eq!(
            actual,
            expected_links(line, &case["links"]),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn file_links() {
    let fixtures = fixture!("file-links.json");
    for case in cases(&fixtures["links"]) {
        let line = case["line"].as_str().unwrap();
        let actual = serde_json::to_value(terminal_file_links(line)).unwrap();
        assert_eq!(
            actual,
            expected_links(line, &case["links"]),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn explicit_file_paths() {
    for case in cases(&fixture!("file-links.json")["explicit"]) {
        let value = case["value"].as_str().unwrap();
        assert_eq!(
            is_explicit_terminal_file_path(value),
            case["expected"].as_bool().unwrap(),
            "{value:?}"
        );
    }
}

#[test]
fn resolved_file_paths() {
    for case in cases(&fixture!("file-links.json")["resolve"]) {
        let (value, cwd) = (
            case["value"].as_str().unwrap(),
            case["cwd"].as_str().unwrap(),
        );
        let actual = resolve_terminal_file_path(value, cwd, case["home"].as_str());
        assert_eq!(
            actual.as_deref(),
            case["expected"].as_str(),
            "{value:?} from {cwd:?}"
        );
    }
}

#[test]
fn file_link_cell_ranges() {
    for case in cases(&fixture!("file-links.json")["cellRange"]) {
        let cells: Vec<GridCell> = cases(&case["cells"])
            .iter()
            .map(|cell| GridCell {
                chars: cell["chars"].as_str().unwrap().to_string(),
                width: usize_field(cell, "width"),
                attribute_default: true,
            })
            .collect();
        let text: String = cells
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| {
                if cell.chars.is_empty() {
                    " "
                } else {
                    cell.chars.as_str()
                }
            })
            .collect();
        let line = GridLine {
            cells,
            wrapped: false,
        };
        let start = utf16_to_byte(&text, usize_field(case, "start"));
        let end = utf16_to_byte(&text, usize_field(case, "end"));
        let expected = (!case["expected"].is_null()).then(|| {
            (
                usize_field(&case["expected"], "start"),
                usize_field(&case["expected"], "end"),
            )
        });
        assert_eq!(
            terminal_file_link_cell_range(&line, start, end),
            expected,
            "{}",
            case["name"]
        );
    }
}

#[test]
fn links_across_rendered_rows() {
    for case in cases(&fixture!("buffer-links.json")) {
        let columns = usize_field(case, "columns");
        let buffer = grid(columns, usize_field(case, "bufferRows"), &case["rows"]);
        for query in cases(&case["queries"]) {
            let row = usize_field(query, "row");
            let actual =
                serde_json::to_value(terminal_links_for_buffer_line(&buffer, columns, row))
                    .unwrap();
            assert_eq!(actual, query["links"], "{} row {row}", case["name"]);
        }
    }
}

fn position(value: &Value) -> Position {
    Position {
        x: usize_field(value, "x"),
        y: usize_field(value, "y"),
    }
}

#[test]
fn selection_capture() {
    for case in cases(&fixture!("selection-capture.json")) {
        let columns = usize_field(case, "columns");
        let buffer = grid(columns, usize_field(case, "bufferRows"), &case["rows"]);
        let selection = &case["selection"];
        let range = (!selection["start"].is_null()).then(|| BufferRange {
            start: position(&selection["start"]),
            end: position(&selection["end"]),
        });
        let text = selection["text"].as_str().unwrap();
        let snapshot = capture_terminal_selection(text, range, columns, &buffer);

        let mut expected = case["expected"].clone();
        expected["text"] = json!(text);
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap(),
            expected,
            "{}",
            case["name"]
        );
        assert_eq!(
            Value::String(clean_wrapped_command_selection(&snapshot)),
            case["cleaned"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn wrapped_command_cleanup() {
    for case in cases(&fixture!("selection-clean.json")) {
        let snapshot: TerminalSelectionSnapshot =
            serde_json::from_value(case["snapshot"].clone()).unwrap();
        assert_eq!(
            Value::String(clean_wrapped_command_selection(&snapshot)),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn topology_notifications() {
    for case in cases(&fixture!("topology.json")) {
        let label = case["label"].as_str().unwrap();
        let decoded = decode_topology_notification(label);
        if case["expected"].is_null() {
            assert!(decoded.is_err(), "{label:?} should be rejected");
            continue;
        }
        let decoded = decoded.unwrap_or_else(|error| panic!("{label:?}: {error}"));
        let mut actual = json!({ "name": decoded.name });
        if let Some(grids) = decoded.grids {
            actual["grids"] = grids
                .iter()
                .map(|grid| json!({ "paneId": grid.pane_id, "columns": grid.columns, "rows": grid.rows }))
                .collect();
        }
        assert_eq!(actual, case["expected"], "{label:?}");
    }
}
