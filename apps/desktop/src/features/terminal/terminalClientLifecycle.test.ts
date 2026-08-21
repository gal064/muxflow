import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { startTerminal, stopTerminal } from "./api";

const channels = vi.hoisted(() => [] as Array<{ onmessage?: (message: ArrayBuffer) => void }>);
const boundary = vi.hoisted(() => ({ failAfterStartResolves: false }));

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class MockChannel<T> {
    onmessage?: (message: T) => void;
    constructor() { channels.push(this as { onmessage?: (message: ArrayBuffer) => void }); }
  },
  invoke: vi.fn(() => Promise.resolve()),
}));
vi.mock("../../perf/bootstrap", () => ({ perfProbeReady: () => Promise.resolve(false) }));
/**
 * The real request boundary keeps working after the native call resolves — it
 * records outcome counters and a latency sample — so a throw from that stage
 * lands with the native client already alive and its id known only inside
 * `startTerminal`. That is the exact window this file guards, and mocking the
 * boundary is the only way to enter it deliberately.
 */
vi.mock("../../perf/probe", () => ({
  measurePerfRequest: async <T, Boundary>(
    name: string, _domain: string, request: Boundary, work: (request: Boundary) => Promise<T>,
  ): Promise<T> => {
    const result = await work(request);
    if (name === "workflow.connect" && boundary.failAfterStartResolves) {
      throw new Error("request boundary bookkeeping failed after start_terminal resolved");
    }
    return result;
  },
  recordPerfCounter: () => undefined,
}));

function journalledIncidents(kind: string): Array<Record<string, unknown>> {
  return vi.mocked(invoke).mock.calls
    .filter(([command]) => command === "record_incident")
    .map(([, argument]) => JSON.parse((argument as { line: string }).line) as Record<string, unknown>)
    .filter((incident) => incident.kind === kind);
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "start_terminal") return "client-unnamed";
    return undefined;
  });
  channels.length = 0;
  boundary.failAfterStartResolves = false;
});

describe("native terminal client ownership", () => {
  /**
   * `start_terminal` resolving is the point a native supervisor thread exists:
   * it holds the carrier, reconnects on its own backoff and attaches tmux
   * control clients, and `stop_terminal` is keyed by the id alone. A start that
   * fails after that point must therefore stop the client while the id is still
   * in hand, or it runs for the app's lifetime with nothing able to name it.
   */
  it("stops the native client when startup fails after the id is minted", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-orphaned";
      return undefined;
    });
    boundary.failAfterStartResolves = true;

    await expect(startTerminal("", [], { mode: "local" }, () => undefined))
      .rejects.toThrow("boundary bookkeeping failed");

    expect(invoke).toHaveBeenCalledWith("stop_terminal", { clientId: "client-orphaned" });
  });

  it("leaves no registration behind for a client whose startup failed", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-orphaned";
      return undefined;
    });
    boundary.failAfterStartResolves = true;
    await expect(startTerminal("", [], { mode: "local" }, () => undefined)).rejects.toThrow();

    // The registration maps are module-private, so their state is read the way
    // the app reads it: a later start that finds the failed id still tracked
    // would journal itself as the second live client.
    boundary.failAfterStartResolves = false;
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-after-failure";
      return undefined;
    });
    const clientId = await startTerminal("", [], { mode: "local" }, () => undefined);

    expect(clientId).toBe("client-after-failure");
    expect(journalledIncidents("terminal.multiClient")).toEqual([]);
    await stopTerminal(clientId);
  });

  it("journals nothing when a single client is live", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-only";
      return undefined;
    });

    const clientId = await startTerminal("", [], { mode: "local" }, () => undefined);

    expect(journalledIncidents("terminal.multiClient")).toEqual([]);
    await stopTerminal(clientId);
  });

  /**
   * Every extra live client is an independent carrier, reconnect schedule and
   * tmux control attachment. "How many bridges were actually running" is the
   * first question a reconnect-storm report has to answer, and the modes say
   * whether the overlap is the legitimate kind — a local host beside a remote
   * one, or a host switch whose old bridge has not finished stopping.
   */
  it("journals one incident naming every live client once a second becomes live", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-first";
      return undefined;
    });
    const first = await startTerminal("", [], { mode: "local" }, () => undefined);
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-second";
      return undefined;
    });
    const second = await startTerminal(
      "", [], { mode: "ssh", profileId: "ssh-host-0000000000000000", target: "host" }, () => undefined,
    );

    const incidents = journalledIncidents("terminal.multiClient");
    expect(incidents).toHaveLength(1);
    expect(incidents[0]).toMatchObject({
      count: 2,
      clientIds: ["client-first", "client-second"],
      modes: ["local", "ssh"],
    });
    // The ids are the native UUIDs and the modes are two fixed words: the line
    // carries no target, profile id, or anything else naming a machine.
    expect(incidents[0]).not.toHaveProperty("target");
    expect(JSON.stringify(incidents[0])).not.toContain("host");

    await stopTerminal(first);
    await stopTerminal(second);
  });

  it("stops journalling once the concurrent client is gone", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-first";
      return undefined;
    });
    const first = await startTerminal("", [], { mode: "local" }, () => undefined);
    await stopTerminal(first);
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-second";
      return undefined;
    });
    const second = await startTerminal("", [], { mode: "local" }, () => undefined);

    expect(journalledIncidents("terminal.multiClient")).toEqual([]);
    await stopTerminal(second);
  });
});
