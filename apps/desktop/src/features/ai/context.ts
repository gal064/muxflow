import type { TmuxSnapshot } from "../../app/types";
import type { AgentRecord } from "../agents/types";
import type { AutomationListing } from "../automations/types";
import { describeMinute, describeSchedule } from "../automations/cronText";
import type { GitStatusSnapshot } from "../git/types";
import type { DetectedPort, PortForward } from "../ports/api";

/**
 * What the AI tab offers the model, one source per panel tab it maps onto.
 * "Session" is the tmux view the whole shell is built on; the rest are the
 * tabs by their own names, so an answer can point at a tab the user can click.
 */
export const AI_SOURCES = [
  { id: "session", label: "Session" },
  { id: "files", label: "Files" },
  { id: "git", label: "Git" },
  { id: "ports", label: "Ports" },
  { id: "automations", label: "Automations" },
] as const;

export type AiSourceId = (typeof AI_SOURCES)[number]["id"];

export const ALL_AI_SOURCES: readonly AiSourceId[] = AI_SOURCES.map((source) => source.id);

/** Everything the tabs are showing, as the panel holds it at the moment of asking. */
export interface AiContextInput {
  hostLabel: string;
  transport: "local" | "ssh";
  connected: boolean;
  /** The live tmux view of the host on screen. */
  snapshot: TmuxSnapshot;
  activeSessionId?: string;
  activeWindowId?: string;
  activePaneId?: string;
  /** The host's agents, as the hooks reported them. */
  agents: readonly AgentRecord[];
  /** The document tabs open in the current workspace. */
  openTabs: readonly { kind: string; title: string; resource: string }[];
  activeRootPath?: string;
  gitStatus?: GitStatusSnapshot;
  gitError?: string;
  portForwards: readonly PortForward[];
  /** Filled in only when the source was on and the host answered `ss`. */
  detectedPorts?: readonly DetectedPort[];
  /** Filled in only when the source was on and the helper answered. */
  automations?: AutomationListing;
  automationsError?: string;
}

const AGENT_LIFECYCLE: Record<string, string> = {
  working: "working",
  blocked: "blocked, waiting for input",
  idle: "idle",
  unknown: "of unknown state",
};

const RUN_STATUS: Record<string, string> = {
  launched: "starting",
  working: "working",
  needs_attention: "needs attention",
  done: "done",
  closed: "tab closed",
  failed: "failed",
  skipped: "skipped",
};

/** What each section may hold at most: a snapshot, not an archive. */
const MAX_LINES = { panes: 24, tabs: 30, entries: 60, ports: 30, automations: 20, agents: 20 } as const;

