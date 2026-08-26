import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  HOSTS_STORAGE_KEY,
  createHostsStore,
  hostAddress,
  hostLabel,
  parsePersisted,
  type HostsStorage,
} from "./hostsStore";

/** Stands in for expo-secure-store: one string under one key, async both ways. */
function fakeSecureStore(initial: Record<string, string> = {}) {
  const values = new Map(Object.entries(initial));
  const setItem = vi.fn(async (key: string, value: string) => {
    values.set(key, value);
  });
  const storage: HostsStorage = {
    getItem: async (key) => values.get(key) ?? null,
    setItem,
  };
  return { storage, values, setItem };
}

const draft = { host: "devbox", port: 22, user: "dev", label: "" };

describe("hostsStore", () => {
  it("round-trips saved hosts through the secure store", async () => {
    const first = fakeSecureStore();
    const store = createHostsStore(first.storage);
    await store.getState().hydrate();
    expect(store.getState().hosts).toEqual([]);
    expect(store.getState().hydrated).toBe(true);

    const saved = store.getState().addHost({ ...draft, label: "Devbox" });
    store.getState().setTrustedHostKeyFingerprint(saved.id, "SHA256:abc");
    store.getState().markConnected(saved.id, 1234);
    await vi.waitFor(() => expect(first.setItem).toHaveBeenCalled());

    // A second store reading the same value must see exactly what the first wrote.
    const reopened = createHostsStore({
      getItem: async (key) => first.values.get(key) ?? null,
      setItem: async () => undefined,
    });
    await reopened.getState().hydrate();
    expect(reopened.getState().hosts).toEqual([
      {
        id: saved.id,
        label: "Devbox",
        host: "devbox",
        port: 22,
        user: "dev",
        trustedHostKeyFingerprint: "SHA256:abc",
        connectionEpoch: 0,
        lastConnectedAtMs: 1234,
      },
    ]);
    expect(reopened.getState().lastHostId).toBe(saved.id);
  });

  it("persists only hosts and lastHostId (§8.3)", async () => {
    const { storage, values } = fakeSecureStore();
    const store = createHostsStore(storage);
    await store.getState().hydrate();
    store.getState().addHost(draft);
    await vi.waitFor(() => expect(values.get(HOSTS_STORAGE_KEY)).toBeDefined());
    const written = JSON.parse(values.get(HOSTS_STORAGE_KEY) as string) as Record<string, unknown>;
    expect(Object.keys(written).sort()).toEqual(["hosts", "lastHostId"]);
  });

  it("hands out a fresh connection epoch on every attempt, starting at 1", async () => {
    const { storage } = fakeSecureStore();
    const store = createHostsStore(storage);
    await store.getState().hydrate();
    const saved = store.getState().addHost(draft);
    expect(store.getState().takeConnectionEpoch(saved.id)).toBe(1);
    expect(store.getState().takeConnectionEpoch(saved.id)).toBe(2);
    expect(store.getState().hosts[0]?.connectionEpoch).toBe(2);
    // An unknown host still gets a usable epoch: §7.3 refuses 0.
    expect(store.getState().takeConnectionEpoch("missing")).toBe(1);
  });

  it("forgets a host, its trust and its place as the last host", async () => {
    const { storage } = fakeSecureStore();
    const store = createHostsStore(storage);
    await store.getState().hydrate();
    const saved = store.getState().addHost(draft);
    store.getState().markConnected(saved.id);
    store.getState().removeHost(saved.id);
    expect(store.getState().hosts).toEqual([]);
    expect(store.getState().lastHostId).toBeNull();
  });

  it("survives a corrupt or partial persisted value", () => {
    expect(parsePersisted(null)).toEqual({ hosts: [], lastHostId: null });
    expect(parsePersisted("{not json")).toEqual({ hosts: [], lastHostId: null });
    expect(parsePersisted('{"hosts":[{"id":"a"}],"lastHostId":"a"}')).toEqual({
      hosts: [],
      lastHostId: null,
    });
    const partial = parsePersisted('{"hosts":[{"id":"a","host":"h","user":"u"}],"lastHostId":"b"}');
    expect(partial.hosts[0]).toMatchObject({ port: 22, label: "h", connectionEpoch: 0 });
    // lastHostId is dropped when it names a host that is not there.
    expect(partial.lastHostId).toBeNull();
  });

  it("labels and addresses hosts the way §9.1 and §9.8 show them", () => {
    expect(hostLabel({ label: "  ", host: "devbox" })).toBe("devbox");
    expect(hostLabel({ label: "Work", host: "devbox" })).toBe("Work");
    expect(hostAddress({ user: "dev", host: "10.0.2.2", port: 22222 })).toBe("dev@10.0.2.2:22222");
  });
});

describe("hostsStore state", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("does not write before it has hydrated the previous value", async () => {
    const { storage, setItem } = fakeSecureStore({
      [HOSTS_STORAGE_KEY]: JSON.stringify({ hosts: [], lastHostId: null }),
    });
    const store = createHostsStore(storage);
    expect(setItem).not.toHaveBeenCalled();
    await store.getState().hydrate();
    expect(setItem).not.toHaveBeenCalled();
  });
});
