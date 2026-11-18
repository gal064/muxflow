// What the status line above the reply field says while a turn works, by the
// desktop's rules and in its words (apps/desktop-gpui/src/chat/timeline.rs,
// `doing`, `running`, `elapsed`, `steps_label`; pane.rs, `follow_turn`).
// Pure: the component that draws it is ui/StatusLine.tsx.

import { ChatItemKind, ChatToolKind, ChatToolStatus, ChatTurnState, type ChatItem, type ChatTool } from "../../protocol/gen/chat_pb";
import { turnOf, type ChatPane } from "./chatModel";

/** A tool call or thinking: what a turn does between two pieces of text. */
export function isStep(item: ChatItem): boolean {
  return item.kind === ChatItemKind.TOOL || item.kind === ChatItemKind.REASONING;
}

function isRunningSubagent(item: ChatItem): boolean {
  return item.tool?.kind === ChatToolKind.SUBAGENT && item.tool.status === ChatToolStatus.RUNNING;
}

/** Where the run of steps at the end of `items` starts: after the newest message, reply text or notice. */
function trailingRunStart(items: readonly ChatItem[]): number {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    if (!isStep(items[index]!)) return index + 1;
  }
  return 0;
}

/**
 * What the agent does now, from the newest steps: thinking that streams, else
 * a tool that runs, else the subagents it waits on, else that it works.
 */
export function doing(items: readonly ChatItem[]): string {
  for (let index = items.length - 1, start = trailingRunStart(items); index >= start; index -= 1) {
    const item = items[index]!;
    if (!item.tool) {
      if (item.streaming) return "Thinking…";
      continue;
    }
    // A subagent is counted below instead.
    if (item.tool.status === ChatToolStatus.RUNNING && item.tool.kind !== ChatToolKind.SUBAGENT) return running(item.tool);
  }
  const subagents = items.filter(isRunningSubagent).length;
  if (subagents === 0) return "Working…";
  return subagents === 1 ? "Waiting on 1 subagent…" : `Waiting on ${subagents} subagents…`;
}

/** "Running cargo test…": a running tool's verb and what it acts on. */
export function running(tool: ChatTool): string {
  const detail = tool.detail || tool.path;
  const verb = runningVerb(tool);
  // A tool the chat has no verb for is said by what it does.
  if (verb === undefined) return detail ? `${detail}…` : "Working…";
  return detail ? `${verb} ${detail}…` : `${verb}…`;
}

function runningVerb(tool: ChatTool): string | undefined {
  switch (tool.kind) {
    case ChatToolKind.COMMAND:
      return "Running";
    case ChatToolKind.READ:
      return "Reading";
    case ChatToolKind.EDIT:
      return "Editing";
    case ChatToolKind.SEARCH:
      return "Searching";
    case ChatToolKind.WEB:
      return tool.title === "search" ? "Searching" : "Fetching";
    case ChatToolKind.MCP:
      return "Calling";
    default:
      return undefined;
  }
}

/** How many steps the turn has drawn since its newest message, reply text or notice. */
export function stepsSoFar(items: readonly ChatItem[]): number {
  let steps = 0;
  for (let index = trailingRunStart(items); index < items.length; index += 1) {
    const item = items[index]!;
    if (item.kind === ChatItemKind.TOOL ? item.tool !== undefined : item.text.trim() !== "") steps += 1;
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

/** When what the status line times began, by this phone's clock. */
export interface TurnClock {
  /** The snapshot the times were last worked out for. */
  snapshotSeq: number;
  /** Unix milliseconds; undefined while no turn is in progress. */
  turnStarted: number | undefined;
  /** Unix milliseconds; undefined while the agent is not compacting. */
  compactingSince: number | undefined;
}

export function newTurnClock(): TurnClock {
  return { snapshotSeq: 0, turnStarted: undefined, compactingSince: undefined };
}

/**
 * The clock after the pane changed. A turn is timed from when this phone saw
 * it start; after a snapshot, from the user's newest message, never later than
 * now and never earlier than a start already known. Compacting is timed from
 * when it was first seen. Answers `clock` itself when nothing changed.
 */
export function followTurn(clock: TurnClock, pane: ChatPane | undefined, now: number): TurnClock {
  const snapshotSeq = pane?.snapshotSeq ?? 0;
  const turn = turnOf(pane);
  const busy = turn === ChatTurnState.WORKING || turn === ChatTurnState.WAITING;
  let turnStarted: number | undefined;
  if (!busy) {
    turnStarted = undefined;
  } else if (snapshotSeq === clock.snapshotSeq) {
    turnStarted = clock.turnStarted ?? now;
  } else {
    const since = Math.min(now, sentAt(pane?.items ?? []) ?? now);
    turnStarted = clock.turnStarted === undefined ? since : Math.max(clock.turnStarted, since);
  }
  const compactingSince = pane?.state?.compacting === true ? (clock.compactingSince ?? now) : undefined;
  if (snapshotSeq === clock.snapshotSeq && turnStarted === clock.turnStarted && compactingSince === clock.compactingSince) return clock;
  return { snapshotSeq, turnStarted, compactingSince };
}

/** When the user's newest message was sent, if the host said. */
function sentAt(items: readonly ChatItem[]): number | undefined {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index]!;
    if (item.kind !== ChatItemKind.USER) continue;
    return item.atUnixMillis > 0n ? Number(item.atUnixMillis) : undefined;
  }
  return undefined;
}

/** What the status line shows. */
export interface ChatStatus {
  label: string;
  /** Unix milliseconds the time counts from; undefined shows no time. */
  since: number | undefined;
  /** "7 steps so far"; empty shows nothing. */
  steps: string;
}

/**
 * The status line for `pane`, or undefined when there is none: it shows while
 * the turn works or the agent compacts, and not while the turn waits on the
 * user, whose request card says that. `stopping`: Stop was pressed and the
 * turn has not stopped yet.
 */
export function chatStatus(pane: ChatPane | undefined, clock: TurnClock, stopping: boolean): ChatStatus | undefined {
  if (!pane) return undefined;
  const working = turnOf(pane) === ChatTurnState.WORKING;
  const compacting = pane.state?.compacting === true;
  if (!working && !compacting) return undefined;
  // "Stopping…" stands alone, with no time.
  if (stopping && working) return { label: "Stopping…", since: undefined, steps: "" };
  const count = working ? stepsSoFar(pane.items) : 0;
  const steps = count > 0 ? stepsLabel(count, true) : "";
  // Compacting is said whatever else the turn is doing.
  if (compacting) return { label: "Compacting context…", since: clock.compactingSince, steps };
  return { label: doing(pane.items), since: clock.turnStarted, steps };
}
