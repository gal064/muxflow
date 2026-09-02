// Small user preferences (design.md §9.3.1: the Agents tab's list mode).
// Persisted the way the hosts store is — one JSON value under one
// expo-secure-store key — because that is the only persistence layer the app
// has; the value is not a secret, the store is simply the one that exists.

import { createStore, type StoreApi } from "zustand/vanilla";

import { isAgentListMode, type AgentListMode } from "../features/agents/agentListModel";
import { secureStoreStorage, type KeyValueStorage } from "./secureStorage";

export const PREFS_STORAGE_KEY = "muxflow.prefs.v1";

export interface PrefsPersisted {
  agentListMode: AgentListMode;
}

export interface PrefsState extends PrefsPersisted {
  /** True once the persisted value has been read, or once the user has set a value. */
  hydrated: boolean;
}

export interface PrefsActions {
  /** Reads the persisted value. Safe to call more than once; only the first read does I/O. */
  hydrate(): Promise<void>;
  setAgentListMode(mode: AgentListMode): void;
}

export type PrefsStore = StoreApi<PrefsState & PrefsActions>;

export const DEFAULT_PREFS: PrefsPersisted = { agentListMode: "priority" };

/** Tolerates anything on disk: an unknown or missing field falls back to its default. */
export function parsePersistedPrefs(raw: string | null): PrefsPersisted {
  if (!raw) return { ...DEFAULT_PREFS };
  try {
    const parsed: unknown = JSON.parse(raw);
    const mode = typeof parsed === "object" && parsed !== null ? (parsed as { agentListMode?: unknown }).agentListMode : undefined;
    return { agentListMode: isAgentListMode(mode) ? mode : DEFAULT_PREFS.agentListMode };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

export function createPrefsStore(storage: KeyValueStorage): PrefsStore {
  let writes: Promise<unknown> = Promise.resolve();
  let hydration: Promise<void> | undefined;

  return createStore<PrefsState & PrefsActions>((set, get) => {
    const persist = (): void => {
      const value = JSON.stringify({ agentListMode: get().agentListMode } satisfies PrefsPersisted);
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
    };
  });
}

export const prefsStore: PrefsStore = createPrefsStore(secureStoreStorage);
void prefsStore.getState().hydrate();
