// The last transport close, kept so the connection chrome can pick the right
// §12 row (the state machine records only its message) and so the §9.8 sheet
// can show `Last error`.

import { createStore, type StoreApi } from "zustand/vanilla";

import type { TransportClose } from "../../protocol/Transport";

export interface DiagnosticsState {
  lastClose: TransportClose | null;
}

export interface DiagnosticsActions {
  setLastClose(close: TransportClose | null): void;
}

export type DiagnosticsStore = StoreApi<DiagnosticsState & DiagnosticsActions>;

export function createDiagnosticsStore(): DiagnosticsStore {
  return createStore<DiagnosticsState & DiagnosticsActions>((set) => ({
    lastClose: null,
    setLastClose(lastClose) {
      set({ lastClose });
    },
  }));
}

export const diagnosticsStore: DiagnosticsStore = createDiagnosticsStore();
