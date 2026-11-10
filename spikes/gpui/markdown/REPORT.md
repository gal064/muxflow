# T0.5 `spike-markdown`: can Live Markdown be built on GPUI Kit?

Answer to migration questions S4 (Live Markdown) and the editor half of S5
(IME composition in the editor). Prototype lives in this directory;
`cargo run -p spike-markdown -- markdown/fixtures/sample.md` runs it.

## Verdict

**Yes for the core feature, on `gpui-base`'s `editor_host`, as custom editor
code. No Kit change was found to be required. Not on the Kit code editor.**
Approach B below renders the real fixture with hidden syntax and an editable
table. Two production risks are open and were not prototyped: virtualization
and incremental parsing for large files (today's editor is tuned for 2 MB files,
this spike reparses and re-renders everything per frame), and accessibility for
a custom text surface.

| # | Capability | Verdict |
|---|---|---|
| 1 | Live inline syntax: `**bold**`, `*italic*`, `` `code` ``, links, headings styled with markers hidden; markers reappear when the caret enters | **Works** |
| 2 | Ordered and bullet lists | **Works**; hanging indent and list commands are named custom work below |
| 3 | Editable inline table: grid, cell edit, written back to source | **Works for tables that start their own line** (as today; a table inside a quote or a list item stays as source). Full `liveTable` parity is named custom work |
| 4 | Source stays the single truth; save is correct; undo across rendered and raw | **Works** |
| 5 | Caret movement and selection across hidden ranges | **Works** for Left/Right, Up/Down, Home/End in a cell, click, Shift-selection and drag. Limits: a selection that starts in a cell stays in that cell, and one that starts outside takes the table as one block (no cross-cell selection). Word motion and visual-line Home/End are named custom work |
| 6 | IME preedit renders at the caret (editor half of S5) | **Works through the input-handler path**; real compositor IME is out of scope here and stays unproven |

Evidence is in "What runs" and "QA" below: unit tests of the logic, and the
running GPUI app driven by real X11 key and pointer input on a private Xvfb.
Capability 6 goes through the view's input handler (F8), not a compositor.

## Approaches compared

| | A. Kit code editor (`EditorState` + `TextDecoration`) | B. `gpui-base` `text::editor_host` (custom editor over `BlockNode` + geometry) | C. Custom `Element` over a rope (no Kit text layout) |
|---|---|---|---|
| Hide a marker's width | No. `TextDecoration` is a `HighlightStyle` only: colour and face. See `src/kit_probe.rs` and frame `qa-frames/kit-probe.png`: markers repainted in background colour leave gaps | Yes: display text is a projection of the source | Yes |
| Hide/replace inline | No inline API. The display map's folds are whole wrap rows (`fold_map.rs`), so no inline span can be folded | Yes | Yes |
| Tables inline | No block or replacement decoration, so the grid cannot sit in the text | Yes: `BlockNode::Table` with keyed cell paragraphs | Possible; needs its own table layout |
| Heading sizes | No (no font size in `HighlightStyle`) | Yes: `BlockNode::Heading` | Yes, hand-built |
| Styling, layout, wrapping | Editor's own | Kit's Markdown renderer (same as the preview) | Hand-built |
| Caret, selection, IME, undo | Provided | Host's own (`live.rs`, `doc.rs` and the input handler in `view.rs`, about 1,200 lines) | Host's own, same |
| Verdict | Rejected | **Recommended** | Not needed; would add a layout layer B already gets free |

Approach A was tried first as the cheapest credible one (the probe is one
file, `--probe-kit`). It stops at capability 1 and cannot do 3 at all. Approach
B is what the task's `editor-host` hint pointed at: Kit renders the projection
and hands back per-leaf layout (`LeafGeometry`) for hit-testing, caret placement
and selection. Approach C was not built because B already works; its only
additional leverage is removing Kit's `Paragraph` layer, and B's limits (below)
are not layout limits. Inline tokens (`InputMode`/`TextareaMode` only, not
`EditorMode`) were checked as another way to hide markers and rejected: they are
atomic pills for chat-style inputs, not ranged text styling.

## How the prototype works

- `src/doc.rs`: the source `String` plus selection and an undo stack of byte
  edits. The only mutator.
- `src/projection.rs`: a pure function of (source, AST, selection) to rendered
  leaves. Parser is the `markdown` crate (already in the tree via `gpui-base`)
  with GFM. Each leaf has display text, source-to-display segments
  (`Text`, `Hidden`, `Replaced`), and style marks. Same rules as today's
  `liveRanges`, `softBreaks` and `tableRanges`.
