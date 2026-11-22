// Typed builders for every Request this app sends (design doc §7.5).
//
// Field usage is verified against the host's dispatcher
// (apps/host/src/service/requests/dispatcher.rs and siblings), which wins over
// the desktop bridge where the two differ. In particular the pane-scoped
// terminal operations — TERMINAL_INPUT, SET_TERMINAL_VISIBILITY,
// REQUEST_TERMINAL_SEED — read the pane id from `Request.scope`, not from
// `pane_ids`; only ATTACH_TERMINAL takes `pane_ids`.

import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { ChatAction, ChatAnswerSchema, ChatOptionsSchema, ChatRequestSchema } from "./gen/chat_pb";
import {
  AgentRequestSchema,
  CollisionPolicy,
  FileServiceRequestSchema,
  GitRequestSchema,
  GitSummaryBase,
  Operation,
  RequestSchema,
  TmuxActionKind,
  TmuxActionSchema,
  VoiceRequestSchema,
  type Request,
} from "./gen/envelope_pb";
import { HistoryAction, HistoryRequestSchema } from "./gen/history_pb";

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

/** Claim the exact window displayed on this device; older helpers reject this operation. */
export function resizeTerminalWindow(sessionId: string, windowId: string, columns: number, rows: number): Request {
  return create(RequestSchema, { operation: Operation.RESIZE_TERMINAL_WINDOW, sessionId, scope: windowId, columns, rows });
}

export function selectTerminalSession(sessionId: string): Request {
  return create(RequestSchema, { operation: Operation.SELECT_TERMINAL_SESSION, sessionId });
}

