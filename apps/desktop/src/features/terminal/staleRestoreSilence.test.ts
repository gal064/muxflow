// @vitest-environment jsdom
// A reconnect restores every cached pane, and any pane whose live stream has
// already overtaken its cached screen refuses that restore and asks the host
// for a seed. That is the recovery working — but it was reaching the user as a
// notice reading "A restore through generation 4 arrived for a pane that has
// already been given generation 5…", which is an internal sentence about an
// event nobody has to act on. It belongs in the journal.
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { recordIncident } from "../../diagnostics/incidents";
import { XtermRenderer } from "./TerminalRenderer";
import { ownTerminalBytes, type OwnedTerminalBytes } from "./TerminalBytes";

vi.mock("../../diagnostics/incidents", () => ({ recordIncident: vi.fn() }));

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

  // Loading earlier output is a re-seed in disguise: xterm has no prepend, so
  // the only way to put scrollback above row 0 is to rewrite the buffer with
  // the history and the current screen together. If live output landed while
  // the history was in flight, that rewrite would drop it — so the splice is
  // refused on the same rule a stale restore is, and like it, journals instead
  // of speaking. Nothing is lost: the scrollback is still in tmux, and the next
  // time the user reaches the top the question is asked again.
  it("refuses a history splice when the stream moved under it", () => {
    type HistorySplice = {
      prependHistory(history: OwnedTerminalBytes, throughGeneration: number): "applied" | "superseded";
    };
    const diagnostics: Array<string | undefined> = [];
    const renderer = new XtermRenderer({
      paneId: "%9",
      onDiagnostic: (message) => diagnostics.push(message),
    });
    renderer.open(document.createElement("div"));
    renderer.write(ownTerminalBytes(new TextEncoder().encode("live")), undefined, 5);
    diagnostics.length = 0;

    const splice = renderer as unknown as HistorySplice;
    const history = ownTerminalBytes(new TextEncoder().encode("earlier output"));
    expect(splice.prependHistory(history, 4)).toBe("superseded");

    expect(recordIncident).toHaveBeenCalledWith("pane.historySuperseded", {
      paneId: "%9",
      throughGeneration: 4,
      lastEnqueuedGeneration: 5,
    });
    expect(diagnostics).toEqual([]);

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

    const splice = renderer as unknown as {
      prependHistory(history: OwnedTerminalBytes, throughGeneration: number): "applied" | "superseded";
    };
    const history = ownTerminalBytes(new TextEncoder().encode("earlier output"));
    expect(splice.prependHistory(history, 5)).toBe("applied");

    await waitFor(() => renderer.serialize().includes("earlier output"), "the history to be spliced");
    const spliced = renderer.serialize();
    // Both halves, in the order the user reads them.
    expect(spliced.indexOf("earlier output")).toBeLessThan(spliced.indexOf("the screen"));

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
