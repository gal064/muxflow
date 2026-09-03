// Which mount renders the app-wide modals (§9.10's dialog, §9.8's sheet).
//
// Two rules decide it. React Navigation detaches the screens under the top of
// the stack (react-native-screens sets their activity state to hidden), and a
// `Modal` inside a detached screen does not show — so a host-key prompt raised
// while Home is on top has to be rendered from somewhere that is never
// detached: the root layout. And with more than one mount alive, only one may
// render, or every mount would stack its own copy of the same modal.
//
// A root mount therefore always wins; a screen mount renders the modals only
// when there is no root mount at all (the case a screen-only integration
// leaves).

import { useEffect, useRef } from "react";
import { useStore } from "zustand";
import { createStore, type StoreApi } from "zustand/vanilla";

export type Token = object;

export type ChromeRole = "root" | "screen";

export interface ChromeState {
  roots: Token[];
  screens: Token[];
  claim(token: Token, role: ChromeRole): void;
  release(token: Token): void;
}

export const chromeRegistry: StoreApi<ChromeState> = createStore<ChromeState>((set, get) => ({
  roots: [],
  screens: [],
  claim(token, role) {
    const key = role === "root" ? "roots" : "screens";
    const current = get()[key];
    if (current.includes(token)) return;
    set({ [key]: [...current, token] } as Pick<ChromeState, "roots" | "screens">);
  },
  release(token) {
    const { roots, screens } = get();
    set({
      roots: roots.filter((candidate) => candidate !== token),
      screens: screens.filter((candidate) => candidate !== token),
    });
  },
}));

/** The single mount allowed to render the modals: a root one when one exists. */
export function modalOwner(state: ChromeState): Token | null {
  return state.roots[0] ?? state.screens[0] ?? null;
}

/** True for exactly one mount at a time: the root one when there is one. */
export function useOwnsGlobalModals(role: ChromeRole): boolean {
  const token = useRef<Token>({}).current;
  const owner = useStore(chromeRegistry, modalOwner);
  useEffect(() => {
    chromeRegistry.getState().claim(token, role);
    return () => chromeRegistry.getState().release(token);
  }, [token, role]);
  return owner === token;
}
