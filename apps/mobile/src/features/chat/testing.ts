// Scripted chat events for the tests, shaped like the host's scripted provider
// (apps/host/src/service/chat/scripted.rs): its catalog, its finished
// transcript's last exchange, and the question and permission turns that
// stop on a request. Test-only.

import { create, type MessageInitShape } from "@bufbuild/protobuf";
import {
  type ChatBackgroundTask,
  ChatBackgroundTaskSchema,
  ChatCommandKind,
  ChatConnection,
  ChatEventSchema,
  ChatItemKind,
  ChatPermissionMode,
  ChatRequestKind,
  ChatSpeed,
  ChatTaskKind,
  ChatToolKind,
  ChatToolStatus,
  ChatTurnState,
  type ChatCatalog,
  ChatCatalogSchema,
  type ChatEvent,
  type ChatItem,
  ChatItemSchema,
  type ChatPendingRequest,
  ChatPendingRequestSchema,
  type ChatState,
  ChatStateSchema,
  type ChatTool,
} from "../../protocol/gen/chat_pb";
import { applyChatEvent, newChatPane, type ChatPane } from "./chatModel";
import type { ComposerDictation } from "./ui/ComposerMic";

export const FILE = "crates/terminal-interactions/src/links.rs";
export const PANE = "%7";
export const SERVER = "server-a";
const START = 1_767_225_600_000;

export function catalog(overrides: MessageInitShape<typeof ChatCatalogSchema> = {}): ChatCatalog {
  const command = (name: string, description: string, argumentHint: string, kind: ChatCommandKind) => ({ name, description, argumentHint, kind });
  return create(ChatCatalogSchema, {
    capabilities: { rewind: true, sendMidTurn: true, model: true, effort: true, speed: true, permission: true, fork: true, questions: true, attachments: true },
    models: [
      { id: "opus-5-5", label: "Opus 5.5", efforts: ["low", "medium", "high", "xhigh", "max"], supportsFast: true },
      { id: "fable-5-2", label: "Fable 5.2", efforts: ["low", "medium", "high"], supportsFast: false },
      { id: "sonnet-5-5", label: "Sonnet 5.5", efforts: [], supportsFast: true },
    ],
    commands: [
      command("compact", "Summarise the conversation to free context", "", ChatCommandKind.COMMAND),
      command("clear", "Start over in this session", "", ChatCommandKind.COMMAND),
      command("code-review", "Review the current diff for bugs", "[level] [target]", ChatCommandKind.SKILL),
      command("diagnose-crash", "Find out why a program crashed", "", ChatCommandKind.SKILL),
      command("btw", "Ask a side question in a fork", "<question>", ChatCommandKind.MUXFLOW),
      command("side", "Ask a side question in a fork", "<question>", ChatCommandKind.MUXFLOW),
    ],
    ...overrides,
  } as MessageInitShape<typeof ChatCatalogSchema>);
}

export function state(overrides: MessageInitShape<typeof ChatStateSchema> = {}): ChatState {
  return create(ChatStateSchema, {
    paneId: PANE,
    adapterId: "scripted",
    nativeSessionId: `scripted-${PANE}`,
    turn: ChatTurnState.IDLE,
    connection: ChatConnection.READY,
    options: { model: "opus-5-5", effort: "high", permission: ChatPermissionMode.AUTO, speed: ChatSpeed.STANDARD },
    usage: { usedTokens: 209_000n, windowTokens: 1_000_000n },
    historyComplete: true,
    ...overrides,
  } as MessageInitShape<typeof ChatStateSchema>);
}

let minute = 0;

export function text(id: string, kind: ChatItemKind, body: string, overrides: MessageInitShape<typeof ChatItemSchema> = {}): ChatItem {
  minute += 1;
  return create(ChatItemSchema, { id, kind, text: body, rewindable: kind === ChatItemKind.USER, atUnixMillis: BigInt(START + minute * 60_000), ...overrides });
}

