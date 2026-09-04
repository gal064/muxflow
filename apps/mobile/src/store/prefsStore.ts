// Small user preferences (design.md §9.3.1: the Agents tab's list mode;
// §9.11: the voice screen's playback speed and talk-pane size).
// Persisted the way the hosts store is — one JSON value under one
// expo-secure-store key — because that is the only persistence layer the app
// has; the value is not a secret, the store is simply the one that exists.

import { createStore, type StoreApi } from "zustand/vanilla";

import { isAgentListMode, type AgentListMode } from "../features/agents/agentListModel";
import { secureStoreStorage, type KeyValueStorage } from "./secureStorage";

export const PREFS_STORAGE_KEY = "muxflow.prefs.v1";
export const DEFAULT_AGENT_COMMAND = "codex";

export type VoicePlaybackRate = 1 | 1.5 | 2;
export const VOICE_PLAYBACK_RATES: readonly VoicePlaybackRate[] = [1, 1.5, 2];
export function isVoicePlaybackRate(value: unknown): value is VoicePlaybackRate {
  return (VOICE_PLAYBACK_RATES as readonly unknown[]).includes(value);
}

export interface PrefsPersisted {
  agentListMode: AgentListMode;
  /** Shell command started by the New agent shortcut. */
  agentCommand: string;
  /** How fast voice replies are read back. */
  voicePlaybackRate: VoicePlaybackRate;
  /** The voice screen's talk pane takes most of the window. */
  voiceBigPane: boolean;
}

export interface PrefsState extends PrefsPersisted {
  /** True once the persisted value has been read, or once the user has set a value. */
  hydrated: boolean;
}

export interface PrefsActions {
  /** Reads the persisted value. Safe to call more than once; only the first read does I/O. */
  hydrate(): Promise<void>;
  setAgentListMode(mode: AgentListMode): void;
  setAgentCommand(command: string): void;
  setVoicePlaybackRate(rate: VoicePlaybackRate): void;
  setVoiceBigPane(big: boolean): void;
}

export type PrefsStore = StoreApi<PrefsState & PrefsActions>;

export const DEFAULT_PREFS: PrefsPersisted = { agentListMode: "priority", agentCommand: DEFAULT_AGENT_COMMAND, voicePlaybackRate: 1, voiceBigPane: false };

/** Tolerates anything on disk: an unknown or missing field falls back to its default. */
export function parsePersistedPrefs(raw: string | null): PrefsPersisted {
  if (!raw) return { ...DEFAULT_PREFS };
  try {
    const parsed: unknown = JSON.parse(raw);
    const fields = typeof parsed === "object" && parsed !== null ? (parsed as Partial<Record<keyof PrefsPersisted, unknown>>) : {};
    return {
      agentListMode: isAgentListMode(fields.agentListMode) ? fields.agentListMode : DEFAULT_PREFS.agentListMode,
      agentCommand: typeof fields.agentCommand === "string" ? fields.agentCommand : DEFAULT_PREFS.agentCommand,
      voicePlaybackRate: isVoicePlaybackRate(fields.voicePlaybackRate) ? fields.voicePlaybackRate : DEFAULT_PREFS.voicePlaybackRate,
      voiceBigPane: typeof fields.voiceBigPane === "boolean" ? fields.voiceBigPane : DEFAULT_PREFS.voiceBigPane,
    };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

export function createPrefsStore(storage: KeyValueStorage): PrefsStore {
  let writes: Promise<unknown> = Promise.resolve();
  let hydration: Promise<void> | undefined;

  return createStore<PrefsState & PrefsActions>((set, get) => {
    const persist = (): void => {
      const { agentListMode, agentCommand, voicePlaybackRate, voiceBigPane } = get();
      const value = JSON.stringify({ agentListMode, agentCommand, voicePlaybackRate, voiceBigPane } satisfies PrefsPersisted);
      writes = writes.then(
        () =>
          storage.setItem(PREFS_STORAGE_KEY, value).catch((error: unknown) => {
            console.log(`[muxflow] prefs.store.write.failed ${error instanceof Error ? error.message : String(error)}`);
          }),
        () => undefined,
      );
    };

    return {
      ...DEFAULT_PREFS,
      hydrated: false,

      hydrate() {
        hydration ??= storage
          .getItem(PREFS_STORAGE_KEY)
          .catch((error: unknown) => {
            console.log(`[muxflow] prefs.store.read.failed ${error instanceof Error ? error.message : String(error)}`);
            return null;
          })
          .then((raw) => {
            // A choice made while the read was in flight wins over the disk.
            if (get().hydrated) return;
            set({ ...parsePersistedPrefs(raw), hydrated: true });
          });
        return hydration;
      },

      setAgentListMode(mode) {
        if (get().agentListMode === mode && get().hydrated) return;
        set({ agentListMode: mode, hydrated: true });
        persist();
      },

      setAgentCommand(command) {
        if (get().agentCommand === command && get().hydrated) return;
        set({ agentCommand: command, hydrated: true });
        persist();
      },

      setVoicePlaybackRate(rate) {
        if (get().voicePlaybackRate === rate && get().hydrated) return;
        set({ voicePlaybackRate: rate, hydrated: true });
        persist();
      },

      setVoiceBigPane(big) {
        if (get().voiceBigPane === big && get().hydrated) return;
        set({ voiceBigPane: big, hydrated: true });
        persist();
      },
    };
  });
}

export const prefsStore: PrefsStore = createPrefsStore(secureStoreStorage);
void prefsStore.getState().hydrate();
