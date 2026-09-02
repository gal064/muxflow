// Saved hosts, their trusted host keys and their connection epochs (design.md
// §8.3). Persisted with expo-secure-store: §8.3 says "persist `hosts`,
// `lastHostId`, and nothing else", so the whole slice is one JSON value under
// one key. The SSH private key never appears here — it lives inside the native
// module (§6.2, §14).

import { createStore, type StoreApi } from "zustand/vanilla";
import { secureStoreStorage, type KeyValueStorage } from "./secureStorage";

export interface SavedHost {
  /** uuid */
  id: string;
  /** User-facing; defaults to `host`. */
  label: string;
  host: string;
  port: number;
  user: string;
  trustedHostKeyFingerprint: string | null;
  /** Incremented on every connection attempt (§7.3); the ClientHello carries it. */
  connectionEpoch: number;
  lastConnectedAtMs: number | null;
}

/** The fields §9.2's form collects. */
export interface HostDraft {
  host: string;
  port: number;
  user: string;
  label: string;
}

export interface HostsPersisted {
  hosts: SavedHost[];
  lastHostId: string | null;
}

/** The slice of expo-secure-store this store needs; swapped for a fake in tests. */
export type HostsStorage = KeyValueStorage;

export const HOSTS_STORAGE_KEY = "muxflow.hosts.v1";

export const DEFAULT_SSH_PORT = 22;

export interface HostsState extends HostsPersisted {
  /** False until the persisted value has been read once; the Hosts screen waits for it. */
  hydrated: boolean;
}

export interface HostsActions {
  /** Reads the persisted value. Safe to call more than once; only the first read does I/O. */
  hydrate(): Promise<void>;
  addHost(draft: HostDraft): SavedHost;
  updateHost(id: string, draft: HostDraft): void;
  /** §9.1 step 4: drops the host, its trusted key and its history. */
  removeHost(id: string): void;
  /** §9.2 "Forget host key" and the §9.10 trust dialog both land here. */
  setTrustedHostKeyFingerprint(id: string, fingerprint: string | null): void;
  /** §7.3: bumps and returns this host's epoch. Always >= 1. */
  takeConnectionEpoch(id: string): number;
  markConnected(id: string, atMs?: number): void;
  setLastHostId(id: string | null): void;
}

export type HostsStore = StoreApi<HostsState & HostsActions>;

export function initialHostsState(): HostsState {
  return { hosts: [], lastHostId: null, hydrated: false };
}

/** §9.2: an empty label falls back to the host name. */
export function hostLabel(draft: { label: string; host: string }): string {
  const label = draft.label.trim();
  return label.length > 0 ? label : draft.host.trim();
}

/** `user@host:port`, the second line of a §9.1 row and the `Host` row of the §9.8 sheet. */
export function hostAddress(host: Pick<SavedHost, "user" | "host" | "port">): string {
  return `${host.user}@${host.host}:${host.port}`;
}

/**
 * Hermes has no `crypto.randomUUID` and the app pulls in no uuid package; a
 * random 128-bit v4-shaped id from `Math.random` is enough for a local list key.
 */
export function newHostId(): string {
  const uuid = globalThis.crypto?.randomUUID?.bind(globalThis.crypto);
  if (uuid) return uuid();
  const hex = (length: number) =>
    Array.from({ length }, () => Math.floor(Math.random() * 16).toString(16)).join("");
  return `${hex(8)}-${hex(4)}-4${hex(3)}-${((Math.floor(Math.random() * 4) + 8) % 16).toString(16)}${hex(3)}-${hex(12)}`;
}

/** Rebuilds the persisted shape defensively: a corrupt or partial value must not crash launch. */
export function parsePersisted(raw: string | null): HostsPersisted {
  if (raw === null) return { hosts: [], lastHostId: null };
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    console.log("[muxflow] hosts.store.unreadable");
    return { hosts: [], lastHostId: null };
  }
  if (typeof parsed !== "object" || parsed === null) return { hosts: [], lastHostId: null };
  const value = parsed as Record<string, unknown>;
  const hosts = Array.isArray(value.hosts)
    ? value.hosts.map(parseHost).filter((host): host is SavedHost => host !== null)
    : [];
  const lastHostId = typeof value.lastHostId === "string" ? value.lastHostId : null;
  return { hosts, lastHostId: hosts.some((host) => host.id === lastHostId) ? lastHostId : null };
}

function parseHost(raw: unknown): SavedHost | null {
  if (typeof raw !== "object" || raw === null) return null;
  const value = raw as Record<string, unknown>;
  if (typeof value.id !== "string" || typeof value.host !== "string" || typeof value.user !== "string") {
    return null;
  }
  const port = typeof value.port === "number" && Number.isInteger(value.port) ? value.port : DEFAULT_SSH_PORT;
  const label = typeof value.label === "string" ? value.label : "";
  return {
    id: value.id,
    label: label.length > 0 ? label : value.host,
    host: value.host,
    user: value.user,
    port,
    trustedHostKeyFingerprint:
      typeof value.trustedHostKeyFingerprint === "string" ? value.trustedHostKeyFingerprint : null,
    connectionEpoch:
      typeof value.connectionEpoch === "number" && Number.isFinite(value.connectionEpoch)
        ? Math.max(0, Math.floor(value.connectionEpoch))
        : 0,
    lastConnectedAtMs:
      typeof value.lastConnectedAtMs === "number" && Number.isFinite(value.lastConnectedAtMs)
        ? value.lastConnectedAtMs
        : null,
  };
}

