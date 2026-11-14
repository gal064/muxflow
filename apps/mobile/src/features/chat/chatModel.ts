// One chat pane as the phone holds it, and the pure rules that keep it in step
// with the host (docs/design/chat-plan.md §4, "As built by stream A", and
// the comments on `ChatEvent` and `ChatResponse.revision` in chat.proto).
// Nothing here talks to a connection; `ChatClient` does that.

import {
  ChatCommandKind,
  ChatItemKind,
  ChatTurnState,
  type ChatCatalog,
  type ChatCommand,
  type ChatEvent,
  type ChatItem,
  type ChatModel,
  type ChatState,
} from "../../protocol/gen/chat_pb";

/** A message this phone sent that the host has not shown back yet. */
export interface OutgoingMessage {
  /** The `client_message_id`, which becomes the USER item's id. */
  id: string;
  text: string;
  /** The host accepted the send; the next snapshot is the truth about it. */
  acked: boolean;
  /**
   * The SEND's answer was lost (the connection went or the host did not
   * answer). After the next snapshot, one still missing from the timeline is
   * sent again with the same id, which the host refuses as a duplicate if the
   * first one arrived.
   */
  unsure?: boolean;
}

/** A LOAD_OLDER page held back until the pane's revision reaches the page's. */
export interface OlderPage {
  snapshotSeq: number;
  /** The cursor the page was asked for. */
  cursor: string;
  revision: bigint;
  older: ChatItem[];
  olderCursor: string;
}

export interface ChatPane {
  paneId: string;
  /** The tmux server the pane was opened on; pane ids are local to one. */
  serverIdentity: string;
  /**
   * `opening` until the first snapshot; `open` while following it; `gone`
   * once its pane left the topology; `error` when the host refused to open it.
   */
  status: "opening" | "open" | "gone" | "error";
  error: string;
  /** Undefined until a snapshot arrives. */
  revision: bigint | undefined;
  /** Counts snapshots, so a page asked for before the current one is dropped. */
  snapshotSeq: number;
  /** A change was lost: events are ignored until the fresh snapshot. */
  resyncing: boolean;
  state: ChatState | undefined;
  catalog: ChatCatalog | undefined;
  /** Oldest first. */
  items: ChatItem[];
  /** Empty when nothing older can be loaded. */
  olderCursor: string;
  loadingOlder: boolean;
  pendingPage: OlderPage | undefined;
  /**
   * The newest revision at which an `updated` or a `delta` named an item this
   * phone did not hold, so a page read before it is missing that change.
   */
  ignoredUnheldAt: bigint;
  outgoing: OutgoingMessage[];
}

export function newChatPane(paneId: string, serverIdentity: string): ChatPane {
  return {
    paneId,
    serverIdentity,
    status: "opening",
    error: "",
    revision: undefined,
    snapshotSeq: 0,
    resyncing: false,
    state: undefined,
    catalog: undefined,
    items: [],
    olderCursor: "",
    loadingOlder: false,
    pendingPage: undefined,
    ignoredUnheldAt: 0n,
    outgoing: [],
  };
}

export interface EventOutcome {
  pane: ChatPane;
  /** A change was lost, or a truncate named an item this phone does not hold: send OPEN again. */
  reopen: boolean;
  /** A held-back page became current; the client applies it. */
  page?: OlderPage | undefined;
}

/**
 * Applies one chat event. A snapshot replaces everything; any other event
 * applies only at exactly the next revision, is ignored at or below the
 * current one, and is a lost change above it.
 */
