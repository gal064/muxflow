import { create } from "@bufbuild/protobuf";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HostConnection, HostError } from "../../protocol/HostConnection";
import { Operation, ResponseSchema, TmuxActionKind } from "../../protocol/gen/envelope_pb";
import { FakeTransport, hostEnvelope, okResponse, serverHello, topologySnapshot } from "../../protocol/testing/fakeTransport";
import { createSessionStore } from "../../store/sessionStore";
import { logLines, logStore } from "../../session/log";
import { closeTarget } from "./closeTarget";

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
  return { transport, connection };
}

describe("Close from the hold sheet (§9.3, §9.4)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    logStore.getState().clear();
  });
  afterEach(() => vi.useRealTimers());

  it("sends one confirmed close against the live server identity", async () => {
    const { transport, connection } = await connected();
    const pending = closeTarget(connection, { kind: "pane", sessionId: "$1", windowId: "@2", paneId: "%3" }, "server-a");
    const [frame] = transport.drain();
    if (frame?.payload.case !== "request") throw new Error("expected a request");
    expect(frame.payload.value.operation).toBe(Operation.TMUX_ACTION);
    expect(frame.payload.value.tmuxAction).toMatchObject({ kind: TmuxActionKind.CLOSE_PANE, paneId: "%3", expectedServerIdentity: "server-a", expectedGeneration: 0n, confirmed: true });
    transport.feed(hostEnvelope({ case: "response", value: okResponse({}) }, { requestId: frame.requestId }));
    await expect(pending).resolves.toBeUndefined();
    expect(logLines().join("\n")).toContain("close.target response kind=pane attempt=1 target=%3");
  });

  it("propagates a refusal without retrying", async () => {
    const { transport, connection } = await connected();
    const pending = closeTarget(connection, { kind: "session", sessionId: "$1" }, "server-a");
    const [frame] = transport.drain();
    transport.feed(hostEnvelope({ case: "response", value: create(ResponseSchema, { ok: false, errorCode: "invalid_target", displayMessage: "session no longer exists" }) }, { requestId: frame!.requestId }));
    await expect(pending).rejects.toBeInstanceOf(HostError);
    expect(transport.drain()).toHaveLength(0);
    expect(logLines().join("\n")).toContain("close.target refused kind=session attempt=1 target=$1 code=invalid_target");
  });

  it("sends once more after a stale_topology refusal, against the held server, and no further", async () => {
    const { transport, connection } = await connected();
    const stale = () => create(ResponseSchema, { ok: false, errorCode: "stale_topology", displayMessage: "stale topology" });
    const pending = closeTarget(connection, { kind: "window", sessionId: "$1", windowId: "@2" }, "server-held");
    const [first] = transport.drain();
    transport.feed(hostEnvelope({ case: "response", value: stale() }, { requestId: first!.requestId }));
    await settle();
    const [second] = transport.drain();
    if (second?.payload.case !== "request") throw new Error("expected a retry");
    expect(second.payload.value.tmuxAction).toMatchObject({ kind: TmuxActionKind.CLOSE_WINDOW, windowId: "@2", expectedServerIdentity: "server-held" });
    transport.feed(hostEnvelope({ case: "response", value: stale() }, { requestId: second.requestId }));
    await expect(pending).rejects.toMatchObject({ code: "stale_topology" });
    expect(transport.drain()).toHaveLength(0);
  });
});
