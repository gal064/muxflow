// @vitest-environment jsdom
// A real DOM, because the shell's dialogs and menus move focus and read
// `document.activeElement`; a hand-built `window` stub cannot answer that.
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { defaultAppState } from "../features/shell/types";
import { App } from "./App";

const invokeMock = vi.hoisted(() => vi.fn());
const listenMock = vi.hoisted(() => vi.fn(async () => () => undefined));
const closeRequestedMock = vi.hoisted(() => vi.fn(async () => () => undefined));

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class { onmessage?: (value: ArrayBuffer) => void },
  invoke: invokeMock,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@tauri-apps/api/window", () => ({
  availableMonitors: vi.fn(async () => [{ position: { x: 0, y: 0 }, size: { width: 1920, height: 1080 }, scaleFactor: 1 }]),
  getCurrentWindow: () => ({
    destroy: vi.fn(), innerSize: vi.fn(async () => ({ width: 1200, height: 800 })), isMaximized: vi.fn(async () => false), maximize: vi.fn(),
    onCloseRequested: closeRequestedMock, onMoved: vi.fn(async () => () => undefined), onResized: vi.fn(async () => () => undefined),
    outerPosition: vi.fn(async () => ({ x: 0, y: 0 })), scaleFactor: vi.fn(async () => 1), setPosition: vi.fn(async () => undefined), setSize: vi.fn(async () => undefined),
  }),
  primaryMonitor: vi.fn(async () => ({ position: { x: 0, y: 0 }, size: { width: 1920, height: 1080 }, scaleFactor: 1 })),
}));

describe("App orchestration", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockClear();
    closeRequestedMock.mockClear();
    Object.assign(globalThis, {
      IS_REACT_ACT_ENVIRONMENT: true,
      // jsdom ships neither of these, and the shell measures the terminal
      // surface with one and never resizes the window in this test.
      ResizeObserver: class { observe() {} unobserve() {} disconnect() {} },
    });
  });

  it("hydrates the saved SSH profile before starting exactly one intended bridge", async () => {
    let finishProfiles: ((value: unknown) => void) | undefined;
    const profiles = new Promise((resolve) => { finishProfiles = resolve; });
    invokeMock.mockImplementation((command: string) => {
      if (command === "load_app_state") return Promise.resolve(defaultAppState);
      if (command === "list_host_profiles") return profiles;
      if (command === "start_terminal") return Promise.resolve("client-1");
      return Promise.resolve(undefined);
    });
    let renderer: ReactTestRenderer;
    await act(async () => { renderer = create(<App />); });
    expect(invokeMock.mock.calls.filter(([command]) => command === "start_terminal")).toHaveLength(0);

    await act(async () => {
      finishProfiles?.({
        lastProfileId: "remote-a",
        profiles: [{ id: "remote-a", label: "qa", connection: { mode: "ssh", profileId: "remote-a", target: "qa-host" } }],
      });
      await profiles;
    });
    const starts = invokeMock.mock.calls.filter(([command]) => command === "start_terminal");
    expect(starts).toHaveLength(1);
    expect(starts[0][1]).toMatchObject({ connection: { mode: "ssh", profileId: "remote-a", target: "qa-host" } });
    await act(async () => renderer!.unmount());
  });

  it("reconstructs an already-selected local bridge when Connect is used as retry", async () => {
    let client = 0;
    invokeMock.mockImplementation((command: string) => {
      if (command === "load_app_state") return Promise.resolve(defaultAppState);
      if (command === "list_host_profiles") return Promise.resolve({
        lastProfileId: "local",
        profiles: [{ id: "local", label: "Local", connection: { mode: "local" } }],
      });
      if (command === "start_terminal") return Promise.resolve(`client-${++client}`);
      return Promise.resolve(undefined);
    });
    let renderer: ReactTestRenderer;
    await act(async () => { renderer = create(<App />); });
    expect(invokeMock.mock.calls.filter(([command]) => command === "start_terminal")).toHaveLength(1);

    // Connection settings are no longer docked in the shell; the sidebar's host
    // row — the app's one resting connection indicator — is what opens them.
    const hostRow = renderer!.root.findAllByType("button")
      .find((button) => String(button.props["aria-label"] ?? "").startsWith("Host "));
    expect(hostRow).toBeDefined();
    await act(async () => { hostRow!.props.onClick(); });
    const connect = renderer!.root.findAllByType("button")
      .find((button) => button.props.children === "Connect");
    expect(connect).toBeDefined();
    await act(async () => { connect!.props.onClick(); });

    expect(invokeMock.mock.calls.filter(([command]) => command === "start_terminal")).toHaveLength(2);
    await act(async () => renderer!.unmount());
  });
});
