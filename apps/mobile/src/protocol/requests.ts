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
  VoiceRequestSchema,
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
export function terminalInput(paneId: string, data: Uint8Array, options: { paste?: boolean; agentId?: string } = {}): Request {
  return create(RequestSchema, {
    operation: Operation.TERMINAL_INPUT,
    scope: paneId,
    data,
    terminalInputPaste: options.paste === true,
    terminalInputAgentId: options.agentId ?? "",
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

// Voice (docs/mobile/voice-mode-plan.md §3). Every voice operation reads
// `Request.voice` only; `scope`, `data` and the other payloads stay empty. The
// host echoes `operationId` in `Response.voice.operationId` on success and on
// error alike, and carries the retry hint there too.

/**
 * VOICE_STATUS: readiness probe. `warm` asks a READY host to spawn and load
 * the sidecar in a detached task so the first utterance is not the cold one;
 * the phone sets it when the Voice screen opens. Never provisions anything.
 */
export function voiceStatus(operationId: string, warm = false): Request {
  return create(RequestSchema, {
    operation: Operation.VOICE_STATUS,
    voice: create(VoiceRequestSchema, { operationId, warm }),
  });
}

/**
 * VOICE_PROVISION: the ~640 MB model download. `confirmed` is the user's
 * consent and the host refuses without it (`voice_consent_required`), so the
 * builder sets it: the consent dialog is what calls this. Progress arrives as
 * EVENT_KIND_VOICE_PROVISION events; the response is the READY status.
 */
export function voiceProvision(operationId: string): Request {
  return create(RequestSchema, {
    operation: Operation.VOICE_PROVISION,
    voice: create(VoiceRequestSchema, { operationId, confirmed: true }),
  });
}

/**
 * VOICE_TRANSCRIBE: one recorded utterance -> one-line transcript. `audio` is
 * the whole file (expo-audio's `.m4a`, so `audioMime` is "audio/mp4"); the host
 * caps it and rejects an unknown mime. No `languageHint`: parakeet v3 detects
 * the language itself.
 */
export function voiceTranscribe(operationId: string, audio: Uint8Array, audioMime: string): Request {
  return create(RequestSchema, {
    operation: Operation.VOICE_TRANSCRIBE,
    voice: create(VoiceRequestSchema, { operationId, audio, audioMime }),
  });
}

/**
 * VOICE_SPEAK: `text` -> mp3 bytes, for replaying a reply whose pushed audio
 * failed, or arbitrary text. `voice` empty means the host default
 * ("en-US-AvaNeural"); `provider` is left unspecified, which the host reads as
 * EDGE_TTS.
 */
export function voiceSpeak(operationId: string, text: string, voice = ""): Request {
  return create(RequestSchema, {
    operation: Operation.VOICE_SPEAK,
    voice: create(VoiceRequestSchema, { operationId, text, voice }),
  });
}

/**
 * VOICE_SESSION: register `agentId` as the agent whose replies this connection
 * wants pushed as EVENT_KIND_VOICE_REPLY; "" clears it. Per connection, so it
 * is re-sent after every reconnect and refreshed periodically (the host holds
 * a 10-minute TTL). No operation id: the answer is a bare ok/error.
 */
export function voiceSession(agentId: string): Request {
  return create(RequestSchema, {
    operation: Operation.VOICE_SESSION,
    voice: create(VoiceRequestSchema, { agentId }),
  });
}

/** A fresh file-service operation id (UUID where the runtime has one). */
export function newOperationId(): string {
  const crypto = globalThis.crypto as { randomUUID?: () => string } | undefined;
  if (crypto?.randomUUID) return crypto.randomUUID();
  return `op-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}
