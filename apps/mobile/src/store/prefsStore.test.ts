import { describe, expect, it, vi } from "vitest";

import { createPrefsStore, parsePersistedPrefs, PREFS_STORAGE_KEY } from "./prefsStore";
import type { KeyValueStorage } from "./secureStorage";

function fakeStore(initial: Record<string, string> = {}, options: { failRead?: boolean; failWrite?: boolean; gate?: Promise<void> } = {}) {
  const values = new Map(Object.entries(initial));
  const setItem = vi.fn(async (key: string, value: string) => {
    if (options.failWrite) throw new Error("write failed");
    values.set(key, value);
  });
  const storage: KeyValueStorage = {
    getItem: async (key) => {
      await options.gate;
      if (options.failRead) throw new Error("read failed");
      return values.get(key) ?? null;
    },
    setItem,
  };
  return { storage, values, setItem };
}

describe("parsePersistedPrefs", () => {
  it.each<[string | null, string]>([
    [null, "priority"],
    ["", "priority"],
    ["not json", "priority"],
    ["[]", "priority"],
    ["{}", "priority"],
    ['{"agentListMode":"status"}', "priority"],
    ['{"agentListMode":"workspace"}', "workspace"],
    ['{"agentListMode":"pinned"}', "pinned"],
    ['{"agentListMode":"Pinned"}', "priority"],
    ['{"agentListMode":"priority"}', "priority"],
  ])("%j → %s", (raw, mode) => {
    expect(parsePersistedPrefs(raw)).toEqual({ agentListMode: mode, voicePlaybackRate: 1, voiceBigPane: false });
  });

  it.each<[string, number, boolean]>([
    ['{"voicePlaybackRate":1.5,"voiceBigPane":true}', 1.5, true],
    ['{"voicePlaybackRate":2}', 2, false],
    // Unknown speeds and non-boolean flags fall back; a persisted value never disables the screen.
    ['{"voicePlaybackRate":3,"voiceBigPane":"yes"}', 1, false],
    ['{"voicePlaybackRate":"1.5","voiceBigPane":1}', 1, false],
  ])("voice fields %j → %s× big=%s", (raw, rate, big) => {
    expect(parsePersistedPrefs(raw)).toMatchObject({ voicePlaybackRate: rate, voiceBigPane: big });
  });
});

describe("prefsStore", () => {
  it("defaults to priority before hydration and reads the persisted mode", async () => {
    const { storage } = fakeStore({ [PREFS_STORAGE_KEY]: '{"agentListMode":"workspace"}' });
    const store = createPrefsStore(storage);
    expect(store.getState()).toMatchObject({ agentListMode: "priority", hydrated: false });
    await store.getState().hydrate();
    expect(store.getState()).toMatchObject({ agentListMode: "workspace", hydrated: true });
  });

  it("persists a change so a fresh store on the same storage restores it", async () => {
    const { storage, values, setItem } = fakeStore();
    const store = createPrefsStore(storage);
    await store.getState().hydrate();
    store.getState().setAgentListMode("workspace");
    expect(store.getState().agentListMode).toBe("workspace");
    await vi.waitFor(() => expect(setItem).toHaveBeenCalledTimes(1));
    expect(JSON.parse(values.get(PREFS_STORAGE_KEY)!)).toEqual({ agentListMode: "workspace", voicePlaybackRate: 1, voiceBigPane: false });

    const restarted = createPrefsStore(storage);
    await restarted.getState().hydrate();
    expect(restarted.getState().agentListMode).toBe("workspace");

    restarted.getState().setAgentListMode("pinned");
    await vi.waitFor(() => expect(setItem).toHaveBeenCalledTimes(2));
    const again = createPrefsStore(storage);
    await again.getState().hydrate();
    expect(again.getState().agentListMode).toBe("pinned");
  });

  it("persists the voice speed and pane size alongside the list mode", async () => {
    const { storage, values, setItem } = fakeStore({ [PREFS_STORAGE_KEY]: '{"agentListMode":"pinned"}' });
    const store = createPrefsStore(storage);
    await store.getState().hydrate();
    store.getState().setVoicePlaybackRate(2);
    store.getState().setVoiceBigPane(true);
    // Setting the value already held writes nothing.
    store.getState().setVoiceBigPane(true);
    await vi.waitFor(() => expect(setItem).toHaveBeenCalledTimes(2));
    expect(JSON.parse(values.get(PREFS_STORAGE_KEY)!)).toEqual({ agentListMode: "pinned", voicePlaybackRate: 2, voiceBigPane: true });
    const restarted = createPrefsStore(storage);
    await restarted.getState().hydrate();
    expect(restarted.getState()).toMatchObject({ agentListMode: "pinned", voicePlaybackRate: 2, voiceBigPane: true });
  });

  it("hydrates once: repeated calls share the read", async () => {
    const { storage } = fakeStore();
    const getItem = vi.spyOn(storage, "getItem");
    const store = createPrefsStore(storage);
    await Promise.all([store.getState().hydrate(), store.getState().hydrate()]);
    await store.getState().hydrate();
    expect(getItem).toHaveBeenCalledTimes(1);
  });

  it("lets a choice made during the read win over the disk", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const { storage } = fakeStore({ [PREFS_STORAGE_KEY]: '{"agentListMode":"priority"}' }, { gate });
    const store = createPrefsStore(storage);
    const hydration = store.getState().hydrate();
    store.getState().setAgentListMode("workspace");
    release();
    await hydration;
    expect(store.getState()).toMatchObject({ agentListMode: "workspace", hydrated: true });
  });

  it("falls back to the default when the read fails, and survives a failed write", async () => {
    const log = vi.spyOn(console, "log").mockImplementation(() => undefined);
    const { storage } = fakeStore({}, { failRead: true, failWrite: true });
    const store = createPrefsStore(storage);
    await store.getState().hydrate();
    expect(store.getState()).toMatchObject({ agentListMode: "priority", hydrated: true });
    store.getState().setAgentListMode("workspace");
    store.getState().setAgentListMode("priority");
    await vi.waitFor(() => expect(log).toHaveBeenCalledWith(expect.stringContaining("prefs.store.write.failed")));
    expect(store.getState().agentListMode).toBe("priority");
    log.mockRestore();
  });

  it("does not write when the mode is already what was asked for", async () => {
    const { storage, setItem } = fakeStore();
    const store = createPrefsStore(storage);
    await store.getState().hydrate();
    store.getState().setAgentListMode("priority");
    await Promise.resolve();
    expect(setItem).not.toHaveBeenCalled();
  });
});
