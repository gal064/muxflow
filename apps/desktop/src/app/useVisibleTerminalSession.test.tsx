// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  useVisibleTerminalSession,
  VISIBLE_SESSION_RETRIES,
  VISIBLE_SESSION_RETRY_MS,
} from "./useVisibleTerminalSession";

const selectMock = vi.hoisted(() => vi.fn(async (_clientId: string, _sessionId: string) => undefined));
vi.mock("../features/terminal/api", () => ({ selectTerminalSession: selectMock }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

interface HarnessProps {
  activeSessionId?: string;
  canMutate?: boolean;
  clientId?: string;
  terminalEpoch?: number;
  onStatus?: (message: string) => void;
  topologyGeneration?: number;
  selectionAcknowledgement?: { clientId: string; sessionId: string; terminalEpoch: number; version: number };
}

function Harness(props: HarnessProps) {
  useVisibleTerminalSession({
    activeSessionId: props.activeSessionId,
    canMutate: props.canMutate ?? true,
    clientId: props.clientId,
    onStatus: props.onStatus ?? (() => undefined),
    selectionAcknowledgement: props.selectionAcknowledgement,
    terminalEpoch: props.terminalEpoch ?? 1,
    topologyGeneration: props.topologyGeneration ?? 1,
  });
  return null;
}

async function render(props: HarnessProps) {
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(<Harness {...props} />); });
  return {
    update: async (next: HarnessProps) => {
      await act(async () => { renderer.update(<Harness {...next} />); });
    },
  };
}

/** Long enough for every retry the hook is allowed. */
async function exhaustRetries() {
  await act(async () => { await vi.advanceTimersByTimeAsync(VISIBLE_SESSION_RETRY_MS * (VISIBLE_SESSION_RETRIES + 1)); });
}

