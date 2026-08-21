// @vitest-environment jsdom
/**
 * The visibility handshake, end to end, against a host that plays by the real
 * rules.
 *
 * Every other test in this directory stubs one side of the handshake, and a
 * whole class of pane bugs lives in the seam between them: a pane that is
 * painted but never revealed, a screen the host hands back that is not the one
 * this side cached, output deferred forever by a reveal answer nothing in the
 * reducer had a rule for. None of those can be expressed against a fanout stub
 * with a hard-coded checkpoint, which is why 1000 passing tests said nothing
 * about the tab-switch regression this file exists to pin.
 *
 * So three things here are real rather than mocked:
 *
 *  - the real `TerminalEventHub`, including its backlog replay at subscribe,
 *    its generation ordering and its seed-debt latches;
 *  - a renderer that keeps xterm at arm's length but delegates every ordering
 *    decision to the production `TerminalWriteScheduler`,
 *    `TerminalGenerationWatermark` and `restoreDecision` — so empty-write
 *    barriers, ESC c replacement and refused restores behave as they ship;
 *  - `FakeHost`, a port of `PaneResourceStore` from
 *    `crates/tmux-control/src/replay.rs` plus the emission rules in
 *    `apps/host/src/service/terminal.rs`, so `snapshot_generation`,
 *    `raw_tail` and the state the desktop actually receives are decided by the
 *    host's logic instead of by the test's convenience.
 *
 * When the host's rules change, change `FakeHost` and let the failures show
 * which desktop assumption the change broke. That is the whole point of it.
 */
import { StrictMode } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Pane } from "../../app/types";

const api = vi.hoisted(() => ({
  setTerminalVisibility: vi.fn(async (..._args: unknown[]) => undefined),
  requestTerminalSeed: vi.fn(async (..._args: unknown[]) => undefined),
}));
vi.mock("./api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api")>()),
  setTerminalVisibility: api.setTerminalVisibility,
  requestTerminalSeed: api.requestTerminalSeed,
}));

const journal = vi.hoisted(() => ({ recordIncident: vi.fn((..._args: unknown[]) => undefined) }));
vi.mock("../../diagnostics/incidents", () => ({ recordIncident: journal.recordIncident }));

/** Every journal record except `link.epoch`, which any clean connect writes. */
function incidentsBesidesEpochAdoption(): unknown[][] {
  return journal.recordIncident.mock.calls.filter(([kind]) => kind !== "link.epoch");
}

interface HarnessRenderer {
  /** What xterm would be showing, with ESC c applied as a wipe. */
  screen: string;
  /** One entry per renderer call, in order, for asserting on repaints. */
  log: string[];
  /** Runs xterm completions and the frames they unblock to quiescence. */
  pump(): void;
  /** Refuses every later write, the way a sealed or overflowed scheduler does. */
  refuseWrites(): void;
}
const renderers = vi.hoisted(() => ({
  created: [] as HarnessRenderer[],
  /** Arms the next renderer to refuse writes, the way a sealed one does. */
  refuseNewWrites: false,
}));

