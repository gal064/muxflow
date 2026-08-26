// The app-level connection singleton (design doc §7, §11.1). Screens and
// features import this rather than constructing HostConnection themselves.
//
//   connectHost(host)        dial the control lane, drive §7.2 into sessionStore
//   getConnection()          the control HostConnection, or null
//   openBulkConnection()     the bulk lane bound to the current control epoch (§11.1)
//   disconnectHost()         user-initiated: no reconnect
//   setTransportFactory(f)   how a lane is dialled (the SSH module, or a dev pipe)

import { HostConnection } from "../protocol/HostConnection";
import type { Transport } from "../protocol/Transport";
import { hostsStore, type SavedHost } from "../store/hostsStore";
import { createSessionStore, sessionStore, type AgentTransition, type ConnectionState, type SessionStore } from "../store/sessionStore";
import { terminalRegistry } from "../features/terminal/terminalRegistry";
import { filesStore } from "../features/files/filesStore";
import { log } from "./log";

export type Lane = "control" | "bulk";
export type TransportFactory = (host: SavedHost, lane: Lane) => Promise<Transport>;

export const APP_VERSION = "0.1.0";

let factory: TransportFactory | undefined;
let control: HostConnection | null = null;
let controlHost: SavedHost | null = null;
let bulk: { epoch: bigint; connection: HostConnection; store: SessionStore; ready: Promise<HostConnection> } | null = null;
const listeners = new Set<(transition: AgentTransition) => void>();
const toasts = new Set<(message: string) => void>();

/** The SSH module (§6.1) plugs in here; QA uses a dev TCP pipe. */
export function setTransportFactory(f: TransportFactory): void {
  factory = f;
}

export function getConnection(): HostConnection | null {
  return control;
}

export function getConnectedHost(): SavedHost | null {
  return controlHost;
}

/** M4 subscribes its notification decision here (§13); M3 only forwards. */
export function onAgentTransition(listener: (transition: AgentTransition) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function onToast(listener: (message: string) => void): () => void {
  toasts.add(listener);
  return () => toasts.delete(listener);
}

export function toast(message: string): void {
  log(`toast: ${message}`);
  for (const listener of toasts) listener(message);
}

/**
 * Starts the control connection. Resolves on the first `connected`; rejects
 * with the strip message on `failed` / `incompatible`. The store reflects
 * every intermediate state immediately, so screens may navigate before this
 * settles (§9.1 navigates at `sshConnecting`).
 */
export async function connectHost(host: SavedHost): Promise<void> {
  if (!factory) throw new Error("no transport factory: call setTransportFactory first");
  await disconnectHost();
  const dial = factory;
  controlHost = host;
  const connection = new HostConnection({
    dial: () => dial(host, "control"),
    appVersion: APP_VERSION,
    // The stored record wins over the caller's copy so the epoch stays monotonic.
    nextConnectionEpoch: () => hostsStore.getState().bumpConnectionEpoch(host),
    store: sessionStore,
    host: { id: host.id, label: host.label, host: host.host, port: host.port, user: host.user },
    terminals: terminalRegistry,
    onConnected: () => {
      hostsStore.getState().markConnected(host.id, Date.now());
      // A new control epoch invalidates the bulk binding (§11.1).
      dropBulk();
      terminalRegistry.onConnected();
    },
    onAgentTransition: (transition) => {
      for (const listener of listeners) listener(transition);
    },
    // §7.4 routes ACTIVE_ROOT / directory / file-stream events to the files feature.
    onFileEvent: (event) => filesStore.getState().applyFileEvent(event),
    onToast: toast,
    log,
  });
  control = connection;
  const settled = waitForState(sessionStore, (state) => state === "connected" ? "ok" : state === "failed" || state === "incompatible" ? "bad" : state === "idle" ? "cancelled" : undefined);
  connection.connect();
  const outcome = await settled;
  if (outcome === "cancelled") throw new Error("disconnected");
  if (outcome === "bad") throw new Error(sessionStore.getState().connection.message ?? "connection failed");
}

/**
 * The bulk lane (§11.1): a second transport handshaken with
 * `bulkConnection: true`, the control `serverIdentity` and the control epoch,
 * no Subscribe. Memoised per control epoch; re-dialled after a reconnect.
 */
export function openBulkConnection(): Promise<HostConnection> {
  const connection = control;
  const host = controlHost;
  if (!factory || !connection || !host || connection.state !== "connected") {
    return Promise.reject(new Error("not connected"));
  }
  const epoch = connection.connectionEpoch;
  // Anything but a live lane on the current epoch is re-dialled: the bulk
  // lane's own reconnect keeps the (now stale) epoch and cannot rebind.
  if (bulk && bulk.epoch === epoch && (bulk.connection.state === "connected" || bulk.connection.state === "handshaking" || bulk.connection.state === "sshConnecting")) {
    return bulk.ready;
  }
  dropBulk();
  const dial = factory;
  const store = createSessionStore();
  const lane = new HostConnection({
    dial: () => dial(host, "bulk"),
    appVersion: APP_VERSION,
    nextConnectionEpoch: () => { throw new Error("bulk lanes reuse the control epoch"); },
    store,
    bulk: { expectedServerIdentity: connection.serverIdentity, connectionEpoch: epoch },
    log: (line) => log(`bulk ${line}`),
  });
  const ready = waitForState(store, (state) => state === "connected" ? "ok" : state === "failed" || state === "incompatible" || state === "idle" || state === "reconnecting" ? "bad" : undefined)
    .then((outcome) => {
      if (outcome !== "ok") {
        const message = store.getState().connection.message ?? "bulk connection failed";
        if (bulk?.connection === lane) bulk = null;
        // The lane would otherwise keep re-dialling on a stale epoch.
        lane.disconnect();
        throw new Error(message);
      }
      return lane;
    });
  bulk = { epoch, connection: lane, store, ready };
  lane.connect();
  return ready;
}

export async function disconnectHost(): Promise<void> {
  dropBulk();
  const connection = control;
  control = null;
  controlHost = null;
  connection?.disconnect();
  filesStore.getState().clearAll();
  sessionStore.getState().clearHostState();
  sessionStore.getState().setConnection({ state: "idle", attempt: 0, message: undefined, host: undefined });
}

function dropBulk(): void {
  if (!bulk) return;
  const lane = bulk;
  bulk = null;
  lane.connection.disconnect();
}

type Outcome = "ok" | "bad" | "cancelled";

function waitForState(store: SessionStore, classify: (state: ConnectionState) => Outcome | undefined): Promise<Outcome> {
  return new Promise((resolve) => {
    const check = (state: ConnectionState): boolean => {
      const outcome = classify(state);
      if (outcome === undefined) return false;
      resolve(outcome);
      return true;
    };
    // The subscription is installed before connect() so no transition is missed.
    const unsubscribe = store.subscribe((state) => {
      if (check(state.connection.state)) unsubscribe();
    });
  });
}
