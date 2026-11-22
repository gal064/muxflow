// "New terminal" (design doc §9.4): TMUX_ACTION CREATE_WINDOW with the one
// stale_topology retry §7.5 asks for. "New agent" and "New terminal" in the
// `random` workspace (docs/design/quick-session-and-shortcuts-plan.md §4.2)
// create that workspace first when it is missing.

import { HostError, type HostConnection } from "../../protocol/HostConnection";
import { createSession, createWindow, terminalInput } from "../../protocol/requests";
import type { Request, Response } from "../../protocol/gen/envelope_pb";
import { log } from "../../session/log";
import type { SessionStore } from "../../store/sessionStore";
import { utf8Encode } from "./bytes";

export interface CreatedWindow { windowId: string; paneId: string }
export type CreateWindowKind = "terminal" | "agent";

export interface ConnectionScope {
  connection: HostConnection;
  connectionEpoch: bigint;
  serverIdentity: string;
  sessionId: string;
}

export type AgentCommandDelivery =
  | { ok: true }
  | { ok: false; error: Error };

export interface CreatedAgentWindow extends CreatedWindow {
  scope: ConnectionScope;
  commandDelivery: Promise<AgentCommandDelivery>;
}

export interface CreateTerminalWindowOptions {
  kind?: CreateWindowKind;
  staleWaitMs?: number;
}

/**
 * How long a stale_topology retry waits for the newer TOPOLOGY_SNAPSHOT to
 * land. Returns the moment it does, so a fast link is unchanged; the ceiling
 * is link-sized (the desktop's `actionReconciliation.ts`, 1 s) because a 250 ms
 * wait was never met by a 300 ms round trip and an ordinary refusal reached
 * the user (docs/bugs/slow-link.md).
 */
export const STALE_TOPOLOGY_WAIT_MS = 1_000;

function waitForNewerGeneration(store: SessionStore, seen: bigint, timeoutMs: number): Promise<void> {
  return new Promise((resolve) => {
    if (store.getState().topologyGeneration > seen) return resolve();
    const timer = setTimeout(() => {
      unsubscribe();
      resolve();
    }, timeoutMs);
    const unsubscribe = store.subscribe((state) => {
      if (state.topologyGeneration > seen) {
        clearTimeout(timer);
        unsubscribe();
        resolve();
      }
    });
  });
}

/**
 * Sends one TMUX_ACTION built for the store's generation at send time. The host
 * reconciles topology in the background, so a `stale_topology` refusal is
 * ordinary; the fresh TOPOLOGY_SNAPSHOT travels on the ordered event channel
 * and can land just after the refusal, so the retry waits briefly for a newer
 * generation, then builds and sends once more. A second refusal propagates.
 */
async function requestWithStaleRetry(
  connection: HostConnection,
  store: SessionStore,
  staleWaitMs: number,
  event: string,
  kind: CreateWindowKind,
  build: (generation: bigint) => { request: Request; target: string },
): Promise<Response> {
  let attemptNumber = 0;
  const attempt = async () => {
    attemptNumber += 1;
    const generation = store.getState().topologyGeneration;
    const { request, target } = build(generation);
    log(`${event} request kind=${kind} attempt=${attemptNumber} ${target} topology=${generation}`);
    try {
      const response = await connection.request(request);
      const result = response.tmuxActionResult;
      log(`${event} response kind=${kind} attempt=${attemptNumber} result=${result?.paneId ? "present" : "missing"} window=${result?.windowId || "none"} pane=${result?.paneId || "none"} responseTopology=${result?.topologyGeneration ?? 0n} storeTopology=${store.getState().topologyGeneration}`);
      return response;
    } catch (error) {
      log(`${event} refused kind=${kind} attempt=${attemptNumber} code=${diagnosticErrorCode(error)} storeTopology=${store.getState().topologyGeneration}`);
      throw Object.assign(error instanceof Error ? error : new Error(String(error)), { generation });
    }
  };
  try {
    return await attempt();
  } catch (error) {
    if (!(error instanceof HostError) || error.code !== "stale_topology") throw error;
    await waitForNewerGeneration(store, (error as { generation?: bigint }).generation ?? 0n, staleWaitMs);
    return attempt();
  }
}

