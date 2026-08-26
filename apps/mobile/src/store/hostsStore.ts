// Saved hosts (design doc §8.3). M3 ships the type and an in-memory store so
// the connection manager has something to hand out; secure-store persistence
// of `hosts` and `lastHostId` is M6's (§9.1, §12 cold-start auto-connect).

import { createStore, type StoreApi } from "zustand/vanilla";

export interface SavedHost {
  id: string;
  /** User-facing, defaults to `host`. */
  label: string;
  host: string;
  port: number;
  user: string;
  trustedHostKeyFingerprint: string | null;
  /** Incremented on every connection attempt; sent in ClientHello (§7.3). */
  connectionEpoch: number;
  lastConnectedAtMs: number | null;
}

export interface HostsState {
  hosts: SavedHost[];
  lastHostId: string | null;
}

export interface HostsActions {
  upsertHost(host: SavedHost): void;
  removeHost(id: string): void;
  setLastHostId(id: string | null): void;
  /** Returns the new epoch (>= 1). Unknown ids are added implicitly so a transient host still gets a valid epoch. */
  bumpConnectionEpoch(host: SavedHost): number;
  markConnected(id: string, atMs: number): void;
}

export type HostsStore = StoreApi<HostsState & HostsActions>;

export function createHostsStore(): HostsStore {
  return createStore<HostsState & HostsActions>((set, get) => ({
    hosts: [],
    lastHostId: null,

    upsertHost(host) {
      set((state) => {
        const index = state.hosts.findIndex((h) => h.id === host.id);
        const hosts = [...state.hosts];
        if (index === -1) hosts.push(host);
        else hosts[index] = host;
        return { hosts };
      });
    },

    removeHost(id) {
      set((state) => ({
        hosts: state.hosts.filter((h) => h.id !== id),
        lastHostId: state.lastHostId === id ? null : state.lastHostId,
      }));
    },

    setLastHostId(lastHostId) {
      set({ lastHostId });
    },

    bumpConnectionEpoch(host) {
      const stored = get().hosts.find((h) => h.id === host.id) ?? host;
      const next = { ...stored, connectionEpoch: Math.max(0, stored.connectionEpoch) + 1 };
      get().upsertHost(next);
      return next.connectionEpoch;
    },

    markConnected(id, atMs) {
      set((state) => ({
        hosts: state.hosts.map((h) => (h.id === id ? { ...h, lastConnectedAtMs: atMs } : h)),
        lastHostId: id,
      }));
    },
  }));
}

export const hostsStore: HostsStore = createHostsStore();
