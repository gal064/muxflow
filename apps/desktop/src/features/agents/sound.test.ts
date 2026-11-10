import { describe, expect, it, vi } from "vitest";
import { clearLegacyAgentSoundPreferences, legacyAgentSoundPreferences, normalizeAgentSoundPreferences, playAgentSound } from "./sound";
import { defaultAgentSoundPreferences } from "./types";

describe("agent sound preferences", () => {
  it("normalizes out-of-range saved values and ignores anything that is not an object", () => {
    expect(normalizeAgentSoundPreferences({ enabled: true, blocked: "loud", completed: "none", volume: 7 }))
      .toEqual({ enabled: true, blocked: "subtle", completed: "none", volume: 1 });
    expect(normalizeAgentSoundPreferences({ enabled: false, volume: Number.NaN }))
      .toEqual({ ...defaultAgentSoundPreferences, enabled: false });
    expect(normalizeAgentSoundPreferences(undefined)).toBeUndefined();
    expect(normalizeAgentSoundPreferences("loud")).toBeUndefined();
  });

  it("reads the localStorage copy once and clears it", () => {
    const store = new Map([["muxflow.agent-sounds.v1", JSON.stringify({ enabled: false, blocked: "none", completed: "subtle", volume: 0.2 })]]);
    const storage = { getItem: (key: string) => store.get(key) ?? null, removeItem: (key: string) => { store.delete(key); } };
    expect(legacyAgentSoundPreferences(storage)).toEqual({ enabled: false, blocked: "none", completed: "subtle", volume: 0.2 });
    clearLegacyAgentSoundPreferences(storage);
    expect(legacyAgentSoundPreferences(storage)).toBeUndefined();
  });

  it("has nothing to import from missing, corrupt or unavailable storage", () => {
    expect(legacyAgentSoundPreferences({ getItem: () => null })).toBeUndefined();
    expect(legacyAgentSoundPreferences({ getItem: () => "not json" })).toBeUndefined();
    expect(legacyAgentSoundPreferences({ getItem: () => { throw new Error("denied"); } })).toBeUndefined();
    expect(() => clearLegacyAgentSoundPreferences({ removeItem: () => { throw new Error("denied"); } })).not.toThrow();
  });

  it("instruments disabled playback without touching an audio device", async () => {
    const instrument = vi.fn();
    await playAgentSound("blocked", { ...defaultAgentSoundPreferences, enabled: false }, instrument);
    expect(instrument).toHaveBeenCalledWith({ event: "blocked", outcome: "disabled" });
  });
});
