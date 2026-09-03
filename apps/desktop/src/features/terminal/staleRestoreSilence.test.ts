// @vitest-environment jsdom
// A reconnect restores every cached pane, and any pane whose live stream has
// already overtaken its cached screen refuses that restore and asks the host
// for a seed. That is the recovery working — but it was reaching the user as a
// notice reading "A restore through generation 4 arrived for a pane that has
// already been given generation 5…", which is an internal sentence about an
// event nobody has to act on. It belongs in the journal.
//
// A history splice is here for the same reason and not a second subject: xterm
// has no prepend, so putting scrollback above row 0 is a rewrite of the whole
// buffer — a restore in disguise, and journalled rather than spoken on the rare
// occasions it cannot be done. It refuses on far less than a restore does,
// because a pane that never stops printing is a pane whose user could otherwise
// never scroll up: what the request anchors on is the rows this side holds, not
// the generation the stream is on.
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { recordIncident } from "../../diagnostics/incidents";
import { XtermRenderer } from "./TerminalRenderer";
import { ownTerminalBytes } from "./TerminalBytes";

vi.mock("../../diagnostics/incidents", () => ({ recordIncident: vi.fn() }));

/**
 * What a page is asked against: the rows this terminal held, at the grid it
 * held them at. A test that has not resized reads the grid off the terminal.
 */
function anchoredAt(renderer: XtermRenderer, skip: number) {
  return { skip, ...renderer.grid };
}

/** Bounded polling: the splice completes on a frame or on a fallback timer. */
async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  for (let attempt = 0; attempt < 400; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error(`timed out waiting for ${label}`);
}

