// The steps of a turn (its tool calls and thinking) as the phone says them:
// which are drawn, what a folded run's line reads, what each row reads, and
// the words for subagents and background tasks. The rules and strings are the
// desktop's (apps/desktop-gpui/src/chat/timeline.rs: `is_drawn`, `summary`,
// `steps_label`, `subagent_name`, `subagent_line`, `figures`, `tokens`,
// `background_label`, `elapsed`; view.rs: `verb`, `outcome`), with the phone's
// own departures from docs/design/mobile-chat-plan.md §3 said where they are.
// Pure: the components that draw them are ui/StepsRow.tsx and ui/ToolRow.tsx.

import { ChatItemKind, ChatTaskKind, ChatToolKind, ChatToolStatus, ChatTurnState, type ChatItem, type ChatTool } from "../../protocol/gen/chat_pb";

/** Runs of this many drawn steps or more fold into one line. */
export const GROUP_FROM = 3;

/** A tool call or thinking: what a turn does between two pieces of text. */
export function isStep(item: ChatItem): boolean {
  return item.kind === ChatItemKind.TOOL || item.kind === ChatItemKind.REASONING;
}

/** A turn is in progress: the agent works, or waits on the user. */
export function isBusy(turn: ChatTurnState): boolean {
  return turn === ChatTurnState.WORKING || turn === ChatTurnState.WAITING;
}

export function isRunningSubagent(item: ChatItem): boolean {
  return item.tool?.kind === ChatToolKind.SUBAGENT && item.tool.status === ChatToolStatus.RUNNING;
}

/**
 * Whether `item` has a row while the turn is `turn`. Thinking with no text
 * and a tool item without its tool have nothing to show, and thinking that
 * still streams while the turn is in progress is said by the status line
 * instead. A running subagent keeps its row: the desktop moves it under its
 * working line, the phone does not.
 */
export function isDrawn(item: ChatItem, turn: ChatTurnState): boolean {
  switch (item.kind) {
    case ChatItemKind.USER:
    case ChatItemKind.ASSISTANT:
    case ChatItemKind.NOTICE:
      return true;
    case ChatItemKind.REASONING:
      return item.text.trim() !== "" && !(isBusy(turn) && item.streaming);
    case ChatItemKind.TOOL:
      return item.tool !== undefined;
    default:
      return false;
  }
}

/** Where the run of steps at the end of `items` starts: after the newest message, reply text or notice. */
export function trailingRunStart(items: readonly ChatItem[]): number {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    if (!isStep(items[index]!)) return index + 1;
  }
  return 0;
}

/**
 * How many steps a turn in progress has drawn since its newest message, reply
 * text or notice: what the status line and the newest group's line both count.
 */
export function stepsSoFar(items: readonly ChatItem[], turn: ChatTurnState): number {
  let steps = 0;
  for (let index = trailingRunStart(items); index < items.length; index += 1) {
    if (isDrawn(items[index]!, turn)) steps += 1;
  }
  return steps;
}

/** "N steps", or "N steps so far" while the turn adds to them. */
export function stepsLabel(steps: number, live: boolean): string {
  const noun = steps === 1 ? "step" : "steps";
  return live ? `${steps} ${noun} so far` : `${steps} ${noun}`;
}