describe("useVisibleTerminalSession", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    selectMock.mockClear();
    selectMock.mockImplementation(async () => undefined);
  });
  afterEach(() => vi.useRealTimers());

  it("tells the host which workspace is on screen as soon as there is a bridge", async () => {
    await render({ activeSessionId: "$1", clientId: "client-1" });
    expect(selectMock.mock.calls).toEqual([["client-1", "$1"]]);
  });

  it("does not restate a selection acknowledged by the atomic host action", async () => {
    await render({
      activeSessionId: "$1",
      clientId: "client-1",
      selectionAcknowledgement: { clientId: "client-1", sessionId: "$1", terminalEpoch: 1, version: 1 },
    });
    expect(selectMock).not.toHaveBeenCalled();
  });

  it("does not apply a selection acknowledgement from another bridge", async () => {
    await render({
      activeSessionId: "$1",
      clientId: "client-2",
      selectionAcknowledgement: { clientId: "client-1", sessionId: "$1", terminalEpoch: 1, version: 1 },
    });
    expect(selectMock.mock.calls).toEqual([["client-2", "$1"]]);
  });

  it("says nothing without a bridge, a workspace, or the right to mutate", async () => {
    await render({ activeSessionId: "$1", clientId: undefined });
    await render({ activeSessionId: undefined, clientId: "client-1" });
    await render({ activeSessionId: "$1", clientId: "client-1", canMutate: false });
    expect(selectMock).not.toHaveBeenCalled();
  });

  /**
   * The reconnect is the case this exists for. The bridge re-attaches to
   * whichever session the fresh snapshot lists first, which after the first
   * connect has nothing to do with what is on screen — so a new client id has
   * to re-state the fact even though the workspace never changed.
   */
  it("re-states the workspace on a workspace switch and on a new bridge", async () => {
    const { update } = await render({ activeSessionId: "$1", clientId: "client-1" });
    await update({ activeSessionId: "$2", clientId: "client-1" });
    await update({ activeSessionId: "$2", clientId: "client-2" });
    expect(selectMock.mock.calls).toEqual([
      ["client-1", "$1"],
      ["client-1", "$2"],
      ["client-2", "$2"],
    ]);
  });

  it("re-states the workspace when the same native client reconnects at a new epoch", async () => {
    const { update } = await render({
      activeSessionId: "$1",
      clientId: "client-1",
      terminalEpoch: 7,
      selectionAcknowledgement: {
        clientId: "client-1", sessionId: "$1", terminalEpoch: 7, version: 1,
      },
    });
    expect(selectMock).not.toHaveBeenCalled();
    await update({
      activeSessionId: "$1",
      clientId: "client-1",
      terminalEpoch: 8,
      selectionAcknowledgement: {
        clientId: "client-1", sessionId: "$1", terminalEpoch: 7, version: 1,
      },
    });
    expect(selectMock.mock.calls).toEqual([["client-1", "$1"]]);
  });

  /**
   * The host refuses a session whose control client it has not attached yet,
   * and on a fresh bridge its reconciler is still doing that. Transient, so it
   * is retried; permanent, so it is eventually said out loud — a host that will
   * not take the selection is about to size the user's windows from a workspace
   * they cannot see.
   */
  it("retries a refused selection, then reports it once", async () => {
    selectMock.mockImplementation(async () => { throw new Error("session has no control client"); });
    const statuses: string[] = [];
    await render({ activeSessionId: "$1", clientId: "client-1", onStatus: (message) => statuses.push(message) });
    expect(selectMock).toHaveBeenCalledTimes(1);
    expect(statuses).toEqual([]);
    await exhaustRetries();
    expect(selectMock).toHaveBeenCalledTimes(VISIBLE_SESSION_RETRIES + 1);
    expect(statuses).toEqual([
      "This workspace may render at the wrong size: Error: session has no control client",
    ]);
  });

  it("stops retrying as soon as one lands", async () => {
    selectMock.mockImplementationOnce(async () => { throw new Error("not attached yet"); });
    const statuses: string[] = [];
    await render({ activeSessionId: "$1", clientId: "client-1", onStatus: (message) => statuses.push(message) });
    await exhaustRetries();
    expect(selectMock).toHaveBeenCalledTimes(2);
    expect(statuses).toEqual([]);
  });

  /** A retry for a workspace the user has already left must not land. */
  it("abandons the retries when the workspace changes under them", async () => {
    selectMock.mockImplementation(async () => { throw new Error("not attached yet"); });
    const { update } = await render({ activeSessionId: "$1", clientId: "client-1" });
    await update({ activeSessionId: "$2", clientId: "client-1" });
    selectMock.mockClear();
    await exhaustRetries();
    // Both halves: the retries kept going for the workspace that is *now*
    // shown, and none of them was for the one the user left. Asserting only
    // the second half would also pass if the retry loop were deleted.
    expect(selectMock.mock.calls.length).toBe(VISIBLE_SESSION_RETRIES);
    expect(selectMock.mock.calls.every(([, sessionId]) => sessionId === "$2")).toBe(true);
  });

  /**
   * The timed budget is not the only clock. What actually ends the host's
   * refusal is a reconciliation attaching the session's control client, and on
   * a slow link with several sessions that can land after the budget is gone —
   * leaving the workspace at tmux's 80x24 default with nothing left to try.
   */
  it("tries again on a topology change after the timed budget is gone", async () => {
    selectMock.mockImplementation(async () => { throw new Error("session has no control client"); });
    const { update } = await render({ activeSessionId: "$1", clientId: "client-1", topologyGeneration: 1 });
    await exhaustRetries();
    expect(selectMock).toHaveBeenCalledTimes(VISIBLE_SESSION_RETRIES + 1);

    selectMock.mockClear();
    selectMock.mockImplementation(async () => undefined);
    await update({ activeSessionId: "$1", clientId: "client-1", topologyGeneration: 2 });
    expect(selectMock.mock.calls).toEqual([["client-1", "$1"]]);
  });

  /**
   * …but a topology change is not a reason to re-send something the host has
   * already accepted. Every window rename produces one, and each re-send is a
   * `refresh-client -C` the desktop's own dedupe exists to avoid.
   */
  it("does not re-send a selection the host already took", async () => {
    const { update } = await render({ activeSessionId: "$1", clientId: "client-1", topologyGeneration: 1 });
    expect(selectMock).toHaveBeenCalledTimes(1);
    await update({ activeSessionId: "$1", clientId: "client-1", topologyGeneration: 2 });
    await update({ activeSessionId: "$1", clientId: "client-1", topologyGeneration: 3 });
    expect(selectMock).toHaveBeenCalledTimes(1);
  });

  /**
   * A host that refuses permanently — an older helper that does not know the
   * operation — must not cost a toast and a fresh budget of round trips on
   * every window rename. It keeps one cheap attempt per topology change, so a
   * refusal that turns out to have been transient is still recovered from, and
   * says nothing more.
   */
  it("says a permanent refusal once, and keeps trying cheaply", async () => {
    selectMock.mockImplementation(async () => { throw new Error("unsupported operation"); });
    const statuses: string[] = [];
    const props = { activeSessionId: "$1", clientId: "client-1", onStatus: (message: string) => statuses.push(message) };
    const { update } = await render({ ...props, topologyGeneration: 1 });
    await exhaustRetries();
    expect(selectMock).toHaveBeenCalledTimes(VISIBLE_SESSION_RETRIES + 1);
    expect(statuses).toHaveLength(1);

    selectMock.mockClear();
    for (const generation of [2, 3, 4]) {
      await update({ ...props, topologyGeneration: generation });
      await exhaustRetries();
    }
    expect(selectMock).toHaveBeenCalledTimes(3);
    expect(statuses).toHaveLength(1);
  });

  /**
   * Two selections outstanding at once race for the transport on the other
   * side of the IPC boundary, and an earlier one landing last leaves the host
   * sizing from the workspace the user has left. Chained, so the host is never
   * asked two things at once — and a superseded answer never records itself as
   * the current fact, which is what would stop the correct one being re-sent.
   */
  it("never has two selections outstanding, and ignores a superseded answer", async () => {
    const settle: Array<() => void> = [];
    selectMock.mockImplementation(() => new Promise<undefined>((resolve) => {
      settle.push(() => resolve(undefined));
    }));
    const { update } = await render({ activeSessionId: "$1", clientId: "client-1" });
    await update({ activeSessionId: "$2", clientId: "client-1" });
    expect(selectMock.mock.calls).toEqual([["client-1", "$1"]]);

    // The first lands; only now may the second be asked.
    await act(async () => { settle[0](); });
    expect(selectMock.mock.calls).toEqual([["client-1", "$1"], ["client-1", "$2"]]);
    await act(async () => { settle[1](); });

    // And the superseded first answer did not record `$1` as the fact: a
    // return to it is still sent.
    selectMock.mockClear();
    selectMock.mockImplementation(async () => undefined);
    await update({ activeSessionId: "$1", clientId: "client-1" });
    expect(selectMock.mock.calls).toEqual([["client-1", "$1"]]);
  });
});