vi.mock("./TerminalRenderer", async (importOriginal) => {
  const original = await importOriginal<typeof import("./TerminalRenderer")>();
  const { TerminalWriteScheduler } = await import("./TerminalWriteScheduler");
  const { TerminalGenerationWatermark } = await import("./TerminalGenerationWatermark");
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  type Size = { columns: number; rows: number };

  class HarnessXtermRenderer {
    screen = "";
    log: string[] = [];
    grid: Size = { columns: 80, rows: 24 };
    #frames: Array<() => void> = [];
    #xtermPending: Array<() => void> = [];
    #generations = new TerminalGenerationWatermark();
    #scheduler: InstanceType<typeof TerminalWriteScheduler>;
    #onResnapshotRequired?: (reason: string) => void | Promise<void>;
    #seedRequested = false;
    #refuseWrites = false;

    constructor(options: { onResnapshotRequired?: (reason: string) => void | Promise<void> } = {}) {
      this.#onResnapshotRequired = options.onResnapshotRequired;
      this.#scheduler = new TerminalWriteScheduler(
        (chunk, done) => {
          const text = decoder.decode(chunk);
          // xterm's RIS: everything before an ESC c is gone.
          const reset = text.lastIndexOf("c");
          this.screen = reset >= 0 ? text.slice(reset + 2) : this.screen + text;
          this.#xtermPending.push(done);
        },
        (callback) => { this.#frames.push(() => callback(0)); return this.#frames.length; },
        () => undefined,
      );
      this.#refuseWrites = renderers.refuseNewWrites;
      renderers.created.push(this as unknown as HarnessRenderer);
    }

    pump(): void {
      // Frames unblock writes and completions unblock frames, so this alternates
      // until neither has anything left rather than assuming a fixed depth.
      for (let round = 0; round < 24; round += 1) {
        const frames = this.#frames;
        this.#frames = [];
        for (const frame of frames) frame();
        const pending = this.#xtermPending;
        this.#xtermPending = [];
        for (const done of pending) done();
        if (frames.length === 0 && pending.length === 0) return;
      }
      throw new Error("terminal harness renderer did not settle");
    }

    refuseWrites(): void { this.#refuseWrites = true; }

    #requestSeed(reason: string): void {
      if (this.#seedRequested) return;
      this.#seedRequested = true;
      void Promise.resolve(this.#onResnapshotRequired?.(reason)).catch(() => { this.#seedRequested = false; });
    }

    open(): void {}
    measure(): Size | undefined { return undefined; }
    measurements(): undefined { return undefined; }
    setGrid(size: Size): { kind: "applied"; size: Size } | { kind: "unchanged" } | { kind: "rejected"; reason: string } {
      if (size.columns < 2 || size.rows < 2) return { kind: "rejected", reason: `${size.columns}x${size.rows} is unusable` };
      if (this.grid.columns === size.columns && this.grid.rows === size.rows) return { kind: "unchanged" };
      this.grid = size;
      return { kind: "applied", size };
    }
    onInput(): () => void { return () => undefined; }
    onViewportChange(): () => void { return () => undefined; }
    focus(): void {}
    blur(): void {}
    hasSelection(): boolean { return false; }
    getSelection(): string { return ""; }
    paste(): void {}
    search(): boolean { return false; }
    clearSearch(): void {}
    scrollToBottom(): void {}
    serialize(): string { return this.screen; }
    disposeGpuRenderer(): void {}
    dispose(): void { this.#scheduler.dispose(); }

    drainAndSerialize(): Promise<{ serialized: string; outputGeneration: number }> {
      const drained = this.#scheduler.sealAndDrain();
      this.pump();
      return drained.then(() => ({
        serialized: this.screen,
        outputGeneration: this.#generations.appliedGeneration,
      }));
    }

    seed(bytes: Uint8Array, onRendered?: () => void, generation = 0): void {
      this.log.push(`seed@${generation}`);
      this.#generations.resetAuthoritativeStream();
      this.#seedRequested = false;
      this.#scheduler.replace(bytes, true, this.#generations.enqueued(generation, onRendered));
    }

    restore(serialized: string, onRendered?: () => void, generation = 0, throughGeneration = generation): boolean {
      const decision = original.restoreDecision(
        throughGeneration,
        this.#generations.enqueuedGeneration,
        this.#scheduler.overflowed,
      );
      if (decision.kind === "reseed") {
        this.log.push(`restore-refused@${generation}`);
        this.#requestSeed(decision.reason);
        return false;
      }
      this.log.push(`restore(${serialized})@${generation}`);
      this.#scheduler.replace(encoder.encode(serialized), false, this.#generations.enqueued(generation, onRendered));
      return true;
    }

    write(bytes: Uint8Array, onRendered?: () => void, generation = 0): boolean {
      this.log.push(`write:${bytes.byteLength}@${generation}`);
      if (this.#refuseWrites) return false;
      const queued = this.#scheduler.enqueueOwned(
        bytes as never,
        this.#generations.enqueued(generation, onRendered),
      );
      if (!queued && this.#scheduler.overflowed) this.#requestSeed("scheduler overflow");
      return queued;
    }
  }

  return { ...original, XtermRenderer: HarnessXtermRenderer };
});

import { TerminalPane } from "./TerminalPane";
import { TerminalEventHub } from "./TerminalEventHub";
import { terminalStateCache } from "./TerminalStateCache";
import { ownTerminalBytes } from "./TerminalBytes";
import { resetPerfProbe } from "../../perf/probe";
import { REVEAL_RETRY_DELAY_MS, STALE_REVEAL_EPOCH_CODE } from "./revealRetry";
import type { TerminalEvent } from "./api";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
/** Any non-zero epoch; the host rejects a zero one. */
const EPOCH = 7;
/** `TerminalPane`'s own reveal backstop, plus room for the timers around it. */
const PAST_REVEAL_FALLBACK_MS = 400;

type PaneResourceState = "visible" | "hiddenBuffered" | "released" | "unspecified";

interface HostResource {
  state: PaneResourceState;
  serializedSnapshot: string;
  rawTail: string;
  generation: number;
  snapshotGeneration: number;
  tailThroughGeneration: number;
  requiresSeed: boolean;
  recoveryReason: string;
}

/**
 * `PaneResourceStore` (crates/tmux-control/src/replay.rs) and the emission
 * rules that wrap it (apps/host/src/service/terminal.rs).
 *
 * Only the parts the desktop can observe are modelled, and each one names the
 * Rust it mirrors. The generation counter is global and strictly increasing,
 * exactly as `self.generation.fetch_add` makes it — several desktop assumptions
 * rest on that, and a test that hands out generations by hand would not test
 * them at all.
 */
class FakeHost {
  generation = 0;
  sequence = 0;
  readonly resources = new Map<string, HostResource>();
  readonly journals = new Map<string, Array<{ generation: number; bytes: string }>>();
  readonly checkpoints = new Map<string, string>();
  /** What tmux itself holds, i.e. what a `capture-pane` would return. */
  readonly tmux = new Map<string, string>();
  /** Every event this host put on the wire, for asserting on the exchange. */
  readonly emitted: string[] = [];

  constructor(readonly hub: TerminalEventHub) {}

  #nextGeneration(): number { return (this.generation += 1); }
  #nextSequence(): number { return (this.sequence += 1); }

  #publish(event: TerminalEvent): void {
    this.hub.publish(event);
  }

  /** `PaneResourceStore::ensure`. */
  #ensure(paneId: string, visible: boolean, generation: number): HostResource {
    const existing = this.resources.get(paneId);
    if (existing) return existing;
    const resource: HostResource = {
      state: visible ? "visible" : "hiddenBuffered",
      serializedSnapshot: "", rawTail: "",
      generation, snapshotGeneration: generation, tailThroughGeneration: generation,
      requiresSeed: false, recoveryReason: "",
    };
    this.resources.set(paneId, resource);
    return resource;
  }

  announceEpoch(): void {
    this.#publish({ kind: "generationEpoch", epoch: EPOCH, sequence: this.#nextSequence() });
  }

  /**
   * `record_output`. A visible pane's output is emitted and journalled — the
   * journal is only ever read to compute the tail at hide time. A hidden pane's
   * output goes straight onto the resource's `raw_tail` (`append_hidden`), and
   * a released pane's is dropped.
   */
  output(paneId: string, text: string): number {
    const generation = this.#nextGeneration();
    const resource = this.#ensure(paneId, false, generation);
    this.tmux.set(paneId, (this.tmux.get(paneId) ?? "") + text);
    resource.generation = generation;
    if (resource.state === "visible") {
      const journal = this.journals.get(paneId) ?? [];
      journal.push({ generation, bytes: text });
      this.journals.set(paneId, journal);
      resource.tailThroughGeneration = generation;
      this.emitted.push(`output@${generation}`);
      this.#publish({
        kind: "output", paneId, generation, sequence: this.#nextSequence(),
        data: ownTerminalBytes(encoder.encode(text)),
      });
    } else if (resource.state === "hiddenBuffered") {
      resource.rawTail += text;
      resource.tailThroughGeneration = generation;
    }
    return generation;
  }

  /**
   * `PaneResourceStore::snapshot` — the stream's capture path. The capture is
   * stored against a brand-new generation (`terminal_generation.fetch_add`),
   * and it is only put on the wire when the pane is already visible to the
   * host, which is why a pane the desktop has not revealed yet gets its first
   * screen through the reveal handshake rather than as a seed.
   */
  capture(paneId: string): void {
    const generation = this.#nextGeneration();
    const resource = this.#ensure(paneId, false, generation);
    const screen = this.tmux.get(paneId) ?? "";
    this.journals.delete(paneId);
    resource.serializedSnapshot = screen;
    resource.rawTail = "";
    resource.snapshotGeneration = generation;
    resource.tailThroughGeneration = generation;
    resource.requiresSeed = false;
    resource.recoveryReason = "";
    resource.generation = generation;
    if (resource.state === "visible") {
      this.emitted.push(`seed@${generation}`);
      this.#publish({
        kind: "seed", paneId, generation, sequence: this.#nextSequence(),
        data: ownTerminalBytes(encoder.encode(screen)),
      });
    }
  }

  /** The seed the desktop asks for: forced visible, then emitted. */
  seedOnRequest(paneId: string): void {
    const resource = this.resources.get(paneId);
    // `reveal_for_seed_request`: asking for a screen is a statement of
    // visibility, so the resource is forced visible before the capture.
    if (resource) {
      resource.state = "visible";
      resource.serializedSnapshot = "";
      resource.rawTail = "";
      resource.requiresSeed = false;
      resource.recoveryReason = "";
      this.checkpoints.delete(paneId);
    } else this.#ensure(paneId, true, this.generation);
    this.capture(paneId);
  }

  /**
   * `PaneResourceStore::reveal`, then `set_terminal_visibility`'s
   * `if visible { resource.state = Visible }` — the emitted state is always
   * `visible` on this path however the stored resource was parked.
   */
  reveal(paneId: string): void {
    const generation = this.#nextGeneration();
    const resource = this.#ensure(paneId, true, generation);
    let recovery: HostResource;
    if (resource.state === "visible") {
      resource.generation = generation;
      recovery = {
        ...resource, serializedSnapshot: "", rawTail: "",
        generation, snapshotGeneration: generation, tailThroughGeneration: generation,
      };
    } else {
      recovery = { ...resource };
      resource.serializedSnapshot = "";
      resource.rawTail = "";
      resource.state = "visible";
      resource.generation = generation;
      this.journals.delete(paneId);
      this.checkpoints.delete(paneId);
    }
    this.emitted.push(`reveal(snapshot=${recovery.snapshotGeneration},bytes=${recovery.serializedSnapshot.length})`);
    this.#publishResource(paneId, { ...recovery, state: "visible" });
  }

  /** `PaneResourceStore::hide_with_checkpoint`, including its release rules. */
  hide(paneId: string, snapshot: Uint8Array, checkpoint: { terminalEpoch: number; outputGeneration: number }): void {
    const generation = this.#nextGeneration();
    const resource = this.#ensure(paneId, true, generation);
    const key = `${checkpoint.terminalEpoch}:${checkpoint.outputGeneration}`;
    if (this.checkpoints.get(paneId) === key) {
      // Idempotent re-hide: the stored resource is returned untouched, which is
      // one of the two ways the host can hand back a snapshot that is not the
      // one this side has since serialized under the same generation.
      this.#publishResource(paneId, { ...resource });
      return;
    }
    if (resource.state !== "visible") throw new Error("pane renderer ownership is already held by the host");
    if (checkpoint.outputGeneration > resource.generation) {
      throw new Error("renderer visibility cutoff is newer than host-observed pane output");
    }
    const journal = this.journals.get(paneId) ?? [];
    this.journals.delete(paneId);
    let tail = "";
    let tailThrough = checkpoint.outputGeneration;
    for (const entry of journal) {
      if (entry.generation > checkpoint.outputGeneration) {
        tail += entry.bytes;
        tailThrough = entry.generation;
      }
    }
    resource.generation = Math.max(tailThrough, generation);
    resource.snapshotGeneration = checkpoint.outputGeneration;
    resource.tailThroughGeneration = tailThrough;
    if (snapshot.byteLength === 0) {
      // "An empty IPC payload cannot distinguish a valid blank renderer
      // serialization from an omitted one", so the host refuses to claim it has
      // a recovery base.
      resource.state = "released";
      resource.requiresSeed = true;
      resource.recoveryReason = "renderer handoff omitted a recoverable snapshot";
      resource.serializedSnapshot = "";
      resource.rawTail = "";
    } else {
      resource.state = "hiddenBuffered";
      resource.serializedSnapshot = decoder.decode(snapshot);
      resource.rawTail = tail;
      resource.requiresSeed = false;
      resource.recoveryReason = "";
    }
    this.checkpoints.set(paneId, key);
    this.emitted.push(`hide(state=${resource.state},snapshot=${resource.snapshotGeneration})`);
    this.#publishResource(paneId, { ...resource });
  }

  /**
   * The `set_visible(true)` hazard, in one call: `replay.rs` restamps
   * `snapshot_generation` to the current generation and never clears
   * `serialized_snapshot`, so a resource can end up offering old bytes under a
   * generation the desktop recognises as its own checkpoint.
   */
  forgeSnapshotUnderGeneration(paneId: string, bytes: string, snapshotGeneration: number): void {
    const resource = this.#ensure(paneId, false, this.generation);
    resource.state = "hiddenBuffered";
    resource.serializedSnapshot = bytes;
    resource.rawTail = "";
    resource.snapshotGeneration = snapshotGeneration;
    resource.tailThroughGeneration = snapshotGeneration;
    resource.requiresSeed = false;
    // The restamp itself advances the store's generation, which is what keeps
    // the resulting event ahead of the hub's per-pane watermark.
    resource.generation = this.#nextGeneration();
  }

  /** A resource event in a state this desktop build has no rule for. */
  publishUnusableResource(paneId: string, state: PaneResourceState): void {
    const generation = this.#nextGeneration();
    this.emitted.push(`unusable(${state})`);
    this.#publishResource(paneId, {
      state, serializedSnapshot: "", rawTail: "",
      generation, snapshotGeneration: generation, tailThroughGeneration: generation,
      requiresSeed: false, recoveryReason: "",
    });
  }

  #publishResource(paneId: string, resource: HostResource): void {
    this.#publish({
      kind: "paneResource", paneId, state: resource.state, requiresSeed: resource.requiresSeed,
      recoveryReason: resource.recoveryReason, generation: resource.generation,
      snapshotGeneration: resource.snapshotGeneration,
      tailThroughGeneration: resource.tailThroughGeneration,
      sequence: this.#nextSequence(),
      serializedSnapshot: ownTerminalBytes(encoder.encode(resource.serializedSnapshot)),
      rawTail: ownTerminalBytes(encoder.encode(resource.rawTail)),
    });
  }
}