export function tool(id: string, fields: Omit<Partial<ChatTool>, "$typeName" | "$unknown">): ChatItem {
  minute += 1;
  return create(ChatItemSchema, {
    id,
    kind: ChatItemKind.TOOL,
    atUnixMillis: BigInt(START + minute * 60_000),
    tool: { kind: ChatToolKind.COMMAND, title: "run", status: ChatToolStatus.OK, ...fields },
  });
}

/** The phone mock "Chat": a question, the answer, and a test run. */
export function chatItems(): ChatItem[] {
  return [
    text("q1", ChatItemKind.USER, "Find out why wrapped paths are missed."),
    text("think", ChatItemKind.REASONING, "Boxed output is hard-wrapped by the agent, so the soft-wrap join never sees it."),
    text("a1", ChatItemKind.ASSISTANT, "The detector joins rows that the terminal marks as soft-wrapped. Boxed output is hard-wrapped by the agent."),
    tool("read", { kind: ChatToolKind.READ, title: "read", detail: FILE, path: FILE, resultSummary: "done" }),
    tool("test", {
      title: "run",
      detail: "cargo test links",
      resultSummary: "ok",
      outputPreview: "running 14 tests\n..............\ntest result: ok. 14 passed; 0 failed\n",
    }),
  ];
}

/** A subagent's row as Claude Code reports it: its type, what it was asked, what it does now and its figures. */
export function subagent(id: string, fields: Omit<Partial<ChatTool>, "$typeName" | "$unknown"> = {}): ChatItem {
  return tool(id, { kind: ChatToolKind.SUBAGENT, title: "agent", agentType: "Explore", detail: "Find the other callers", ...fields });
}

/**
 * The steps of the phone mock's finished turn (mobile-chat-mock.html, S2):
 * a read, a test run with output, an edit, and a command that failed.
 */
export function stepItems(): ChatItem[] {
  return [
    tool("s-read", { kind: ChatToolKind.READ, title: "read", detail: FILE, path: FILE }),
    tool("s-test", { title: "run", detail: "cargo test -p terminal-interactions links", resultSummary: "14 passed", outputPreview: "test result: ok. 14 passed; 0 failed\n" }),
    tool("s-edit", { kind: ChatToolKind.EDIT, title: "edit", detail: FILE, path: FILE, added: 3, removed: 1 }),
    tool("s-clippy", { title: "run", detail: "cargo clippy", status: ChatToolStatus.FAILED, resultSummary: "exit 101" }),
  ];
}

/** One background task, started `ranSeconds` before now. */
export function backgroundTask(id: string, description: string, fields: { kind?: ChatTaskKind; ranSeconds?: number; stoppable?: boolean } = {}): ChatBackgroundTask {
  return create(ChatBackgroundTaskSchema, {
    id,
    description,
    kind: fields.kind ?? ChatTaskKind.COMMAND,
    startedAtUnixMillis: fields.ranSeconds === undefined ? 0n : BigInt(Date.now() - fields.ranSeconds * 1000),
    stoppable: fields.stoppable ?? true,
  });
}

export function permissionRequest(): ChatPendingRequest {
  return create(ChatPendingRequestSchema, {
    requestId: "turn-2-permission",
    kind: ChatRequestKind.PERMISSION,
    title: "Edit a file?",
    detail: "links.rs",
  });
}

export function questionRequest(): ChatPendingRequest {
  const option = (id: string, label: string, description: string) => ({ id, label, description });
  return create(ChatPendingRequestSchema, {
    requestId: "turn-1-question",
    kind: ChatRequestKind.QUESTION,
    title: "How should the wrapped-path fix be scoped?",
    questions: [
      {
        id: "scope",
        header: "Scope",
        prompt: "How should the wrapped-path fix be scoped?",
        options: [
          option("boxed", "Boxed output only", "Strip box borders before joining rows. Smallest change."),
          option("any", "Any hard-wrapped path", "Also join rows without a border. Catches more, with some risk."),
        ],
        multiSelect: false,
        allowOther: true,
      },
      {
        id: "checks",
        header: "Checks",
        prompt: "Which checks should run afterwards?",
        options: [
          option("tests", "Unit tests", "cargo test for the crate."),
          option("clippy", "Clippy", "With warnings denied."),
          option("fixture", "A new fixture", "One boxed transcript."),
        ],
        multiSelect: true,
        allowOther: false,
      },
    ],
  });
}

