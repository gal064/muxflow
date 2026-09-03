// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TerminalEvent } from "../features/terminal/api";
import type { ConnectionSpec } from "./types";
import { useAppConnectionController } from "./useAppConnectionController";

const invokeMock = vi.hoisted(() => vi.fn());
const startTerminalMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class { onmessage?: (value: ArrayBuffer) => void },
  invoke: invokeMock,
}));
// The bridge is not under test; the failures it repeats are.
vi.mock("../features/terminal/api", async (importOriginal) => ({
  ...await importOriginal<typeof import("../features/terminal/api")>(),
  startTerminal: startTerminalMock,
  stopTerminal: vi.fn(async () => undefined),
  requestTerminalSeed: vi.fn(async () => undefined),
}));

const phase = (state: "connected" | "disconnected" | "reconnecting"): TerminalEvent =>
  ({ kind: "connectionState", state, sequence: 0 });
const failure = (message: string): TerminalEvent => ({ kind: "error", message, sequence: 0 });

/** What the supervisor actually says, once per failed reconnect attempt. */
const unreachable = "OpenSSH control master failed: ssh: connect to host omarchy port 22: Undefined error: 0";
const refused = "OpenSSH control master failed: ssh: connect to host omarchy port 22: Connection refused";

/** The controller alone, with every status it asked the shell to show. */
async function connected() {
  let publish!: (event: TerminalEvent) => void;
  startTerminalMock.mockImplementation(async (
    _sessionId: string, _paneIds: string[], _connection: ConnectionSpec, _attach: boolean, onEvent: (event: TerminalEvent) => void,
  ) => {
    publish = onEvent;
    return "client-1";
  });
  invokeMock.mockImplementation((command: string) => {
    if (command === "list_host_profiles") {
      return Promise.resolve({
        schemaVersion: 1,
        lastProfileId: "local",
        profiles: [{ id: "local", label: "Local", connection: { mode: "local" } }],
      });
    }
    return Promise.resolve(undefined);
  });
  const client = { publishWireEvent: vi.fn(), publishWireSnapshot: vi.fn(), retireConnection: vi.fn() } as never;
  const statuses: string[] = [];
  // Stable, like the shell's own `useState` setter: the controller keys its
  // profile load on it, and a fresh function per render re-runs that forever.
  const setStatus = (status: string) => { statuses.push(status); };
  let detail = "";
  let reconnect = () => undefined as void;
  function Harness() {
    const controller = useAppConnectionController({
      agentClient: client, fileClient: client, gitClient: client, setStatus,
    });
    detail = controller.connectionDetail;
    reconnect = () => controller.setConnectionEpoch((value) => value + 1);
    return null;
  }
  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<Harness />); });
  return {
    renderer,
    publish: (event: TerminalEvent) => publish(event),
    notices: (message: string) => statuses.filter((status) => status === message).length,
    detail: () => detail,
    // What the disconnected strip's Reconnect button does: a new bridge on the
    // same connection.
    reconnect: () => reconnect(),
  };
}

describe("a link outage the supervisor reports once per attempt", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    startTerminalMock.mockReset();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  });

  it("shows a repeated failure once, and speaks again for new wording or a new outage", async () => {
    const { detail, notices, publish, renderer } = await connected();
    await act(async () => { publish(phase("connected")); });

    // The first failure of an outage arrives while the link is still believed
    // up, and always speaks.
    await act(async () => { publish(failure(unreachable)); });
    expect(notices(unreachable)).toBe(1);

    // The backoff ladder repeats it verbatim. The strip is already saying it.
    await act(async () => { publish(phase("disconnected")); });
    await act(async () => { publish(failure(unreachable)); publish(failure(unreachable)); });
    expect(notices(unreachable)).toBe(1);
    expect(detail()).toBe(unreachable);

    // A host that answers differently is news, and is announced once too.
    await act(async () => { publish(failure(refused)); publish(failure(refused)); });
    expect(notices(refused)).toBe(1);
    expect(detail()).toBe(refused);

    // The next outage starts from silence, whatever the last one said.
    await act(async () => { publish(phase("reconnecting")); publish(phase("connected")); });
    await act(async () => { publish(phase("disconnected")); });
    await act(async () => { publish(failure(refused)); });
    expect(notices(refused)).toBe(2);

    await act(async () => renderer.unmount());
  });

  it("answers a Reconnect press that fails exactly the way the last attempt did", async () => {
    const { notices, publish, reconnect, renderer } = await connected();
    await act(async () => { publish(phase("connected")); });
    await act(async () => { publish(failure(unreachable)); publish(phase("disconnected")); });
    expect(notices(unreachable)).toBe(1);

    // The user presses Reconnect on the strip. The bridge is replaced and never
    // reaches `connected`, so nothing else would clear the memory — and a
    // deliberate press whose only visible result is the strip it started from
    // reads as a button that does nothing.
    await act(async () => { reconnect(); });
    await act(async () => { publish(failure(unreachable)); });
    expect(notices(unreachable)).toBe(2);

    await act(async () => renderer.unmount());
  });
});
