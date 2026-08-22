// @vitest-environment jsdom
import { useEffect, useState } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AutosaveController, type SaveState } from "./autosave";
import { projectVisibleSaveState, SLOW_SAVE_DELAY_MILLIS, useVisibleSaveState, type VisibleSaveState } from "./saveStatus";
import type { WriteTextResult } from "./types";

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
    expect(projectVisibleSaveState("dirty", false, true)).toBeUndefined();
    expect(projectVisibleSaveState("saving", false, true)).toBeUndefined();
    // A save owed long enough to report reads the same whether the controller
    // is waiting on its debounce or on the host.
    expect(projectVisibleSaveState("dirty", true, true)).toBe("saving");
    expect(projectVisibleSaveState("saving", true, true)).toBe("saving");
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

    await act(async () => { await vi.advanceTimersByTimeAsync(SLOW_SAVE_DELAY_MILLIS - 1); });
    expect(shown(renderer)).toBe("hidden");
    await act(async () => { renderer.update(<SaveStatusProbe state="saved" />); });
    expect(shown(renderer)).toBe("hidden");

    await act(async () => { await vi.advanceTimersByTimeAsync(SLOW_SAVE_DELAY_MILLIS); });
    expect(shown(renderer), "a cancelled saving timer published after completion").toBe("hidden");
    await act(async () => { renderer.unmount(); });
  });

  it("measures one delay across the whole save the keystroke owes", async () => {
    vi.useFakeTimers();
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<SaveStatusProbe state="dirty" />); });

    // Half the delay is spent in the debounce and half in the write. The
    // handover between them is not a reason to start counting again.
    await act(async () => { await vi.advanceTimersByTimeAsync(SLOW_SAVE_DELAY_MILLIS / 2); });
    await act(async () => { renderer.update(<SaveStatusProbe state="saving" />); });
    await act(async () => { await vi.advanceTimersByTimeAsync(SLOW_SAVE_DELAY_MILLIS / 2 - 1); });
    expect(shown(renderer)).toBe("hidden");

    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(shown(renderer), "the dirty → saving handover restarted the delay").toBe("saving");
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
    await act(async () => { await vi.advanceTimersByTimeAsync(SLOW_SAVE_DELAY_MILLIS - 1); });
    await act(async () => { renderer.update(<SaveStatusProbe state="error" />); });
    expect(shown(renderer)).toBe("error");

    await act(async () => { await vi.advanceTimersByTimeAsync(SLOW_SAVE_DELAY_MILLIS); });
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

// ── Typing against a real controller ─────────────────────────────────────
//
// The status used to be read off the controller's exact state, and the bug it
// caused was invisible to every test above: each state was correct in
// isolation, and only a real sequence of keystoke → debounce → write → next
// keystroke made the label blink. So this drives the real AutosaveController
// at four host latencies and records every word the toolbar would have shown.

/** The toolbar's own wording, mirrored from AppTabSurface. */
const WORDING: Record<VisibleSaveState, string> = { saving: "Saving…", dirty: "Unsaved", error: "Save failed" };

/** Gaps between keystrokes, cycled: a plausible mix of runs and small pauses. */
const KEYSTROKE_GAPS = [60, 140, 90, 260, 400];
const KEYSTROKES = 44;
/** How finely the clock is stepped, so no label can flash between two reads. */
const OBSERVED_SLICE = 10;

interface Shown { label: string; at: number }

function StatusRecorder({ shownLabels, subscribe }: {
  shownLabels: Shown[];
  subscribe(listener: (state: SaveState) => void): void;
}) {
  const [state, setState] = useState<SaveState>("saved");
  useEffect(() => subscribe(setState), [subscribe]);
  const visible = useVisibleSaveState(state, true);
  const label = visible ? WORDING[visible] : "";
  useEffect(() => { shownLabels.push({ label, at: Date.now() }); }, [label, shownLabels]);
  return <span>{label}</span>;
}

