import { describe, expect, it, vi } from "vitest";
import {
  HISTORY_MAX_AWAITING,
  HISTORY_PAGE_LINES,
  PaneHistoryPager,
  type PagerRenderer,
} from "./PaneHistoryPager";
import { ownTerminalBytes } from "./TerminalBytes";
import type { TerminalEvent } from "./api";

/**
 * The four renderer members the pager reads, and nothing else — no xterm, no
 * DOM, no React. What the pager decides is arithmetic over the rows a terminal
 * reports holding, and this is the whole of what it needs to do it.
 */
class FakeRenderer implements PagerRenderer {
  scrollbackRows = 0;
  scrollbackLimit = 10_000;
  alternateScreen = false;
  readonly splices: Array<{ bytes: number }> = [];
  #outcome: "applied" | "superseded" = "applied";
  #pending: Array<() => void> = [];

  isAlternateScreenActive(): boolean {
    return this.alternateScreen;
  }

  prependHistory(history: Uint8Array): Promise<"applied" | "superseded"> {
    this.splices.push({ bytes: history.byteLength });
    const outcome = this.#outcome;
    return new Promise((resolve) => this.#pending.push(() => resolve(outcome)));
  }

  refuseSplices(): void {
    this.#outcome = "superseded";
  }

  /** Settles every splice this renderer was handed, as xterm eventually does. */
  async settleSplices(): Promise<void> {
    const pending = this.#pending;
    this.#pending = [];
    pending.forEach((resolve) => resolve());
    await Promise.resolve();
  }
}

function historyEvent(text: string, historySize?: number): Extract<TerminalEvent, { kind: "terminalHistory" }> {
  return {
    kind: "terminalHistory",
    paneId: "%1",
    sequence: 2,
    data: ownTerminalBytes(new TextEncoder().encode(text)),
    ...(historySize === undefined ? {} : { historySize }),
  };
}

/** One line per captured row, joined the way the host joins them. */
function historyPage(rows: number): string {
  return Array.from({ length: rows }, (_, index) => `row-${index}`).join("\r\n");
}

function pagerFor(renderer: FakeRenderer) {
  const request = vi.fn().mockResolvedValue(undefined);
  const journal = vi.fn();
  const pager = new PaneHistoryPager({
    paneId: "%1",
    renderer,
    clientId: () => "client-a",
    journal,
    request,
  });
  return { pager, request, journal };
}

/**
 * Walks the pager up one page: the answer to what is outstanding, the splice
 * settling, and the rows it added arriving in the renderer's own count.
 */
async function deliverPage(
  pager: PaneHistoryPager,
  renderer: FakeRenderer,
  rows: number,
  historySize?: number,
): Promise<void> {
  pager.receive(historyEvent(historyPage(10), historySize));
  await renderer.settleSplices();
  renderer.scrollbackRows += rows;
}

describe("PaneHistoryPager", () => {
  /**
   * tmux's `history-limit` can be larger than anything this side can hold, and
   * then `skip + page >= history_size` is never true: the host clamps the skip
   * it is given at `MAX_HISTORY_SKIP_LINES`, xterm drops rows off the top of
   * the buffer as new ones are spliced in, and the answer stops moving. Every
   * wheel-up used to re-fetch the same clamped rows and splice them in again.
   */
  it("stops at what it can hold, when tmux holds more history than this side ever can", async () => {
    const renderer = new FakeRenderer();
    const { pager, request } = pagerFor(renderer);
    pager.noteScreenSeeded();
    pager.requestPage("prefetch");
    expect(request).toHaveBeenCalledTimes(1);

    // 50,000 lines in tmux, and the buffer now as full as xterm will let it be.
    await deliverPage(pager, renderer, 10_000, 50_000);

    pager.requestPage("scrolledToTop");
    pager.requestPage("scrolledToTop");
    expect(request, "asked again for rows it cannot hold and the host would clamp").toHaveBeenCalledTimes(1);

    // The same end from the other ceiling: even a renderer that could hold more
    // stops here, because the host will not start a capture further up than its
    // own `MAX_HISTORY_SKIP_LINES` and would answer with these rows again.
    renderer.scrollbackLimit = 50_000;
    pager.requestPage("scrolledToTop");
    expect(request).toHaveBeenCalledTimes(1);

    // Latched, not merely refused once: the screen carries it across a hide.
    expect(pager.snapshot().historyExhausted).toBe(true);
  });

  /**
   * Each page is applied by rewriting the whole buffer — xterm has no prepend —
   * so a fixed page size makes N pages cost O(N^2) bytes through xterm, and
   * past a couple of hundred kilobytes the reset and the content land in
   * different frames, which the user reads as a flicker. The page doubles as
   * the buffer grows, so the whole 10,000-row scrollback is six rewrites.
   */
  it("doubles the page as the buffer grows, capping it, so a full scrollback is six rewrites", async () => {
    const renderer = new FakeRenderer();
    const { pager, request } = pagerFor(renderer);
    pager.noteScreenSeeded();
    pager.requestPage("prefetch");

    // Never the last page: tmux is holding far more than this walk fetches, so
    // nothing but the ladder decides the sizes below.
    for (const rows of [300, 600, 1_200, 2_400, 4_800]) {
      await deliverPage(pager, renderer, rows, 50_000);
      pager.requestPage("scrolledToTop");
    }

    expect(request.mock.calls).toEqual([
      ["client-a", "%1", 300, 0],
      ["client-a", "%1", 600, 300],
      ["client-a", "%1", 1_200, 900],
      ["client-a", "%1", 2_400, 2_100],
      // Capped: the largest single answer stays well under the whole-history
      // capture this replaced, and the host clamps at `MAX_HISTORY_LINES` too.
      ["client-a", "%1", 4_800, 4_500],
      ["client-a", "%1", 4_800, 9_300],
    ]);
    // Six pages, and the sixth reaches past the 10,000 rows this side can hold.
    expect(9_300 + 4_800).toBeGreaterThanOrEqual(10_000);
  });

  /**
   * A refusal is not an answer.
   *
   * The splice can still be refused at its barrier — the scrollback filled up
   * while the page crossed the link — and the pager has to be able to ask again
   * rather than latch "this pane is holding its scrollback" on a page that
   * never landed.
   */
  it("reopens its latch when the splice is refused, so reaching the top asks again", async () => {
    const renderer = new FakeRenderer();
    const { pager, request } = pagerFor(renderer);
    pager.noteScreenSeeded();
    pager.requestPage("prefetch");
    expect(request).toHaveBeenCalledTimes(1);

    renderer.refuseSplices();
    await deliverPage(pager, renderer, 0, 50_000);

    // Nothing was spliced, so nothing above this screen was accounted for: the
    // walk has not ended and the next page is still the first size.
    expect(pager.snapshot().historyExhausted).toBe(false);
    expect(pager.snapshot().historyNextPageLines).toBe(HISTORY_PAGE_LINES);

    pager.requestPage("scrolledToTop");
    expect(request).toHaveBeenCalledTimes(2);
  });

  /**
   * A request the host never answers — a link that dropped under it — would
   * otherwise sit at the head of the expectation queue forever and misattribute
   * every answer after it. Unreachable through the component, because getting
   * nine requests outstanding needs nine reseeds landing between nine answers
   * that never come.
   */
  it("drops its oldest expectation rather than misattributing answers forever", async () => {
    const renderer = new FakeRenderer();
    const { pager, journal } = pagerFor(renderer);
    // Each seed reopens the latch without answering the page before it, which
    // is exactly the shape a dropped link leaves behind.
    for (let page = 0; page <= HISTORY_MAX_AWAITING; page += 1) {
      pager.noteScreenSeeded();
      pager.requestPage("prefetch");
    }

    expect(journal.mock.calls).toEqual([
      ["pane.historyExpectationDropped", { paneId: "%1", serial: 1 }],
    ]);

    // And the queue still places what does arrive: the head is now the second
    // request, orphaned by the seed that followed it, so its answer is dropped
    // rather than spliced above a screen it never sat above.
    pager.receive(historyEvent("earlier output", 2_000));
    await renderer.settleSplices();
    expect(renderer.splices).toEqual([]);
    expect(journal.mock.calls.at(-1)).toEqual([
      "pane.historySupersededByReseed",
      { paneId: "%1", serial: 2 },
    ]);
  });

  /**
   * The pane is unmounting: answers still on the wire are never delivered,
   * because the subscription that would carry them is gone with it, and asking
   * for one more page would put it on the link for nobody.
   */
  it("asks for nothing once the pane it pages is gone", () => {
    const renderer = new FakeRenderer();
    const { pager, request } = pagerFor(renderer);
    pager.noteScreenSeeded();
    pager.dispose();

    pager.requestPage("scrolledToTop");
    expect(request).not.toHaveBeenCalled();
  });
});