function sessionSection(input: AiContextInput): string {
  const { snapshot } = input;
  const lines: string[] = [
    `Host: ${input.hostLabel} (${input.transport}, ${input.connected ? "connected" : "disconnected"}).`,
  ];
  if (snapshot.sessions.length === 0) lines.push("No tmux sessions.");
  else {
    lines.push(`Workspaces (tmux sessions): ${snapshot.sessions
      .map((session) => `${session.name} (${session.windowCount} window${session.windowCount === 1 ? "" : "s"}${session.attachedClients ? `, ${session.attachedClients} attached` : ""})`)
      .join(", ")}.`);
  }
  const window = snapshot.windows.find((candidate) => candidate.id === input.activeWindowId);
  const session = snapshot.sessions.find((candidate) => candidate.id === input.activeSessionId)
    ?? (window ? snapshot.sessions.find((candidate) => candidate.id === window.sessionId) : undefined);
  if (window && session) lines.push(`On screen: window "${window.name}" of workspace "${session.name}".`);
  if (window) {
    const panes = snapshot.panes.filter((pane) => pane.windowId === window.id).slice(0, MAX_LINES.panes);
    if (panes.length > 0) {
      lines.push(`Panes in this window: ${panes
        .map((pane) => `#${pane.index}${pane.id === input.activePaneId || pane.active ? " (active)" : ""} in ${pane.currentPath || "?"}${pane.currentCommand ? ` running ${pane.currentCommand}` : ""}`)
        .join("; ")}.`);
    }
  }
  const agents = input.agents.filter((agent) => agent.present).slice(0, MAX_LINES.agents);
  if (agents.length > 0) {
    lines.push(`Coding agents: ${agents
      .map((agent) => `${agent.displayName || agent.adapterId} in ${agent.sessionName}/${agent.windowName} — ${AGENT_LIFECYCLE[agent.lifecycle] ?? agent.lifecycle}`)
      .join("; ")}.`);
  } else {
    lines.push("No coding agents reported by the host.");
  }
  return lines.join("\n");
}

function filesSection(input: AiContextInput): string {
  const lines: string[] = [];
  lines.push(input.activeRootPath
    ? `Explorer root: ${input.activeRootPath}`
    : "The Files tab has no root right now — no pane is open, or the host has not answered.");
  if (input.openTabs.length === 0) lines.push("No documents are open.");
  else {
    lines.push(`Open documents (${input.openTabs.length}): ${input.openTabs
      .slice(0, MAX_LINES.tabs)
      .map((tab) => `${tab.title} [${tab.kind}]`)
      .join("; ")}.`);
  }
  return lines.join("\n");
}

function gitSection(input: AiContextInput): string {
  if (input.gitError) return `The Git tab reports an error: ${input.gitError}`;
  const status = input.gitStatus;
  if (!status) {
    return "The Git tab has no repository — the active pane's directory is not a worktree, or nothing has loaded yet.";
  }
  const head = status.repository.initial ? "no commits yet"
    : status.repository.detachedHead ? `detached at ${(status.repository.headOid ?? "").slice(0, 9)}`
      : `branch ${status.repository.headName ?? "?"}`;
  const lines = [`Repository ${status.repository.worktreeRoot}, ${head}.`];
  if (status.error) lines.push(`The host reported: ${status.error}`);
  if (status.oversized) {
    lines.push(`The status is oversized: only part of ${status.totalEntryCount ?? "many"} entries is listed.`);
  }
  if (status.entries.length === 0) lines.push("No changes.");
  else {
    const listed = status.entries.slice(0, MAX_LINES.entries);
    lines.push(`Changes (${status.entries.length}): ${listed
      .map((entry) => `${(entry.indexStatus + entry.worktreeStatus).trim()} ${entry.displayPath}`)
      .join("; ")}.${status.entries.length > listed.length ? ` …and ${status.entries.length - listed.length} more.` : ""}`);
    if (status.entries.some((entry) => entry.conflicted)) lines.push("Some files are conflicted.");
  }
  return lines.join("\n");
}

function portsSection(input: AiContextInput): string {
  const lines: string[] = [];
  if (input.portForwards.length === 0) lines.push("No port forwards are active.");
  else {
    lines.push(`Forwards: ${input.portForwards
      .map((forward) => `${forward.remotePort}→localhost:${forward.localPort} (${forward.state}${forward.error ? `: ${forward.error}` : ""})`)
      .join("; ")}.`);
  }
  if (input.detectedPorts) {
    lines.push(input.detectedPorts.length === 0
      ? "Nothing is listening on the host (or the check failed)."
      : `Listening on the host: ${input.detectedPorts.slice(0, MAX_LINES.ports)
        .map((port) => `${port.port}${port.process ? ` (${port.process})` : ""}`)
        .join(", ")}.`);
  }
  return lines.join("\n");
}

function automationsSection(input: AiContextInput): string {
  if (input.automationsError) return `The Automations tab could not be read: ${input.automationsError}`;
  const listing = input.automations;
  if (!listing) return "The Automations tab was not reachable for this host.";
  const lines: string[] = [];
  if (listing.fileError) lines.push(`The host cannot read its automations file: ${listing.fileError}`);
  if (listing.automations.length === 0) return [...lines, "No automations are defined on this host."].join("\n");
  for (const summary of listing.automations.slice(0, MAX_LINES.automations)) {
    const state = summary.definitionError ? "invalid" : summary.definition.enabled ? "on" : "paused";
    const last = summary.recentRuns[0];
    lines.push(
      `- ${summary.definition.name || summary.definition.id}: ${describeSchedule(summary.definition.schedule)} `
      + `(\`${summary.definition.schedule}\`), ${state}`
      + (summary.nextRun ? `, next ${describeMinute(summary.nextRun)}` : "")
      + (last ? `; last run #${last.number} ${RUN_STATUS[last.status] ?? last.status}` : "")
      + `${summary.definitionError ? `; definition error: ${summary.definitionError}` : ""}`,
    );
  }
  return lines.join("\n");
}

/** One source's section, or nothing to report when the tab holds nothing. */
export function contextSection(source: AiSourceId, input: AiContextInput): string {
  const body = source === "session" ? sessionSection(input)
    : source === "files" ? filesSection(input)
      : source === "git" ? gitSection(input)
        : source === "ports" ? portsSection(input)
          : automationsSection(input);
  const label = AI_SOURCES.find((candidate) => candidate.id === source)?.label ?? source;
  return `## ${label} tab\n${body}`;
}

/** The snapshot the model is shown: each included source, under its tab name. */
export function buildContext(sources: readonly AiSourceId[], input: AiContextInput): string {
  if (sources.length === 0) return "";
  return AI_SOURCES
    .filter((source) => sources.includes(source.id))
    .map((source) => contextSection(source.id, input))
    .join("\n\n");
}

/**
 * The system prompt. It fixes three things the answer goes wrong without:
 * what the app is, that the snapshot is the only evidence, and that a missing
 * section is answered as "that tab has nothing" rather than invented.
 */
export function buildSystemPrompt(context: string): string {
  return "You are the assistant inside Muxflow, a desktop app for working with tmux sessions on local and SSH hosts. "
    + "Its right-hand panel has tabs — Files, Git, Ports, Automations and this AI tab — and the user is asking about the live session behind them. "
    + "This conversation belongs to one tab of the strip: the window named in the Session section.\n\n"
    + "Below is a snapshot taken from those tabs the moment the question was sent; each section is named for the tab it came from.\n\n"
    + (context || "(No context was included this time.)")
    + "\n\nYou can act on the current tab with tools: send_to_pane types into a pane and run_automation starts a scheduled run — "
    + "the user approves both before anything happens, and a denial is final, so report a denial plainly instead of retrying. "
    + "read_pane shows what a pane's screen actually says (its output is not in the snapshot — read it instead of guessing), "
    + "list_panes shows the tab's panes as they are now (call it after acting — the snapshot goes stale), and show_pane brings a pane on screen. "
    + "Never claim an action ran unless its tool result says it did.\n\n"
    + "Answer from the snapshot and the tool results only. When a section is missing or says it has nothing, say that tab has nothing to report — never invent what it might contain. "
    + "Be brief, and name the tab when pointing at something.";
}