const paneNodes: HTMLElement[] = [];
function paneNode(): HTMLElement {
  const node = paneNodes.at(-1);
  if (!node) throw new Error("no terminal pane element was mounted");
  return node;
}
function painted(): boolean {
  return paneNode().getAttribute("data-painted") === "true";
}
function renderer(index = -1): HarnessRenderer {
  const created = renderers.created.at(index);
  if (!created) throw new Error("no renderer was created");
  return created;
}
function pumpAll(): void {
  for (const created of renderers.created) created.pump();
}

function fixturePane(id: string): Pane {
  return {
    id, sessionId: "$1", windowId: "@1", index: 0, active: true,
    width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
  };
}

let host: FakeHost;
let hub: TerminalEventHub;

function paneElement(pane: Pane, strict: boolean) {
  const element = <TerminalPane
    appFocused
    clientId="client-a"
    pane={pane}
    hub={hub}
    onInput={() => undefined}
    onFocus={() => undefined}
    onMeasurements={() => undefined}
    onController={() => undefined}
  />;
  return strict ? <StrictMode>{element}</StrictMode> : element;
}

async function mountPane(pane: Pane, { strict = false } = {}): Promise<ReactTestRenderer> {
  let mounted!: ReactTestRenderer;
  await act(async () => {
    mounted = create(paneElement(pane, strict), {
      createNodeMock: (node) => {
        const div = document.createElement("div");
        if ((node.props as Record<string, unknown>)["data-terminal-surface"]) paneNodes.push(div);
        return div;
      },
    });
  });
  await settle();
  return mounted;
}

