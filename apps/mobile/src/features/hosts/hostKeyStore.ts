// The trust-on-first-use gate (design.md §9.10, §14). `sshTransport` asks;
// this store parks the question so the modal can render it, and the modal's
// answer resolves the promise the transport is waiting on.

import { createStore, type StoreApi } from "zustand/vanilla";

import type { HostKeyPrompt } from "../../ssh/sshTransport";

export interface PendingHostKey {
  prompt: HostKeyPrompt;
  /** The saved host this key was presented for, when there is one. */
  hostId: string | null;
  hostLabel: string;
}

export interface HostKeyState {
  pending: PendingHostKey | null;
}

export interface HostKeyActions {
  /** Resolves true once the user taps `Trust`, false on `Cancel`. */
  request(pending: PendingHostKey): Promise<boolean>;
  /** §9.10's two buttons. */
  answer(trusted: boolean): void;
  /**
   * Takes the dialog down when the channel it belongs to is gone (the native
   * verifier's 60 s timeout, a disconnect, a connect that failed first).
   * Without it the resolver stays parked and every later prompt is refused.
   */
  cancel(): void;
}

export type HostKeyStore = StoreApi<HostKeyState & HostKeyActions>;

export function createHostKeyStore(): HostKeyStore {
  let answer: ((trusted: boolean) => void) | undefined;
  return createStore<HostKeyState & HostKeyActions>((set) => ({
    pending: null,
    request(pending) {
      // One dialog at a time. Both lanes share a single SSH transport (the
      // native module authenticates once), so a second prompt means something
      // unexpected; refusing it is the safe answer.
      if (answer) {
        console.log("[muxflow] hostKey.prompt.busy");
        return Promise.resolve(false);
      }
      set({ pending });
      return new Promise<boolean>((resolve) => {
        answer = resolve;
      });
    },
    answer(trusted) {
      const resolve = answer;
      answer = undefined;
      set({ pending: null });
      resolve?.(trusted);
    },
    cancel() {
      if (!answer) return;
      console.log("[muxflow] hostKey.prompt.cancelled");
      const resolve = answer;
      answer = undefined;
      set({ pending: null });
      resolve(false);
    },
  }));
}

export const hostKeyStore: HostKeyStore = createHostKeyStore();