export function createHostsStore(storage: HostsStorage): HostsStore {
  const transientEpochs = new Map<string, number>();
  // Every write goes through one chain: two mutations in the same tick must not
  // race each other into the store and leave the older value persisted.
  let writes: Promise<unknown> = Promise.resolve();
  let hydration: Promise<void> | undefined;

  const store: HostsStore = createStore<HostsState & HostsActions>((set, get) => {
    const persist = (): void => {
      const { hosts, lastHostId } = get();
      const value = JSON.stringify({ hosts, lastHostId } satisfies HostsPersisted);
      writes = writes.then(
        () =>
          storage.setItem(HOSTS_STORAGE_KEY, value).catch((error: unknown) => {
            console.log(`[muxflow] hosts.store.write.failed ${describe(error)}`);
          }),
        () => undefined,
      );
    };

    /** Replaces one host and persists; a missing id is a no-op. */
    const mutate = (id: string, change: (host: SavedHost) => SavedHost): void => {
      let touched = false;
      set((state) => ({
        hosts: state.hosts.map((host) => {
          if (host.id !== id) return host;
          touched = true;
          return change(host);
        }),
      }));
      if (touched) persist();
    };

    return {
      ...initialHostsState(),

      hydrate() {
        hydration ??= storage
          .getItem(HOSTS_STORAGE_KEY)
          .catch((error: unknown) => {
            console.log(`[muxflow] hosts.store.read.failed ${describe(error)}`);
            return null;
          })
          .then((raw) => {
            const persisted = parsePersisted(raw);
            set({ ...persisted, hydrated: true });
          });
        return hydration;
      },

      addHost(draft) {
        const host: SavedHost = {
          id: newHostId(),
          label: hostLabel(draft),
          host: draft.host.trim(),
          port: draft.port,
          user: draft.user.trim(),
          trustedHostKeyFingerprint: null,
          connectionEpoch: 0,
          lastConnectedAtMs: null,
        };
        set((state) => ({ hosts: [...state.hosts, host] }));
        persist();
        return host;
      },

      updateHost(id, draft) {
        mutate(id, (host) => {
          const address = { host: draft.host.trim(), port: draft.port, user: draft.user.trim() };
          // Re-pointing an entry at another machine means the pin belongs to a
          // host this one is not: §9.10's mismatch screen is for a key that
          // changed under the same address, not for a row the user edited. A
          // host key belongs to host:port, so a new user name is not a move.
          const moved = address.host !== host.host || address.port !== host.port;
          return {
            ...host,
            ...address,
            label: hostLabel(draft),
            ...(moved ? { trustedHostKeyFingerprint: null } : {}),
          };
        });
      },

      removeHost(id) {
        set((state) => ({
          hosts: state.hosts.filter((host) => host.id !== id),
          lastHostId: state.lastHostId === id ? null : state.lastHostId,
        }));
        persist();
      },

      setTrustedHostKeyFingerprint(id, fingerprint) {
        mutate(id, (host) => ({ ...host, trustedHostKeyFingerprint: fingerprint }));
      },

      takeConnectionEpoch(id) {
        const current = get().hosts.find((host) => host.id === id);
        if (current) {
          const next = current.connectionEpoch + 1;
          mutate(id, (host) => ({ ...host, connectionEpoch: next }));
          return next;
        }
        // An unsaved host (the dev bridge, a test double) still needs an epoch
        // that is monotonic across reconnects: the host compares it with the
        // previous one (§7.3), and 0 is refused. Track it here, unpersisted.
        const next = (transientEpochs.get(id) ?? 0) + 1;
        transientEpochs.set(id, next);
        return next;
      },

      markConnected(id, atMs = Date.now()) {
        // Only a saved host can be the §12 cold-start target; a dev bridge or a
        // transient host must not displace it.
        if (!get().hosts.some((host) => host.id === id)) return;
        mutate(id, (host) => ({ ...host, lastConnectedAtMs: atMs }));
        set({ lastHostId: id });
        persist();
      },

      setLastHostId(id) {
        set({ lastHostId: id });
        persist();
      },
    };
  });

  return store;
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * The app-wide store. It reads the persisted value as soon as it is imported,
 * so a route entered without the Hosts screen — a notification tap, a deep
 * link — sees the same hosts. `hydrate()` is idempotent; screens await it to
 * know the read finished.
 */
export const hostsStore: HostsStore = createHostsStore(secureStoreStorage);
void hostsStore.getState().hydrate();

export function findHost(state: HostsState, id: string | null | undefined): SavedHost | undefined {
  return id ? state.hosts.find((host) => host.id === id) : undefined;
}
