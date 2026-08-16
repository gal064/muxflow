import { describe, expect, it, vi } from "vitest";
import { AppStatePersistence } from "./appStatePersistence";
import { defaultAppState, type PersistedAppState } from "./types";

function state(surface: "files" | "git"): PersistedAppState {
  return { ...defaultAppState, shell: { ...defaultAppState.shell, panelSurface: surface } };
}

describe("AppStatePersistence", () => {
  it("flushes the newest coalesced state before desktop destruction", async () => {
    vi.useFakeTimers();
    const saved: PersistedAppState[] = [];
    const persistence = new AppStatePersistence(async (value) => { saved.push(value); });
    persistence.schedule(state("files"));
    persistence.schedule(state("git"));
    await persistence.flush();
    expect(saved.map((value) => value.shell.panelSurface)).toEqual(["git"]);
    vi.useRealTimers();
  });

  it("serializes an in-flight save ahead of a close-time flush", async () => {
    let release: (() => void) | undefined;
    const saved: string[] = [];
    const persistence = new AppStatePersistence(async (value) => {
      saved.push(value.shell.panelSurface);
      if (saved.length === 1) await new Promise<void>((resolve) => { release = resolve; });
    }, 0);
    persistence.schedule(state("files"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    const flushed = persistence.flush(state("git"));
    release?.();
    await flushed;
    expect(saved).toEqual(["files", "git"]);
  });

  it("recovers after a rejected write and retries the newest state", async () => {
    let attempts = 0;
    const failures: unknown[] = [];
    const saved: string[] = [];
    const persistence = new AppStatePersistence(async (value) => {
      attempts += 1;
      if (attempts === 1) throw new Error("disk unavailable");
      saved.push(value.shell.panelSurface);
    }, 0, (error) => failures.push(error));
    persistence.schedule(state("files"));
    await expect(persistence.flush()).rejects.toThrow("disk unavailable");
    persistence.schedule(state("git"));
    await expect(persistence.flush()).resolves.toBeUndefined();
    expect(failures).toHaveLength(1);
    expect(saved).toEqual(["git"]);
  });

  it("never restores a failed stale write over newer desired state", async () => {
    let rejectFirst: ((error: Error) => void) | undefined;
    const attempts: string[] = [];
    const persistence = new AppStatePersistence(async (value) => {
      attempts.push(value.shell.panelSurface);
      if (attempts.length === 1) await new Promise<void>((_, reject) => { rejectFirst = reject; });
    }, 0);
    persistence.schedule(state("files"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    persistence.schedule(state("git"));
    const firstFlush = persistence.flush();
    rejectFirst?.(new Error("first write failed"));
    await expect(firstFlush).rejects.toThrow("first write failed");
    await expect(persistence.flush()).resolves.toBeUndefined();
    expect(attempts).toEqual(["files", "git"]);
  });
});
