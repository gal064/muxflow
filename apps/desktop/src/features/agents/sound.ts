import { defaultAgentSoundPreferences, type AgentSoundPreferences } from "./types";

const LEGACY_STORAGE_KEY = "muxflow.agent-sounds.v1";

/** A saved value made safe to use: unknown modes and out-of-range volumes fall back. `undefined` when there is nothing to read. */
export function normalizeAgentSoundPreferences(value: unknown): AgentSoundPreferences | undefined {
  if (!value || typeof value !== "object") return undefined;
  const saved = value as Partial<AgentSoundPreferences>;
  return {
    enabled: saved.enabled !== false,
    blocked: saved.blocked === "none" ? "none" : "subtle",
    completed: saved.completed === "none" ? "none" : "subtle",
    volume: typeof saved.volume === "number" && Number.isFinite(saved.volume)
      ? Math.max(0, Math.min(1, saved.volume)) : defaultAgentSoundPreferences.volume,
  };
}

/**
 * The sounds `localStorage` held before they moved into app state, which the
 * native app cannot read. Read once, when app state has none of its own.
 */
export function legacyAgentSoundPreferences(storage: Pick<Storage, "getItem"> | undefined = globalThis.localStorage): AgentSoundPreferences | undefined {
  try {
    return normalizeAgentSoundPreferences(JSON.parse(storage?.getItem(LEGACY_STORAGE_KEY) ?? "null"));
  } catch {
    return undefined;
  }
}

/** Dropped once app state has the value, so a reset of app state is not undone by a stale copy. */
export function clearLegacyAgentSoundPreferences(storage: Pick<Storage, "removeItem"> | undefined = globalThis.localStorage): void {
  try {
    storage?.removeItem(LEGACY_STORAGE_KEY);
  } catch {
    // Nothing to clear if storage is unavailable.
  }
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
