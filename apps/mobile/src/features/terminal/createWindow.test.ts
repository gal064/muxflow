import { create } from "@bufbuild/protobuf";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HostConnection } from "../../protocol/HostConnection";
import { EventKind, HostEventSchema, Operation, ResponseSchema, TmuxActionResultSchema } from "../../protocol/gen/envelope_pb";
import { FakeTransport, hostEnvelope, okResponse, serverHello, topologySnapshot } from "../../protocol/testing/fakeTransport";
import { createSessionStore } from "../../store/sessionStore";
import { logLines, logStore } from "../../session/log";
import { createTerminalWindow } from "./createWindow";

const settle = () => vi.advanceTimersByTimeAsync(0);

async function connected() {
  const store = createSessionStore();
  const transport = new FakeTransport();
  const connection = new HostConnection({ dial: async () => transport, appVersion: "t", nextConnectionEpoch: () => 1, store });
  connection.connect();
  await settle();
  transport.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n }));
  transport.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot({ generation: 7n }) }) }, { requestId: 2n }));
  transport.drain();
  return { store, transport, connection };
}

describe("New terminal (§9.4, §7.5)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    logStore.getState().clear();
  });
  afterEach(() => vi.useRealTimers());

  it("sends TMUX_ACTION CREATE_WINDOW with the generation read at call time", async () => {
    const { store, transport, connection } = await connected();
    const pending = createTerminalWindow(connection, store, "$1", "codex --full-auto");
    const [frame] = transport.drain();
    if (frame?.payload.case !== "request") throw new Error("expected a request");
    expect(frame.payload.value.operation).toBe(Operation.TMUX_ACTION);
    expect(frame.payload.value.tmuxAction).toMatchObject({ sessionId: "$1", expectedServerIdentity: "server-a", expectedGeneration: 7n, command: "codex --full-auto" });
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { windowId: "@9", paneId: "%9" }) }) }, { requestId: frame.requestId }));
    await expect(pending).resolves.toEqual({ windowId: "@9", paneId: "%9" });
    const diagnostics = logLines().join("\n");
    expect(diagnostics).toContain("create.window response kind=agent attempt=1 result=present window=@9 pane=%9");
    expect(diagnostics).not.toContain("codex --full-auto");
  });

  it("retries once on stale_topology, waiting for the snapshot that follows the refusal", async () => {
    const { store, transport, connection } = await connected();
    const pending = createTerminalWindow(connection, store, "$1");
    const [first] = transport.drain();
    // The refusal is answered directly; the fresh TOPOLOGY_SNAPSHOT travels on
    // the ordered event channel and can land just after it.
    transport.feed(hostEnvelope({ case: "response", value: create(ResponseSchema, { ok: false, errorCode: "stale_topology", displayMessage: "topology changed" }) }, { requestId: first!.requestId }));
    await vi.advanceTimersByTimeAsync(100);
    expect(transport.drain()).toHaveLength(0);
    transport.feed(hostEnvelope({ case: "event", value: create(HostEventSchema, { kind: EventKind.TOPOLOGY_SNAPSHOT, snapshot: topologySnapshot({ generation: 8n }) }) }, { sequence: 1n }));
    await settle();
    const [second] = transport.drain();
    if (second?.payload.case !== "request") throw new Error("expected a retry");
    expect(second.payload.value.tmuxAction?.expectedGeneration).toBe(8n);
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { windowId: "@2", paneId: "%2" }) }) }, { requestId: second.requestId }));
    await expect(pending).resolves.toEqual({ windowId: "@2", paneId: "%2" });
  });

  it("gives up after a second stale_topology and surfaces other errors untouched", async () => {
    const { store, transport, connection } = await connected();
    const pending = createTerminalWindow(connection, store, "$1");
    const stale = (requestId: bigint) => transport.feed(hostEnvelope({ case: "response", value: create(ResponseSchema, { ok: false, errorCode: "stale_topology", displayMessage: "again" }) }, { requestId }));
    stale(transport.drain()[0]!.requestId);
    // No newer snapshot arrives: the retry goes out after the 1 s wait.
    await vi.advanceTimersByTimeAsync(999);
    expect(transport.drain()).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    stale(transport.drain()[0]!.requestId);
    await expect(pending).rejects.toMatchObject({ code: "stale_topology" });
    expect(logLines().join("\n")).toContain("create.window refused kind=terminal attempt=2 code=stale_topology");

    const other = createTerminalWindow(connection, store, "$1");
    transport.feed(hostEnvelope({ case: "response", value: create(ResponseSchema, { ok: false, errorCode: "session_missing", displayMessage: "no such session" }) }, { requestId: transport.drain()[0]!.requestId }));
    await expect(other).rejects.toMatchObject({ code: "session_missing", message: "no such session" });
    expect(transport.drain()).toHaveLength(0);
  });
});
