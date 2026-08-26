// The app's single host connection, and the bulk lane bound to it (§7, §11.1).
//
// ─────────────────────────────────────────────────────────────────────────────
// STUB. This file belongs to the session milestone and is expected to be
// replaced wholesale on merge. It exists here so the Files feature can be
// written, built and run against the two getters it actually uses —
// `getConnection()` and `openBulkConnection()` — whose signatures are fixed.
// Everything below those two is the smallest thing that works.
// ─────────────────────────────────────────────────────────────────────────────

import { HostConnection } from "../protocol/HostConnection";
import type { Transport } from "../protocol/Transport";
import { filesStore } from "../features/files/filesStore";
import { createSessionStore, sessionStore, type SavedHostRef, type SessionStore } from "../store/sessionStore";

/** Opens one `muxflow-host bridge --stdio` channel. The SSH module supplies the real one. */
export type TransportFactory = (lane: "control" | "bulk") => Promise<Transport>;

interface BulkLane {
  epoch: bigint;
  connection: HostConnection;
  store: SessionStore;
  opening: Promise<HostConnection>;
}

let transportFactory: TransportFactory | undefined;
let control: HostConnection | undefined;
let bulk: BulkLane | undefined;
let epochCounter = 0;

export function setTransportFactory(factory: TransportFactory | undefined): void {
  transportFactory = factory;
}

export function getConnection(): HostConnection | null {
  return control ?? null;
}

export function connectHost(host: SavedHostRef): HostConnection {
  disconnectHost();
  control = new HostConnection({
    dial: () => dial("control"),
    appVersion: "0.1.0",
    nextConnectionEpoch: () => (epochCounter += 1),
    store: sessionStore,
    host,
    onFileEvent: (event) => filesStore.getState().applyFileEvent(event),
    log: (line) => console.log(line),
  });
  control.connect();
  return control;
}

export function disconnectHost(): void {
  closeBulk();
  control?.disconnect();
  control = undefined;
  filesStore.getState().clearAll();
}

/**
 * The bulk lane, memoised per control epoch: file bodies are served only there
 * (§11.1), and a reconnect of the control lane changes the epoch the host binds
 * against, so the stale lane is dropped and a new one dialled.
 */
export function openBulkConnection(): Promise<HostConnection> {
  const connection = control;
  if (!connection) return Promise.reject(new Error("not connected"));
  const expectedServerIdentity = connection.serverIdentity;
  const epoch = connection.connectionEpoch;
  if (!expectedServerIdentity || epoch === 0n) return Promise.reject(new Error("not connected"));
  if (bulk) {
    const live = bulk.epoch === epoch && LIVE_STATES.has(bulk.store.getState().connection.state);
    if (live) return bulk.opening;
    closeBulk();
  }
  const store = createSessionStore();
  const lane = new HostConnection({
    dial: () => dial("bulk"),
    appVersion: "0.1.0",
    nextConnectionEpoch: () => {
      throw new Error("a bulk lane reuses the control connection's epoch");
    },
    store,
    bulk: { expectedServerIdentity, connectionEpoch: epoch },
    log: (line) => console.log(`[muxflow] bulk ${line}`),
  });
  const opening = new Promise<HostConnection>((resolve, reject) => {
    const unsubscribe = store.subscribe((state) => {
      if (state.connection.state === "connected") {
        unsubscribe();
        resolve(lane);
      } else if (state.connection.state === "failed" || state.connection.state === "incompatible") {
        unsubscribe();
        reject(new Error(state.connection.message ?? "the bulk connection failed"));
      } else if (state.connection.state === "idle") {
        // `connect()` moves off `idle` synchronously, so seeing it here means
        // the lane was disconnected before it ever handshook.
        unsubscribe();
        reject(new Error("the bulk connection was closed"));
      }
    });
    lane.connect();
  });
  bulk = { epoch, connection: lane, store, opening };
  return opening;
}

/** States in which the memoised lane is still worth handing out. */
const LIVE_STATES = new Set(["sshConnecting", "handshaking", "connected", "reconnecting"]);

function dial(lane: "control" | "bulk"): Promise<Transport> {
  if (!transportFactory) return Promise.reject(new Error("no transport factory is configured"));
  return transportFactory(lane);
}

function closeBulk(): void {
  const lane = bulk;
  bulk = undefined;
  if (!lane) return;
  // Settle a still-pending `opening` before the disconnect, so nobody is left
  // waiting on a lane that will never connect and no rejection goes unhandled.
  lane.opening.catch(() => {});
  lane.connection.disconnect();
}
