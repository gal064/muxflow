// Typed builders for every Request this app sends (design doc §7.5).
//
// Field usage is verified against the host's dispatcher
// (apps/host/src/service/requests/dispatcher.rs and siblings), which wins over
// the desktop bridge where the two differ. In particular the pane-scoped
// terminal operations — TERMINAL_INPUT, SET_TERMINAL_VISIBILITY,
// REQUEST_TERMINAL_SEED — read the pane id from `Request.scope`, not from
// `pane_ids`; only ATTACH_TERMINAL takes `pane_ids`.

import { create } from "@bufbuild/protobuf";
import {
  AgentRequestSchema,
  FileServiceRequestSchema,
  Operation,
  RequestSchema,
  TmuxActionKind,
  TmuxActionSchema,
  type Request,
} from "./gen/envelope_pb";

export const DIRECTORY_PAGE_SIZE = 500;

const EMPTY = new Uint8Array(0);

export function subscribeFull(): Request {
  return create(RequestSchema, { operation: Operation.SUBSCRIBE, scope: "full" });
}

export function attachTerminal(sessionId: string, paneId: string): Request {
  return create(RequestSchema, { operation: Operation.ATTACH_TERMINAL, sessionId, paneIds: [paneId] });
}

export interface VisibilityCheckpoint {
  /**
   * Must be non-zero on hide: `hide_with_checkpoint` in
   * crates/tmux-control/src/replay.rs refuses epoch 0. The phone uses its
   * connection epoch, as the desktop does.
   */
  terminalEpoch: bigint;
  /** Last `TerminalBytes.generation` this client rendered for the pane; 0 is always accepted. */
  generationCutoff: bigint;
}

export function setTerminalVisibility(paneId: string, visible: boolean, checkpoint: VisibilityCheckpoint): Request {
  return create(RequestSchema, {
    operation: Operation.SET_TERMINAL_VISIBILITY,
    scope: paneId,
    visible,
    data: EMPTY,
    terminalEpoch: checkpoint.terminalEpoch,
    terminalGenerationCutoff: checkpoint.generationCutoff,
  });
}

/** Connection-wide: sizes every window the visible session shows. Requires a selected session. */
export function resizeTerminal(columns: number, rows: number): Request {
  return create(RequestSchema, { operation: Operation.RESIZE_TERMINAL, columns, rows });
}

export function selectTerminalSession(sessionId: string): Request {
  return create(RequestSchema, { operation: Operation.SELECT_TERMINAL_SESSION, sessionId });
}

export function requestTerminalSeed(paneId: string): Request {
  return create(RequestSchema, { operation: Operation.REQUEST_TERMINAL_SEED, scope: paneId });
}

/**
 * §7.6.1: the scrollback above a pane's screen, which a screen-only seed
 * deliberately does not carry. `lines` rows above the `skip` rows the renderer
 * already holds; the answer is one TERMINAL_HISTORY event and moves no
 * generation.
 */
export function requestTerminalHistory(paneId: string, lines: number, skip: number): Request {
  return create(RequestSchema, {
    operation: Operation.REQUEST_TERMINAL_HISTORY,
    scope: paneId,
    terminalHistoryLines: lines,
    terminalHistorySkipLines: skip,
  });
}

/**
 * `paste` delivers the bytes through tmux's paste path, which brackets them
 * iff the pane's application asked for bracketed paste, and is never
 * coalesced with the input around it; the default is keystrokes.
 */
export function terminalInput(paneId: string, data: Uint8Array, options: { paste?: boolean } = {}): Request {
  return create(RequestSchema, {
    operation: Operation.TERMINAL_INPUT,
    scope: paneId,
    data,
    terminalInputPaste: options.paste === true,
  });
}

export function agentSnapshot(expectedServerIdentity: string): Request {
  return create(RequestSchema, {
    operation: Operation.AGENT_SNAPSHOT,
    agent: create(AgentRequestSchema, { expectedServerIdentity }),
  });
}

export function agentMarkSeen(agentId: string, attentionGeneration: bigint, expectedServerIdentity: string): Request {
  return create(RequestSchema, {
    operation: Operation.AGENT_MARK_SEEN,
    agent: create(AgentRequestSchema, { agentId, attentionGeneration, expectedServerIdentity }),
  });
}

export function createWindow(sessionId: string, expectedServerIdentity: string, expectedGeneration: bigint): Request {
  return create(RequestSchema, {
    operation: Operation.TMUX_ACTION,
    tmuxAction: create(TmuxActionSchema, {
      kind: TmuxActionKind.CREATE_WINDOW,
      sessionId,
      expectedServerIdentity,
      expectedGeneration,
    }),
  });
}

export function resolveActiveRoot(
  operationId: string,
  paneId: string,
  expectedServerIdentity: string,
  knownRootToken = "",
): Request {
  return create(RequestSchema, {
    operation: Operation.RESOLVE_ACTIVE_ROOT,
    file: create(FileServiceRequestSchema, { operationId, paneId, expectedServerIdentity, knownRootToken }),
  });
}

export interface RootedPath {
  root: string;
  rootToken: string;
  /** Exactly as returned in `FileMetadata.path`, or `root` itself. */
  path: string;
}

export function listDirectory(
  operationId: string,
  target: RootedPath,
  expectedServerIdentity: string,
  pageToken = "",
): Request {
  return create(RequestSchema, {
    operation: Operation.LIST_DIRECTORY,
    file: create(FileServiceRequestSchema, {
      operationId,
      root: target.root,
      rootToken: target.rootToken,
      path: target.path,
      pageSize: DIRECTORY_PAGE_SIZE,
      pageToken,
      expectedServerIdentity,
    }),
  });
}

export function openFileStream(operationId: string, target: RootedPath, expectedServerIdentity: string): Request {
  return create(RequestSchema, {
    operation: Operation.OPEN_FILE_STREAM,
    file: create(FileServiceRequestSchema, {
      operationId,
      root: target.root,
      rootToken: target.rootToken,
      path: target.path,
      expectedServerIdentity,
    }),
  });
}

/** A fresh file-service operation id (UUID where the runtime has one). */
export function newOperationId(): string {
  const crypto = globalThis.crypto as { randomUUID?: () => string } | undefined;
  if (crypto?.randomUUID) return crypto.randomUUID();
  return `op-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}