/** Lets the handshake's promises, the renderer's frames and the timers land. */
async function settle(milliseconds = 10): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(milliseconds);
    pumpAll();
    await vi.advanceTimersByTimeAsync(0);
    pumpAll();
  });
}

async function unmountPane(mounted: ReactTestRenderer): Promise<void> {
  await act(async () => { await mounted.unmount(); });
  await settle();
}

beforeEach(() => {
  vi.useFakeTimers();
  resetPerfProbe();
  terminalStateCache.clear();
  renderers.created.length = 0;
  renderers.refuseNewWrites = false;
  paneNodes.length = 0;
  api.setTerminalVisibility.mockReset();
  api.requestTerminalSeed.mockReset();
  journal.recordIncident.mockReset();
  hub = new TerminalEventHub((paneId) => { host.seedOnRequest(paneId); pumpAll(); });
  host = new FakeHost(hub);
  // The transport, with one turn of latency so nothing in the pane can rely on
  // the host answering inside its own call stack.
  api.setTerminalVisibility.mockImplementation(async (_client, paneId, visible, snapshot, checkpoint) => {
    await Promise.resolve();
    if (visible) host.reveal(paneId as string);
    else host.hide(paneId as string, snapshot as Uint8Array, checkpoint as { terminalEpoch: number; outputGeneration: number });
    pumpAll();
  });
  api.requestTerminalSeed.mockImplementation(async (_client, paneId) => {
    await Promise.resolve();
    host.seedOnRequest(paneId as string);
    pumpAll();
  });
  Object.assign(globalThis, {
    IS_REACT_ACT_ENVIRONMENT: true,
    ResizeObserver: class { observe() {} unobserve() {} disconnect() {} },
  });
});

