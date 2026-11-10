// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, create } from "react-test-renderer";
import { usePersistedAppState } from "./usePersistedAppState";
import { defaultAppState, type PersistedAppState } from "./types";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  window: {
    destroy: vi.fn(),
    onCloseRequested: vi.fn(async () => () => undefined),
    onMoved: vi.fn(async () => () => undefined),
    onResized: vi.fn(async () => () => undefined),
  },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/window", () => ({
  availableMonitors: vi.fn(),
  getCurrentWindow: () => mocks.window,
  primaryMonitor: vi.fn(),
}));

const LEGACY_KEY = "muxflow.agent-sounds.v1";
const legacy = { enabled: false, blocked: "none", completed: "subtle", volume: 0.2 };
const saved = (shell: Record<string, unknown> = {}) => ({ ...defaultAppState, shell });

let latest: PersistedAppState | undefined;
// Stable, as the app's `setStatus` is: a new one per render would rebuild the persistence.
const report = () => undefined;
function Probe() {
  latest = usePersistedAppState(report, "linux").appState;
  return null;
}

async function mount(loadedState: unknown) {
  mocks.invoke.mockImplementation(async (command: string) => command === "load_app_state" ? loadedState : undefined);
  await act(async () => { create(<Probe />); });
}

const savedStates = () => mocks.invoke.mock.calls
  .filter(([command]) => command === "save_app_state")
  .map(([, args]) => (args as { state: PersistedAppState }).state);

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  latest = undefined;
  mocks.invoke.mockReset();
  localStorage.clear();
});

describe("one-time import of the sounds kept in localStorage", () => {
  it("moves them into app state, saves, and only then drops the old copy", async () => {
    localStorage.setItem(LEGACY_KEY, JSON.stringify(legacy));
    let finishSaves!: () => void;
    const gate = new Promise<void>((resolve) => { finishSaves = resolve; });
    mocks.invoke.mockImplementation((command: string) => command === "load_app_state" ? Promise.resolve(saved()) : gate);
    await act(async () => { create(<Probe />); });
    expect(latest?.shell.agentSounds).toEqual(legacy);
    expect(savedStates()[0]?.shell.agentSounds).toEqual(legacy);
    expect(localStorage.getItem(LEGACY_KEY)).not.toBeNull();
    await act(async () => { finishSaves(); await gate; });
    expect(localStorage.getItem(LEGACY_KEY)).toBeNull();
  });

  it("keeps the old copy when the save fails", async () => {
    localStorage.setItem(LEGACY_KEY, JSON.stringify(legacy));
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "load_app_state") return saved();
      throw new Error("disk full");
    });
    await act(async () => { create(<Probe />); });
    expect(latest?.shell.agentSounds).toEqual(legacy);
    expect(localStorage.getItem(LEGACY_KEY)).not.toBeNull();
  });

  it("leaves what app state already has alone", async () => {
    const current = { enabled: true, blocked: "subtle", completed: "none", volume: 0.9 };
    localStorage.setItem(LEGACY_KEY, JSON.stringify(legacy));
    await mount(saved({ agentSounds: current }));
    expect(latest?.shell.agentSounds).toEqual(current);
    expect(localStorage.getItem(LEGACY_KEY)).not.toBeNull();
  });

  it("imports nothing when there is nothing to import", async () => {
    await mount(saved());
    expect(latest?.shell.agentSounds).toBeUndefined();
  });
});