describe("stale cached restore", () => {
  beforeAll(() => {
    if (!window.matchMedia) {
      window.matchMedia = ((query: string) => ({
        matches: false, media: query, onchange: null,
        addEventListener: () => undefined, removeEventListener: () => undefined,
        addListener: () => undefined, removeListener: () => undefined,
        dispatchEvent: () => false,
      })) as typeof window.matchMedia;
    }
  });

  beforeEach(() => {
    vi.mocked(recordIncident).mockClear();
  });

  it("journals the refusal and asks for a seed without saying anything to the user", () => {
    const diagnostics: Array<string | undefined> = [];
    const reseeds: string[] = [];
    const renderer = new XtermRenderer({
      paneId: "%7",
      onDiagnostic: (message) => diagnostics.push(message),
      onResnapshotRequired: (reason) => { reseeds.push(reason); },
    });
    renderer.open(document.createElement("div"));
    renderer.write(ownTerminalBytes(new TextEncoder().encode("live")), undefined, 5);
    // jsdom has no GPU, so opening already spoke once about the DOM renderer.
    diagnostics.length = 0;

    expect(renderer.restore("cached", undefined, 4, 4)).toBe(false);

    expect(recordIncident).toHaveBeenCalledWith("pane.staleRestore", {
      paneId: "%7",
      throughGeneration: 4,
      lastEnqueuedGeneration: 5,
    });
    // The host is still asked for the seed; only the sentence is withheld.
    expect(reseeds).toHaveLength(1);
    expect(diagnostics).toEqual([]);

    renderer.dispose();
  });

  /**
   * Loading earlier output is a re-seed in disguise: xterm has no prepend, so
   * the only way to put scrollback above row 0 is to rewrite the buffer with
   * the history and the current screen together.
   *
   * The one thing that can break is eviction. The request said how many rows
   * this pane already held and tmux captured the range ending exactly there, so
   * rows printed since are *below* the captured range and arrive inside the
   * serialization — but once the scrollback is full, xterm drops rows off the
   * top to make room, and then the captured range no longer reaches what this
   * buffer begins with. Splicing the two together would leave a hole in the
   * middle of the user's output, so it is refused, and like a stale restore it
   * journals instead of speaking. Nothing is lost: the rows are still in tmux,
   * and the next time the user reaches the top the question is asked again.
   */
  it("refuses a history splice when the scrollback hit its cap since the request", async () => {
    const diagnostics: Array<string | undefined> = [];
    const renderer = new XtermRenderer({
      paneId: "%9",
      onDiagnostic: (message) => diagnostics.push(message),
    });
    renderer.open(document.createElement("div"));
    renderer.setGrid({ columns: 20, rows: 5 });
    // Past the ceiling xterm enforces, so the top of this buffer is rows it
    // evicted rather than the rows the capture was measured from.
    const flood = Array.from(
      { length: renderer.scrollbackLimit + 50 },
      (_, index) => `row-${index}`,
    ).join("\r\n");
    renderer.write(ownTerminalBytes(new TextEncoder().encode(flood)), undefined, 5);
    // Polled on the row count rather than on a serialization: this buffer is
    // ten thousand rows deep and serializing it on every tick is the test.
    await waitFor(
      () => renderer.scrollbackRows >= renderer.scrollbackLimit,
      "the scrollback to reach its cap",
    );
    diagnostics.length = 0;

    const history = ownTerminalBytes(new TextEncoder().encode("earlier output"));
    await expect(renderer.prependHistory(history, anchoredAt(renderer, 0))).resolves.toBe("superseded");

    expect(recordIncident).toHaveBeenCalledWith("pane.historySuperseded", {
      paneId: "%9",
      reason: "scrollbackCapped",
      scrollbackRows: renderer.scrollbackLimit,
      scrollbackLimit: renderer.scrollbackLimit,
    });
    expect(diagnostics).toEqual([]);

    renderer.dispose();
  });

  /** A TUI's frame is not scrollback, and the rewrite would destroy it. */
  it("refuses a history splice while the alternate screen is up", async () => {
    const renderer = new XtermRenderer({ paneId: "%14" });
    renderer.open(document.createElement("div"));
    renderer.write(ownTerminalBytes(new TextEncoder().encode("\u001b[?1049h")), undefined, 5);
    await waitFor(() => renderer.isAlternateScreenActive(), "the alternate screen to come up");

    const history = ownTerminalBytes(new TextEncoder().encode("earlier output"));
    await expect(renderer.prependHistory(history, anchoredAt(renderer, 0))).resolves.toBe("superseded");

    // Every refusal says which one it was. A splice that is turned away in
    // silence is a pane that quietly stops holding its scrollback.
    expect(recordIncident).toHaveBeenCalledWith(
      "pane.historySuperseded",
      expect.objectContaining({ paneId: "%14", reason: "alternateScreen" }),
    );

    renderer.dispose();
  });

  /**
   * The page tmux answers with is not the page that was asked for.
   *
   * `-S`/`-E` are evaluated against the pane's display at the moment tmux
   * *runs* the capture. A pane that printed three rows between the request
   * leaving and the capture running has moved its display down by three, so the
   * page comes back ending three rows too low — and those three rows are rows
   * this buffer already holds. Spliced in whole they appear twice, and the next
   * page inherits the error through the skip it is asked with.
   *
   * The stream is ordered, so those rows reached this terminal before the
   * answer did: the overlap is exactly what this buffer has gained since it
   * quoted its skip, and it comes off the foot of the page.
   */
  it("trims the rows a busy pane printed over before splicing the page", async () => {
    const renderer = new XtermRenderer({ paneId: "%13" });
    renderer.open(document.createElement("div"));
    renderer.setGrid({ columns: 20, rows: 5 });
    // Fixed-width names, so no row's name is a prefix of another's and counting
    // occurrences means what it says.
    const rows = (from: number, to: number) => Array.from(
      { length: to - from + 1 },
      (_, index) => `row-${String(from + index).padStart(2, "0")}`,
    );
    renderer.write(ownTerminalBytes(new TextEncoder().encode(rows(1, 12).join("\r\n"))), undefined, 1);
    await waitFor(() => renderer.scrollbackRows === 7, "the first twelve rows to land");

    // What the request quotes: twelve rows into a five-row grid.
    const skip = renderer.scrollbackRows;

    // And then three more rows, printed before tmux got round to the capture.
    renderer.write(ownTerminalBytes(new TextEncoder().encode(`\r\n${rows(13, 15).join("\r\n")}`)), undefined, 2);
    await waitFor(() => renderer.scrollbackRows === 10, "the three later rows to land");

    // tmux's answer, measured three rows too low: two rows this pane has never
    // seen, and then the three oldest rows it is already holding.
    const page = ["top-1", "top-2", ...rows(1, 3)].join("\r\n");
    await expect(
      renderer.prependHistory(ownTerminalBytes(new TextEncoder().encode(page)), anchoredAt(renderer, skip)),
    ).resolves.toBe("applied");
    await waitFor(() => renderer.serialize().includes("top-1"), "the page to be spliced");

    const spliced = renderer.serialize();
    const everyRow = ["top-1", "top-2", ...rows(1, 15)];
    for (const row of everyRow) expect(spliced.split(row), row).toHaveLength(2);
    const positions = everyRow.map((row) => spliced.indexOf(row));
    expect(positions).toEqual([...positions].sort((left, right) => left - right));

    renderer.dispose();
  });

  /**
   * The same trim, on the output this actually happens to: wrapped.
   *
   * The history capture runs without `-J`, so every captured line is one
   * physical row and the overlap — a row count — can be taken off the page a row
   * at a time. Under `-J` the page was *lines*, and trimming three rows off a
   * page of joined lines removed however many rows those three lines covered.
   *
   * And the rows that survive still wrap: a captured row that fills the grid is
   * written with no break after it, so xterm wraps it itself rather than this
   * side hard-breaking scrollback that then reflows differently from the output
   * printed live beside it.
   */
  it("trims wrapped rows by the row, and leaves the wrap to xterm", async () => {
    const renderer = new XtermRenderer({ paneId: "%16" });
    renderer.open(document.createElement("div"));
    renderer.setGrid({ columns: 20, rows: 5 });
    const rows = (from: number, to: number) => Array.from(
      { length: to - from + 1 },
      (_, index) => `row-${String(from + index).padStart(2, "0")}`,
    );
    renderer.write(ownTerminalBytes(new TextEncoder().encode(rows(1, 12).join("\r\n"))), undefined, 1);
    await waitFor(() => renderer.scrollbackRows === 7, "the first twelve rows to land");
    const skip = renderer.scrollbackRows;

    // Three more rows before tmux got round to the capture.
    renderer.write(ownTerminalBytes(new TextEncoder().encode(`\r\n${rows(13, 15).join("\r\n")}`)), undefined, 2);
    await waitFor(() => renderer.scrollbackRows === 10, "the three later rows to land");

    // The answer: one row filling the twenty-column grid and its continuation,
    // one short row, and then the three rows this pane printed over.
    const head = `wrap-head${"x".repeat(11)}`;
    const page = [head, "wrap-tail", "top-3", ...rows(1, 3)].join("\r\n");
    await expect(
      renderer.prependHistory(ownTerminalBytes(new TextEncoder().encode(page)), anchoredAt(renderer, skip)),
    ).resolves.toBe("applied");
    await waitFor(() => renderer.serialize().includes("wrap-head"), "the page to be spliced");

    const spliced = renderer.serialize();
    for (const row of ["wrap-head", "wrap-tail", "top-3", ...rows(1, 15)]) {
      expect(spliced.split(row), row).toHaveLength(2);
    }
    // The full-width row and its continuation are one line again, with no break
    // between them: xterm wrapped it, so it reflows and copies as one.
    expect(spliced).toContain(`${head}wrap-tail`);

    renderer.dispose();
  });

  /**
   * A row is the same thing on both sides of the splice only at one width.
   *
   * A reflow moves rows across the boundary between what tmux keeps in its
   * history and what it shows, and rewraps this buffer besides — so the page's
   * rows are neither this buffer's rows nor countable against the skip it
   * quoted. The pager orphans pages on a reflow it hears about; this is the
   * window where one lands between the answer and the barrier.
   */
  it("refuses a page whose grid changed under it", async () => {
    const renderer = new XtermRenderer({ paneId: "%17" });
    renderer.open(document.createElement("div"));
    renderer.setGrid({ columns: 20, rows: 5 });
    const asked = anchoredAt(renderer, 0);

    renderer.setGrid({ columns: 40, rows: 5 });
    const history = ownTerminalBytes(new TextEncoder().encode("earlier output"));
    await expect(renderer.prependHistory(history, asked)).resolves.toBe("superseded");
    expect(recordIncident).toHaveBeenCalledWith(
      "pane.historySuperseded",
      expect.objectContaining({ paneId: "%17", reason: "gridChanged" }),
    );

    renderer.dispose();
  });

  /**
   * The whole page turned out to be rows this buffer already held — a pane that
   * printed at least a page's worth while the page crossed the link. There is
   * nothing left to splice, so nothing is latched: the next reach-the-top asks
   * from where this buffer begins now, which is above everything that page held.
   */
  /**
   * The serialize addon trims the blank rows under a short screen and restores
   * the cursor relatively. Spliced behind a page, that shortfall let the page's
   * last rows into the viewport and moved the whole screen down by the same
   * count — the ghost rows a TUI then painted around until its next full redraw.
   */
  it("keeps a short screen at its height and cursor when a page goes above it", async () => {
    const renderer = new XtermRenderer({ paneId: "%15" });
    renderer.open(document.createElement("div"));
    renderer.setGrid({ columns: 20, rows: 8 });
    // Three rows on an eight-row screen, bracketed paste on as any shell leaves
    // it, and the cursor parked on the first row as a TUI parks it in its
    // input line: the serialization then ends in relative cursor moves *and* a
    // mode string, which is what a real pane's does.
    renderer.write(
      ownTerminalBytes(new TextEncoder().encode("one\r\ntwo\r\nthree\u001b[?2004h\u001b[1;1H")),
      undefined,
      1,
    );
    await waitFor(() => renderer.serialize().includes("three"), "the rows to land");
    const before = renderer.screenText();
    expect(before.cursor).toEqual([0, 0]);

    const page = ["h-1", "h-2", "h-3", "h-4"].join("\r\n");
    await expect(
      renderer.prependHistory(ownTerminalBytes(new TextEncoder().encode(page)), anchoredAt(renderer, 0)),
    ).resolves.toBe("applied");
    await waitFor(() => renderer.serialize().includes("h-1"), "the page to be spliced");

    const after = renderer.screenText();
    expect(after.rows).toEqual(before.rows);
    expect(after.cursor).toEqual(before.cursor);
    expect(renderer.scrollbackRows).toBe(4);
    expect(renderer.serialize().endsWith("\u001b[?2004h")).toBe(true);
    renderer.dispose();
  });

  /**
   * A full-width coloured input area followed by a short default-background
   * status line is the shape of Codex's bottom frame. The serialize addon may
   * omit default cells between its positioned segments and after its text;
   * once history is written before the snapshot, BCE otherwise creates those
   * cells with the input area's background and leaves coloured bands behind.
   */
  it("keeps default background gaps when history is put above a coloured screen", async () => {
    const renderer = new XtermRenderer({ paneId: "%18" });
    renderer.open(document.createElement("div"));
    renderer.setGrid({ columns: 20, rows: 5 });
    renderer.write(
      ownTerminalBytes(new TextEncoder().encode(
        "\u001b[3;1H\u001b[48;2;65;69;76m\u001b[2K"
        + "\u001b[4;1H\u001b[2K"
        + "\u001b[0m\u001b[5;1Hstatus\u001b[5;13Hbranch"
        + "\u001b[3;6H\u001b[48;2;65;69;76m",
      )),
      undefined,
      1,
    );
    await waitFor(() => renderer.serialize().includes("status"), "the coloured screen to land");
    const before = renderer.screenBackgrounds();
    const beforeText = renderer.screenText();

    await expect(
      renderer.prependHistory(
        ownTerminalBytes(new TextEncoder().encode("earlier output")),
        anchoredAt(renderer, 0),
      ),
    ).resolves.toBe("applied");
    await waitFor(() => renderer.serialize().includes("earlier output"), "the history to be spliced");

    expect(renderer.screenBackgrounds()).toEqual(before);
    expect(renderer.screenText()).toEqual(beforeText);

    // The correction uses the default pen, then gives the live grey pen back:
    // the next erase from the program must still paint with that live colour.
    renderer.write(ownTerminalBytes(new TextEncoder().encode("\u001b[1;1H\u001b[1X")), undefined, 2);
    await waitFor(
      () => renderer.screenBackgrounds()[0]?.[0] === before[2]?.[0],
      "the live pen to paint after the splice",
    );
    renderer.dispose();
  });

  it("keeps a live pen that matches the final content through the background correction", async () => {
    const renderer = new XtermRenderer({ paneId: "%19" });
    renderer.open(document.createElement("div"));
    renderer.setGrid({ columns: 20, rows: 5 });
    renderer.write(
      ownTerminalBytes(new TextEncoder().encode("\u001b[5;1H\u001b[31mstatus")),
      undefined,
      1,
    );
    await waitFor(() => renderer.serialize().includes("status"), "the red status to land");

    await expect(
      renderer.prependHistory(
        ownTerminalBytes(new TextEncoder().encode("earlier output")),
        anchoredAt(renderer, 0),
      ),
    ).resolves.toBe("applied");
    renderer.write(ownTerminalBytes(new TextEncoder().encode("X")), undefined, 2);
    await waitFor(() => renderer.serialize().includes("statusX"), "output with the restored pen to land");

    expect(renderer.serialize()).toContain("\u001b[31mstatusX");
    renderer.dispose();
  });

  it("refuses a page a busy pane printed straight past", async () => {
    const renderer = new XtermRenderer({ paneId: "%15" });
    renderer.open(document.createElement("div"));
    renderer.setGrid({ columns: 20, rows: 5 });
    renderer.write(
      ownTerminalBytes(new TextEncoder().encode(Array.from({ length: 12 }, (_, i) => `row-${i}`).join("\r\n"))),
      undefined,
      1,
    );
    await waitFor(() => renderer.scrollbackRows === 7, "the rows to land");

    // Asked for when this buffer held nothing above its screen, answered when it
    // holds seven — more rows than the two the page carries.
    const history = ownTerminalBytes(new TextEncoder().encode("a\r\nb"));
    // Answered in its own words, not as one more refusal: the caller's remedy
    // is a larger page next time, not the same question again.
    await expect(renderer.prependHistory(history, anchoredAt(renderer, 0)))
      .resolves.toBe("overlapExceedsPage");
    expect(recordIncident).toHaveBeenCalledWith(
      "pane.historySuperseded",
      expect.objectContaining({ paneId: "%15", reason: "overlapExceedsPage" }),
    );

    renderer.dispose();
  });

  /**
   * The other half of the splice: when nothing moved, the scrollback really
   * does end up above the screen, and the screen is still there under it.
   *
   * Polled rather than awaited on a promise, because the splice deliberately
   * waits for xterm to finish with everything it was already given — that
   * barrier is what keeps the rewrite from dropping bytes still inside the
   * parser, and it lands on a frame or on the scheduler's fallback timer.
   */
  it("puts earlier output above the screen when the stream held still", async () => {
    const renderer = new XtermRenderer({ paneId: "%10" });
    renderer.open(document.createElement("div"));
    renderer.write(ownTerminalBytes(new TextEncoder().encode("the screen")), undefined, 5);
    await waitFor(() => renderer.serialize().includes("the screen"), "the screen to be applied");

    const history = ownTerminalBytes(new TextEncoder().encode("earlier output"));
    await expect(renderer.prependHistory(history, anchoredAt(renderer, 0))).resolves.toBe("applied");

    await waitFor(() => renderer.serialize().includes("earlier output"), "the history to be spliced");
    const spliced = renderer.serialize();
    // Both halves, in the order the user reads them.
    expect(spliced.indexOf("earlier output")).toBeLessThan(spliced.indexOf("the screen"));

    renderer.dispose();
  });

  /**
   * What the pane tells the host it is already holding.
   *
   * tmux photographs the scrollback relative to the pane's current display, so
   * a pane that has printed since it was seeded has rows above that display
   * which are already in this buffer. The count is how the request says where
   * to start, and counting it wrong is a splice that shows those rows twice.
   */
  it("counts the rows above its screen, and only those", async () => {
    const renderer = new XtermRenderer({ paneId: "%12" });
    renderer.open(document.createElement("div"));
    renderer.setGrid({ columns: 20, rows: 5 });
    expect(renderer.scrollbackRows).toBe(0);

    const printed = Array.from({ length: 12 }, (_, index) => `line-${index + 1}`).join("\r\n");
    renderer.write(ownTerminalBytes(new TextEncoder().encode(printed)), undefined, 1);
    await waitFor(() => renderer.scrollbackRows > 0, "the screen to scroll");

    // Twelve rows into a five-row grid: seven of them are above it now.
    expect(renderer.scrollbackRows).toBe(7);

    renderer.dispose();
  });

  /**
   * The pane that never stops printing — a Codex agent working — and the reason
   * this stopped being a refusal.
   *
   * Output that lands while the page is on the wire is queued *behind* the
   * barrier, so it is neither in the serialization the rewrite is built from nor
   * in the buffer the rewrite replaces. Refusing on it made every page a
   * `pane.historySuperseded` — twenty-seven in one minute on one pane — and the
   * user could never scroll up. The writes are kept instead and re-queued behind
   * the rewrite, which is where they would have run anyway.
   */
  it("splices onto a pane that never stopped printing, keeping the writes behind the barrier", async () => {
    const renderer = new XtermRenderer({ paneId: "%11" });
    renderer.open(document.createElement("div"));
    renderer.setGrid({ columns: 20, rows: 5 });
    // Not awaited: this write is still in flight, so the barrier queues behind
    // it rather than being delivered inline.
    renderer.write(ownTerminalBytes(new TextEncoder().encode("live-1\r\nlive-2")), undefined, 5);

    const outcome = renderer.prependHistory(
      ownTerminalBytes(new TextEncoder().encode("older-1\r\nolder-2")),
      anchoredAt(renderer, 0),
    );
    // And the pane keeps printing while the splice waits for its barrier.
    renderer.write(ownTerminalBytes(new TextEncoder().encode("\r\nlive-3")), undefined, 6);
    renderer.write(ownTerminalBytes(new TextEncoder().encode("\r\nlive-4")), undefined, 7);

    await expect(outcome).resolves.toBe("applied");
    await waitFor(
      () => renderer.serialize().includes("live-4"),
      "the output queued behind the barrier to be applied",
    );

    const spliced = renderer.serialize();
    const rows = ["older-1", "older-2", "live-1", "live-2", "live-3", "live-4"];
    // Nothing duplicated by the rewrite and nothing dropped with the queue…
    for (const row of rows) expect(spliced.split(row), row).toHaveLength(2);
    // …and every row where the user reads it, the history above the screen.
    const positions = rows.map((row) => spliced.indexOf(row));
    expect(positions).toEqual([...positions].sort((left, right) => left - right));

    renderer.dispose();
  });

  it("keeps the overflow itself loud, and the restore it refuses afterwards quiet", () => {
    const diagnostics: Array<string | undefined> = [];
    const renderer = new XtermRenderer({ paneId: "%8", onDiagnostic: (message) => diagnostics.push(message) });
    renderer.open(document.createElement("div"));
    // Past the scheduler's record bound in one synchronous burst, which no
    // frame can interrupt: output really did outrun the renderer.
    for (let record = 0; record < 4_200; record += 1) {
      renderer.write(ownTerminalBytes(new TextEncoder().encode("x")), undefined, 2);
    }
    // The overflow is a genuine loss of bytes and still speaks for itself.
    expect(diagnostics.some((message) => message?.includes("exceeded its bound"))).toBe(true);
    const spoken = diagnostics.length;

    expect(renderer.restore("cached", undefined, 9, 9)).toBe(false);
    expect(recordIncident).toHaveBeenCalledWith("pane.overflowRestore", { paneId: "%8" });
    expect(diagnostics).toHaveLength(spoken);

    renderer.dispose();
  });
});
