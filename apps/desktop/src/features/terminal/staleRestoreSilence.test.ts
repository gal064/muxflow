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
import { ownTerminalBytes } from "./TerminalBytes";

vi.mock("../../diagnostics/incidents", () => ({ recordIncident: vi.fn() }));

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
