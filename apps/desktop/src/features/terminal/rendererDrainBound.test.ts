// @vitest-environment jsdom
// A pane hides by draining its renderer and serializing what xterm applied, and
// the reveal that follows waits on that drain. The drain was unbounded and
// memoized, so a single xterm write completion that never fires removed the pane
// from the visibility protocol permanently — the frozen-pane failure this bound
// exists for.
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { XtermRenderer } from "./TerminalRenderer";
import { ownTerminalBytes } from "./TerminalBytes";

const frames: FrameRequestCallback[] = [];
const realRequestFrame = window.requestAnimationFrame;
const realCancelFrame = window.cancelAnimationFrame;

/**
 * Takes the write scheduler's frames away from the clock and gives them to the
 * test. Without this the test races xterm's parse against a real frame, and the
 * scheduler's idle fast path can carry the write this test needs to strand.
 */
function holdFrames(): void {
  window.requestAnimationFrame = ((callback: FrameRequestCallback) => frames.push(callback)) as typeof window.requestAnimationFrame;
  window.cancelAnimationFrame = (() => undefined) as typeof window.cancelAnimationFrame;
}

function releaseFrames(): void {
  for (const frame of frames.splice(0)) frame(0);
}

async function waitFor(condition: () => boolean, description: string): Promise<void> {
  for (let attempt = 0; attempt < 1_000; attempt += 1) {
    if (condition()) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  throw new Error(`timed out waiting for ${description}`);
}

describe("XtermRenderer drain bound", () => {
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

  afterEach(() => {
    frames.length = 0;
    window.requestAnimationFrame = realRequestFrame;
    window.cancelAnimationFrame = realCancelFrame;
  });

  it("answers a drain xterm never completes, and freezes the watermark it reported", async () => {
    const renderer = new XtermRenderer({ drainTimeoutMs: 50 });
    renderer.open(document.createElement("div"));
    holdFrames();

    let firstApplied = false;
    let strandedApplied = false;
    // The first write takes the scheduler's idle fast path straight into xterm.
    renderer.write(ownTerminalBytes(new TextEncoder().encode("applied")), () => { firstApplied = true; }, 5);
    // The second lands while the first is in flight, so it waits for a frame —
    // and every frame from here belongs to the test.
    renderer.write(ownTerminalBytes(new TextEncoder().encode("stranded")), () => { strandedApplied = true; }, 6);
    await waitFor(() => firstApplied, "xterm to apply the first write");
    expect(strandedApplied).toBe(false);

    // Nothing will ever move the second write to xterm, so the drain that the
    // next reveal of this pane waits behind can only be answered by the bound.
    const drained = await renderer.drainAndSerialize();
    // What it reports is what xterm actually applied: the checkpoint built from
    // this claims generation 5, and 6 stays the host's to resend.
    expect(drained.outputGeneration).toBe(5);
    expect(drained.serialized).toContain("applied");
    expect(drained.serialized).not.toContain("stranded");

    // The checkpoint has already been published by the time a late completion
    // arrives, so it may not be allowed to advance the watermark behind it.
    releaseFrames();
    await waitFor(() => renderer.serialize().includes("stranded"), "the stranded write to reach xterm");
    // The bytes really did land; it is only the report of them that is
    // suppressed, which is what keeps the published checkpoint honest.
    expect(strandedApplied).toBe(false);
    expect((await renderer.drainAndSerialize()).outputGeneration).toBe(5);

    renderer.dispose();
  });

  it("resolves from the drain itself, and once, when xterm does complete", async () => {
    // Long enough that only the real drain can answer this one.
    const renderer = new XtermRenderer({ drainTimeoutMs: 30_000 });
    renderer.open(document.createElement("div"));
    renderer.write(ownTerminalBytes(new TextEncoder().encode("done")), undefined, 4);

    const drained = await renderer.drainAndSerialize();
    expect(drained.outputGeneration).toBe(4);
    expect(drained.serialized).toContain("done");
    // Memoized: a second hide of the same instance gets the same answer rather
    // than sealing an already-sealed scheduler again.
    expect(await renderer.drainAndSerialize()).toBe(drained);
    renderer.dispose();
  });

  it("says so when it could not queue a write, so the caller's callback is not simply lost", async () => {
    const renderer = new XtermRenderer({ drainTimeoutMs: 30_000 });
    renderer.open(document.createElement("div"));
    expect(renderer.write(ownTerminalBytes(new TextEncoder().encode("queued")), undefined, 1)).toBe(true);

    // Sealing for the hide drain is the ordinary way a scheduler stops
    // accepting. Anything written after it is dropped along with its `onRendered`
    // — including the empty barrier the handshake hangs its acknowledgement and
    // its reveal on, which is why the answer has to reach the caller.
    await renderer.drainAndSerialize();
    let rendered = false;
    const accepted = renderer.write(
      ownTerminalBytes(new TextEncoder().encode("refused")),
      () => { rendered = true; },
      2,
    );

    expect(accepted).toBe(false);
    expect(rendered).toBe(false);
    // The empty-record barrier is refused on exactly the same terms.
    expect(renderer.write(ownTerminalBytes(new Uint8Array()), undefined, 3)).toBe(false);
    renderer.dispose();
  });
});
