import { create } from "@bufbuild/protobuf";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HostConnection } from "../../protocol/HostConnection";
import { EventKind, HostEventSchema, Operation, ResponseSchema, TmuxActionKind, TmuxActionResultSchema } from "../../protocol/gen/envelope_pb";
import { FakeTransport, hostEnvelope, okResponse, serverHello, topologySnapshot } from "../../protocol/testing/fakeTransport";
import { createSessionStore } from "../../store/sessionStore";
import { logLines, logStore } from "../../session/log";
import { NO_TMUX_SERVER, STALE_TOPOLOGY_WAIT_MS, MissingAgentCommandError, createAgentWindow, createTerminalWindow, findRandomSession, isConnectionScopeCurrent, openInRandom } from "./createWindow";

const settle = () => vi.advanceTimersByTimeAsync(0);

async function connected() {
  const store = createSessionStore();
  const transport = new FakeTransport();
  const connection = new HostConnection({ dial: async () => transport, nextConnectionEpoch: () => 1, store });
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

  it("sends command-free TMUX_ACTION CREATE_WINDOW with explicit diagnostic intent", async () => {
    const { store, transport, connection } = await connected();
    const pending = createTerminalWindow(connection, store, "$1", { kind: "agent" });
    const [frame] = transport.drain();
    if (frame?.payload.case !== "request") throw new Error("expected a request");
    expect(frame.payload.value.operation).toBe(Operation.TMUX_ACTION);
    expect(frame.payload.value.tmuxAction).toMatchObject({ sessionId: "$1", expectedServerIdentity: "server-a", expectedGeneration: 7n });
    expect(frame.payload.value.tmuxAction).not.toHaveProperty("command");
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { windowId: "@9", paneId: "%9" }) }) }, { requestId: frame.requestId }));
    await expect(pending).resolves.toEqual({ windowId: "@9", paneId: "%9" });
    const diagnostics = logLines().join("\n");
    expect(diagnostics).toContain("create.window response kind=agent attempt=1 result=present window=@9 pane=%9");
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

