# S9: does `alacritty_terminal` agree with tmux?

Spike S9 of the GPUI migration (`docs/gpui-migration.md`). Produced by
`tests/performance/runtime/run-vt-parity.sh` with `alacritty_terminal = "=0.26.0"`
against tmux 3.8. Every number below is from one run of the lane on this tree,
except the `variation-selector-always-wide off` probe, which was run separately.

## Verdict

**Not a clean pass.** The engine agrees with tmux on CJK, plain emoji, flags,
combining marks, box drawing and a wide glyph at the margin. It disagrees on
emoji sequences that use VS16 or ZWJ, at every quiesce point. Autowrap of
history pages is proven and passes. The lane exits non-zero, which is the
truthful result: the S9 criterion is "no grid disagreement".

| Gate | Result |
|---|---|
| `vt100` lane (shell and agent panes) | PASS at all five points, with pauseContinue compared after another pane prints |
| `alacritty_terminal`, shell and agent panes | PASS at all five points |
| `alacritty_terminal`, width fixture | FAIL at all five points |
| History-page wrap | PASS |
| Pause recovery with no other output (`vt-parity-pause-idle`, not engine-related) | FAIL: not recovered within 5 s |

**pauseContinue caveat.** The `vt100` comparison at this point runs after the lane
repaints another pane. Measured on this run: 5 s after the flood was interrupted,
the shell and agent panes had not both recovered
(`pauseRecoveredWithoutOtherOutput: false`). After the width pane printed, both
had, within the 15 s budget (`pauseRecoverySeeded: true`, 2 `TerminalFlowPaused`
events). So the comparison proves "correct once something else prints", and the
unassisted case is its own failing gate. See "Lane repairs".

## How it is measured

- Delivered bytes for each pane are replayed through a real `Term` and its `vte`
  processor (`vt_engine::replay`), sized to tmux's grid.
- tmux's truth is `capture-pane -p -e` read as text and SGR with **no terminal
  parser** (`vt_engine::parse_capture`). The `vt100` lane renders the capture
  through the parser it is checking, so a width disagreement cancels out there.
- A third pane, in its own window so the first two keep their geometry, runs the
  width fixture (`performance-test-driver vt-width-fixture`). Each row is
  `[class] sample`, then an absolute move to column 60, then an anchor `|`.
  tmux's anchor is at column index 59 by construction. A capture has no padding
  cells, so row text alone cannot show which column something landed in, and on
  the seed path the text is tmux's own and matches even when the grid does not.
  So an anchored row passes only if its text matches **and** the engine's anchor
  is in column 59.
- At each quiesce point the width pane is compared twice: `delivered` (as the
  recovery left it, normally a seed built from a capture) and `live` (after the
  fixture repaints with its own bytes). The engine receives different bytes in
  each.
- Text, attributes (bold, dim, foreground, background) and anchors are all gates.
- The `seed`, `recovery` and `reconnect` points all reach the host through
  `RequestTerminalSeed`, so they share a path; only `hideReveal` and
  `pauseContinue` recover differently.
- The same bytes also go through `vt100` for the width pane, reported per class,
  so a limit of that parser is not blamed on the engine. It does not gate.

## Results per quiesce point

Mismatch totals over four comparisons per point: shell, agent, width delivered,
width live.

| Quiesce point | Result | Text rows | Attribute cells | Anchor rows | Shell | Agent | Width delivered | Width live |
|---|---|---|---|---|---|---|---|---|
| seed | FAIL | 8 | 0 | 5 | pass | pass | 3 text, 5 anchor | 5 text |
| hideReveal | FAIL | 8 | 0 | 5 | pass | pass | 3 text, 5 anchor | 5 text |
| pauseContinue | FAIL | 10 | 0 | 0 | pass | pass | 5 text | 5 text |
| recovery | FAIL | 8 | 0 | 5 | pass | pass | 3 text, 5 anchor | 5 text |
| reconnect | FAIL | 8 | 0 | 5 | pass | pass | 3 text, 5 anchor | 5 text |

Every failing row is one of the five VS16 or ZWJ rows. At `pauseContinue` the
`delivered` comparison is a live repaint, not a seed, because the round repaints
the width pane to release the pause (see "Lane repairs"), hence no anchor misses.
Attributes matched everywhere, including on the rows whose columns did not.

`vt100` lane, same run: 0 text mismatches and 0 attribute mismatches at all five
points, 0 connection-wide resyncs, no sequence gap.

