// The app-level connection singleton (design doc §7, §11.1). Screens and
// features import this rather than constructing HostConnection themselves.
//
//   connectHost(host)        dial the control lane, drive §7.2 into sessionStore
//   getConnection()          the control HostConnection, or null
//   openBulkConnection()     the bulk lane bound to the current control epoch (§11.1)
//   disconnectHost()         user-initiated: no reconnect
//   setTransportFactory(f)   how a lane is dialled (the SSH module, or a dev pipe)
//   setForegroundService(s)  the §6.3 notification: its text, and its Disconnect action

import { HostConnection } from "../protocol/HostConnection";
import type { Transport } from "../protocol/Transport";
import { hostLabel, hostsStore, type SavedHost } from "../store/hostsStore";
import { createSessionStore, sessionStore, type AgentTransition, type ConnectionState, type SessionStore } from "../store/sessionStore";
import { terminalRegistry } from "../features/terminal/terminalRegistry";
import { filesStore } from "../features/files/filesStore";
import { connectedNotificationText, reconnectingNotificationText } from "../features/hosts/connectionLabels";
import { notificationAttention } from "../features/notifications/attention";
import type { MuxflowSsh } from "../ssh/MuxflowSsh";
import { backgroundTimer } from "./backgroundTimer";
import { voiceRegistry } from "../features/voice/voiceRegistry";
import { log } from "./log";

export type Lane = "control" | "bulk";
export type TransportFactory = (host: SavedHost, lane: Lane, signal?: AbortSignal) => Promise<Transport>;
export type ForegroundService = Pick<MuxflowSsh, "setServiceNotification" | "addDisconnectListener">;

export const APP_VERSION = "0.1.0";
/** §6.3: the ongoing notification's title. */
export const SERVICE_NOTIFICATION_TITLE = "Muxflow";

let factory: TransportFactory | undefined;
let service: ForegroundService | undefined;
let unwireService: (() => void) | undefined;
let control: HostConnection | null = null;
let controlHost: SavedHost | null = null;
let bulk: { epoch: bigint; connection: HostConnection; store: SessionStore; ready: Promise<HostConnection> } | null = null;
const listeners = new Set<(transition: AgentTransition) => void>();
const toasts = new Set<(message: string) => void>();

/** The SSH module (§6.1) plugs in here; QA uses a dev TCP pipe. */
export function setTransportFactory(f: TransportFactory): void {
  factory = f;
}

/**
 * The foreground service's notification (§6.3). Its Disconnect action is the
 * user disconnecting: it goes through `disconnectHost`, which also cancels a
 * pending backoff — the `closed` event an open channel reports on that tap
 * cannot cover the gap between attempts, when there is no channel. The body
 * follows the control connection: "Connected to …" / "Reconnecting to …".
 */
export function setForegroundService(next: ForegroundService | undefined): void {
  unwireService?.();
  unwireService = undefined;
  service = next;
  if (!next) return;
  const offDisconnect = next.addDisconnectListener(() => {
    log("disconnect.requested source=notification");
    void disconnectHost();
  });
  let previous = sessionStore.getState().connection.state;
  const offStore = sessionStore.subscribe((state) => {
    const current = state.connection.state;
    if (current === previous) return;
    previous = current;
    if (controlHost && (current === "connected" || current === "reconnecting")) {
      postServiceNotification(current, controlHost);
    }
  });
  unwireService = () => {
    offDisconnect();
    offStore();
  };
}