describe("New agent create-then-send", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    logStore.getState().clear();
  });
  afterEach(() => vi.useRealTimers());

  it("waits for an acknowledged pane, then sends the configured text plus one newline exactly once", async () => {
    const { store, transport, connection } = await connected();
    const pending = createAgentWindow(connection, store, "$1", "  cx --profile dev  ", () => connection);
    const [createFrame] = transport.drain();
    if (createFrame?.payload.case !== "request") throw new Error("expected create request");
    expect(createFrame.payload.value.operation).toBe(Operation.TMUX_ACTION);
    expect(transport.drain()).toHaveLength(0);

    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { windowId: "@9", paneId: "%9" }) }) }, { requestId: createFrame.requestId }));
    const created = await pending;
    const [inputFrame] = transport.drain();
    if (inputFrame?.payload.case !== "request") throw new Error("expected terminal input request");
    expect(inputFrame.payload.value).toMatchObject({ operation: Operation.TERMINAL_INPUT, scope: "%9", terminalInputVoice: false });
    expect(new TextDecoder().decode(inputFrame.payload.value.data)).toBe("  cx --profile dev  \n");
    expect(transport.drain()).toHaveLength(0);

    transport.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: inputFrame.requestId }));
    await expect(created.commandDelivery).resolves.toEqual({ ok: true });
    expect(transport.drain()).toHaveLength(0);
    const diagnostics = logLines().join("\n");
    expect(diagnostics).toContain("command.sent kind=agent pane=%9 bytes=21 epoch=1");
    expect(diagnostics).not.toContain("cx --profile dev");
  });

  it("does not send across a replaced connection even when tmux IDs coincide", async () => {
    const { store, transport, connection } = await connected();
    let current: HostConnection | null = connection;
    const pending = createAgentWindow(connection, store, "$1", "cx", () => current);
    const [createFrame] = transport.drain();
    current = null;
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { windowId: "@9", paneId: "%9" }) }) }, { requestId: createFrame!.requestId }));
    const created = await pending;

    await expect(created.commandDelivery).resolves.toMatchObject({ ok: false, error: expect.any(Error) });
    expect(transport.drain()).toHaveLength(0);
    expect(isConnectionScopeCurrent(created.scope, () => connection)).toBe(true);
    expect(isConnectionScopeCurrent({ ...created.scope, connectionEpoch: 2n }, () => connection)).toBe(false);
  });

  it("turns an input refusal into partial success without retrying delivery", async () => {
    const { store, transport, connection } = await connected();
    const pending = createAgentWindow(connection, store, "$1", "missing-alias", () => connection);
    const [createFrame] = transport.drain();
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { windowId: "@3", paneId: "%3" }) }) }, { requestId: createFrame!.requestId }));
    const created = await pending;
    const [inputFrame] = transport.drain();
    transport.feed(hostEnvelope({ case: "response", value: create(ResponseSchema, { ok: false, errorCode: "input_failed", displayMessage: "not delivered" }) }, { requestId: inputFrame!.requestId }));

    await expect(created.commandDelivery).resolves.toMatchObject({ ok: false, error: { code: "input_failed" } });
    expect(created).toMatchObject({ windowId: "@3", paneId: "%3" });
    expect(transport.drain()).toHaveLength(0);
  });

  it("does not retry terminal input when its outcome times out", async () => {
    const { store, transport, connection } = await connected();
    const pending = createAgentWindow(connection, store, "$1", "cx", () => connection);
    const [createFrame] = transport.drain();
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { windowId: "@5", paneId: "%5" }) }) }, { requestId: createFrame!.requestId }));
    const created = await pending;
    const [inputFrame] = transport.drain();
    expect(inputFrame?.payload.case).toBe("request");

    await vi.advanceTimersByTimeAsync(20_000);
    await expect(created.commandDelivery).resolves.toMatchObject({ ok: false, error: { name: "RequestTimeoutError" } });
    expect(transport.drain()).toHaveLength(0);
  });

  it("sends nothing when creation fails or returns no pane ID", async () => {
    const first = await connected();
    const refused = createAgentWindow(first.connection, first.store, "$1", "cx", () => first.connection);
    const [refusedFrame] = first.transport.drain();
    first.transport.feed(hostEnvelope({ case: "response", value: create(ResponseSchema, { ok: false, errorCode: "session_missing", displayMessage: "gone" }) }, { requestId: refusedFrame!.requestId }));
    await expect(refused).rejects.toMatchObject({ code: "session_missing" });
    expect(first.transport.drain()).toHaveLength(0);

    const second = await connected();
    const missingPane = createAgentWindow(second.connection, second.store, "$1", "cx", () => second.connection);
    const [missingFrame] = second.transport.drain();
    second.transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { windowId: "@4" }) }) }, { requestId: missingFrame!.requestId }));
    await expect(missingPane).rejects.toMatchObject({ code: "missing_result" });
    expect(second.transport.drain()).toHaveLength(0);
  });
});