export function applyChatEvent(pane: ChatPane, event: ChatEvent): EventOutcome {
  const change = event.change;
  if (change.case === "snapshot") {
    const snapshot = change.value;
    const items = [...snapshot.items];
    return {
      pane: {
        ...pane,
        status: "open",
        error: "",
        revision: snapshot.revision,
        snapshotSeq: pane.snapshotSeq + 1,
        resyncing: false,
        state: snapshot.state,
        catalog: snapshot.catalog,
        items,
        olderCursor: snapshot.olderCursor,
        loadingOlder: false,
        pendingPage: undefined,
        ignoredUnheldAt: 0n,
        // The snapshot is the truth for everything the host already accepted.
        outgoing: pane.outgoing.filter((message) => !message.acked && !items.some((item) => item.id === message.id)),
      },
      reopen: false,
    };
  }
  if (pane.revision === undefined || pane.resyncing || pane.status !== "open") return { pane, reopen: false };
  if (event.revision <= pane.revision) return { pane, reopen: false };
  if (event.revision !== pane.revision + 1n) return { pane: { ...pane, resyncing: true }, reopen: true };

  let next: ChatPane = { ...pane, revision: event.revision };
  switch (change.case) {
    case "item": {
      const item = change.value;
      next.items = [...pane.items.filter((held) => held.id !== item.id), item];
      next.outgoing = pane.outgoing.filter((message) => message.id !== item.id);
      break;
    }
    case "updated": {
      const item = change.value;
      const index = pane.items.findIndex((held) => held.id === item.id);
      if (index < 0) {
        next.ignoredUnheldAt = event.revision;
        break;
      }
      next.items = pane.items.map((held, at) => (at === index ? item : held));
      break;
    }
    case "delta": {
      const delta = change.value;
      const index = pane.items.findIndex((held) => held.id === delta.itemId);
      if (index < 0) {
        next.ignoredUnheldAt = event.revision;
        break;
      }
      const held = pane.items[index]!;
      next.items = pane.items.map((other, at) => (at === index ? { ...held, text: held.text + delta.textAppend } : other));
      break;
    }
    case "state":
      next.state = change.value;
      break;
    case "catalog":
      next.catalog = change.value;
      break;
    case "truncate": {
      const after = change.value.afterItemId;
      if (after === "") {
        // Rewinding the first message drops everything, loaded or not.
        next = { ...next, items: [], olderCursor: "", pendingPage: undefined };
        break;
      }
      const index = pane.items.findIndex((held) => held.id === after);
      if (index < 0) return { pane: { ...pane, resyncing: true }, reopen: true };
      next.items = pane.items.slice(0, index + 1);
      break;
    }
    default:
      break;
  }
  const page = next.pendingPage;
  if (page && next.revision !== undefined && next.revision >= page.revision) {
    next.pendingPage = undefined;
    return { pane: next, reopen: false, page };
  }
  return { pane: next, reopen: false };
}

export type PageOutcome =
  | { kind: "applied"; pane: ChatPane }
  | { kind: "held"; pane: ChatPane }
  | { kind: "dropped"; pane: ChatPane }
  | { kind: "refetch"; pane: ChatPane };

/**
 * One LOAD_OLDER page. It is dropped when it answers a request made before
 * the current snapshot or for a cursor the pane no longer has, held back while
 * the pane's revision is behind the page's, and asked for again when a change
 * to an item the phone did not hold was ignored after the page was read.
 */
export function applyOlderPage(pane: ChatPane, page: OlderPage): PageOutcome {
  if (page.snapshotSeq !== pane.snapshotSeq || page.cursor !== pane.olderCursor || pane.status !== "open") {
    return { kind: "dropped", pane: { ...pane, loadingOlder: false } };
  }
  if (pane.revision === undefined || pane.revision < page.revision) {
    return { kind: "held", pane: { ...pane, pendingPage: page } };
  }
  if (pane.ignoredUnheldAt > page.revision) return { kind: "refetch", pane: { ...pane, pendingPage: undefined } };
  const held = new Set(pane.items.map((item) => item.id));
  return {
    kind: "applied",
    pane: {
      ...pane,
      items: [...page.older.filter((item) => !held.has(item.id)), ...pane.items],
      olderCursor: page.olderCursor,
      loadingOlder: false,
      pendingPage: undefined,
    },
  };
}

// ---- what the screen reads ---------------------------------------------------

export function turnOf(pane: ChatPane | undefined): ChatTurnState {
  return pane?.state?.turn ?? ChatTurnState.UNSPECIFIED;
}

export function currentModel(pane: ChatPane | undefined): ChatModel | undefined {
  const id = pane?.state?.options?.model;
  return id ? pane?.catalog?.models.find((model) => model.id === id) : undefined;
}

/** "Opus 5.5" for a listed model; the raw id for one the catalog does not list. */
export function modelLabel(pane: ChatPane | undefined): string {
  return currentModel(pane)?.label ?? pane?.state?.options?.model ?? "";
}

/** "High" from "high", "Xhigh" from "xhigh". */
export function effortLabel(effort: string): string {
  return effort ? effort.charAt(0).toUpperCase() + effort.slice(1) : "";
}

/** "209k / 1M"; "209k" when the window is unknown; empty with no usage. */
export function usageLabel(pane: ChatPane | undefined): string {
  const usage = pane?.state?.usage;
  if (!usage || usage.usedTokens === 0n) return "";
  const used = compactTokens(usage.usedTokens);
  return usage.windowTokens > 0n ? `${used} / ${compactTokens(usage.windowTokens)}` : used;
}

/** 0..1 of the context window, or undefined when the window is unknown. */
export function usageFraction(pane: ChatPane | undefined): number | undefined {
  const usage = pane?.state?.usage;
  if (!usage || usage.windowTokens === 0n) return undefined;
  return Math.min(1, Number(usage.usedTokens) / Number(usage.windowTokens));
}

