import { defaultAgentSoundPreferences, type AgentSoundPreferences } from "./types";

const STORAGE_KEY = "tmux-agent-ide.agent-sounds.v1";

export function loadAgentSoundPreferences(storage: Pick<Storage, "getItem"> | undefined = globalThis.localStorage): AgentSoundPreferences {
  if (!storage) return defaultAgentSoundPreferences;
  try {
    const value = JSON.parse(storage.getItem(STORAGE_KEY) ?? "null") as Partial<AgentSoundPreferences> | null;
    if (!value) return defaultAgentSoundPreferences;
    return {
      enabled: value.enabled !== false,
      blocked: value.blocked === "none" ? "none" : "subtle",
      completed: value.completed === "none" ? "none" : "subtle",
      volume: typeof value.volume === "number" && Number.isFinite(value.volume)
        ? Math.max(0, Math.min(1, value.volume)) : defaultAgentSoundPreferences.volume,
    };
  } catch {
    return defaultAgentSoundPreferences;
  }
}

export function saveAgentSoundPreferences(
  preferences: AgentSoundPreferences,
  storage: Pick<Storage, "setItem"> | undefined = globalThis.localStorage,
): void {
  storage?.setItem(STORAGE_KEY, JSON.stringify(preferences));
}

export interface SoundInstrumentation {
  event: "blocked" | "completed";
  outcome: "played" | "disabled" | "unsupported" | "failed";
  error?: string;
}

/** Plays a short synthesized cue; no media URL, prompt, or output leaves the app. */
export async function playAgentSound(
  event: "blocked" | "completed",
  preferences: AgentSoundPreferences,
  instrument: (event: SoundInstrumentation) => void = () => undefined,
): Promise<void> {
  if (!preferences.enabled || preferences[event] === "none" || preferences.volume === 0) {
    instrument({ event, outcome: "disabled" });
    return;
  }
  const AudioContextConstructor = globalThis.AudioContext;
  if (!AudioContextConstructor) {
    instrument({ event, outcome: "unsupported" });
    return;
  }
  let context: AudioContext | undefined;
  try {
    context = new AudioContextConstructor();
    const oscillator = context.createOscillator();
    const gain = context.createGain();
    oscillator.type = "sine";
    oscillator.frequency.value = event === "blocked" ? 520 : 660;
    gain.gain.setValueAtTime(0.0001, context.currentTime);
    gain.gain.exponentialRampToValueAtTime(Math.max(0.0001, preferences.volume * 0.12), context.currentTime + 0.012);
    gain.gain.exponentialRampToValueAtTime(0.0001, context.currentTime + 0.16);
    oscillator.connect(gain);
    gain.connect(context.destination);
    oscillator.start();
    oscillator.stop(context.currentTime + 0.17);
    oscillator.addEventListener("ended", () => void context?.close(), { once: true });
    instrument({ event, outcome: "played" });
  } catch (error) {
    await context?.close().catch(() => undefined);
    instrument({ event, outcome: "failed", error: String(error) });
  }
}
