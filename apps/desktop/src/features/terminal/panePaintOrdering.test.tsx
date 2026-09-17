// @vitest-environment jsdom
/**
 * The paint gate is only worth anything if the pane arms it before the sidebar
 * asks to be ordered behind it.
 *
 * Both halves are tested elsewhere, and both by hand: `useWorkspaceFiles`'s
 * tests call `armPanePaint` themselves, and `TerminalPane`'s never look at the
 * gate at all. That leaves the one fact the whole mechanism rests on untested —
 * that in the real tree the pane's reveal effect runs *before* the root probe's
 * effect awaits. If it did not, `awaitPanePaint` would find nothing pending,
 * resolve synchronously, and the Explorer's root — which cascades into a
 * directory listing and a Git watch whose bootstrap is a whole `git status` —
 * would go out on the same ordered lane in front of the screen the user asked
 * for, with every test still green.
 *
 * So this mounts the real shapes together: a parent that owns the root probe,
 * with the pane as a descendant, both changing in one commit on a switch. React
 * runs a child's effects before its parent's, which is what makes the ordering
 * true; this is the test that fails if that ever stops being so.
 */
import { act, create } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Pane } from "../../app/types";
import type { ActiveRoot, FileWorkspaceClient, FileWorkspaceScope } from "../files/types";
import { resetPanePaintGate } from "./panePaintGate";

vi.mock("./api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api")>()),
  setTerminalVisibility: vi.fn(async () => undefined),
  requestTerminalSeed: vi.fn(async () => undefined),
  requestTerminalHistory: vi.fn(async () => undefined),
}));

const { FakeRenderer, renderers } = vi.hoisted(() => {
  /** Only what `TerminalPane` actually touches; the paint is the point here. */
  class FakeRenderer {
    #rendered: Array<() => void> = [];
    scrollbackRows = 0;
    scrollbackLimit = 10_000;
    grid = { columns: 80, rows: 24 };
    open(): void {}
    measure(): undefined { return undefined; }
    measurements(): undefined { return undefined; }
    onMeasurementsChange(): () => void { return () => undefined; }
    setFontSize(): void {}
    setGrid(): { kind: "unchanged" } { return { kind: "unchanged" }; }
    restoreViewport(): void {}
    onGridApplied(): () => void { return () => undefined; }
    isAlternateScreenActive(): boolean { return false; }
    onInput(): () => void { return () => undefined; }
    onSelectionChange(): () => void { return () => undefined; }
    onViewportChange(): () => void { return () => undefined; }
    onScrollbackTopReached(): () => void { return () => undefined; }
    async prependHistory(): Promise<"applied"> { return "applied"; }
    focus(): void {}
    hasSelection(): boolean { return false; }
    getSelection(): string { return ""; }
    search(): boolean { return false; }
    clearSearch(): void {}
    scrollToBottom(): void {}
    noteUnrenderedOutput(): void {}
    disposeGpuRenderer(): void {}
    dispose(): void {}
    async drainAndSerialize(): Promise<{ serialized: string; outputGeneration: number; viewport: { atBottom: boolean; viewportLine: number; grid: { columns: number; rows: number } } }> {
      return {
        serialized: "",
        outputGeneration: 0,
        viewport: { atBottom: true, viewportLine: 0, grid: this.grid },
      };
    }
    seed(_bytes: Uint8Array, onRendered?: () => void): void {
      if (onRendered) this.#rendered.push(onRendered);
    }
    restore(_serialized: string, onRendered?: () => void): boolean {
      if (onRendered) this.#rendered.push(onRendered);
      return true;
    }
    write(_bytes: Uint8Array, onRendered?: () => void): boolean {
      if (onRendered) this.#rendered.push(onRendered);
      return true;
    }
    /** xterm reaching the glass, which is what releases the gate. */
    flushRendered(): void {
      const pending = this.#rendered;
      this.#rendered = [];
      for (const rendered of pending) rendered();
    }
  }
  return { FakeRenderer, renderers: { created: [] as InstanceType<typeof FakeRenderer>[] } };
});

vi.mock("./TerminalRenderer", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./TerminalRenderer")>()),
  XtermRenderer: class extends FakeRenderer {
    constructor() {
      super();
      renderers.created.push(this);
    }
  },
}));

import { TerminalPane } from "./TerminalPane";
import { useWorkspaceFiles } from "../files/useWorkspaceFiles";
import { ownTerminalBytes } from "./TerminalBytes";
import type { TerminalEvent } from "./api";
import type { PaneHealth, TerminalEventHub } from "./TerminalEventHub";

type PaneEvent = Extract<TerminalEvent, { kind: "seed" }>;

class FakeHub {
  generationEpoch: number | undefined = 7;
  #paneListeners = new Map<string, (event: PaneEvent) => void>();