export function compactTokens(tokens: bigint): string {
  const value = Number(tokens);
  if (value >= 1_000_000) return `${trimDecimal(value / 1_000_000)}M`;
  if (value >= 1_000) return `${Math.round(value / 1_000)}k`;
  return String(value);
}

function trimDecimal(value: number): string {
  return Number.isInteger(value) ? String(value) : value.toFixed(1).replace(/\.0$/, "");
}

/** The id the user item for `text` will carry, and what the screen shows meanwhile. */
export function newClientMessageId(): string {
  const crypto = globalThis.crypto as { randomUUID?: () => string } | undefined;
  if (crypto?.randomUUID) return crypto.randomUUID();
  return `m-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`;
}

/** The host's agent mode as Settings shows it. */
export function agentModeLabel(mode: "terminal" | "chat" | "unknown"): string {
  return mode === "chat" ? "Chat" : mode === "terminal" ? "Terminal" : "Unknown";
}

// ---- the slash list ------------------------------------------------------------

export interface CommandGroup {
  title: string;
  commands: ChatCommand[];
}

const GROUPS: { kind: ChatCommandKind; title: string }[] = [
  { kind: ChatCommandKind.MUXFLOW, title: "Muxflow" },
  { kind: ChatCommandKind.COMMAND, title: "Commands" },
  { kind: ChatCommandKind.SKILL, title: "Skills" },
];

/** At most this many entries show at once; typing narrows the rest. */
export const SLASH_LIST_LIMIT = 30;

/**
 * The composer's slash list for `draft`: shown while the draft is a slash
 * followed by a name and nothing after it. Prefix matches rank before
 * matches inside the name. Muxflow's entries need a provider that can fork.
 */
export function slashMatches(catalog: ChatCatalog | undefined, draft: string): CommandGroup[] {
  const match = /^\/(\S*)$/.exec(draft);
  if (!match || !catalog) return [];
  const query = match[1]!.toLowerCase();
  const canFork = catalog.capabilities?.fork === true;
  const ranked = catalog.commands
    .filter((command) => command.kind !== ChatCommandKind.MUXFLOW || canFork)
    .map((command) => ({ command, at: command.name.toLowerCase().indexOf(query) }))
    .filter((entry) => entry.at >= 0)
    .sort((a, b) => Number(a.at !== 0) - Number(b.at !== 0) || a.command.name.localeCompare(b.command.name))
    .slice(0, SLASH_LIST_LIMIT)
    .map((entry) => entry.command);
  const groups: CommandGroup[] = [];
  for (const { kind, title } of GROUPS) {
    const commands = ranked.filter((command) => command.kind === kind);
    if (commands.length > 0) groups.push({ title, commands });
  }
  const other = ranked.filter((command) => !GROUPS.some((group) => group.kind === command.kind));
  if (other.length > 0) groups.push({ title: "Commands", commands: other });
  return groups;
}

/**
 * `/btw why?` and `/side why?` are Muxflow's own: a side question in a fork.
 * Answers the question, or undefined when the draft is anything else or the
 * provider cannot fork.
 */
export function sideQuestion(catalog: ChatCatalog | undefined, draft: string): string | undefined {
  if (catalog?.capabilities?.fork !== true) return undefined;
  const match = /^\/(\S+)\s+([\s\S]*\S[\s\S]*)$/.exec(draft.trim());
  if (!match) return undefined;
  const name = match[1]!;
  const muxflow = catalog.commands.some((command) => command.kind === ChatCommandKind.MUXFLOW && command.name === name);
  return muxflow ? match[2]!.trim() : undefined;
}

// ---- voice -----------------------------------------------------------------------

/**
 * The reply voice mode reads aloud when a turn ends: the newest assistant
 * message after the newest user message. Undefined when the turn did not just
 * end, when nothing new was said, or when it was already read.
 */
export function replyToSpeak(previousTurn: ChatTurnState, pane: ChatPane | undefined, lastSpokenId: string | undefined): ChatItem | undefined {
  if (!pane || turnOf(pane) !== ChatTurnState.IDLE) return undefined;
  if (previousTurn !== ChatTurnState.WORKING && previousTurn !== ChatTurnState.WAITING) return undefined;
  for (let index = pane.items.length - 1; index >= 0; index -= 1) {
    const item = pane.items[index]!;
    if (item.kind === ChatItemKind.USER) return undefined;
    if (item.kind === ChatItemKind.ASSISTANT && item.text.trim() !== "") {
      return item.id === lastSpokenId ? undefined : item;
    }
  }
  return undefined;
}
