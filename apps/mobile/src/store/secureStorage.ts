/** The slice of expo-secure-store the persisted stores need; swapped for a fake in tests. */
export interface KeyValueStorage {
  getItem(key: string): Promise<string | null>;
  setItem(key: string, value: string): Promise<void>;
}

/**
 * expo-secure-store, required lazily so the stores can be imported (and unit
 * tested) without a native runtime.
 */
export const secureStoreStorage: KeyValueStorage = {
  async getItem(key) {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const store = require("expo-secure-store") as typeof import("expo-secure-store");
    return store.getItemAsync(key);
  },
  async setItem(key, value) {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const store = require("expo-secure-store") as typeof import("expo-secure-store");
    await store.setItemAsync(key, value);
  },
};
