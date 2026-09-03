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
/** The host row anchors its menu under itself; the test renderer has no box to measure. */
const hostRowClick = { currentTarget: { getBoundingClientRect: () => ({ left: 0, bottom: 0 }) } };

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
      if (command === "stop_terminal") return Promise.resolve(undefined);
      if (command === "bridge_final_totals") return Promise.resolve({ cumulativeFrameCount: 0, cumulativeByteLength: 0, quiesced: true });
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

  /**
   * The picker names the machine the form belongs to, and Connect keeps it.
   *
   * Connect used to derive the profile id from the form values on every press,
   * so correcting one host's address connected to the corrected host and left
   * the original in the list beside it — two entries for one machine, and a
   * picker that grew an entry every time an address changed.
   */
  it("edits the picked host in place, and adds one only when asked to", async () => {
    const saved: unknown[] = [];
    invokeMock.mockImplementation((command: string, request: { profile?: unknown }) => {
      if (command === "load_app_state") return Promise.resolve(defaultAppState);
      if (command === "list_host_profiles") return Promise.resolve({
        lastProfileId: "remote-a",
        profiles: [{ id: "remote-a", label: "qa", connection: { mode: "ssh", profileId: "remote-a", target: "qa-host" } }],
      });
      if (command === "save_host_profile") saved.push(request.profile);
      if (command === "start_terminal") return Promise.resolve("client-1");
      if (command === "stop_terminal") return Promise.resolve(undefined);
      if (command === "bridge_final_totals") return Promise.resolve({ cumulativeFrameCount: 0, cumulativeByteLength: 0, quiesced: true });
      return Promise.resolve(undefined);
    });
    let renderer: ReactTestRenderer;
    await act(async () => { renderer = create(<App />); });
    const openSettings = async () => {
      // The host row opens the host menu; settings is its last item.
      const hostRow = renderer.root.findAllByType("button")
        .find((button) => String(button.props["aria-label"] ?? "").startsWith("Host "))!;
      await act(async () => { hostRow.props.onClick(hostRowClick); });
      await act(async () => { renderer.root.findByProps({ "data-menu-item": "settings" }).props.onClick(); });
    };
    const sshTarget = () => renderer.root.findAllByType("input")
      .find((input) => input.props.placeholder === "Host or config alias")!;
    const picker = () => renderer.root.findByProps({ "aria-label": "Host" });
    const connect = async () => {
      const button = renderer.root.findAllByType("button").find((node) => node.props.children === "Connect")!;
      await act(async () => { button.props.onClick(); });
    };

    await openSettings();
    // The saved host is what the form is editing, so its address is what is
    // being corrected — not the seed for a new entry.
    expect(picker().props.value).toBe("remote-a");
    await act(async () => { sshTarget().props.onChange({ target: { value: "qa-host-2" } }); });
    expect(picker().props.value).toBe("remote-a");
    await connect();
    expect(saved).toEqual([{
      id: "remote-a",
      label: "qa-host-2",
      connection: { mode: "ssh", profileId: "remote-a", target: "qa-host-2" },
      shown: true,
    }]);

    // A second machine is a deliberate act, and it starts from an empty form
    // with no saved host behind it.
    await openSettings();
    const add = renderer!.root.findAllByType("button").find((node) => String(node.children).includes("Add host"))!;
    await act(async () => { add.props.onClick(); });
    expect(sshTarget().props.value).toBe("");
    expect(picker().props.value).toBe("");
    expect(renderer!.root.findAllByType("option").map((node) => node.props.children)).toContain("New host…");
    await act(async () => { sshTarget().props.onChange({ target: { value: "staging-host" } }); });
    await connect();
    expect(saved).toHaveLength(2);
    expect(saved[1]).toMatchObject({ label: "staging-host", connection: { target: "staging-host" } });
    expect((saved[1] as { id: string }).id).not.toBe("remote-a");
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
      if (command === "stop_terminal") return Promise.resolve(undefined);
      if (command === "bridge_final_totals") return Promise.resolve({ cumulativeFrameCount: 0, cumulativeByteLength: 0, quiesced: true });
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
    await act(async () => { hostRow!.props.onClick(hostRowClick); });
    await act(async () => { renderer!.root.findByProps({ "data-menu-item": "settings" }).props.onClick(); });
    const connect = renderer!.root.findAllByType("button")
      .find((button) => button.props.children === "Connect");
    expect(connect).toBeDefined();
    await act(async () => { connect!.props.onClick(); });

    expect(invokeMock.mock.calls.filter(([command]) => command === "start_terminal")).toHaveLength(2);
    await act(async () => renderer!.unmount());
  });
});