function postServiceNotification(state: "connected" | "reconnecting", host: SavedHost): void {
  const label = hostLabel(host);
  const body = state === "connected" ? connectedNotificationText(label) : reconnectingNotificationText(label);
  service?.setServiceNotification(SERVICE_NOTIFICATION_TITLE, body).catch((error: unknown) => {
    log(`service.notification failed: ${error instanceof Error ? error.message : String(error)}`);
  });
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

/**
 * §13 runs the decision rule "for every AGENT_STATE event and for every agent
 * in a reconciling snapshot". `HostConnection` does that for the Subscribe
 * snapshot; a pull-to-refresh snapshot (§9.3) reconciles outside it, so
 * `features/agents/refresh.ts` publishes its transitions here.
 */
export function emitAgentTransition(transition: AgentTransition): void {
  for (const listener of listeners) listener(transition);
}

export function onToast(listener: (message: string) => void): () => void {
  toasts.add(listener);
  return () => toasts.delete(listener);
}

export function toast(message: string): void {
  // Toast text can be supplied by the connected host. Record the UI action,
  // not arbitrary remote content that could contain user data or credentials.
  log(`toast shown chars=${message.length}`);
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
  // A reconnect to the same host keeps the voice sessions: their host-side
  // registrations are per connection and are re-sent on `onConnected`.
  await disconnectHost({ keepVoiceSessions: controlHost?.id === host.id });
  const dial = factory;
  controlHost = host;
  const connection = new HostConnection({
    dial: (signal) => dial(host, "control", signal),
    appVersion: APP_VERSION,
    // The stored record wins over the caller's copy so the epoch stays monotonic.
    nextConnectionEpoch: () => hostsStore.getState().takeConnectionEpoch(host.id),
    store: sessionStore,
    host: { id: host.id, label: host.label, host: host.host, port: host.port, user: host.user },
    terminals: terminalRegistry,
    onConnected: () => {
      hostsStore.getState().markConnected(host.id, Date.now());
      // A new control epoch invalidates the bulk binding (§11.1).
      dropBulk();
      terminalRegistry.onConnected();
      voiceRegistry.onConnected();
    },
    onAgentTransition: (transition) => {
      for (const listener of listeners) listener(transition);
    },
    onAgentIdentityPromotion: (promotion) => {
      const accepted = voiceRegistry.promoteAgent(promotion.retiredAgentIds, promotion.agent);
      if (accepted) {
        notificationAttention.promoteAgent(accepted.oldAgentId, promotion.agent.id);
        log(`voice identity.promoted old=${accepted.oldAgentId} new=${promotion.agent.id} adapter=${promotion.agent.adapterId} pane=${promotion.agent.route.paneId}`);
      }
    },
    // §7.4 routes ACTIVE_ROOT / directory / file-stream events to the files feature.
    onFileEvent: (event) => filesStore.getState().applyFileEvent(event),
    // VOICE_PROVISION / VOICE_REPLY (voice-mode-plan.md §3) go to whichever voice session they name.
    onVoiceEvent: (event) => voiceRegistry.onVoiceEvent(event),
    onToast: toast,
    log,
    reconnectTimer: backgroundTimer(),
  });
  control = connection;
  const settled = waitForState(sessionStore, (state) => state === "connected" ? "ok" : state === "failed" || state === "incompatible" ? "bad" : state === "idle" ? "cancelled" : undefined);
  // Stored before the dial: the native side starts the service on its own when
  // the channel reaches `connected`, with whatever text it was last given.
  postServiceNotification("connected", host);
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
    dial: (signal) => dial(host, "bulk", signal),
    appVersion: APP_VERSION,
    nextConnectionEpoch: () => { throw new Error("bulk lanes reuse the control epoch"); },
    store,
    bulk: { expectedServerIdentity: connection.serverIdentity, connectionEpoch: epoch },
    log: (line) => log(`bulk ${line}`),
    reconnectTimer: backgroundTimer(),
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

export async function disconnectHost({ keepVoiceSessions = false }: { keepVoiceSessions?: boolean } = {}): Promise<void> {
  dropBulk();
  const connection = control;
  control = null;
  controlHost = null;
  connection?.disconnect();
  filesStore.getState().clearAll();
  // Voice sessions are registered per connection on the host; without one (or on another host) they are dead weight.
  if (!keepVoiceSessions) voiceRegistry.disposeAll();
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