  subscribePane(
    paneId: string,
    listener: (event: PaneEvent) => void,
    onHealthChange?: (health: PaneHealth) => void,
  ): () => void {
    this.#paneListeners.set(paneId, listener);
    onHealthChange?.({ awaitingSeed: false, conflictReseedRequested: false });
    return () => { this.#paneListeners.delete(paneId); };
  }

  paneHealth(): PaneHealth { return { awaitingSeed: false, conflictReseedRequested: false }; }
  retryPaneSeed(): void {}
  subscribeEpoch(): () => void { return () => undefined; }
  markRendered(): void {}
  visibilityCheckpoint(): { terminalEpoch: number; outputGeneration: number } | undefined {
    return this.generationEpoch === undefined
      ? undefined
      : { terminalEpoch: this.generationEpoch, outputGeneration: 0 };
  }

  seed(paneId: string, generation: number): void {
    this.#paneListeners.get(paneId)?.({
      kind: "seed", paneId, generation, sequence: 1,
      data: ownTerminalBytes(new TextEncoder().encode("screen")),
    });
  }

  asHub(): TerminalEventHub { return this as unknown as TerminalEventHub; }
}

function fixturePane(id: string): Pane {
  return {
    id, sessionId: "$1", windowId: "@1", index: 0, active: true,
    width: 80, height: 24, left: 0, top: 0, currentPath: "/repo", currentCommand: "zsh",
  };
}

function scopeFor(paneId: string, sessionId: string): FileWorkspaceScope {
  return {
    clientId: "client-a", hostProfileId: "local", serverIdentity: "s",
    generation: 1, terminalEpoch: 7, sessionId, paneId,
  };
}

const ROOT: ActiveRoot = {
  token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1",
};

function filesClient(): FileWorkspaceClient {
  return {
    resolveActiveRoot: vi.fn(async () => ROOT),
    listDirectory: vi.fn(async (_scope, active, directory) => ({
      rootToken: active.token, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true,
    })),
    acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({
      fresh: true,
      snapshot: {
        rootToken: active.token, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true,
      },
      release: () => undefined,
    })),
    openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
    subscribe: vi.fn(async () => () => undefined),
  };
}

beforeEach(() => {
  (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  Object.assign(globalThis, {
    ResizeObserver: class {
      observe() {} unobserve() {} disconnect() {}
    },
  });
  renderers.created.length = 0;
});
afterEach(() => { resetPanePaintGate(); });

describe("the paint gate in the real tree", () => {
  /**
   * The shape of the app: the component that owns the root probe also renders
   * the terminal surface below it, so on a switch both change in one commit.
   */
  function Shell({ client, pane }: { client: FileWorkspaceClient; pane: Pane }) {
    useWorkspaceFiles(client, scopeFor(pane.id, pane.sessionId));
    return <TerminalPane
      appFocused
      cacheScope="local"
      clientId="client-a"
      pane={pane}
      hub={hub.asHub()}
      onInput={() => undefined}
      onFocus={() => undefined}
      onMeasurements={() => undefined}
      onController={() => undefined}
      terminalFontSize={13}
    />;
  }

  let hub: FakeHub;

  const nodeMock = () => document.createElement("div");

  it("arms from the pane's own mount, so the root probe is behind the first paint", async () => {
    hub = new FakeHub();
    const client = filesClient();
    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(<Shell client={client} pane={fixturePane("%1")} />, { createNodeMock: nodeMock });
      await Promise.resolve();
    });
    await act(async () => { await Promise.resolve(); });

    // Nobody armed this by hand: the pane's reveal effect ran before the root
    // probe's effect awaited, which is the whole claim.
    expect(
      client.resolveActiveRoot,
      "the root probe ran ahead of the pane's own screen",
    ).not.toHaveBeenCalled();

    await act(async () => { hub.seed("%1", 4); });
    await act(async () => { renderers.created[0].flushRendered(); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(client.resolveActiveRoot).toHaveBeenCalledTimes(1);

    await act(async () => { renderer.unmount(); });
  });

  it("arms again on a switch, where the scope and the pane change in one commit", async () => {
    hub = new FakeHub();
    const client = filesClient();
    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(<Shell client={client} pane={fixturePane("%1")} />, { createNodeMock: nodeMock });
      await Promise.resolve();
    });
    await act(async () => { hub.seed("%1", 4); });
    await act(async () => { renderers.created[0].flushRendered(); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(client.resolveActiveRoot).toHaveBeenCalledTimes(1);

    // The switch: a different pane and a different scope in the same commit,
    // which is also the commit whose ordering the gate depends on.
    const switched = { ...fixturePane("%2"), sessionId: "$2" };
    await act(async () => {
      renderer.update(<Shell client={client} pane={switched} />);
      await Promise.resolve();
    });
    await act(async () => { await Promise.resolve(); });
    expect(
      client.resolveActiveRoot,
      "the switch's root probe ran ahead of the pane it switched to",
    ).toHaveBeenCalledTimes(1);

    await act(async () => { hub.seed("%2", 5); });
    await act(async () => { renderers.created.at(-1)?.flushRendered(); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(client.resolveActiveRoot).toHaveBeenCalledTimes(2);

    await act(async () => { renderer.unmount(); });
  });
});