- `src/live.rs`: all editing behaviour with no GPUI types: caret motion and
  stops, typing, Backspace/Delete across atomic markers, table cells, Enter,
  Tab, IME composition. Unit tests cover these; `select_all` and paste do not
  have one.
- `src/table.rs`: Rust port of the pure half of `liveTable.ts` (cell spans,
  `cellSource`, `cellWrite`, `cellChange`, `rowInsertion`), with today's test
  cases ported. The live path writes incrementally through `cell_source` and
  `row_insertion`; `cell_write` and `cell_change` are ported and tested but not
  yet wired, so filling a short row's missing cell is a gap.
- `src/view.rs`: GPUI view. Builds a `BlockNode` tree of keyed `Paragraph`
  leaves, wraps it in `LiveRoot` (a small `Element` that runs prepaint under
  `collect_leaf_geometry`, paints the caret, and registers the
  `EntityInputHandler`), and translates actions/mouse into `live.rs` calls.
- `src/kit_probe.rs`: approach A.

Design choice worth keeping if this goes to production: **the caret is always a
source byte offset**, and "is the marker revealed" is a pure function of the
selection. Undo, save and reveal never have separate state to diverge. Cell
editing uses the same offsets as the rest of the document, which is simpler than
today's contenteditable cell plus `TableState.writing`. Each cell edit still
runs today's whole-cell normalization (`cellSource`), so a pipe cannot be left
unescaped.

## What runs (cargo test)

58 unit tests (`cargo test -p spike-markdown --lib`), including the required
source-to-display mapping and caret mapping across hidden ranges:

- `projection::tests`: marker hiding and reveal per construct, multi-byte
  (Japanese + combining) offsets around hidden markers, soft breaks as a single
  atomic space, bullets and task boxes, active-cell raw text, alignment padding
  not being a stop, tables in quotes staying source, a selection passing over a
  table leaving it rendered, fences, escapes.
- `live::tests`: Left/Right visit every source offset through revealed
  markers, caret never rests inside an atomic stretch, Backspace deletes
  atomic markers whole (the `deletes a block marker whole` and
  `Backspace across the break joins it` cases from `liveMarkdown.test.ts`),
  Enter continuation cases, cell edits write only that cell and undo exactly,
  `|` and a trailing backslash in a cell, Tab/add-row/remove-row, IME
  composition replaces, commits as one undo step, and cancels cleanly;
  extending a selection across markers, a selection staying inside its cell,
  Home/End inside a cell, the caret snapping off markup an edit creates, Enter
  keeping nesting and quote prefixes.
- `view::tests`: UTF-16 and byte conversion around multi-byte and astral text.
- `table::tests`: the `tableLayout`/`cellSource`/`cellWrite`/`cellChange` cases
  from `liveTable.test.ts`.
- `doc::tests`: undo restores bytes exactly.

Run: `cargo test -p spike-markdown --lib`.

## What runs (GPUI, on the private Xvfb from `env.sh`)

Driven with real X11 key and pointer input (`xdotool`) against the running
binary, not a test harness:

- Fixture renders headings, bold (via static faces, see findings), italic, inline code, links, bullets, numbers, tasks, quote, table,
  fenced code, rule, with markers hidden.
- Click into a bold run: the `**` markers appear around it; arrow keys walk
  through them (state log confirms byte offsets 307 and up, as the unit tests
  predict); moving away hides them again.
- Click into a table cell (`Rewritten in Rust`), type `QA` through the normal
  input path: the Kit table repaints with `Rewritten iQAn Rust` and Ctrl+S
  writes only that cell's bytes. Ctrl+Z then Ctrl+S gives a byte-identical file
  (`cmp` passes).
- Up and Down move by visual row, and by cell inside a table; Shift+arrows
  and mouse drag extend a selection across hidden markers.
- F8 steps a composition through the view's `EntityInputHandler`
  (`replace_and_mark_text_in_range`, then `replace_text_in_range`), the same
  calls the platform handler makes: preedit text is drawn underlined at the
  caret and the caret rect the IME would use for its window (`bounds_for_range`)
  sits at the caret.

The independent QA run (`gdev_qa_t05`) is summarized in "QA" below.

## QA (`gdev_qa_t05`, independent, reports only)

Headless on a private Xvfb from `env.sh`, driven by real X11 key and pointer
input against the running binary, frames viewed and described. Eight frames are
not published. Results:

- **Fixture render** (`01-initial.png`): headings, bold, italic, inline code,
  links, autolinks, strikethrough, lists, tasks, quote, table, fenced code and
  rule all render with markers hidden. CJK is tofu in the test environment (see
  findings).
- **Reveal** (`02a-bold-click.png`, `02e-bold-left-299.png`): the bold construct
  at bytes 300..323 shows `**` for carets 300 to 323 inclusive and hides them at
  299 and at 324. Left and Right visit each offset one press at a time. Same
  reveal behaviour checked for a link, inline code and a heading.
- **Lists:** numbers, nested bullets and task boxes render; list prefixes stay
  rendered while inline markers reveal.
- **Table cell** (`05b-cell-typed.png`, `05d-cell-tab-typed.png`): click and
  type in `Rewritten in Rust`, Tab to the next cell, type. The grid holds; the
  active cell shows its source. Ctrl+S: `diff` shows only those two table lines,
  1260 to 1264 bytes (`cmp` first differs at byte 946), and other bytes are
  untouched. Two Ctrl+Z then Ctrl+S: `cmp` exit 0, sha256 matches the fixture.
- **Up/Down in a table:** stays in the column and leaves the table at its top
  and bottom. It lands at the end of the target cell's text, which is today's
  `go(row, col, "end")` behaviour in `liveTable.ts`, not preserved x-position.
- **Selection** (`07f-sel-5.png`): Shift+arrows and mouse drag across bold
  markers give one continuous highlight over plain text, revealed markers and
  bold; collapsing hides the markers again.
- **IME via F8** (`08d-ime-3.png`): preedit `n`, `ni`, `nihon` is underlined at
  the insertion point with the caret after it, commit removes the underline and
  one Ctrl+Z undoes the whole composition. The state log's `caret` rect lags by
  one frame right after a reveal changes layout; frames are correct.
- **Approach A probe** (`kit-probe.png`): markers repainted in the background
  colour leave visible gaps, including wide gaps where link URLs were, and the
  table stays as pipes.


## Findings that matter beyond this spike

1. **Variable Inter has no bold on Linux GPUI.** `text_system.add_fonts` with
   `InterVariable.ttf` registers one face; `FontWeight::BOLD` paints at regular
   weight (`cosmic-text`/`fontdb` do not apply the `wght` axis here). Static
   instances of the same family fixed it for this prototype: generated with
   `fonttools varLib.instancer`, kept in `assets/fonts/`. Anything in the shell
   that wants real Inter bold needs the same (or a separate static weight). Not
   specific to this editor; flagging for T0.3's fonts or the theme task.
2. **The test environment has no CJK system font**, so CJK in the fixture
   shows as tofu. The mapping and IME path are unaffected and tests cover
   multi-byte offsets. CJK glyph rendering needs a CJK font check on the real
   target; the IME frame uses Latin preedit so the underline is legible.

## Cost estimate for full parity

Planning estimate for one engineer working with agents, not measured. Assumes
approach B, with `spike-markdown` as the base. The prototype is about 2,400 lines of
Rust plus about 690 lines of tests reaching the capabilities above.