afterEach(() => {
  resetPerfProbe();
  terminalStateCache.clear();
  vi.useRealTimers();
});

describe("fresh connection", () => {
  it("shows a pane the content tmux was already holding, and reveals it", async () => {
    host.announceEpoch();
    // A busy server: the pane has a screenful before the app ever asks, and the
    // host's attach capture stores it without emitting — the pane is not
    // visible to the host yet.
    host.output("%1", "BEFORE-CONNECT");
    host.capture("%1");
    expect(host.emitted).toEqual([]);

    const mounted = await mountPane(fixturePane("%1"));

    expect(renderer().screen).toBe("BEFORE-CONNECT");
    expect(painted()).toBe(true);
    // The hub journals its epoch adoption on every connect; only records
    // beyond that benign line would mean this handshake misbehaved.
    expect(incidentsBesidesEpochAdoption()).toEqual([]);
    await unmountPane(mounted);
  });

  it("keeps live output flowing onto the screen the handshake brought", async () => {
    host.announceEpoch();
    host.output("%1", "HISTORY");
    host.capture("%1");
    const mounted = await mountPane(fixturePane("%1"));

    await act(async () => { host.output("%1", "|LIVE"); pumpAll(); });
    await settle();

    expect(renderer().screen).toBe("HISTORY|LIVE");
    expect(painted()).toBe(true);
    await unmountPane(mounted);
  });

  it("reveals a pane whose only content ever arrives as output", async () => {
    host.announceEpoch();
    host.capture("%1");
    const mounted = await mountPane(fixturePane("%1"));
    // The invariant this pins is "bytes on the screen means a visible pane",
    // whichever path put them there. Output used to be the one content path
    // that acknowledged without revealing, leaving the fallback timer to do it
    // — which it always eventually does, so this reads as a timing guarantee
    // rather than as the difference between working and blank.
    await act(async () => { host.output("%1", "FIRST OUTPUT"); pumpAll(); });
    await act(async () => { pumpAll(); });

    expect(renderer().screen).toBe("FIRST OUTPUT");
    expect(painted()).toBe(true);
    await unmountPane(mounted);
  });
});

