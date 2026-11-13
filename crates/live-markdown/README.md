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
| `doc` | The source text, the selection, undo and redo (kept across an outside change), and what changed since the host and the parse last took it |
| `projection` | Source to drawn leaves: which bytes show, hide or are replaced, and their styles. A list item's marker is a leaf of its own, drawn in a marker column. Pure |
| `link` | The link under a source position, for ⌘-click |
| `reparse` | The parsed tree, kept up to date by parsing only a window around each change |
| `lists` | The parse as Tauri's lezer reads lists (through `projection::parse`, which draws a source markdown-rs panics on as plain lines): inside a list, an empty item or one not numbered 1 may break into a paragraph (CommonMark refuses both) |
| `edit/mod.rs` | `LiveDoc`: caret motion across hidden ranges, typing, Backspace and Delete, IME preedit, undo |
| `edit/list.rs` | List commands: Enter, Tab and Shift-Tab, renumbering |
| `edit/commands.rs` | ⌘B and ⌘I, the task box, Enter before a heading |
| `edit/motion.rs` | Word motion, double- and triple-click selection, Home and End |
| `edit/changes.rs` | Several replacements made as one undo step |
| `edit/drawn.rs` | What is drawn, one segment per top-level block, with the heights the view measured |
| `edit/table.rs` | Editing inside a rendered table: cell writes, Tab, Enter, the arrows and Escape, adding and removing rows, Ctrl+B and Ctrl+I |
| `table` | Pure port of `liveTable.ts`'s source logic: cell spans and cell writes |
| `view` | The GPUI view. The only module that uses GPUI types |
| `view/viewport.rs` | Draws only the blocks on screen: the scroll anchor, the height estimates, the scrollbar |
| `view/wide.rs` | A table wider than its column: the width it is drawn at, from its columns' floors |
| `find` | What the view marks where it draws: the find bar's matches and the selection's other occurrences, as CodeMirror's search and `highlightSelectionMatches` |

## Find

