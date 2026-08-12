import { describe, expect, it, vi } from "vitest";
import { loadAgentSoundPreferences, playAgentSound, saveAgentSoundPreferences } from "./sound";
import { defaultAgentSoundPreferences } from "./types";

describe("agent sound preferences", () => {
  it("normalizes corrupt or out-of-range persisted values", () => {
    expect(loadAgentSoundPreferences({ getItem: () => "not json" })).toEqual(defaultAgentSoundPreferences);
    expect(loadAgentSoundPreferences({ getItem: () => JSON.stringify({ enabled: true, blocked: "loud", completed: "none", volume: 7 }) })).toMatchObject({ blocked: "subtle", completed: "none", volume: 1 });
  });

  it("persists only bounded sound configuration", () => {
    const setItem = vi.fn();
    saveAgentSoundPreferences({ ...defaultAgentSoundPreferences, volume: 0.2 }, { setItem });
    expect(setItem.mock.calls[0][0]).toContain("agent-sounds.v1");
    expect(JSON.parse(setItem.mock.calls[0][1])).toMatchObject({ enabled: true, volume: 0.2 });
  });

  it("instruments disabled playback without touching an audio device", async () => {
    const instrument = vi.fn();
    await playAgentSound("blocked", { ...defaultAgentSoundPreferences, enabled: false }, instrument);
    expect(instrument).toHaveBeenCalledWith({ event: "blocked", outcome: "disabled" });
  });
});