describe("reveal answers the reducer has no rule for", () => {
  // The shape that used to fall through `reducePaneReveal` to `{ kind: "none" }`
  // with the state untouched: the pane stayed `ready: false` for the rest of its
  // life, deferring every later output instead of writing it, and said nothing
  // to the watchdog, the diagnostic banner or the journal. An unset or newer
  // `PaneResourceState` decodes as `unspecified` and lands here.
  for (const state of ["unspecified", "hiddenBuffered"] as const) {
    it(`asks for a seed when the host answers with ${state} and no material`, async () => {
      host.announceEpoch();
      host.output("%1", "TMUX HAS THIS");
      const mounted = await mountPane(fixturePane("%1"));
      api.requestTerminalSeed.mockClear();

      await act(async () => { host.publishUnusableResource("%1", state); pumpAll(); });
      await settle();

      expect(api.requestTerminalSeed).toHaveBeenCalled();
      expect(journal.recordIncident).toHaveBeenCalledWith("pane.revealDeadEnd", { paneId: "%1", state });
      // And the recovery actually lands: the pane is not merely noisy about it.
      expect(renderer().screen).toBe("TMUX HAS THIS");
      await unmountPane(mounted);
    });
  }

  // A companion invariant rather than a second regression test: the recovery
  // above is what fails without the fix, and this pins that recovering from it
  // leaves the pane working rather than merely audible.
  it("keeps painting output after a dead-end answer", async () => {
    host.announceEpoch();
    host.capture("%1");
    const mounted = await mountPane(fixturePane("%1"));

    await act(async () => { host.publishUnusableResource("%1", "unspecified"); pumpAll(); });
    await settle();
    await act(async () => { host.output("%1", "AFTER"); pumpAll(); });
    await settle();

    expect(renderer().screen).toContain("AFTER");
    await unmountPane(mounted);
  });
});