/** "8s", "2m 05s", "1h 02m". */
export function elapsed(millis: number): string {
  const seconds = Math.max(0, Math.floor(millis / 1000));
  const two = (value: number) => String(value).padStart(2, "0");
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${two(seconds % 60)}s`;
  return `${Math.floor(seconds / 3600)}h ${two(Math.floor((seconds % 3600) / 60))}m`;
}

/** When the host says something began, as this phone's clock reads it; undefined when the host did not say. */
export function startedAt(unixMillis: bigint): number | undefined {
  return unixMillis > 0n ? Number(unixMillis) : undefined;
}

// ---- a folded run's line ---------------------------------------------------------

/** What a group's line says about its steps. */
export interface StepsLine {
  /** "9 steps", or "3 steps so far" while the turn adds to them. */
  title: string;
  /** "read 1 · ran 2 · edited 1 file"; empty while live, where the status line says what runs. */
  counts: string;
  /** "1 failed"; empty when none did. */
  failed: string;
  /** Lines added and removed by the edits that did not fail. */
  added: number;
  removed: number;
  /** The whole line for a screen reader. */
  label: string;
}

/**
 * The counts by kind, in the desktop's words. The phone does not time
 * thoughts, so thinking is "thought" or "thought N times".
 */
export function stepsCounts(steps: readonly ChatItem[]): { counts: string; failed: number; added: number; removed: number } {
  let thoughts = 0;
  let failed = 0;
  let added = 0;
  let removed = 0;
  let pathlessEdits = 0;
  const files = new Set<string>();
  // Read, ran, searched, fetched, called, subagents, other; edits apart.
  const byKind = [0, 0, 0, 0, 0, 0, 0];
  for (const item of steps) {
    const tool = item.tool;
    if (!tool) {
      thoughts += 1;
      continue;
    }
    const didFail = tool.status === ChatToolStatus.FAILED;
    if (didFail) failed += 1;
    if (tool.kind === ChatToolKind.EDIT) {
      if (tool.path === "") pathlessEdits += 1;
      else files.add(tool.path);
      if (!didFail) {
        added += tool.added;
        removed += tool.removed;
      }
      continue;
    }
    byKind[kindSlot(tool.kind)]! += 1;
  }
  const parts: string[] = [];
  if (thoughts === 1) parts.push("thought");
  else if (thoughts > 1) parts.push(`thought ${thoughts} times`);
  ["read", "ran", "searched", "fetched", "called"].forEach((verb, slot) => {
    if (byKind[slot]! > 0) parts.push(`${verb} ${byKind[slot]}`);
  });
  // A subagent is counted as a noun.
  if (byKind[5] === 1) parts.push("1 subagent");
  else if (byKind[5]! > 1) parts.push(`${byKind[5]} subagents`);
  if (files.size === 1) parts.push("edited 1 file");
  else if (files.size > 1) parts.push(`edited ${files.size} files`);
  else if (pathlessEdits > 0) parts.push(`edited ${pathlessEdits}`);
  if (byKind[6]! > 0) parts.push(`${byKind[6]} other`);
  return { counts: parts.join(" · "), failed, added, removed };
}

function kindSlot(kind: ChatToolKind): number {
  switch (kind) {
    case ChatToolKind.READ:
      return 0;
    case ChatToolKind.COMMAND:
      return 1;
    case ChatToolKind.SEARCH:
      return 2;
    case ChatToolKind.WEB:
      return 3;
    case ChatToolKind.MCP:
      return 4;
    case ChatToolKind.SUBAGENT:
      return 5;
    default:
      return 6;
  }
}

/** The line of a group of `steps`; `live` while its turn still adds to them. */
export function stepsLine(steps: readonly ChatItem[], live: boolean): StepsLine {
  const said = stepsCounts(steps);
  const title = stepsLabel(steps.length, live);
  const failed = said.failed > 0 ? `${said.failed} failed` : "";
  const counts = live ? "" : said.counts;
  return { title, counts, failed, added: said.added, removed: said.removed, label: [title, failed, counts].filter(Boolean).join(", ") };
}

/**
 * The rows a live group shows before the reader opens it: every subagent that
 * still runs, and the newest other step, in the order they happened.
 */
export function liveSteps(steps: readonly ChatItem[]): ChatItem[] {
  let newest = -1;
  for (let index = steps.length - 1; index >= 0; index -= 1) {
    if (!isRunningSubagent(steps[index]!)) {
      newest = index;
      break;
    }
  }
  return steps.filter((item, index) => index === newest || isRunningSubagent(item));
}

// ---- one row ---------------------------------------------------------------------

/** One tool call as a row reads: verb, what it acted on, how it went. */
export interface ToolRowText {
  verb: string;
  subject: string;
  /** What `subject` stands for in full: the whole path of a file named by its name, every line of a command. Equal to `subject` when the row shortened nothing. */
  full: string;
  outcome: string;
  tone: "ok" | "failed" | "working";
  /** An edit that changed lines shows them instead of `outcome`. */
  lines?: { added: number; removed: number };
}

const KIND_VERBS: Partial<Record<ChatToolKind, string>> = {
  [ChatToolKind.COMMAND]: "run",
  [ChatToolKind.READ]: "read",
  [ChatToolKind.EDIT]: "edit",
  [ChatToolKind.SEARCH]: "search",
  [ChatToolKind.WEB]: "web",
  [ChatToolKind.MCP]: "mcp",
  [ChatToolKind.SUBAGENT]: "agent",
};

export function fileName(path: string): string {
  return path.replace(/\/+$/, "").split("/").pop() || path;
}

function firstLine(text: string): string {
  return (text.trim().split("\n", 1)[0] ?? "").trim();
}

export function toolRowText(tool: ChatTool): ToolRowText {
  const subagent = tool.kind === ChatToolKind.SUBAGENT;
  const failed = tool.status === ChatToolStatus.FAILED;
  const running = tool.status === ChatToolStatus.RUNNING;
  // A phone row names a file by its name; the whole path shows when the row opens.
  const fileRow = (tool.kind === ChatToolKind.READ || tool.kind === ChatToolKind.EDIT) && tool.path !== "";
  const full = fileRow ? tool.path : (tool.detail || tool.path).trim();
  const row = {
    verb: subagent ? subagentName(tool)[0] : tool.title || KIND_VERBS[tool.kind] || "tool",
    subject: fileRow ? fileName(tool.path) : firstLine(full),
    full,
  };
  if (tool.kind === ChatToolKind.EDIT && !failed && (tool.added > 0 || tool.removed > 0)) {
    return { ...row, outcome: `+${tool.added} −${tool.removed}`, tone: "ok", lines: { added: tool.added, removed: tool.removed } };
  }
  if (running) return { ...row, outcome: tool.background ? "in background" : tool.resultSummary || "running", tone: "working" };
  if (failed) return { ...row, outcome: tool.resultSummary || "failed", tone: "failed" };
  return { ...row, outcome: tool.resultSummary || "done", tone: "ok" };
}

// ---- subagents -------------------------------------------------------------------

/** What a subagent's row is named by: its type, and the first line of what it was asked. Codex names no type: "Agent". */
export function subagentName(tool: ChatTool): [kind: string, asked: string] {
  return [tool.agentType || "Agent", firstLine(tool.detail)];
}

/** A subagent's figures: "12 tools · 61k tokens". Empty when the provider counts none. */
export function figures(tool: ChatTool): string {
  const parts: string[] = [];
  if (tool.toolUses === 1) parts.push("1 tool");
  else if (tool.toolUses > 1) parts.push(`${tool.toolUses} tools`);
  if (tool.tokens > 0n) parts.push(`${tokens(Number(tool.tokens))} tokens`);
  return parts.join(" · ");
}

/**
 * The line under a running subagent: what it does now, its figures, and how
 * long it has run (`ranMillis`, when known). A subagent that has a type is
 * Claude Code's, which reports all of it; Codex reports only its state, and
 * that is the whole line.
 */
export function subagentLine(tool: ChatTool, ranMillis: number | undefined): string {
  if (tool.agentType === "") return tool.activity || "running";
  return [tool.activity, figures(tool), ranMillis === undefined ? "" : elapsed(ranMillis)].filter(Boolean).join(" · ");
}

/** `950`, `24.1k`, `61k`, `182k`, `1.3M`: a count of tokens as a subagent's line says it. */
export function tokens(count: number): string {
  // One decimal, rounded half up, and none when it is zero.
  const tenths = (unitSize: number, unit: string) => {
    const value = Math.floor((count + unitSize / 20) / (unitSize / 10));
    return value % 10 === 0 ? `${value / 10}${unit}` : `${Math.floor(value / 10)}.${value % 10}${unit}`;
  };
  if (count < 1_000) return String(count);
  if (count < 99_950) return tenths(1_000, "k");
  if (count < 999_500) return `${Math.floor((count + 500) / 1_000)}k`;
  return tenths(1_000_000, "M");
}

// ---- background tasks --------------------------------------------------------------

/** "2 background tasks": the chip above the reply field. */
export function backgroundLabel(count: number): string {
  return count === 1 ? "1 background task" : `${count} background tasks`;
}

/** "Command · 3m 12s": what a background task is and how long it has run, the time left out when the host did not say when it started. */
export function taskLine(kind: ChatTaskKind, ranMillis: number | undefined): string {
  const what = kind === ChatTaskKind.COMMAND ? "Command" : kind === ChatTaskKind.SUBAGENT ? "Subagent" : "Task";
  return ranMillis === undefined ? what : `${what} · ${elapsed(ranMillis)}`;
}
