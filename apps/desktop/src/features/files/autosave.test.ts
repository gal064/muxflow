import { afterEach, describe, expect, it, vi } from "vitest";
import { AutosaveController, type AutosaveView } from "./autosave";

afterEach(() => vi.useRealTimers());

describe("AutosaveController", () => {
  it("debounces edits for 150ms and records the authoritative generation", async () => {
    vi.useFakeTimers();
    const views: AutosaveView[] = [];
    const save = vi.fn(async (_snapshot, operationId: string) => ({ path: "/r/a", generation: "2", operationId, sizeBytes: "4" }));
    const controller = new AutosaveController({ content: "one", generation: "1", lineEnding: "lf" }, save, (view) => views.push({ ...view }));
    controller.edit("two");
    controller.edit("last");
    await vi.advanceTimersByTimeAsync(149);
    expect(save).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(save).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0][0]).toMatchObject({ content: "last", generation: "1", lineEnding: "lf" });
    expect(controller.current()).toMatchObject({ content: "last", generation: "2", state: "saved" });
    expect(views.map((view) => view.state)).toEqual(["dirty", "dirty", "saving", "saved"]);
  });

  it("uses immediate last-writer-wins external content and cancels a pending local save", async () => {
    vi.useFakeTimers();
    const save = vi.fn();
    const controller = new AutosaveController({ content: "one", generation: "1", lineEnding: "crlf" }, save, vi.fn());
    controller.edit("local");
    controller.external({ content: "agent", generation: "9", lineEnding: "crlf" });
    await vi.advanceTimersByTimeAsync(500);
    expect(save).not.toHaveBeenCalled();
    expect(controller.current()).toMatchObject({ content: "agent", generation: "9", lineEnding: "crlf", state: "saved" });
  });

  it("surfaces save errors and a later edit can recover", async () => {
    vi.useFakeTimers();
    const save = vi.fn()
      .mockRejectedValueOnce(new Error("permission denied"))
      .mockImplementationOnce(async (_snapshot, operationId: string) => ({ path: "/r/a", generation: "3", operationId, sizeBytes: "3" }));
    const controller = new AutosaveController({ content: "a", generation: "1", lineEnding: "lf" }, save, vi.fn());
    controller.edit("b");
    await vi.advanceTimersByTimeAsync(150);
    expect(controller.current()).toMatchObject({ state: "error", error: "Error: permission denied" });
    controller.edit("ccc");
    await vi.advanceTimersByTimeAsync(150);
    expect(controller.current()).toMatchObject({ state: "saved", generation: "3", content: "ccc" });
  });

  it("serializes an edit made during an in-flight save onto the returned generation", async () => {
    vi.useFakeTimers();
    let finishFirst!: (value: { path: string; generation: string; operationId: string; sizeBytes: string }) => void;
    const first = new Promise<{ path: string; generation: string; operationId: string; sizeBytes: string }>((resolve) => { finishFirst = resolve; });
    const save = vi.fn()
      .mockImplementationOnce((_snapshot, operationId: string) => first.then((value) => ({ ...value, operationId })))
      .mockImplementationOnce(async (_snapshot, operationId: string) => ({ path: "/r/a", generation: "3", operationId, sizeBytes: "3" }));
    const controller = new AutosaveController({ content: "a", generation: "1", lineEnding: "lf" }, save, vi.fn());
    controller.edit("b");
    await vi.advanceTimersByTimeAsync(150);
    controller.edit("ccc");
    await vi.advanceTimersByTimeAsync(150);
    finishFirst({ path: "/r/a", generation: "2", operationId: "ignored", sizeBytes: "1" });
    await vi.runAllTimersAsync();
    await controller.flush();
    expect(save).toHaveBeenCalledTimes(2);
    expect(save.mock.calls[1][0]).toMatchObject({ content: "ccc", generation: "2" });
    expect(controller.current()).toMatchObject({ content: "ccc", generation: "3", state: "saved" });
  });

  it("does not let an in-flight local completion overwrite a newer external snapshot", async () => {
    vi.useFakeTimers();
    let finish!: (value: { path: string; generation: string; operationId: string; sizeBytes: string }) => void;
    let localOperation = "";
    const pending = new Promise<{ path: string; generation: string; operationId: string; sizeBytes: string }>((resolve) => { finish = resolve; });
    const controller = new AutosaveController(
      { content: "old", generation: "1", lineEnding: "lf" },
      vi.fn((_snapshot, operationId: string) => { localOperation = operationId; return pending.then((value) => ({ ...value, operationId })); }),
      vi.fn(),
    );
    controller.edit("local");
    await vi.advanceTimersByTimeAsync(150);
    controller.external({ content: "agent", generation: "9", lineEnding: "lf" }, "agent-op");
    finish({ path: "/r/a", generation: "2", operationId: "ignored", sizeBytes: "5" });
    await Promise.resolve();
    await Promise.resolve();
    expect(controller.current()).toEqual({ content: "agent", generation: "9", lineEnding: "lf", state: "saved" });
    controller.external({ content: "local", generation: "2", lineEnding: "lf" }, localOperation);
    expect(controller.current()).toEqual({ content: "local", generation: "2", lineEnding: "lf", state: "saved" });
  });

  it("close flush awaits an in-flight save without duplicating its stale content", async () => {
    vi.useFakeTimers();
    let finish!: (value: { path: string; generation: string; operationId: string; sizeBytes: string }) => void;
    const pending = new Promise<{ path: string; generation: string; operationId: string; sizeBytes: string }>((resolve) => { finish = resolve; });
    const save = vi.fn((_snapshot, operationId: string) => pending.then((value) => ({ ...value, operationId })));
    const controller = new AutosaveController({ content: "old", generation: "1", lineEnding: "lf" }, save, vi.fn());
    controller.edit("local");
    await vi.advanceTimersByTimeAsync(150);
    const flushing = controller.flush();
    controller.external({ content: "external", generation: "9", lineEnding: "lf" }, "agent");
    finish({ path: "/r/a", generation: "2", operationId: "ignored", sizeBytes: "5" });
    await flushing;
    expect(save).toHaveBeenCalledTimes(1);
    expect(controller.current()).toMatchObject({ content: "external", generation: "9", state: "saved" });
  });
});