describe("the redundant restore a tab switch used to repaint", () => {
  /** Mounts, lets the host seed the pane, then hides it so a cache exists. */
  async function warmPane(paneId: string, screen: string): Promise<void> {
    host.announceEpoch();
    host.output(paneId, screen);
    host.capture(paneId);
    const first = await mountPane(fixturePane(paneId));
    await unmountPane(first);
    expect(terminalStateCache.get(paneId)?.serialized).toBe(screen);
  }

  it("writes only the tail when the host hands back the screen the cache painted", async () => {
    await warmPane("%1", "WARM SCREEN");
    host.output("%1", "|WHILE HIDDEN");

    const mounted = await mountPane(fixturePane("%1"));

    // One restore, not two: the cache's, and then only the new bytes.
    expect(renderer().log.filter((entry) => entry.startsWith("restore("))).toHaveLength(1);
    expect(renderer().screen).toBe("WARM SCREEN|WHILE HIDDEN");
    expect(painted()).toBe(true);
    await unmountPane(mounted);
  });

  it("still acknowledges and reveals when the skipped restore has no tail", async () => {
    await warmPane("%1", "WARM SCREEN");

    const mounted = await mountPane(fixturePane("%1"));

    expect(renderer().log.filter((entry) => entry.startsWith("restore("))).toHaveLength(1);
    // The empty tail rides the scheduler's ordered barrier, which is what
    // carries the acknowledgement and the reveal the skipped restore would have.
    expect(renderer().log.some((entry) => entry.startsWith("write:0@"))).toBe(true);
    expect(renderer().screen).toBe("WARM SCREEN");
    expect(painted()).toBe(true);
    await unmountPane(mounted);
  });

  it("restores when the host's bytes differ from the cache under the same generation", async () => {
    await warmPane("%1", "WARM SCREEN");
    const cached = terminalStateCache.get("%1");
    // The `set_visible(true)` / idempotent-hide hazard: same generation, other
    // bytes. A skip decided on the generation alone keeps the stale screen and
    // never shows what the host is trying to hand over.
    host.forgeSnapshotUnderGeneration("%1", "HOST SCREEN", cached!.outputGeneration);

    const mounted = await mountPane(fixturePane("%1"));

    expect(renderer().log.filter((entry) => entry.startsWith("restore("))).toHaveLength(2);
    expect(renderer().screen).toBe("HOST SCREEN");
    expect(painted()).toBe(true);
    await unmountPane(mounted);
  });

  it("recovers instead of going silent when the tail cannot be queued", async () => {
    await warmPane("%1", "WARM SCREEN");
    api.requestTerminalSeed.mockClear();
    // A sealed or overflowed scheduler drops the record and the callback with
    // it, so the acknowledgement and the reveal the skipped restore delegated
    // to that empty write are simply lost. Losing them quietly is what this
    // pane must never do.
    renderers.refuseNewWrites = true;
    const mounted = await mountPane(fixturePane("%1"));
    await settle();

    expect(api.requestTerminalSeed).toHaveBeenCalled();
    await unmountPane(mounted);
  });
});