## Results per fixture class

Each count is one row compared at one point on one path. The
`alacritty_terminal` column counts text, attributes and anchor; the `vt100` column
counts text and anchor only.

| Class | Sample | Result | `alacritty_terminal` | `vt100` |
|---|---|---|---|---|
| `cjk` | `漢字かな混じり文`, halfwidth kana mixed with CJK | PASS | 20/20 | 20/20 |
| `emoji` | `🚀🎉😊` | PASS | 10/10 | 10/10 |
| `emoji-flag` | `🇯🇵🇺🇸` regional indicators | PASS | 10/10 | 10/10 |
| `emoji-vs16` | `❤️☀️✈️`, keycap `#️⃣` | FAIL | 0/20 | 0/20 |
| `emoji-zwj` | `👩‍💻`, family, `🏳️‍🌈` | FAIL | 0/30 | 0/30 |
| `combining` | NFD `éäôñ`, four marks stacked on one base | PASS | 20/20 | 20/20 |
| `box` | `┌─┬─┐│├┼┤└┴┘═║╔╗╚╝` | PASS | 10/10 | 10/10 |
| `margin` | a wide glyph that does not fit in the last column | PASS | 20/20 | 20/20 |

`vt100` fails on exactly the rows the engine fails on and places the anchor in
the same column every time. So this is not a defect the engine introduced
relative to the comparison parser, but it is still a real disagreement with
tmux: `vt100` simply has the same per-code-point width model.

## Divergences