/**
 * Mounts the status region over a real controller whose host takes
 * `latencyMillis` to answer — `undefined` for a write that never lands.
 */
async function mountRecorder(latencyMillis: number | undefined) {
  vi.useFakeTimers();
  const recorded: Shown[] = [];
  let listener: (state: SaveState) => void = () => {};
  const subscribe = (next: (state: SaveState) => void) => { listener = next; };
  let generation = 0;
  const controller = new AutosaveController(
    { content: "", generation: "g0", lineEnding: "lf" },
    (snapshot, operationId) => new Promise<WriteTextResult>((resolve) => {
      if (latencyMillis === undefined) return;
      setTimeout(() => resolve({
        path: "/repo/note.txt", generation: `g${++generation}`, operationId, sizeBytes: String(snapshot.content.length),
      }), latencyMillis);
    }),
    (view) => listener(view.state),
  );

  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(<StatusRecorder shownLabels={recorded} subscribe={subscribe} />); });
  const started = Date.now();
  let typed = 0;
  return {
    controller,
    /** Every label displayed so far, in order, timed from the mount. */
    shown: (): Shown[] => recorded.map(({ label, at }) => ({ label, at: at - started })),
    labels: (): string[] => recorded.map((entry) => entry.label),
    /**
     * Walks the clock forward in {@link OBSERVED_SLICE} steps. A single jump
     * would let React collapse a whole appear-and-vanish into one commit —
     * exactly the flicker being measured — so time is watched, not skipped.
     */
    async advance(millis: number) {
      for (let elapsed = 0; elapsed < millis; elapsed += OBSERVED_SLICE) {
        await act(async () => { await vi.advanceTimersByTimeAsync(Math.min(OBSERVED_SLICE, millis - elapsed)); });
      }
    },
    async keystroke() { await act(async () => { controller.edit("x".repeat((typed += 1))); }); },
    async close() {
      await act(async () => { renderer.unmount(); });
      controller.dispose();
    },
  };
}

/** Types {@link KEYSTROKES} characters, then waits for the host to catch up. */
async function typeThrough(session: Awaited<ReturnType<typeof mountRecorder>>) {
  for (let index = 0; index < KEYSTROKES; index += 1) {
    await session.keystroke();
    await session.advance(KEYSTROKE_GAPS[index % KEYSTROKE_GAPS.length]);
  }
  await session.advance(5_000);
}

describe("the save status under sustained typing", () => {
  it.each([40, 120, 200])("stays silent while a %dms host keeps up", async (latency) => {
    const session = await mountRecorder(latency);
    await typeThrough(session);
    expect(session.labels(), "typing made the status region flicker").toEqual([""]);
    await session.close();
  });

  it("says it once, and only once, when the host falls a second behind", async () => {
    // At 400ms a write plus its debounce outlasts the longest gap in the
    // script, so a save is owed without interruption: the status is earned,
    // and the point is that it arrives once and holds rather than blinking
    // once per write. It clears when the host finally catches up.
    const session = await mountRecorder(400);
    await typeThrough(session);
    expect(session.labels()).toEqual(["", "Saving…", ""]);
    expect(session.shown()[1].at).toBe(SLOW_SAVE_DELAY_MILLIS);
    await session.close();
  });

  it("says it once at the delay when the write never lands at all", async () => {
    const session = await mountRecorder(undefined);
    await session.keystroke();
    await session.advance(SLOW_SAVE_DELAY_MILLIS - 1);
    expect(session.labels()).toEqual([""]);

    await session.advance(1);
    expect(session.shown()).toEqual([{ label: "", at: 0 }, { label: "Saving…", at: SLOW_SAVE_DELAY_MILLIS }]);

    // The stalled write keeps the controller cycling dirty ↔ saving under
    // every further keystroke; none of that reaches the toolbar.
    await typeThrough(session);
    expect(session.labels(), "a stalled save churned the status region").toEqual(["", "Saving…"]);
    await session.close();
  });
});
