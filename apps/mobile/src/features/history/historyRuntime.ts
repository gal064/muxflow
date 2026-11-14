// The app's one history client. `connectionManager` feeds it events and tells
// it where the connection is.

import { log } from "../../session/log";
import { sessionStore } from "../../store/sessionStore";
import { createHistoryStore, HistoryClient, type HistoryConnection } from "./HistoryClient";

let source: () => HistoryConnection | null = () => null;
let say: (message: string) => void = () => undefined;

/** Called once by `connectionManager`, which owns the connection and the toast. */
export function bindHistory(connection: () => HistoryConnection | null, toast: (message: string) => void): void {
  source = connection;
  say = toast;
}

export const historyStore = createHistoryStore();

export const historyClient = new HistoryClient({
  store: historyStore,
  getConnection: () => source(),
  topology: () => {
    const state = sessionStore.getState();
    return { expectedServerIdentity: state.serverIdentity, expectedTopologyGeneration: state.topologyGeneration };
  },
  onWoke: (entries) => {
    for (const entry of entries) say(`${entry.title || "A snoozed session"} woke up. Resume it from History.`);
  },
  log,
});
