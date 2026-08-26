import { useStore } from "zustand";
import { sessionStore, type SessionActions, type SessionState } from "../store/sessionStore";

/** Subscribe a component to a slice of the app-wide session store. */
export function useSession<T>(selector: (state: SessionState & SessionActions) => T): T {
  return useStore(sessionStore, selector);
}
