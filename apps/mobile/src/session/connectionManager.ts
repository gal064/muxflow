// One connected host at a time (design.md §1): owns the control `HostConnection`,
// the lazily dialled bulk lane (§11.1), and the transport factory the SSH
// module registers at app start.
//
// STUB. The terminal milestone owns this file; this version exists so the
// hosts UX can connect for real, and keeps the agreed signatures —
// `connectHost`, `getConnection`, `openBulkConnection`, `disconnectHost`,
// `setTransportFactory` — so the two can be merged by taking that one.

import { ToastAndroid } from "react-native";

import { HostConnection } from "../protocol/HostConnection";
import type { Transport } from "../protocol/Transport";
import { log } from "../features/hosts/logBuffer";
import { hostsStore, hostLabel, type SavedHost } from "../store/hostsStore";
import { createSessionStore, sessionStore, type SavedHostRef } from "../store/sessionStore";
import { muxflowSsh } from "../ssh/MuxflowSsh";

export type TransportLane = "control" | "bulk";

export type TransportFactory = (host: SavedHost, lane: TransportLane) => Promise<Transport>;

const APP_VERSION = "0.1.0";

let transportFactory: TransportFactory | undefined;
let control: HostConnection | undefined;
let bulk: HostConnection | undefined;
let bulkPending: Promise<HostConnection> | undefined;
let connectedHost: SavedHost | undefined;

/** Called once at app start by `src/ssh/registerTransport.ts`. */
export function setTransportFactory(factory: TransportFactory): void {
  transportFactory = factory;
}

function dial(host: SavedHost, lane: TransportLane): Promise<Transport> {
  if (!transportFactory) {
    return Promise.reject(new Error("No transport factory registered."));
  }
  return transportFactory(host, lane);
}

function hostRef(host: SavedHost): SavedHostRef {
  return { id: host.id, label: hostLabel(host), host: host.host, port: host.port, user: host.user };
}

/** Connects to `host`, replacing any connection already open. */
export function connectHost(host: SavedHost): HostConnection {
  disconnectHost();
  connectedHost = host;
  hostsStore.getState().setLastHostId(host.id);
  const connection = new HostConnection({
    dial: () => dial(host, "control"),
    appVersion: APP_VERSION,
    nextConnectionEpoch: () => hostsStore.getState().takeConnectionEpoch(host.id),
    store: sessionStore,
    host: hostRef(host),
    onToast: (message) => {
      log(`toast ${message}`);
      ToastAndroid.show(message, ToastAndroid.LONG);
    },
    onConnected: () => {
      hostsStore.getState().markConnected(host.id);
      const label = hostLabel(host);
      const agents = Object.keys(sessionStore.getState().agents).length;
      // §7.3: the foreground service is what keeps the JS runtime — and with
      // it this connection — alive when the app goes to the background.
      muxflowSsh()
        .startForegroundService(`Connected to ${label}`, `Watching ${agents} agents`)
        .catch((error: unknown) => log(`service.start.failed ${describe(error)}`));
    },
    log,
  });
  control = connection;
  connection.connect();
  return connection;
}

export function getConnection(): HostConnection | undefined {
  return control;
}

/** The host the current connection belongs to. */
export function getConnectedHost(): SavedHost | undefined {
  return connectedHost;
}

/**
 * The second `bridge --stdio` channel file bodies are served on (§11.1).
 * Dialled lazily and bound to the control lane's identity and epoch.
 */
export function openBulkConnection(): Promise<HostConnection> {
  const controlConnection = control;
  const host = connectedHost;
  if (!controlConnection || !host) {
    return Promise.reject(new Error("Not connected."));
  }
  if (bulk) return Promise.resolve(bulk);
  bulkPending ??= new Promise<HostConnection>((resolve, reject) => {
    const identity = controlConnection.serverIdentity;
    const epoch = controlConnection.connectionEpoch;
    if (identity === "" || epoch === 0n) {
      reject(new Error("The control connection is not ready."));
      return;
    }
    const store = createSessionStore();
    // The bulk lane reports into its own store, so its failures are visible
    // only here; without this the caller would wait out the request timeout.
    const unsubscribe = store.subscribe((state) => {
      if (state.connection.state === "failed" || state.connection.state === "incompatible") {
        unsubscribe();
        connection.disconnect();
        reject(new Error(state.connection.message ?? "The bulk connection failed."));
      }
    });
    const connection = new HostConnection({
      dial: () => dial(host, "bulk"),
      appVersion: APP_VERSION,
      // A bulk lane reuses the control lane's epoch; this is never called.
      nextConnectionEpoch: () => Number(epoch),
      store,
      bulk: { expectedServerIdentity: identity, connectionEpoch: epoch },
      host: hostRef(host),
      log,
      onConnected: () => {
        unsubscribe();
        bulk = connection;
        resolve(connection);
      },
    });
    connection.connect();
  }).finally(() => {
    bulkPending = undefined;
  });
  return bulkPending;
}

/** §9.8 Disconnect: closes both lanes and stops the foreground service. */
export function disconnectHost(): void {
  bulk?.disconnect();
  bulk = undefined;
  bulkPending = undefined;
  control?.disconnect();
  control = undefined;
  connectedHost = undefined;
  muxflowSsh()
    .stopForegroundService()
    .catch((error: unknown) => log(`service.stop.failed ${describe(error)}`));
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
