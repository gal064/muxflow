/**
 * A page of scrollback, from the rows tmux captured to the bytes xterm is given.
 *
 * The unit here is the physical row, and keeping it that way is the whole point
 * of the module. `capture-pane` for history runs without `-J` (see
 * `capture_history_command`), so every line the host joined into the answer is
 * one row of tmux's grid at the width the pane had — which is the width this
 * terminal had, so it is one row of xterm's grid too. The renderer's `skip`,
 * tmux's `-S`/`-E` and `#{history_size}` are all rows as well, so the overlap a
 * busy pane creates can be counted and trimmed exactly.
 *
 * What `-J` used to buy is bought back by [`composeHistoryPage`]: joined
 * scrollback reflows on a resize and copies as one line, and hard-broken
 * scrollback does neither. A row that fills the grid is written with no line
 * break after it, so xterm wraps it itself and marks the continuation
 * `isWrapped` — the same row, with the same flag, as if the pane had printed it
 * live.
 */

/** What the host joins captured rows with, and never appends a trailing one. */
const ROW_SEPARATOR = Uint8Array.of(0x0d, 0x0a);

const decoder = new TextDecoder();

/**
 * The captured rows, in order.
 *
 * `n` separators mean `n + 1` rows, always — including when the last row is
 * blank and the payload ends on a separator, which is a row the user printed
 * and not an artefact of the joining.
 */
export function splitHistoryRows(history: Uint8Array): Uint8Array[] {
  const rows: Uint8Array[] = [];
  let start = 0;
  for (let index = 0; index + 1 < history.byteLength; index += 1) {
    if (history[index] !== 0x0d || history[index + 1] !== 0x0a) continue;
    rows.push(history.subarray(start, index));
    index += 1;
    start = index + 1;
  }
  rows.push(history.subarray(start));
  return rows;
}

/** Advances past one escape sequence, given the index of its `ESC`. */
function afterEscape(text: string, escape: number): number {
  const introducer = text[escape + 1];
  if (introducer === "[") {
    // CSI: parameter and intermediate bytes, ended by one final byte.
    let index = escape + 2;
    while (index < text.length) {
      const code = text.charCodeAt(index);
      index += 1;
      if (code >= 0x40 && code <= 0x7e) break;
    }
    return index;
  }
  if (introducer === "]") {
    // OSC: ended by BEL or by ST (`ESC \`).
    let index = escape + 2;
    while (index < text.length) {
      const code = text.charCodeAt(index);
      if (code === 0x07) return index + 1;
      if (code === 0x1b && text[index + 1] === "\\") return index + 2;
      index += 1;
    }
    return index;
  }
  return escape + 2;
}

/**
 * How many cells a captured row occupies, as nearly as this side can tell.
 *
 * `capture-pane -e` emits SGR sequences, which take no cells, and the row is
 * otherwise text. Deliberately an approximation in one direction: a double-width
 * character counts as one cell rather than two, so a row of CJK that filled the
 * grid measures short and is treated as unwrapped. That costs a hard break in a
 * place a resize would have reflowed — the same thing the whole page used to do
 * — and never a joined row that should have stayed two.
 */
export function visibleWidth(row: Uint8Array): number {
  const text = decoder.decode(row);
  let width = 0;
  let index = 0;
  while (index < text.length) {
    const code = text.codePointAt(index) ?? 0;
    if (code === 0x1b) {
      index = afterEscape(text, index);
      continue;
    }
    if (code < 0x20 || code === 0x7f) {
      index += 1;
      continue;
    }
    width += 1;
    index += code > 0xffff ? 2 : 1;
  }
  return width;
}

/**
 * The bytes for a page of captured rows, wrapped the way tmux had them.
 *
 * A row narrower than the grid ends with a line break. A row that fills the
 * grid is one tmux wrapped, so it is written with nothing after it and xterm's
 * own auto-wrap carries the next row onto the continuation line, `isWrapped`
 * and all. Both cost exactly one row, so the composed page occupies as many
 * rows as it was captured with — which is what the caller counts on to put the
 * viewport back where the user was reading.
 *
 * Two deliberate imprecisions, both in the safe direction. A row that fills the
 * grid without having wrapped — a full-width rule in a TUI — is joined to the
 * row below it: the same pixels in the same places, and only a resize or a copy
 * can tell. And a row followed by a blank one is never joined, because the
 * blank row is real output and the `\r` that would carry the join cancels the
 * pending wrap and swallows it.
 *
 * No trailing break: the caller separates the page from the screen below it.
 */
export function composeHistoryPage(rows: readonly Uint8Array[], columns: number): Uint8Array {
  const pieces: Uint8Array[] = [];
  let length = 0;
  const push = (piece: Uint8Array) => {
    pieces.push(piece);
    length += piece.byteLength;
  };
  for (let index = 0; index < rows.length; index += 1) {
    push(rows[index]);
    if (index === rows.length - 1) break;
    const wrapsIntoTheNext = columns > 0
      && rows[index + 1].byteLength > 0
      && visibleWidth(rows[index]) >= columns;
    if (!wrapsIntoTheNext) push(ROW_SEPARATOR);
  }
  const composed = new Uint8Array(length);
  let offset = 0;
  for (const piece of pieces) {
    composed.set(piece, offset);
    offset += piece.byteLength;
  }
  return composed;
}