/** Keep the terminal attached while this connection's selected client stops sizing tmux. */
export function yieldTerminalSizing(): Request {
  return create(RequestSchema, { operation: Operation.YIELD_TERMINAL_SIZING });
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
export function terminalInput(paneId: string, data: Uint8Array, options: { paste?: boolean; voice?: boolean; expectedServerIdentity?: string } = {}): Request {
  return create(RequestSchema, {
    operation: Operation.TERMINAL_INPUT,
    scope: paneId,
    data,
    terminalInputPaste: options.paste === true,
    terminalInputVoice: options.voice === true,
    terminalInputExpectedServerIdentity: options.expectedServerIdentity ?? "",
  });
}

export function agentSnapshot(expectedServerIdentity: string): Request {
  return create(RequestSchema, {
    operation: Operation.AGENT_SNAPSHOT,
    agent: create(AgentRequestSchema, { expectedServerIdentity }),
  });
}

/** Privacy-safe host lifecycle ring, fetched only when Copy Diagnostics is tapped. */
export function agentDiagnostics(): Request {
  return create(RequestSchema, {
    operation: Operation.AGENT_DIAGNOSTICS,
    agent: create(AgentRequestSchema),
  });
}

export function agentMarkSeen(agentId: string, attentionGeneration: bigint, expectedServerIdentity: string): Request {
  return create(RequestSchema, {
    operation: Operation.AGENT_MARK_SEEN,
    agent: create(AgentRequestSchema, { agentId, attentionGeneration, expectedServerIdentity }),
  });
}

/**
 * `directory` is where the window starts; empty keeps the host's choice (the
 * current pane's path).
 */
export function createWindow(sessionId: string, expectedServerIdentity: string, expectedGeneration: bigint, directory = ""): Request {
  return create(RequestSchema, {
    operation: Operation.TMUX_ACTION,
    tmuxAction: create(TmuxActionSchema, {
      kind: TmuxActionKind.CREATE_WINDOW,
      sessionId,
      expectedServerIdentity,
      expectedGeneration,
      directory,
    }),
  });
}

/** A new tmux session named `name` whose first pane starts in `directory` (absolute or `~`). */
export function createSession(name: string, directory: string, expectedServerIdentity: string, expectedGeneration: bigint): Request {
  return create(RequestSchema, {
    operation: Operation.TMUX_ACTION,
    tmuxAction: create(TmuxActionSchema, {
      kind: TmuxActionKind.CREATE_SESSION,
      name,
      directory,
      expectedServerIdentity,
      expectedGeneration,
    }),
  });
}

export type CloseTarget =
  | { kind: "session"; sessionId: string }
  | { kind: "window"; sessionId: string; windowId: string }
  | { kind: "pane"; sessionId: string; windowId: string; paneId: string };

const CLOSE_KINDS = {
  session: TmuxActionKind.CLOSE_SESSION,
  window: TmuxActionKind.CLOSE_WINDOW,
  pane: TmuxActionKind.CLOSE_PANE,
} as const;

/**
 * Closing a window's only pane closes the window too (tmux). The hold sheet's
 * tap is the consent, so `confirmed` is always set. No generation is pinned:
 * the target id plus server identity is the whole guard, as the desktop's
 * `closePrecondition` (features/tmux/actions.ts) explains.
 */
export function closeTmuxTarget(target: CloseTarget, expectedServerIdentity: string): Request {
  return create(RequestSchema, {
    operation: Operation.TMUX_ACTION,
    tmuxAction: create(TmuxActionSchema, {
      kind: CLOSE_KINDS[target.kind],
      sessionId: target.sessionId,
      windowId: target.kind === "session" ? "" : target.windowId,
      paneId: target.kind === "pane" ? target.paneId : "",
      expectedServerIdentity,
      expectedGeneration: 0n,
      confirmed: true,
    }),
  });
}

/** A workspace pin names no window; an agent's pin is its tab's, as on the desktop. */
export type PinTarget = Exclude<CloseTarget, { kind: "pane" }>;

/**
 * SET_PINNED writes the flag it is given, so a repeat is harmless. Guarded like
 * a close: by target id and server identity, no generation.
 */
export function setPinnedTarget(target: PinTarget, pinned: boolean, expectedServerIdentity: string): Request {
  return create(RequestSchema, {
    operation: Operation.TMUX_ACTION,
    tmuxAction: create(TmuxActionSchema, {
      kind: TmuxActionKind.SET_PINNED,
      sessionId: target.sessionId,
      windowId: target.kind === "window" ? target.windowId : "",
      pinned,
      expectedServerIdentity,
      expectedGeneration: 0n,
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

export interface TerminalFileRoute {
  sessionId: string;
  windowId: string;
  cwd: string;
}

/** Resolves one terminal-emitted path against the exact pane route the user tapped. */
export function resolveTerminalFile(
  operationId: string,
  paneId: string,
  path: string,
  expectedServerIdentity: string,
  expectedTopologyGeneration: bigint,
  route: TerminalFileRoute,
): Request {
  return create(RequestSchema, {
    operation: Operation.RESOLVE_TERMINAL_FILE,
    file: create(FileServiceRequestSchema, {
      operationId,
      paneId,
      path,
      expectedServerIdentity,
      expectedTopologyGeneration,
      expectedSessionId: route.sessionId,
      expectedWindowId: route.windowId,
      expectedCwd: route.cwd,
    }),
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

// Git, read only (docs/design/mobile-chat-plan.md §3 "Git rows"). Control
// lane, in `Request.git`. `root` and `rootToken` are an active root's, which
// is the worktree's root inside a repository; the host refuses a folder
// outside one with `not_git_repository`.

/**
 * GIT_BRANCH_SUMMARY: what the working tree changes, in
 * `Response.git.branchSummary`. `summaryBase` unset compares with the merge
 * base of HEAD and the default branch; `GitSummaryBase.HEAD` with HEAD, which
 * is the uncommitted work, and that answer's `baseRef` is "HEAD".
 */
export function gitBranchSummary(
  operationId: string,
  root: string,
  rootToken: string,
  expectedServerIdentity: string,
  summaryBase: GitSummaryBase = GitSummaryBase.UNSPECIFIED,
): Request {
  return create(RequestSchema, {
    operation: Operation.GIT_BRANCH_SUMMARY,
    git: create(GitRequestSchema, { operationId, root, rootToken, expectedServerIdentity, summaryBase }),
  });
}

/** GIT_BRANCHES: the local branches and the current one, in `Response.git.branches`. */
export function gitBranches(operationId: string, root: string, rootToken: string, expectedServerIdentity: string): Request {
  return create(RequestSchema, {
    operation: Operation.GIT_BRANCHES,
    git: create(GitRequestSchema, { operationId, root, rootToken, expectedServerIdentity }),
  });
}

// Terminal upload (docs/design/mobile-chat-plan.md §3 "Attachments"): how a
// file reaches the host's upload folder. Bulk lane only, and all four on the
// same connection: the host keeps an upload with the connection that prepared
// it and removes the partial when that connection goes. Field usage follows
// the desktop's client (crates/desktop-core/src/connection/files/upload_manager.rs).
// `transferId` must be a UUID: the host refuses any other id.

/** The most bytes one WRITE_TERMINAL_UPLOAD_CHUNK carries; the host refuses a larger chunk. */
export const UPLOAD_CHUNK_BYTES = 1024 * 1024;

/**
 * PREPARE_TERMINAL_UPLOAD: reserves `name` in the upload folder for
 * `totalBytes`. A name already taken is renamed, "shot.png" to "shot (1).png";
 * the answer's `upload.destinationName` is the name reserved.
 */
export function prepareTerminalUpload(transferId: string, name: string, totalBytes: bigint): Request {
  return create(RequestSchema, {
    operation: Operation.PREPARE_TERMINAL_UPLOAD,
    file: create(FileServiceRequestSchema, {
      operationId: transferId,
      transferId,
      destination: name,
      sourceName: name,
      totalBytes,
      collisionPolicy: CollisionPolicy.RENAME,
    }),
  });
}

/**
 * WRITE_TERMINAL_UPLOAD_CHUNK: 1 byte to 1 MiB at `offset`, which must be
 * where the last chunk ended. The answer's `transferChunk.offset` is the next
 * offset.
 */
export function writeTerminalUploadChunk(transferId: string, offset: bigint, totalBytes: bigint, content: Uint8Array): Request {
  return create(RequestSchema, {
    operation: Operation.WRITE_TERMINAL_UPLOAD_CHUNK,
    file: create(FileServiceRequestSchema, { operationId: transferId, transferId, offset, totalBytes, content }),
  });
}

/**
 * COMMIT_TERMINAL_UPLOAD: `blake3` is the hex BLAKE3 of every byte sent. The
 * host publishes the file only when its own digest agrees, and answers with
 * `upload.finalPath`.
 */
export function commitTerminalUpload(transferId: string, totalBytes: bigint, blake3: string): Request {
  return create(RequestSchema, {
    operation: Operation.COMMIT_TERMINAL_UPLOAD,
    file: create(FileServiceRequestSchema, { operationId: transferId, transferId, totalBytes, blake3 }),
  });
}

/** CANCEL_TERMINAL_UPLOAD: removes the partial. An upload the host no longer holds is not an error. */
export function cancelTerminalUpload(transferId: string): Request {
  return create(RequestSchema, {
    operation: Operation.CANCEL_TERMINAL_UPLOAD,
    file: create(FileServiceRequestSchema, { operationId: transferId, transferId }),
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
 * VOICE_PROVISION: the 697 MiB model download. `confirmed` is the user's
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
 * caps it and rejects an unknown mime. The selected model is English-only.
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
 * VOICE_SESSION: register `paneId` as the pane whose root replies this connection
 * wants pushed as EVENT_KIND_VOICE_REPLY; "" clears it. Per connection, so it
 * is re-sent after every reconnect and refreshed periodically (the host holds
 * a 10-minute TTL). No operation id: the answer is a bare ok/error.
 */
export function voiceSession(paneId: string, expectedServerIdentity = ""): Request {
  return create(RequestSchema, {
    operation: Operation.VOICE_SESSION,
    voice: create(VoiceRequestSchema, { paneId, expectedServerIdentity }),
  });
}

// Chat (docs/design/chat-plan.md §4). Every chat operation reads
// `Request.chat` only. A refusal is a `HostError` whose `code` says why
// (`chat_turn_busy`, `chat_request_not_pending`, `chat_cursor_invalid`, …).

/** The pane a chat action names, on the tmux server the caller saw it on. */
export interface ChatTarget {
  paneId: string;
  expectedServerIdentity: string;
}

function chat(action: ChatAction, target: ChatTarget, fields: MessageInitShape<typeof ChatRequestSchema> = {}): Request {
  return create(RequestSchema, {
    operation: Operation.CHAT,
    chat: create(ChatRequestSchema, { ...fields, action, paneId: target.paneId, expectedServerIdentity: target.expectedServerIdentity }),
  });
}

/**
 * CHAT_ACTION_OPEN: subscribes this connection to the pane. The pane's
 * snapshot arrives through `onChatEvent` before this request resolves, and
 * every later change follows it there, each with the next `revision`.
 * `ChatEvent` in chat.proto says how to follow them. Send this again on a
 * gap, and for every open chat after a reconnect: subscriptions are per
 * connection.
 */
export function chatOpen(target: ChatTarget): Request {
  return chat(ChatAction.OPEN, target);
}

/** CHAT_ACTION_CLOSE: ends this connection's subscription. The chat keeps running. */
export function chatClose(target: ChatTarget): Request {
  return chat(ChatAction.CLOSE, target);
}

/** A file SEND carries: `path` is an upload's `finalPath`, `name` what the message shows. */
export interface ChatSendAttachment {
  path: string;
  name: string;
}

/**
 * CHAT_ACTION_SEND: `clientMessageId` becomes the id of the USER item this
 * message appears as. `attachments` are files already in the host's upload
 * folder, at most 10; the host works out each one's type and size itself.
 * `text` may be empty when there are any.
 */
export function chatSend(target: ChatTarget, text: string, clientMessageId: string, attachments: readonly ChatSendAttachment[] = []): Request {
  return chat(ChatAction.SEND, target, {
    text,
    clientMessageId,
    attachments: attachments.map(({ path, name }) => ({ path, name })),
  });
}

/** CHAT_ACTION_INTERRUPT: stops the running turn. */
export function chatInterrupt(target: ChatTarget): Request {
  return chat(ChatAction.INTERRUPT, target);
}

/** CHAT_ACTION_ANSWER: answers `ChatState.pending`, named by its `requestId`. */
export function chatAnswer(target: ChatTarget, requestId: string, answer: MessageInitShape<typeof ChatAnswerSchema>): Request {
  return chat(ChatAction.ANSWER, target, { requestId, answer });
}

/** CHAT_ACTION_REWIND: drops the `rewindable` USER item and everything after it; the edited text is then a normal send. */
export function chatRewind(target: ChatTarget, itemId: string): Request {
  return chat(ChatAction.REWIND, target, { itemId });
}

/** CHAT_ACTION_SET_OPTIONS: an empty string or unspecified enum keeps the current value. */
export function chatSetOptions(target: ChatTarget, options: MessageInitShape<typeof ChatOptionsSchema>): Request {
  return chat(ChatAction.SET_OPTIONS, target, { options });
}

/** CHAT_ACTION_LOAD_OLDER: the page before `olderCursor`, in `Response.chat.older`, with the next cursor. */
export function chatLoadOlder(target: ChatTarget, olderCursor: string): Request {
  return chat(ChatAction.LOAD_OLDER, target, { olderCursor });
}

/** CHAT_ACTION_SIDE_FORK (`/btw`, `/side`): forks into a split and sends `text` there. */
export function chatSideFork(target: ChatTarget, text: string): Request {
  return chat(ChatAction.SIDE_FORK, target, { text });
}

/**
 * CHAT_ACTION_FORK: copies the conversation through the end of the turn
 * `itemId` is in into a new pane, named by `Response.chat.forkedPaneId`. Any
 * item of the turn names it; the newest turn can be forked only once it has
 * ended.
 */
export function chatFork(target: ChatTarget, itemId: string): Request {
  return chat(ChatAction.FORK, target, { itemId });
}

/** CHAT_ACTION_STOP_TASK: stops the background task `taskId`, one of `ChatState.background`. The answer is empty; the task leaves the list by a later state event. */
export function chatStopTask(target: ChatTarget, taskId: string): Request {
  return chat(ChatAction.STOP_TASK, target, { taskId });
}

/**
 * CHAT_ACTION_TERMINAL_COMMANDS: the slash commands and skills of the agent
 * in a terminal pane, in `Response.chat.commands`. A host from before it
 * refuses the action, and the terminal then lists none.
 */
export function chatTerminalCommands(target: ChatTarget): Request {
  return chat(ChatAction.TERMINAL_COMMANDS, target);
}

// Session history and snooze (docs/design/chat-plan.md §4), in
// `Request.history`. The phone lists on open and after every SESSION_HISTORY
// event; search is local.

/** The recorded session a history action names. */
export interface HistoryTarget {
  adapterId: string;
  nativeSessionId: string;
}

/** What a history action that acts on tmux quotes, as a tmux action does. */
export interface HistoryTopology {
  expectedServerIdentity: string;
  expectedTopologyGeneration: bigint;
}

function history(action: HistoryAction, fields: MessageInitShape<typeof HistoryRequestSchema> = {}): Request {
  return create(RequestSchema, {
    operation: Operation.SESSION_HISTORY,
    history: create(HistoryRequestSchema, { ...fields, action }),
  });
}

export function historyList(): Request {
  return history(HistoryAction.LIST);
}

export function historyResume(target: HistoryTarget, topology: HistoryTopology): Request {
  return history(HistoryAction.RESUME, { ...target, ...topology });
}

export function historySnooze(target: HistoryTarget, snoozeUntilUnixMillis: bigint, topology: HistoryTopology): Request {
  return history(HistoryAction.SNOOZE, { ...target, ...topology, snoozeUntilUnixMillis });
}

export function historyWake(target: HistoryTarget): Request {
  return history(HistoryAction.WAKE, target);
}

/** A fresh file-service operation id (UUID where the runtime has one). */
export function newOperationId(): string {
  const crypto = globalThis.crypto as { randomUUID?: () => string } | undefined;
  if (crypto?.randomUUID) return crypto.randomUUID();
  return `op-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}
