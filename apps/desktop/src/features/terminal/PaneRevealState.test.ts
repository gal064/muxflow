import { describe, expect, it } from "vitest";
import { outputAfterRecovery, reducePaneReveal, type PaneRevealState } from "./PaneRevealState";
import { copyTerminalBytes } from "./TerminalBytes";

const hiddenResource = (overrides = {}) => ({
  kind: "paneResource" as const,
  paneId: "%1",
  state: "hiddenBuffered" as const,
  requiresSeed: false,
  resumeFromRenderer: false,
  recoveryReason: "",
  generation: 1,
  snapshotGeneration: 0,
  tailThroughGeneration: 1,
  rawTail: copyTerminalBytes(Uint8Array.of(2)),
  sequence: 1,
  ...overrides,
});

/**
 * A reveal the host answered from its record of the handoff: no screen at all,
 * and here an idle pane's empty tail.
 */
const resumeAnswer = () => hiddenResource({
  state: "visible",
  generation: 21,
  snapshotGeneration: 20,
  tailThroughGeneration: 20,
  rawTail: copyTerminalBytes(new Uint8Array()),
  resumeFromRenderer: true,
});

describe("mounted pane reveal ordering", () => {
  it("defers output until the visibility response supplies PaneResource recovery", () => {
    let state: PaneRevealState = { ready: false, hasLocalState: true };
    const early = reducePaneReveal(state, { kind: "output", paneId: "%1", generation: 1, data: copyTerminalBytes(Uint8Array.of(1)), sequence: 1 });
    expect(early.effect.kind).toBe("deferOutput");
    state = early.state;
    const recovery = reducePaneReveal(state, resumeAnswer());
    expect(recovery.effect).toMatchObject({ kind: "resume" });
    expect(recovery.state.ready).toBe(true);
  });

  it("drops output already represented by the raw tail and keeps only later generations", () => {
    const tail = copyTerminalBytes(Uint8Array.from([66, 67, 68]));
    const recovery = reducePaneReveal(
      { ready: false, hasLocalState: true },
      hiddenResource({
        state: "visible",
        resumeFromRenderer: true,
        generation: 24,
        snapshotGeneration: 20,
        tailThroughGeneration: 23,
        rawTail: tail,
      }),
    );
    expect(recovery.effect).toMatchObject({
      kind: "resume", snapshotGeneration: 20, tailThroughGeneration: 23, rawTail: tail,
    });
    const deferred = [
      { generation: 21, data: Uint8Array.of(66) },
      { generation: 22, data: Uint8Array.of(67) },
      { generation: 23, data: Uint8Array.of(68) },
      { generation: 25, data: Uint8Array.of(69) },
    ];
    const combined = Uint8Array.from([
      ...tail,
      ...outputAfterRecovery(deferred, 23).flatMap((item) => Array.from(item.data)),
    ]);
    expect(new TextDecoder().decode(combined)).toBe("BCDE");
  });

  it("waits for the host-triggered reseed after a released resource", () => {
    const result = reducePaneReveal({ ready: false, hasLocalState: true }, hiddenResource({
      state: "released",
      requiresSeed: true,
      recoveryReason: "host LRU eviction",
      rawTail: copyTerminalBytes(new Uint8Array()),
    }));
    expect(result).toEqual({
      state: { ready: false, hasLocalState: false },
      effect: { kind: "awaitSeed", reason: "host LRU eviction", requestSeed: false },
    });
  });

  // The host stops keeping a copy of the renderer's screen, so the ordinary
  // answer to a reveal carries no bytes at all: "the screen you are holding is
  // the one I verified, and nothing has printed since". Every place that reads
  // readiness off a byte count turns that answer into a permanently blank pane,
  // which is why the flag — not the length — is the signal.
  //
  it("makes the pane ready on a zero-byte resume answer", () => {
    const result = reducePaneReveal({ ready: false, hasLocalState: true }, resumeAnswer());
    expect(result.state).toEqual({ ready: true, hasLocalState: true });
    const effect: { kind: string; snapshotGeneration?: number; tailThroughGeneration?: number } =
      result.effect;
    expect(effect.kind).toBe("resume");
    expect(effect.snapshotGeneration).toBe(20);
    expect(effect.tailThroughGeneration).toBe(20);
  });

  // The mirror image, and the reason the flag alone is not enough: the host
  // verified its own record of the handoff, not this renderer's cache. A pane
  // with nothing to resume from has to say so and be seeded.
  it("asks for a seed when a resume answer reaches a pane holding no screen", () => {
    const result = reducePaneReveal({ ready: false, hasLocalState: false }, resumeAnswer());
    expect(result.state).toEqual({ ready: false, hasLocalState: false });
    expect(result.effect).toMatchObject({ kind: "awaitSeed", requestSeed: true });
  });

  // Every hide is acknowledged with an empty `hiddenBuffered` answer, which the
  // hub buffers and replays into the pane's next mount. `reveal` stamps
  // `Visible` on every path it can return by, so this shape is only ever this
  // side's own hide coming back and is never the authority on a mounted pane.
  // The pane reading it is waiting for the answer to the reveal it has already
  // sent; treating the echo as a dead end spends one capture per switch and
  // blanks a pane that had a screen.
  const hideEcho = () => hiddenResource({
    rawTail: copyTerminalBytes(new Uint8Array()),
  });

  it("ignores the hide echo replayed into a pane that is showing its cached screen", () => {
    const state: PaneRevealState = { ready: false, hasLocalState: true };
    const echo = reducePaneReveal(state, hideEcho());
    expect(echo.effect).toEqual({ kind: "awaitAnswer", showingScreen: true });
    expect(echo.state).toEqual(state);
  });

  // The case that reached the journal as `pane.revealDeadEnd` on a busy pane
  // during ordinary Wi-Fi use: the same echo, at a mount whose cached screen was
  // missing — the biggest pane in the session is the one whose serialization the
  // cache drops. It used to blank the terminal and buy a second seed. It is
  // ignored now, and what is owed instead is a bound on the wait, because this
  // pane really is showing nothing until the reveal is answered.
  it("bounds the wait instead of blanking a pane that has no screen to show", () => {
    const state: PaneRevealState = { ready: false, hasLocalState: false };
    const echo = reducePaneReveal(state, hideEcho());
    expect(echo.effect).toEqual({ kind: "awaitAnswer", showingScreen: false });
    expect(echo.state).toEqual(state);
  });

  // The destructive variant, and the reason the rule is not narrowed to a pane
  // that is still waiting: a hide processed after the reveal it raced arrives at
  // a pane that is already drawing the right screen, and acting on it wipes
  // that screen. Ignored too — but recorded, because unlike the others this one
  // means a hide and a reveal crossed.
  it("ignores the hide echo at a pane that is already drawing, and says so", () => {
    const state: PaneRevealState = { ready: true, hasLocalState: true };
    const echo = reducePaneReveal(state, hideEcho());
    expect(echo.effect).toEqual({ kind: "hideEchoAfterReady" });
    expect(echo.state).toEqual(state);
  });

  it("treats a seed diagnostic as nonfatal and leaves reveal readiness unchanged", () => {
    const state = { ready: true, hasLocalState: true };
    expect(reducePaneReveal(state, { kind: "seedDiagnostic", paneId: "%1", message: "partial metadata", sequence: 1 })).toEqual({
      state,
      effect: { kind: "diagnostic", message: "partial metadata" },
    });
  });
});
