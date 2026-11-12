# live-markdown

Live Markdown for the GPUI desktop app: a Markdown file edited in its rendered
form, with the syntax hidden until the caret reaches it. It replaces Tauri's
CodeMirror view (`LiveMarkdownEditor.tsx`, `liveMarkdown.ts`, `liveTable.ts`)
and grew out of the prototype in `spikes/gpui/markdown` (see its `REPORT.md`).

The app draws it for a Markdown tab's Live mode through one seam,
`editor::set_live_markdown`, registered in `apps/desktop-gpui/src/markdown`.
The view never owns the file: it edits the tab's document buffer, so autosave,
dirty state, changes on disk and reconnects stay in the editor module.

## The rule

**The caret is always a source byte offset.** What is revealed is a pure
function of the selection, so undo, saving and reveal have no state of their
own to drift apart. A table cell is edited at the same offsets as the rest of
the document.

## Modules

| Module | What it holds |
|---|---|
| `doc` | The source text, the selection, undo and redo, and whether the host has taken the latest text |
| `projection` | Source to drawn leaves: which bytes show, hide or are replaced, and their styles. Pure |
| `edit/mod.rs` | `LiveDoc`: caret motion across hidden ranges, typing, Backspace and Delete, IME preedit, undo |
| `edit/list.rs` | Enter in a list item |
| `edit/table.rs` | Editing inside a rendered table: cell writes, Tab, Enter, the arrows and Escape, adding and removing rows, Ctrl+B and Ctrl+I |
| `table` | Pure port of `liveTable.ts`'s source logic: cell spans and cell writes |
| `view` | The GPUI view. The only module that uses GPUI types |

A `find` module (⌘F) goes beside these, at `src/find.rs`.

The source is parsed once per change, and the drawn projection is kept until
the text or the selection changes. A question about one caret position
projects only the top-level blocks around it.

## Tables

A table is edited as Tauri's widget edits it (`liveTable.ts`). A short row is
drawn to the header's width, as GFM draws it: its missing cells take the caret
and write nothing until typed into, which fills the row out. Cells past the
header's width are not drawn. In a read-only document no cell takes the caret.

A wide table wraps its columns to fit, as Tauri's does. Past gpui-base's 4 rem
column floor (about 11 columns at full width) it clips, where Tauri's scrolls
sideways: gpui-base's scrolling table keeps its scroll position to itself, so
the caret could not be kept in view, and it would scroll from 160 px columns
on, well before Tauri.

## Not here

Spellcheck, screen-reader text, a virtualized view, an incremental parser and a
rope are out of scope. Images draw no picture: the alt text is styled like a
link. Files over `LIMIT_BYTES` open as source.

## Size limit

`LIMIT_BYTES` is 48 KiB for now; Tauri's is 1 MiB, and T7.5 (on-screen-only
drawing and block-scoped reparse) restores it. `examples/typing_bench.rs`
measures it: a headless window types 60 keys in the middle of a file and moves
the caret. Release build, software rendering, a 1600 x 1000 window, Markdown
with many tables. "Key" is one keystroke with its redraw; "frame" is a full
redraw.

| Size | Text leaves | Parse | Key p95 | Frame p95 |
|---|---|---|---|---|
| 25 KB | 168 | 3.6 ms | 8.3 ms | 3.1 ms |
| 50 KB | 499 | 8.8 ms | 21.6 ms | 11.7 ms |
| 75 KB | 919 | 12.2 ms | 41.4 ms | 23.6 ms |
| 100 KB | 1311 | 17.1 ms | 60.6 ms | 35.1 ms |
| 150 KB | 3425 | 47.7 ms | 319 ms | 246 ms |
| 250 KB | 6235 | 128 ms | 1268 ms | 1089 ms |
| 500 KB | 10083 | 277 ms | about 2.9 s | about 2.55 s |

Three causes:

- Every block is laid out and prepainted on every frame, on screen or not:
  about 20 to 26 µs per text leaf.
- Every keystroke reparses the whole file: about 0.17 ms per KB.
- `gpui-base` 0.9.0 sweeps its whole retained-layout table on each insert once
  it holds 4096 entries or more (`src/text/inline.rs:480`), one entry per text
  piece, so a frame turns quadratic past that.