/** `expectedGeneration` is read from the store at call time; see `requestWithStaleRetry`. */
export async function createTerminalWindow(
  connection: HostConnection,
  store: SessionStore,
  sessionId: string,
  options: CreateTerminalWindowOptions = {},
): Promise<CreatedWindow> {
  const kind = options.kind ?? "terminal";
  const response = await requestWithStaleRetry(connection, store, options.staleWaitMs ?? STALE_TOPOLOGY_WAIT_MS, "create.window", kind, (generation) => ({
    request: createWindow(sessionId, connection.serverIdentity, generation),
    target: `session=${sessionId}`,
  }));
  const result = response.tmuxActionResult;
  if (!result || !result.paneId) throw new HostError("missing_result", "the host did not return the new pane");
  return { windowId: result.windowId, paneId: result.paneId };
}

/**
 * Opens the same interactive shell as New terminal, then queues the configured
 * command as ordinary keystrokes once the host has acknowledged the pane ID.
 * The delivery promise always settles to an outcome: creating the terminal is
 * successful even when sending its first line is not.
 */
export async function createAgentWindow(
  connection: HostConnection,
  store: SessionStore,
  sessionId: string,
  command: string,
  getCurrentConnection: () => HostConnection | null,
): Promise<CreatedAgentWindow> {
  const scope: ConnectionScope = {
    connection,
    connectionEpoch: connection.connectionEpoch,
    serverIdentity: connection.serverIdentity,
    sessionId,
  };
  const created = await createTerminalWindow(connection, store, sessionId, { kind: "agent" });
  const bytes = utf8Encode(`${command}\n`);
  const commandDelivery = deliverAgentCommand(scope, created.paneId, bytes, getCurrentConnection);
  return { ...created, scope, commandDelivery };
}

/** The workspace New agent and New terminal open in. Matched by exact name. */
export const RANDOM_WORKSPACE = "random";

export interface OpenInRandomOptions {
  kind: CreateWindowKind;
  /** Where a created workspace, or a window in an existing one, starts: absolute or `~`. */
  directory: string;
  /** What an agent window types first. Required, and not blank, for `kind: "agent"`. */
  command?: string;
  getCurrentConnection: () => HostConnection | null;
  staleWaitMs?: number;
}

export interface OpenedInRandom extends CreatedWindow {
  sessionId: string;
  /** True when this call created the `random` workspace. */
  createdWorkspace: boolean;
  scope: ConnectionScope;
  /** Present for an agent: settles once its command was sent, or could not be. */
  commandDelivery?: Promise<AgentCommandDelivery>;
}

export class MissingAgentCommandError extends Error {
  constructor() {
    super("no agent command is set");
    this.name = "MissingAgentCommandError";
  }
}

/** The session named exactly `random` (names are compared raw, never glyph-stripped). */
export function findRandomSession(store: SessionStore): string | undefined {
  return Object.values(store.getState().sessions).find((session) => session.name === RANDOM_WORKSPACE)?.id;
}

/**
 * Opens a terminal, or an agent, in the `random` workspace: CREATE_SESSION
 * when it is missing, CREATE_WINDOW in it when it is present. A
 * `stale_topology` refusal is retried once, and the retry looks the workspace
 * up again, since the topology that changed may be the one that made it. An
 * agent's command is delivered as in `createAgentWindow`, only while the
 * connection that created the pane is still the current one.
 */