describe("New agent and New terminal in random (quick-session plan §4.2)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    logStore.getState().clear();
  });
  afterEach(() => vi.useRealTimers());

  const withRandom = { sessions: [
    { id: "$1", name: "primary", windowCount: 1, attachedClients: 0, order: 0 },
    { id: "$4", name: "random", windowCount: 1, attachedClients: 0, order: 1 },
  ] };

  async function connectedWith(overrides: Parameters<typeof topologySnapshot>[0] = {}, identity = "server-a") {
    const store = createSessionStore();
    const transport = new FakeTransport();
    const connection = new HostConnection({ dial: async () => transport, nextConnectionEpoch: () => 1, store });
    connection.connect();
    await settle();
    transport.feed(hostEnvelope({ case: "serverHello", value: serverHello({ serverIdentity: identity }) }, { requestId: 1n }));
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot({ generation: 7n, serverIdentity: identity, ...overrides }) }) }, { requestId: 2n }));
    transport.drain();
    return { store, transport, connection };
  }

  function request(frame: ReturnType<FakeTransport["drain"]>[number] | undefined) {
    if (frame?.payload.case !== "request") throw new Error("expected a request");
    return { requestId: frame.requestId, value: frame.payload.value };
  }

  it("creates the random workspace in the directory when it is missing", async () => {
    const { store, transport, connection } = await connectedWith();
    expect(findRandomSession(store)).toBeUndefined();
    const pending = openInRandom(connection, store, { kind: "terminal", directory: "~/work", getCurrentConnection: () => connection });
    const sent = request(transport.drain()[0]);
    expect(sent.value.tmuxAction).toMatchObject({ kind: TmuxActionKind.CREATE_SESSION, name: "random", directory: "~/work", sessionId: "", expectedServerIdentity: "server-a", expectedGeneration: 7n });
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { sessionId: "$5", windowId: "@6", paneId: "%7" }) }) }, { requestId: sent.requestId }));
    const opened = await pending;
    expect(opened).toMatchObject({ sessionId: "$5", windowId: "@6", paneId: "%7", createdWorkspace: true });
    expect(opened.commandDelivery).toBeUndefined();
    expect(transport.drain()).toHaveLength(0);
  });

  it("opens a window in the existing random workspace, matched by exact name", async () => {
    const { store, transport, connection } = await connectedWith({ sessions: [
      { id: "$1", name: "Random", windowCount: 1, attachedClients: 0, order: 0 },
      { id: "$2", name: "random ✳", windowCount: 1, attachedClients: 0, order: 1 },
      { id: "$4", name: "random", windowCount: 1, attachedClients: 0, order: 2 },
    ] });
    const pending = openInRandom(connection, store, { kind: "agent", directory: "/srv", command: "codex", getCurrentConnection: () => connection });
    const sent = request(transport.drain()[0]);
    expect(sent.value.tmuxAction).toMatchObject({ kind: TmuxActionKind.CREATE_WINDOW, sessionId: "$4", directory: "/srv", name: "", expectedGeneration: 7n });
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { windowId: "@8", paneId: "%8" }) }) }, { requestId: sent.requestId }));
    const opened = await pending;
    expect(opened).toMatchObject({ sessionId: "$4", windowId: "@8", paneId: "%8", createdWorkspace: false });
    const input = request(transport.drain()[0]);
    expect(input.value).toMatchObject({ operation: Operation.TERMINAL_INPUT, scope: "%8" });
    expect(new TextDecoder().decode(input.value.data)).toBe("codex\n");
    transport.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: input.requestId }));
    await expect(opened.commandDelivery).resolves.toEqual({ ok: true });
  });

  it("retries once on stale_topology and looks the workspace up again", async () => {
    const { store, transport, connection } = await connectedWith();
    const pending = openInRandom(connection, store, { kind: "terminal", directory: "~", getCurrentConnection: () => connection });
    const first = request(transport.drain()[0]);
    expect(first.value.tmuxAction?.kind).toBe(TmuxActionKind.CREATE_SESSION);
    transport.feed(hostEnvelope({ case: "response", value: create(ResponseSchema, { ok: false, errorCode: "stale_topology", displayMessage: "topology changed" }) }, { requestId: first.requestId }));
    await settle();
    // Another client made `random` meanwhile: the retry opens a window in it.
    transport.feed(hostEnvelope({ case: "event", value: create(HostEventSchema, { kind: EventKind.TOPOLOGY_SNAPSHOT, snapshot: topologySnapshot({ generation: 8n, ...withRandom }) }) }, { sequence: 1n }));
    await settle();
    const second = request(transport.drain()[0]);
    expect(second.value.tmuxAction).toMatchObject({ kind: TmuxActionKind.CREATE_WINDOW, sessionId: "$4", directory: "~", expectedGeneration: 8n });
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { windowId: "@9", paneId: "%9" }) }) }, { requestId: second.requestId }));
    await expect(pending).resolves.toMatchObject({ sessionId: "$4", paneId: "%9", createdWorkspace: false });

    const again = openInRandom(connection, store, { kind: "terminal", directory: "~", getCurrentConnection: () => connection });
    const stale = (requestId: bigint) => transport.feed(hostEnvelope({ case: "response", value: create(ResponseSchema, { ok: false, errorCode: "stale_topology", displayMessage: "again" }) }, { requestId }));
    stale(request(transport.drain()[0]).requestId);
    await vi.advanceTimersByTimeAsync(STALE_TOPOLOGY_WAIT_MS);
    stale(request(transport.drain()[0]).requestId);
    await expect(again).rejects.toMatchObject({ code: "stale_topology" });
    expect(transport.drain()).toHaveLength(0);
  });

  it("sends nothing for an agent without a command", async () => {
    const { store, transport, connection } = await connectedWith(withRandom);
    await expect(openInRandom(connection, store, { kind: "agent", directory: "~", command: "  ", getCurrentConnection: () => connection })).rejects.toBeInstanceOf(MissingAgentCommandError);
    await expect(openInRandom(connection, store, { kind: "agent", directory: "~", getCurrentConnection: () => connection })).rejects.toBeInstanceOf(MissingAgentCommandError);
    expect(transport.drain()).toHaveLength(0);
  });

  it("on a host with no tmux server, creates random and keeps the scope across the server it started", async () => {
    const { store, transport, connection } = await connectedWith({ sessions: [], windows: [], panes: [] }, NO_TMUX_SERVER);
    const pending = openInRandom(connection, store, { kind: "agent", directory: "~", command: "codex", getCurrentConnection: () => connection });
    const sent = request(transport.drain()[0]);
    expect(sent.value.tmuxAction).toMatchObject({ kind: TmuxActionKind.CREATE_SESSION, name: "random", expectedServerIdentity: NO_TMUX_SERVER });
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { sessionId: "$0", windowId: "@0", paneId: "%0" }) }) }, { requestId: sent.requestId }));
    const opened = await pending;
    expect(opened.createdWorkspace).toBe(true);
    // The command goes out at once, before the new server's snapshot lands.
    const input = request(transport.drain()[0]);
    expect(input.value).toMatchObject({ operation: Operation.TERMINAL_INPUT, scope: "%0" });
    transport.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: input.requestId }));
    await expect(opened.commandDelivery).resolves.toEqual({ ok: true });
    // The snapshot from the server the create started: still the same scope.
    transport.feed(hostEnvelope({ case: "event", value: create(HostEventSchema, { kind: EventKind.TOPOLOGY_SNAPSHOT, snapshot: topologySnapshot({ generation: 8n, serverIdentity: "server-b" }) }) }, { sequence: 1n }));
    await settle();
    expect(connection.serverIdentity).toBe("server-b");
    expect(isConnectionScopeCurrent(opened.scope, () => connection)).toBe(true);
  });

  it("accepts only the move from no server to a server", async () => {
    const { connection } = await connectedWith();
    const scope = { connection, connectionEpoch: connection.connectionEpoch, sessionId: "$1" };
    expect(isConnectionScopeCurrent({ ...scope, serverIdentity: NO_TMUX_SERVER }, () => connection)).toBe(true);
    expect(isConnectionScopeCurrent({ ...scope, serverIdentity: "server-a" }, () => connection)).toBe(true);
    expect(isConnectionScopeCurrent({ ...scope, serverIdentity: "server-old" }, () => connection)).toBe(false);
    const empty = await connectedWith({ sessions: [], windows: [], panes: [] }, NO_TMUX_SERVER);
    const ended = { connection: empty.connection, connectionEpoch: empty.connection.connectionEpoch, sessionId: "$1" };
    expect(isConnectionScopeCurrent({ ...ended, serverIdentity: "server-a" }, () => empty.connection)).toBe(false);
  });

  it("does not type the agent command across a replaced connection", async () => {
    const { store, transport, connection } = await connectedWith();
    let current: HostConnection | null = connection;
    const pending = openInRandom(connection, store, { kind: "agent", directory: "~", command: "codex", getCurrentConnection: () => current });
    const sent = request(transport.drain()[0]);
    current = null;
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ tmuxActionResult: create(TmuxActionResultSchema, { sessionId: "$5", windowId: "@6", paneId: "%7" }) }) }, { requestId: sent.requestId }));
    const opened = await pending;
    await expect(opened.commandDelivery).resolves.toMatchObject({ ok: false, error: expect.any(Error) });
    expect(transport.drain()).toHaveLength(0);
    expect(isConnectionScopeCurrent(opened.scope, () => connection)).toBe(true);
  });
});
