import { afterEach, describe, expect, it, vi } from "vitest";
import { rowCommandRegistry } from "./rowCommands";

afterEach(() => {
  for (const surface of ["files", "git", "agents"] as const) rowCommandRegistry.publish(surface, undefined);
});

describe("row command registry", () => {
  it("routes a command to the surface that published it, and reports the row it acted on", () => {
    const files = vi.fn();
    const git = vi.fn();
    rowCommandRegistry.publish("files", { subject: "notes.md", available: ["files.rename"], run: files });
    rowCommandRegistry.publish("git", { subject: "src/main.rs", available: ["git.stage"], run: git });

    expect(rowCommandRegistry.run("git.stage")).toEqual({ ran: true, subject: "src/main.rs" });
    expect(git).toHaveBeenCalledWith("git.stage");
    expect(files).not.toHaveBeenCalled();
    expect(rowCommandRegistry.run("files.rename")).toEqual({ ran: true, subject: "notes.md" });
  });

  it("reports that nothing ran once the surface has gone", () => {
    const run = vi.fn();
    rowCommandRegistry.publish("files", { subject: "notes.md", available: ["files.rename"], run });
    // Closing the right panel unmounts the Explorer. The palette must stop
    // offering its row actions rather than invoking a handler for a row that is
    // no longer on screen.
    rowCommandRegistry.publish("files", undefined);
    expect(rowCommandRegistry.run("files.rename")).toEqual({ ran: false });
    expect(run).not.toHaveBeenCalled();
    expect(rowCommandRegistry.available()).toEqual([]);
  });

  it("notifies subscribers only when the offered set actually changes", () => {
    const listener = vi.fn();
    const unsubscribe = rowCommandRegistry.subscribe(listener);
    const source = (available: readonly string[]) => ({ subject: "row", available: available as never, run: vi.fn() });

    rowCommandRegistry.publish("files", source(["files.rename"]));
    expect(listener).toHaveBeenCalledTimes(1);
    const snapshot = rowCommandRegistry.available();
    // Re-publishing the same row on every render is normal; a snapshot that
    // changed identity each time would spin `useSyncExternalStore` forever.
    rowCommandRegistry.publish("files", source(["files.rename"]));
    expect(listener).toHaveBeenCalledTimes(1);
    expect(rowCommandRegistry.available()).toBe(snapshot);

    rowCommandRegistry.publish("files", source(["files.rename", "files.delete"]));
    expect(listener).toHaveBeenCalledTimes(2);
    expect(rowCommandRegistry.available()).toEqual(["files.rename", "files.delete"]);
    unsubscribe();
    rowCommandRegistry.publish("files", undefined);
    expect(listener).toHaveBeenCalledTimes(2);
  });
});