describe("a reveal the host refuses", () => {
  /** Exactly what `terminal_visibility_request` returns for a stale epoch. */
  const STALE_EPOCH =
    `${STALE_REVEAL_EPOCH_CODE}: terminal visibility checkpoint belongs to a stale connection epoch`;

  /** Reveal calls only, in order, as `[clientId, paneId, visible, ...]`. */
  function reveals(): unknown[][] {
    return api.setTerminalVisibility.mock.calls.filter((call) => call[2] === true);
  }

  /**
   * Fails the first `count` reveals with `error` and lets everything else —
   * including every hide — through to the real host.
   */
  function refuseReveals(error: string, count: number): void {
    const deliver = api.setTerminalVisibility.getMockImplementation()!;
    let refused = 0;
    api.setTerminalVisibility.mockImplementation(async (...args: unknown[]) => {
      if (args[2] === true && refused < count) {
        refused += 1;
        // One turn of latency, like the transport itself: the refusal must not
        // land inside the caller's own stack.
        await Promise.resolve();
        throw new Error(error);
      }
      return deliver(...args);
    });
  }

  // The sleep/wake episode: the bridge reconnects, stamps its new epoch into
  // the client, and this side is still building checkpoints from the epoch
  // frame it has not received yet. Every reveal from that window is refused
  // identically, so replaying it burned eight attempts and ~2s and changed
  // nothing.
  it("takes the checkpoint-free seed path instead of replaying a stale epoch", async () => {
    host.announceEpoch();
    host.output("%1", "WOKE UP");
    host.capture("%1");
    refuseReveals(STALE_EPOCH, Number.MAX_SAFE_INTEGER);

    const mounted = await mountPane(fixturePane("%1"));
    await settle(REVEAL_RETRY_DELAY_MS * 4);

    // The point of the fix: one refused attempt, never a second identical one.
    expect(reveals()).toHaveLength(1);
    expect(journal.recordIncident).not.toHaveBeenCalledWith("pane.revealRetry", expect.anything());
    // And one fresh seed — the recovery that carries no checkpoint at all, and
    // which the host answers by forcing the pane visible and capturing it.
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(1);
    expect(journal.recordIncident).toHaveBeenCalledWith(
      "pane.revealRebuilt",
      expect.objectContaining({ paneId: "%1", error: `Error: ${STALE_EPOCH}` }),
    );
    expect(renderer().screen).toBe("WOKE UP");
    expect(painted()).toBe(true);
    await unmountPane(mounted);
  });

  // The other half of the split, unchanged: a transport that is merely coming
  // up still heals by resending the same request.
  it("still retries a transport that is only coming up", async () => {
    host.announceEpoch();
    host.output("%1", "COMING UP");
    host.capture("%1");
    refuseReveals("host bridge is disconnected", 1);

    const mounted = await mountPane(fixturePane("%1"));
    await settle(REVEAL_RETRY_DELAY_MS * 2);

    expect(reveals()).toHaveLength(2);
    expect(journal.recordIncident).toHaveBeenCalledWith(
      "pane.revealRetry",
      { paneId: "%1", attempt: 0, error: "Error: host bridge is disconnected" },
    );
    expect(journal.recordIncident).not.toHaveBeenCalledWith("pane.revealRebuilt", expect.anything());
    // The retry landed, so nothing had to be recovered from a seed.
    expect(api.requestTerminalSeed).not.toHaveBeenCalled();
    expect(renderer().screen).toBe("COMING UP");
    expect(painted()).toBe(true);
    await unmountPane(mounted);
  });
});

describe("StrictMode", () => {
  // The dev app mounts under StrictMode, so every pane runs mount, cleanup and
  // mount again — a full hide handoff and a second reveal before the user has
  // touched anything. The pane has to end up showing its screen exactly once.
  it("survives the double mount with its screen and its reveal intact", async () => {
    host.announceEpoch();
    host.output("%1", "STRICT SCREEN");
    host.capture("%1");

    const mounted = await mountPane(fixturePane("%1"), { strict: true });
    await settle(PAST_REVEAL_FALLBACK_MS);

    expect(renderer().screen).toBe("STRICT SCREEN");
    expect(painted()).toBe(true);
    expect(incidentsBesidesEpochAdoption()).toEqual([]);
    await unmountPane(mounted);
  });
});