| Work | Today | Estimate |
|---|---|---|
| Block and inline projection parity (setext, nested quotes, images, autolink refs, code/fence edges, tasks) | `liveMarkdown.ts` `liveRanges`, `liveCodeBlock`, `liveLink` | 2 to 3 days |
| Incremental parsing and damage. The prototype reparses and reprojects on each edit, and `markdown` 1.0 only offers whole-document parsing, so this needs block splitting or another parser | Lezer incremental, `drawnBlocks` stretches (the "2 MB file" comments) | 3 to 6 days |
| Rope-backed `Document` (the projection slices `&str` throughout) | CodeMirror text | 1 day |
| **Virtualized document element** with retained per-block `BlockNode`s and leaf keys, variable heights, scroll anchoring, selection across unrendered blocks | CodeMirror viewport | 4 to 6 days. `collect_leaf_geometry` only returns prepainted leaves, so caret geometry off-screen and Up/Down across the viewport edge need a design |
| List commands: Tab/Shift-Tab indent (12+ cases), renumber on Enter, Ctrl+B/I toggle, heading split, `> ` Enter | `liveMarkdown` commands | 2 to 3 days |
| Hanging indent and shared marker column for lists; list marker width; quote and code styling polish | `liveListLayout`, `--cm-list-marker` | 1 to 2 days |
| Full `liveTable` parity: arrow semantics at cell edges, Escape/exit to a line, trimming typed trailing spaces (today trims on every write; the prototype leaves them in the file), a backslash left before a pipe by Backspace or Delete, Ctrl+B/I in cells, row insertion after header, filling short rows, read-only | `liveTable.ts` | 1 to 2 days |
| Word motion, double/triple click, visual-line Home/End, drag autoscroll | CodeMirror | 1 to 2 days |
| Task toggle, images (embedded raster only), link open on modified click, reference links | `TaskWidget`, `ImageWidget`, `linkHandler` | 1 to 2 days |
| File sync: `minimalChange`, `PendingEchoes`, outside-change reconciliation, autosave (must not save an uncommitted IME preedit, which lives in the source), read-only, CRLF, per-buffer caret and scroll restore | `LiveMarkdownEditor.tsx`, `FileEditor`, `viewStates` | 2 to 3 days |
| Find with match count, selection-match highlight | `search`, `searchCount`, `highlightSelectionMatches` | 2 to 3 days |
| Spellcheck. GPUI offers none; needs a checker and squiggle painting, or a decision to drop it | `spellcheck: "true"` | 3 to 5 days, or descoped |
| Accessibility: AccessKit role, value, selection and caret offsets for a custom text surface. **Largest unknown.** Today WebKitGTK provides this and already needs workarounds | browser | 3 to 5 days, depends on S10 |
| Real IME on Wayland and macOS (S5), candidate window placement | browser | 1 to 2 days once the platform lane exists |
| Port and adapt the remaining `liveMarkdown`, `liveTable` tests | 60-plus cases | 2 days |

**Total: about 29 to 47 engineer-days (roughly 6 to 9 weeks for one engineer
with agents), or 26 to 42 if spellcheck is descoped.** Accessibility and
virtualization are where the range lives; accessibility could move the total if
S10 forces a different text surface.

## Kit changes needed

None found to be required so far. Virtualization and accessibility were not
prototyped and could add to this list. Useful upstream additions, in priority
order:

1. A text-colour field on `TextMark` (or per-range foreground in the leaf
   decorations), so revealed Markdown markers can be dimmed. Today they read as
   normal text. Cosmetic.
2. `LeafGeometry` per visual line (or a `visual_lines()` accessor), so a host
   can do visual Home/End and Up/Down without probing y-coordinates.
3. GPUI text system honouring the `wght` axis of variable fonts on Linux (the
   bold finding above), so one variable face covers every weight.
4. `editor_host` exposure of Table column-width caching across rebuilds, or a
   keyed block API, to avoid re-measuring tables each frame in a virtualized
   host.

No changes to Kit's editor crate would rescue approach A short of a replacement
and block-widget decoration API, which is effectively a new editor layer.

## Fallback if the cost is too high

Decision 3's fallback: a source editor (Kit `EditorState` with the existing
Tree-sitter Markdown highlighter) with the rendered preview beside it (Kit's
`TextView`). Estimate 3 to 5 days, including the same file-sync work; find and
virtualization come with the Kit editor. It loses in-place editing, hidden syntax
and spellcheck. A middle option,
"live preview with a focused source pane", is not worth building; the
editor-host work gets the real feature for a known cost.

## Gaps and things not attempted

Not attempted in this spike: images, table/list keyboard commands beyond those
named above, files with CRLF line endings, drag-out, `Copy` of rendered text as
rich text, and real IME on Wayland or macOS. A hidden horizontal rule has no
keyed geometry, so a click on it lands on a nearby line; keyboard entry reveals
it. Long-document performance was not
measured; the prototype reparses and re-renders the whole document each frame
(see cost table). No `ropey` was needed at fixture size; a production `Document`
should use a rope.

## License

`cargo deny --manifest-path spikes/gpui/Cargo.toml --config deny.toml --locked
check licenses` passes (`licenses ok`), no new exception. The one new direct
dependency is `markdown` 1.0.0 (MIT), already resolved through `gpui-base`, plus
`unicode-segmentation`. `spikes/gpui/Cargo.lock` adds those two names to the
`spike-markdown` stanza only. The two static Inter font files in `assets/fonts/`
are SIL OFL, derived from the repo's `InterVariable` files; the license text
is alongside them.

The Zed `editor`, `terminal`, `terminal_view` and `workspace` crates were not
opened in any form.

## Acceptance

- Prototype runs on the fixture: yes (QA section).
- REPORT.md verdicts match what actually runs: each has a test or frame above.
- `cargo test` for source-to-display mapping and caret mapping: 58 passing.
- License check: `licenses ok`.
