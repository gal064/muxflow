// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, create } from "react-test-renderer";
import type { Platform } from "../../commands/registry";
import { useWindowChrome, type WindowControls } from "./useWindowChrome";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  record: vi.fn(),
  window: {
    close: vi.fn(async () => undefined),
    isMaximized: vi.fn(async () => false),
    minimize: vi.fn(async () => undefined),
    onResized: vi.fn(async (_listener: () => void) => () => undefined),
    toggleMaximize: vi.fn(async () => undefined),
  },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => mocks.window }));
vi.mock("../../diagnostics/incidents", () => ({ recordIncident: mocks.record }));

let latest: WindowControls | undefined;
function Probe({ platform }: { platform: Platform }) {
  latest = useWindowChrome(platform);
  return null;
}

async function mount(platform: Platform) {
  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<Probe platform={platform} />); });
  return renderer;
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  latest = undefined;
  vi.clearAllMocks();
});
afterEach(() => { latest = undefined; });

describe("useWindowChrome", () => {
  it("offers window buttons on a floating Linux desktop and journals why", async () => {
    mocks.invoke.mockResolvedValue({ mode: "buttons", desktop: "GNOME", reason: "floating desktop" });
    mocks.window.isMaximized.mockResolvedValue(true);
    const renderer = await mount("linux");
    expect(mocks.invoke).toHaveBeenCalledWith("window_chrome");
    expect(mocks.record).toHaveBeenCalledWith("window.chrome", { mode: "buttons", desktop: "GNOME", reason: "floating desktop" });
    expect(latest?.maximized).toBe(true);

    // Close goes through `close()`, which raises close-requested and so the
    // app-state flush, never straight to `destroy()`.
    await act(async () => { latest?.onClose(); });
    expect(mocks.window.close).toHaveBeenCalledOnce();
    await act(async () => { latest?.onMinimize(); latest?.onToggleMaximize(); });
    expect(mocks.window.minimize).toHaveBeenCalledOnce();
    expect(mocks.window.toggleMaximize).toHaveBeenCalledOnce();
    await act(async () => renderer.unmount());
  });

  it("offers none on a tiling desktop or under the native bar", async () => {
    for (const mode of ["bare", "native"]) {
      mocks.invoke.mockResolvedValue({ mode, desktop: "Hyprland", reason: "tiling desktop" });
      const renderer = await mount("linux");
      expect(latest).toBeUndefined();
      await act(async () => renderer.unmount());
    }
  });

  it("never asks on macOS, whose title bar is native", async () => {
    const renderer = await mount("mac");
    expect(mocks.invoke).not.toHaveBeenCalled();
    expect(latest).toBeUndefined();
    await act(async () => renderer.unmount());
  });

  it("journals a window button that fails", async () => {
    mocks.invoke.mockResolvedValue({ mode: "buttons", desktop: "KDE", reason: "floating desktop" });
    mocks.window.minimize.mockRejectedValueOnce(new Error("not allowed"));
    const renderer = await mount("linux");
    await act(async () => { latest?.onMinimize(); });
    expect(mocks.record).toHaveBeenCalledWith("window.controlFailed", { action: "minimize", error: "Error: not allowed" });
    await act(async () => renderer.unmount());
  });
});