// ---- events -----------------------------------------------------------------------

export function snapshotEvent(revision: bigint, items: ChatItem[], fields: { state?: ChatState; catalog?: ChatCatalog; olderCursor?: string } = {}): ChatEvent {
  return create(ChatEventSchema, {
    paneId: PANE,
    revision,
    change: { case: "snapshot", value: { revision, state: fields.state ?? state(), catalog: fields.catalog ?? catalog(), items, olderCursor: fields.olderCursor ?? "" } },
  });
}

export function itemEvent(revision: bigint, item: ChatItem): ChatEvent {
  return create(ChatEventSchema, { paneId: PANE, revision, change: { case: "item", value: item } });
}

export function updatedEvent(revision: bigint, item: ChatItem): ChatEvent {
  return create(ChatEventSchema, { paneId: PANE, revision, change: { case: "updated", value: item } });
}

export function deltaEvent(revision: bigint, itemId: string, textAppend: string): ChatEvent {
  return create(ChatEventSchema, { paneId: PANE, revision, change: { case: "delta", value: { itemId, textAppend } } });
}

export function stateEvent(revision: bigint, value: ChatState): ChatEvent {
  return create(ChatEventSchema, { paneId: PANE, revision, change: { case: "state", value } });
}

export function truncateEvent(revision: bigint, afterItemId: string): ChatEvent {
  return create(ChatEventSchema, { paneId: PANE, revision, change: { case: "truncate", value: { afterItemId } } });
}

/** A pane that has taken `events` in order, from a fresh open. */
export function paneFrom(events: ChatEvent[]): ChatPane {
  let pane = newChatPane(PANE, SERVER);
  for (const event of events) pane = applyChatEvent(pane, event).pane;
  return pane;
}

/** The question turn as the host runs it: reasoning, a running test that finishes, a streamed reply, then the question. */
export function questionTurnEvents(from: bigint): ChatEvent[] {
  let revision = from;
  const next = () => (revision += 1n);
  const running = tool("turn-1-test", { title: "run", detail: "cargo test -p terminal-interactions links", status: ChatToolStatus.RUNNING });
  const passed = tool("turn-1-test", { title: "run", detail: "cargo test -p terminal-interactions links", status: ChatToolStatus.OK, resultSummary: "14 passed" });
  const reply = text("turn-1-reply", ChatItemKind.ASSISTANT, "", { streaming: true });
  return [
    itemEvent(next(), text("turn-1-ask", ChatItemKind.USER, "Add the fixture and run the tests.")),
    stateEvent(next(), state({ turn: ChatTurnState.WORKING })),
    itemEvent(next(), running),
    updatedEvent(next(), passed),
    itemEvent(next(), reply),
    deltaEvent(next(), "turn-1-reply", "The detector joins rows the terminal marks as soft-wrapped. "),
    deltaEvent(next(), "turn-1-reply", "Before I change it, two things to settle."),
    updatedEvent(next(), text("turn-1-reply", ChatItemKind.ASSISTANT, "The detector joins rows the terminal marks as soft-wrapped. Before I change it, two things to settle.")),
    stateEvent(next(), state({ turn: ChatTurnState.WAITING, pending: questionRequest() })),
  ];
}

/** The reply field's dictation with nothing in the way and no take on, each action recorded by name. */
export function dictation(fields: Partial<ComposerDictation> = {}, calls: string[] = []): ComposerDictation {
  return {
    phase: "idle",
    refusal: "",
    onPressIn: () => calls.push("pressIn"),
    onRelease: () => calls.push("release"),
    onBegin: () => calls.push("begin"),
    onSubmit: () => calls.push("submit"),
    onLock: () => calls.push("lock"),
    onCancel: () => calls.push("cancel"),
    onRefused: (reason) => calls.push(`refused: ${reason}`),
    ...fields,
  };
}