export async function openInRandom(
  connection: HostConnection,
  store: SessionStore,
  options: OpenInRandomOptions,
): Promise<OpenedInRandom> {
  const command = options.command ?? "";
  if (options.kind === "agent" && command.trim().length === 0) throw new MissingAgentCommandError();
  const epoch = connection.connectionEpoch;
  // Set by each attempt: a retry may find the workspace the first one missed,
  // and reads the identity again, as `createTerminalWindow` does.
  let target = { create: true } as { create: true } | { create: false; sessionId: string };
  let sentIdentity = "";
  const response = await requestWithStaleRetry(connection, store, options.staleWaitMs ?? STALE_TOPOLOGY_WAIT_MS, "create.random", options.kind, (generation) => {
    const sessionId = findRandomSession(store);
    const serverIdentity = connection.serverIdentity;
    sentIdentity = serverIdentity;
    target = sessionId === undefined ? { create: true } : { create: false, sessionId };
    return sessionId === undefined
      ? {
        request: createSession(RANDOM_WORKSPACE, options.directory, serverIdentity, generation),
        target: "op=session",
      }
      : {
        request: createWindow(sessionId, serverIdentity, generation, options.directory),
        target: `op=window session=${sessionId}`,
      };
  });
  const result = response.tmuxActionResult;
  const sessionId = target.create ? result?.sessionId ?? "" : target.sessionId;
  if (!result || !result.paneId || !sessionId) throw new HostError("missing_result", "the host did not return the new pane");
  // A create on a host with no tmux server starts one: take the identity it
  // has now, which is still `tmux:none` until its snapshot lands (see
  // `isConnectionScopeCurrent`).
  const scope: ConnectionScope = { connection, connectionEpoch: epoch, serverIdentity: target.create ? connection.serverIdentity : sentIdentity, sessionId };
  const opened: OpenedInRandom = { windowId: result.windowId, paneId: result.paneId, sessionId, createdWorkspace: target.create, scope };
  if (options.kind === "agent") {
    opened.commandDelivery = deliverAgentCommand(scope, result.paneId, utf8Encode(`${command}\n`), options.getCurrentConnection);
  }
  return opened;
}

/** What the host reports while no tmux server runs. */
export const NO_TMUX_SERVER = "tmux:none";

/**
 * The first tmux server on an empty host starts on the same connection, so a
 * scope begun against no server still belongs to the server its create
 * started (the desktop's `same_host_connection`). Only that direction: a
 * scope bound to a server does not survive that server ending.
 */
export function isConnectionScopeCurrent(
  scope: ConnectionScope,
  getCurrentConnection: () => HostConnection | null,
): boolean {
  const identity = scope.connection.serverIdentity;
  return getCurrentConnection() === scope.connection
    && scope.connection.state === "connected"
    && scope.connection.connectionEpoch === scope.connectionEpoch
    && (identity === scope.serverIdentity || (scope.serverIdentity === NO_TMUX_SERVER && identity !== ""));
}

async function deliverAgentCommand(
  scope: ConnectionScope,
  paneId: string,
  bytes: Uint8Array,
  getCurrentConnection: () => HostConnection | null,
): Promise<AgentCommandDelivery> {
  if (!isConnectionScopeCurrent(scope, getCurrentConnection)) {
    const error = new Error("the connection changed before the agent command could be sent");
    log(`create.window command.failed kind=agent pane=${paneId} bytes=${bytes.byteLength} epoch=${scope.connectionEpoch} code=connection_scope_changed`);
    return { ok: false, error };
  }
  log(`create.window command.send kind=agent pane=${paneId} bytes=${bytes.byteLength} epoch=${scope.connectionEpoch}`);
  try {
    await scope.connection.request(terminalInput(paneId, bytes));
    log(`create.window command.sent kind=agent pane=${paneId} bytes=${bytes.byteLength} epoch=${scope.connectionEpoch}`);
    return { ok: true };
  } catch (error) {
    const failure = error instanceof Error ? error : new Error(String(error));
    log(`create.window command.failed kind=agent pane=${paneId} bytes=${bytes.byteLength} epoch=${scope.connectionEpoch} code=${diagnosticErrorCode(error)}`);
    return { ok: false, error: failure };
  }
}

function diagnosticErrorCode(error: unknown): string {
  if (error instanceof HostError) return error.code;
  return error instanceof Error ? error.name : typeof error;
}
