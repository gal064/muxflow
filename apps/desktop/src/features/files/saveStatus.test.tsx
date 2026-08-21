// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SaveState } from "./autosave";
import { projectVisibleSaveState, SAVING_STATUS_DELAY_MILLIS, useVisibleSaveState } from "./saveStatus";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function SaveStatusProbe({ state, writeAvailable = true }: { state: SaveState | undefined; writeAvailable?: boolean }) {
  const visible = useVisibleSaveState(state, writeAvailable);
  return <span>{visible ?? "hidden"}</span>;
}

function shown(renderer: ReactTestRenderer): string {
  return renderer.root.findByType("span").children.join("");
}

afterEach(() => vi.useRealTimers());

describe("useVisibleSaveState", () => {
  it("shows only actionable immediate states", () => {
    expect(projectVisibleSaveState("saved", true, true)).toBeUndefined();
    expect(projectVisibleSaveState("dirty", true, true)).toBeUndefined();
    expect(projectVisibleSaveState("dirty", true, false)).toBe("dirty");
    expect(projectVisibleSaveState("error", true, true)).toBe("error");
    expect(projectVisibleSaveState(undefined, true, true)).toBeUndefined();
  });

  it("keeps a fast save calm and never publishes a saved confirmation", async () => {
    vi.useFakeTimers();
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<SaveStatusProbe state="saved" />); });
    expect(shown(renderer)).toBe("hidden");

    await act(async () => { renderer.update(<SaveStatusProbe state="dirty" />); });
    expect(shown(renderer)).toBe("hidden");
    await act(async () => { renderer.update(<SaveStatusProbe state="saving" />); });
    expect(shown(renderer)).toBe("hidden");

    await act(async () => { await vi.advanceTimersByTimeAsync(SAVING_STATUS_DELAY_MILLIS - 1); });
    expect(shown(renderer)).toBe("hidden");
    await act(async () => { renderer.update(<SaveStatusProbe state="saved" />); });
    expect(shown(renderer)).toBe("hidden");

    await act(async () => { await vi.advanceTimersByTimeAsync(SAVING_STATUS_DELAY_MILLIS); });
    expect(shown(renderer), "a cancelled saving timer published after completion").toBe("hidden");
    await act(async () => { renderer.unmount(); });
  });

  it("shows progress after a slow save crosses the delay", async () => {
    vi.useFakeTimers();
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<SaveStatusProbe state="dirty" />); });
    await act(async () => { renderer.update(<SaveStatusProbe state="saving" />); });

    await act(async () => { await vi.advanceTimersByTimeAsync(SAVING_STATUS_DELAY_MILLIS); });
    expect(shown(renderer)).toBe("saving");
    await act(async () => { renderer.unmount(); });
  });

  it("shows dirty when the editor has lost write authority", async () => {
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<SaveStatusProbe state="dirty" writeAvailable={false} />); });
    expect(shown(renderer)).toBe("dirty");
    await act(async () => { renderer.unmount(); });
  });

  it("shows an error immediately and cancels delayed progress", async () => {
    vi.useFakeTimers();
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<SaveStatusProbe state="dirty" />); });
    await act(async () => { renderer.update(<SaveStatusProbe state="saving" />); });
    await act(async () => { await vi.advanceTimersByTimeAsync(SAVING_STATUS_DELAY_MILLIS - 1); });
    await act(async () => { renderer.update(<SaveStatusProbe state="error" />); });
    expect(shown(renderer)).toBe("error");

    await act(async () => { await vi.advanceTimersByTimeAsync(SAVING_STATUS_DELAY_MILLIS); });
    expect(shown(renderer), "delayed progress displaced the persistent failure").toBe("error");
    await act(async () => { renderer.unmount(); });
  });

  it("cancels its pending saving timer when the toolbar unmounts", async () => {
    vi.useFakeTimers();
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<SaveStatusProbe state="saving" />); });
    expect(vi.getTimerCount()).toBe(1);

    await act(async () => { renderer.unmount(); });
    expect(vi.getTimerCount()).toBe(0);
  });
});
