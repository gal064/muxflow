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
  serializedSnapshot: copyTerminalBytes(new TextEncoder().encode("screen")),
  rawTail: copyTerminalBytes(Uint8Array.of(2)),
  sequence: 1,
  ...overrides,
});

/**
 * A reveal the host answered from its record of the handoff rather than from a
 * screen it stored: no snapshot, and here an idle pane's empty tail.
 */
const resumeAnswer = () => hiddenResource({
  state: "visible",
  generation: 21,
  snapshotGeneration: 20,
  tailThroughGeneration: 20,
  serializedSnapshot: copyTerminalBytes(new Uint8Array()),
  rawTail: copyTerminalBytes(new Uint8Array()),
  resumeFromRenderer: true,
});

describe("mounted pane reveal ordering", () => {
  it("defers output until the visibility response supplies PaneResource recovery", () => {
    let state: PaneRevealState = { ready: false, hasLocalState: false };
    const early = reducePaneReveal(state, { kind: "output", paneId: "%1", generation: 1, data: copyTerminalBytes(Uint8Array.of(1)), sequence: 1 });
    expect(early.effect.kind).toBe("deferOutput");
    state = early.state;
    const recovery = reducePaneReveal(state, hiddenResource());
    expect(recovery.effect).toMatchObject({ kind: "restore", serialized: "screen" });
    expect(recovery.state.ready).toBe(true);
  });

  it("restores recovery bytes from the host's visible PaneResource response", () => {
    const recovery = reducePaneReveal(
      { ready: false, hasLocalState: false },
      hiddenResource({ state: "visible" }),
    );
    expect(recovery.effect).toMatchObject({ kind: "restore", serialized: "screen" });
    expect(recovery.state.ready).toBe(true);
  });

  it("drops output already represented by the raw tail and keeps only later generations", () => {
    const tail = copyTerminalBytes(Uint8Array.from([66, 67, 68]));
    const recovery = reducePaneReveal(
      { ready: false, hasLocalState: false },
      hiddenResource({
        state: "visible",
        generation: 24,
        snapshotGeneration: 20,
        tailThroughGeneration: 23,
        serializedSnapshot: copyTerminalBytes(new TextEncoder().encode("A")),
        rawTail: tail,
      }),
    );
    expect(recovery.effect).toMatchObject({
      kind: "restore", snapshotGeneration: 20, tailThroughGeneration: 23, rawTail: tail,
    });
    const deferred = [
      { generation: 21, data: Uint8Array.of(66) },
      { generation: 22, data: Uint8Array.of(67) },
      { generation: 23, data: Uint8Array.of(68) },
      { generation: 25, data: Uint8Array.of(69) },
    ];
    const combined = Uint8Array.from([
      ...new TextEncoder().encode("A"),
      ...tail,
      ...outputAfterRecovery(deferred, 23).flatMap((item) => Array.from(item.data)),
    ]);
    expect(new TextDecoder().decode(combined)).toBe("ABCDE");
  });

  it("waits for the host-triggered reseed after a released resource", () => {
    const result = reducePaneReveal({ ready: false, hasLocalState: true }, hiddenResource({
      state: "released",
      requiresSeed: true,
      recoveryReason: "host LRU eviction",
      serializedSnapshot: copyTerminalBytes(new Uint8Array()),
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
  // hub buffers and replays into the pane's next mount. The pane reading it is
  // showing its own cached screen and waiting for the answer to the reveal it
  // has already sent; treating the echo as a dead end spends one capture per
  // switch on a pane that is not blank.
  it("ignores the hide echo replayed into a pane that is showing its cached screen", () => {
    const state: PaneRevealState = { ready: false, hasLocalState: true };
    const echo = reducePaneReveal(state, hiddenResource({
      serializedSnapshot: copyTerminalBytes(new Uint8Array()),
      rawTail: copyTerminalBytes(new Uint8Array()),
    }));
    expect(echo.effect).toEqual({ kind: "none" });
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