All five come from one mechanism. `Term::input` sizes each `char` on its own
(`unicode-width`): a zero-width code point is attached to the previous cell and
nothing is merged. tmux measures the cluster: VS16 widens its base to two cells
(tmux's default `variation-selector-always-wide on`) and a ZWJ sequence occupies
the two cells of one glyph.

Columns are 0-based. "Seed anchor" is the engine's anchor column on the seed
path; tmux's is 59. Sample cells are derived from the anchor gap.

| Class | Bytes | tmux cells | Engine cells | `vt100` cells | Seed anchor | Arguably right |
|---|---|---|---|---|---|---|
| `emoji-vs16` `❤️☀️✈️` | `e2 9d a4 ef b8 8f e2 98 80 ef b8 8f e2 9c 88 ef b8 8f` | 6 | 3 | 3 | 56 | tmux. VS16 requests emoji presentation, which is two cells wide |
| `emoji-vs16` `#️⃣` | `23 ef b8 8f e2 83 a3` | 2 | 1 | 1 | 58 | tmux, same reason; the keycap sequence displays as an emoji |
| `emoji-zwj` `👩‍💻` | `f0 9f 91 a9 e2 80 8d f0 9f 92 bb` | 2 | 4 | 4 | 61 | tmux. One grapheme cluster, one glyph |
| `emoji-zwj` family | `f0 9f 91 a8 e2 80 8d f0 9f 91 a9 e2 80 8d f0 9f 91 a7 e2 80 8d f0 9f 91 a6` | 2 | 8 | 8 | erased | tmux, same reason |
| `emoji-zwj` `🏳️‍🌈` | `f0 9f 8f b3 ef b8 8f e2 80 8d f0 9f 8c 88` | 2 | 3 | 3 | 60 | tmux, same reason |

Whichever is right in the abstract, tmux is the authority here: applications
address cells by tmux's grid, so a different layout in the renderer is a defect
in the renderer.

What it looks like, for the same row:

- **Live path.** The anchor is moved absolutely, so it lands correctly, but the
  gap before it differs by the width delta: 3 cells for `❤️☀️✈️`, 6 for the family.
- **Seed path, engine wider (ZWJ).** The seed writes the row and then erases the
  blank tail with a cursor move to tmux's column and `ESC [ K`
  (`paint_cell_overlay` in `apps/host/src/service/terminal/seed.rs`). The engine's
  content is further right, so the erase destroys it: `| tail` becomes `| ta` for
  `👩‍💻`, `| tai` for `🏳️‍🌈`, and the family row loses its anchor and tail entirely.
  This is data loss on a seed, not only misalignment.
- **Seed path, engine narrower (VS16).** The anchor sits left of tmux's, and the
  rest of the row shifts with it.

### Not divergences

Combining marks, CJK, flags and the margin case were expected risks and agree.
A flag pair measures two cells on both sides.

## History-page wrap

**PASS.** The check takes a real page from the host (`RequestTerminalHistory`),
composes it with a Rust port of `composeHistoryPage` (`vt_engine::compose_history_page`),
and feeds it to the engine. The control is the same rows replayed as the pane
printed them.

- 11 page rows at 120 columns: 4 of 4 expected continuations carry `WRAPLINE`, and
  no other row does. That includes a row whose bold run crosses the wrap. The
  check compares text and the wrap flag, not the style of the cells.
- Wrapped runs copy back as one line.
- Rows and copied text are identical to the live-printed control at 120, and after
  resizing both to 103 and to 133 columns.

Unit tests cover the cases that make the check meaningful, including that hard
breaks are not marked and that a page fails against a hard-broken control: a full
row before a blank row keeps its break, a blank row carrying only SGR counts as
blank, an NFD row measures in clusters, and a row of wide glyphs measures short
and stays hard-broken, which is the documented safe direction of
`composeHistoryPage`.

## Recommendation

Keep `alacritty_terminal` as the planned engine. Everything S9 set out to de-risk
holds except the width of VS16 and ZWJ sequences, and that has one cause in one
function. Do not mark S9 passed, and keep the lane failing until it is resolved:
no gate was weakened and nothing is allowlisted here.

The decision for the owner is how to close it:

1. **Make the engine's width grapheme-aware**, so VS16 and ZWJ sequences occupy
   the cells tmux gives them. This is the only option that closes both classes.
   It is a change to `Term::input` and needs a patched crate or a fork
   (Apache-2.0 permits it). Recommended, and it should be a named Phase 3 item.
2. **Set `variation-selector-always-wide off` in the host's tmux configuration.**
   In a probe the VS16 rows then matched the engine (`❤️☀️✈️` 3 cells, `#️⃣` 1
   cell). It closes the VS16 class at the cost of changing what tmux tells
   applications. `🏳️‍🌈` also moved (from 2 cells to 1), and it does nothing for the
   other ZWJ rows, so it cannot close S9 alone.
3. **Accept the limit** and change the S9 criterion. Not recommended on its own,
   because the seed path erases content.

Measured afterwards, in `docs/gpui-phase0-report.md`: today's xterm, on its default
Unicode 6 width table, is wrong on VS16 and the family ZWJ too, and right on
`👩‍💻` and `🏳️‍🌈`, where the engine is too wide.

## Lane repairs

The lane did not run at all before this work; both repairs are in the driver.

- **Seeds.** Before these changes the lane failed at `attach` with "fixture panes
  were never seeded". The host mounts a subscribed connection's panes hidden
  (`attach` with `make_visible = false`), seeds them while hidden so nothing is
  emitted, and a later `AttachTerminal` of the same panes captures nothing new.
  The desktop asks for each pane with `requestTerminalSeed` when it mounts, so
  `attach_and_seed` now does too. Other lanes that attach and wait for a seed
  use the same pattern; they were not run or changed here.
- **Pause recovery.** With the flood interrupted and the agent fixture held,
  nothing else writes after the daemon stall, and the round compared a stale
  screen. The lane now measures this in two steps: it waits 5 s with no other
  output and reports the answer, then repaints the width pane and waits for the
  shell and agent recovery seeds. Each step is its own gate. The first came back
  false: **after a host stall with nothing else printing, the panes had not both
  recovered within 5 s.** This run cannot tell "slower than 5 s" from "never without
  output". A one-off control-stream trace on tmux 3.8 (not kept as an artifact) had
  `%pause` reaching the host only after more output reached that control client,
  so the cause may be tmux and not the host; that was not isolated. It is a product
  finding for the host owner, independent of the engine, and it keeps the lane red
  until it is fixed or the owner accepts it.

## Limits

One tmux version (3.8) and one machine. The fixture has one anchor per row, so a
disagreement is located to the row and not to the code point inside it. The lane
measures the engine's grid, not GPUI's glyph rendering. `alacritty_terminal` runs
with `Config::default()`.

The capture reader handles what these panes produce. It does not model
colon-form underline or underline-colour SGR, OSC 8 hyperlinks, or styled trailing
blanks (the capture is taken without `-N`), so such content would show as a false
mismatch. tmux carries SGR across captured rows; the engine comparison follows
that, while the existing `vt100` truth (`repaint_capture`) resets each row, which
would be a false mismatch for a style crossing rows. No current pane does.
