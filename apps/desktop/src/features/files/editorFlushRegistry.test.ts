import { describe, expect, it, vi } from "vitest";
import { editorFlushRegistry } from "./editorFlushRegistry";

describe("editorFlushRegistry", () => {
  it("waits for mounted and already-unmounted editor saves before close", async () => {
    let finishPending!: () => void;
    const pending = new Promise<void>((resolve) => { finishPending = resolve; });
    editorFlushRegistry.track(pending);
    const mounted = vi.fn(async () => undefined);
    const unregister = editorFlushRegistry.register("mounted", mounted);
    let finished = false;
    const close = editorFlushRegistry.flushAll().then(() => { finished = true; });
    await Promise.resolve();
    expect(mounted).toHaveBeenCalledOnce();
    expect(finished).toBe(false);
    finishPending();
    await close;
    expect(finished).toBe(true);
    unregister();
  });

  it("rejects the close barrier when an editor save fails", async () => {
    const unregister = editorFlushRegistry.register("failed", async () => { throw new Error("permission denied"); });
    await expect(editorFlushRegistry.flushAll()).rejects.toThrow("could not be saved");
    unregister();
  });
});
