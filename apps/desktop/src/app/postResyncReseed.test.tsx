// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TerminalEventHub } from "../features/terminal/TerminalEventHub";
import type { TerminalEvent } from "../features/terminal/api";
import type { ConnectionSpec } from "./types";
import { useAppConnectionController } from "./useAppConnectionController";

const invokeMock = vi.hoisted(() => vi.fn());
const startTerminalMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class { onmessage?: (value: ArrayBuffer) => void },
  invoke: invokeMock,
}));
// The bridge is not under test; the phases it reports are.
vi.mock("../features/terminal/api", async (importOriginal) => ({
  ...await importOriginal<typeof import("../features/terminal/api")>(),
  startTerminal: startTerminalMock,
  stopTerminal: vi.fn(async () => undefined),
  requestTerminalSeed: vi.fn(async () => undefined),
}));

const connectionState = (state: "connecting" | "connected" | "reconnecting" | "resyncing"): TerminalEvent =>
  ({ kind: "connectionState", state, sequence: 0 });

/** The controller alone, with its real hub exposed for the assertion. */
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
  const client = { publishWireEvent: vi.fn(), publishWireSnapshot: vi.fn() } as never;
  // Stable, like the shell's own `useState` setter: the controller keys its
  // profile load on it, and a fresh function per render re-runs that forever.
  const setStatus = () => undefined;
  let hub!: TerminalEventHub;
  function Harness() {
    const controller = useAppConnectionController({
      agentClient: client, fileClient: client, gitClient: client, setStatus,
    });
    hub = controller.hub;
    return null;
  }
  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<Harness />); });
  return { hub, renderer, publish: (event: TerminalEvent) => publish(event) };
}

describe("a resync the connection survived", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    startTerminalMock.mockReset();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  });

  it("reseeds the mounted panes when the link comes back from resyncing", async () => {
    const { hub, publish, renderer } = await connected();
    // Spied on the real hub instance the mounted panes subscribe to, because
    // reaching *that* one is the whole behavior.
    const reseed = vi.spyOn(hub, "reseedSubscribedPanes");

    // A first attach: its seeds are already on their way with it.
    await act(async () => { publish(connectionState("connecting")); });
    await act(async () => { publish(connectionState("connected")); });
    expect(reseed).not.toHaveBeenCalled();

    // A native in-place repair: ordering is whole again and the connection was
    // never replaced, so nothing else re-establishes the pane screens.
    await act(async () => { publish(connectionState("resyncing")); });
    await act(async () => { publish(connectionState("connected")); });
    expect(reseed.mock.calls).toEqual([["post-resync reseed"]]);

    // A reconnect re-attaches through the bridge and seeds itself.
    await act(async () => { publish(connectionState("reconnecting")); });
    await act(async () => { publish(connectionState("connected")); });
    expect(reseed).toHaveBeenCalledTimes(1);
    await act(async () => renderer.unmount());
  });
});