Ctrl+F (⌘F) opens the app's find bar, the one the source editor has, over
the Live view: the host matches, counts, steps and replaces through
`LiveView::select_match`, `replace` and `replace_all` (each one undo step),
and hands the view its matcher to mark. The view searches only the blocks it
draws, so marks cost nothing for the rest of a large file and are right on
any block drawn later. A match in syntax hidden while the bar has focus (a
link's URL) is counted and selected but not marked, as in Tauri; where the
hidden markup starts is scrolled into view, and the syntax shows once the
view has focus again. At 1 MiB of prose, opening find on a common word (counting to
the limit, selecting, marking, two frames) takes about 7 ms, a step about
3.5 ms (p95) and a jump to a block never drawn about 10 ms; a key typed with
the bar open costs as one typed without it, plus a count (`--find` in the
benchmark below).

A change parses again only a window of top-level blocks around it, and only
the blocks a change or the selection reaches are projected again. The view
lays out and paints only what is on screen, with a page above and below and
the caret's and the preedit's blocks; the rest count at the height they last
drew at, or an estimate. A scroll position is a block and an offset in it, so
an edit above the screen does not move what is shown.

## Tables

A table is edited as Tauri's widget edits it (`liveTable.ts`). A short row is
drawn to the header's width, as GFM draws it: its missing cells take the caret
and write nothing until typed into, which fills the row out. Cells past the
header's width are not drawn. In a read-only document no cell takes the caret.

A wide table wraps its columns to fit, as Tauri's does. Past gpui-base's 4 rem
column floor (about 11 columns at full width) it scrolls sideways inside its
frame (`view/wide.rs`): it is drawn at the width of its columns' floors and
moved inside the column by a sideways wheel or swipe, or Shift and the wheel.
The caret's cell is brought into view whenever the caret moves or the view
edits the text, and `LiveView::reveal_in_table` does the same for any source
position. Each table's scroll is kept in `LiveDoc` by where the table starts,
so it moves with the table on an edit, and is dropped with the table; a
change that spans the table's start (an outside change above and below it)
scrolls it back to its left edge. No scrollbar is drawn under it, as
gpui-base draws none. gpui-base's own scrolling table is not used: it keeps
its scroll position to itself, and it scrolls from 160 px columns on, well
before Tauri.

## Not here

Spellcheck, screen-reader text, a general incremental parser and a rope are out
of scope. Images draw no picture: the alt text is styled like a
link. Files over `LIMIT_BYTES` open as source.

## Size limit

`LIMIT_BYTES` is 1 MiB, as Tauri's. `examples/typing_bench.rs` measures it: a
headless window types 60 keys at the top, middle or end of a generated file,
then scrolls it with the wheel. Release build, software rendering, a 1600 x
1000 window. "Key" is one keystroke with its redraw, "frame" a full redraw,
"scroll" one wheel step with its redraw; p95 in ms, the worst of the three
places for frame and scroll.

| File | Size | Open | Key: top / middle / end | Frame | Scroll |
|---|---|---|---|---|---|
| Prose | 100 KB | 0.15 s | 3.4 / 4.5 / 2.3 | 1.6 | 4.3 |
| Prose | 500 KB | 0.32 s | 9.6 / 7.2 / 2.0 | 3.2 | 6.1 |
| Prose | 1 MiB | 0.54 s | 8.5 / 6.1 / 1.9 | 2.5 | 3.7 |
| Tables | 100 KB | 0.15 s | 7.4 / 9.3 / 8.6 | 3.2 | 14.6 |
| Tables | 500 KB | 0.36 s | 9.1 / 10.5 / 7.2 | 3.5 | 15.7 |
| Tables | 1 MiB | 0.65 s | 14.2 / 16.0 / 8.3 | 5.9 | 13.7 |

The prose ends in link definitions: at the end, the first space typed into one
turns it into a paragraph, and that one key parses the whole file again (0.5 s
at 1 MiB, see below).

Other files of 1 MiB, typing in the middle (20 keys):

| File | Open | Key |
|---|---|---|
| A changelog: release sections of short lists, 2,651 link definitions | 1.0 s | 31 |
| 1,550 link definitions under the paragraphs that use them | 0.76 s | 17 |
| Sections of short lists | 0.39 s | 7.1 |
| One tight list | 0.57 s | 524 |
| One loose list | 0.90 s | 896 |

What still costs:

- Opening parses the whole file on the UI thread: up to about a second at
  1 MiB. `markdown` 1.0.0 takes time that grows faster than the text on
  tables and lists: parsed whole, 1 MiB of tables takes seconds. A new tree
  is built a piece at a time instead.
- A change that adds, drops or renames a link or footnote definition parses
  the whole document again: about 0.5 s for 1 MiB of prose. Every window is
  parsed with a stand-in for each definition in the file, so a file with
  thousands of them types slower, about 30 ms a key at 1 MiB.
- A top-level block is the least that is parsed, projected and laid out
  again. A file that is one long list or quote is slow to type in, about half
  a second a key at 1 MiB. A code fence opened near the top of a large file
  runs to its end until it is closed: each key in between parses that whole
  block again, seconds a key at 1 MiB. A single table or code block of most
  of a megabyte is slow to draw.
- An edit that changes how much of a file reads (a fence opened or closed
  above most of it, a large paste or its undo) parses all of that again, a
  piece at a time: up to about as long as opening the file.
- `markdown` carries a little table state across even a blank line, so a
  rare edit can leave a block read differently from a fresh parse, until the
  file is opened again.
- `markdown` 1.0.0 panics on a few texts. A window or piece it panics on is
  parsed whole instead; a whole file it panics on draws as plain lines.
- A table's cells are each parsed for inline Markdown when the table is
  projected, which is most of a keystroke in a table.
