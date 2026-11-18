import type { Pane } from "../../app/types";
import type { PanelSessionContext } from "../shell/panelExtensions";
import type { AiToolCall } from "./types";

/**
 * The tools the assistant may ask for, in the OpenAI table shape, passed
 * through the native proxy untouched. The set is small on purpose: each tool
 * is something the panel can do with plumbing it already has. The two that
 * change anything — typing into a pane, starting a run — wait on the user's
 * approval; the three that only look around (read a screen, list panes, show
 * a pane) answer from state the panel already holds and need none.
 */
export const AI_TOOLS = [
  {
    type: "function",
    function: {
      name: "send_to_pane",
      description:
        "Type text into a terminal pane of the current tab. The user approves every use and sees the exact text first. "
        + "Use press_enter=true only when the text is complete and meant to run; with false it only lands on the input line.",
      parameters: {
        type: "object",
        properties: {
          text: { type: "string", description: "The text to type. One command, or a short block." },
          pane: { type: "integer", description: "Pane index in the current window. Defaults to the active pane." },
          press_enter: { type: "boolean", description: "Press Enter after the text, submitting it. Default false." },
        },
        required: ["text"],
      },
    },
  },
  {
    type: "function",
    function: {
      name: "read_pane",
      description:
        "Read a pane's screen as plain text: what the user sees right now, plus as much scrollback as the terminal holds. "
        + "The truthful way to find out what an agent or command printed — the question-time snapshot never contains screen text. Read-only.",
      parameters: {
        type: "object",
        properties: {
          pane: { type: "integer", description: "Pane index in the current window. Defaults to the active pane." },
          lines: { type: "integer", description: "Rows to read from the bottom. Default 200." },
        },
      },
    },
  },
  {
    type: "function",
    function: {
      name: "list_panes",
      description:
        "List the panes of the current tab as they are right now: index, active, working directory and running command, "
        + "plus any coding agents in them. Call it after acting — the snapshot from question time is stale by then.",
      parameters: { type: "object", properties: {} },
    },
  },
  {
    type: "function",
    function: {
      name: "show_pane",
      description: "Bring one pane of the current tab on screen. Harmless: it only moves what the user is looking at.",
      parameters: {
        type: "object",
        properties: { pane: { type: "integer", description: "The pane index to show." } },
        required: ["pane"],
      },
    },
  },
  {
    type: "function",
    function: {
      name: "run_automation",
      description:
        "Start one scheduled automation on this host now, outside its schedule. The user approves it; it opens a new tab and runs the agent there.",
      parameters: {
        type: "object",
        properties: { automation_id: { type: "string", description: "The automation's id, as the Automations snapshot named it." } },
        required: ["automation_id"],
      },
    },
  },
] as const;

/** Tools that change something outside the screen; each one waits on the user. */
export const APPROVAL_TOOLS: ReadonlySet<string> = new Set(["send_to_pane", "run_automation"]);

/** The longest text the assistant may type into a pane in one call. */
const MAX_SEND_CHARS = 8_000;
/** Rows `read_pane` takes when the model does not say, and its hard ceiling. */
const DEFAULT_READ_LINES = 200;
const MAX_READ_LINES = 2_000;
/** Characters `read_pane` returns; the newest tail of the screen is kept. */
const MAX_READ_CHARS = 16_000;

/** One human line naming a call, for the approval card and the action chip. */
export function summarizeCall(call: AiToolCall): string {
  const args = parseArguments(call);
  switch (call.name) {
    case "send_to_pane": {
      const text = typeof args.text === "string" ? args.text : "";
      const oneLine = text.replace(/\s+/g, " ").trim();
      const shown = oneLine.length > 80 ? `${oneLine.slice(0, 77)}…` : oneLine;
      const where = args.pane !== undefined ? `pane ${args.pane}` : "the active pane";
      return args.press_enter === true ? `type “${shown}” into ${where} and press Enter` : `type “${shown}” into ${where}`;
    }
    case "read_pane":
      return args.pane !== undefined ? `read pane ${args.pane}` : "read the active pane's screen";
    case "list_panes":
      return "read the current tab's panes";
    case "show_pane":
      return `show pane ${args.pane}`;
    case "run_automation":
      return `run automation “${args.automation_id}” now`;
    default:
      return call.name;
  }
}

/** The command in full when the summary had to flatten or cut it, for the
 * card's click-to-expand; undefined when the summary already says all. */
export function callDetail(call: AiToolCall): string | undefined {
  if (call.name !== "send_to_pane") return undefined;
  const text = parseArguments(call).text;
  if (typeof text !== "string") return undefined;
  const oneLine = text.replace(/\s+/g, " ").trim();
  // The summary flattens whitespace and cuts at eighty characters; either
  // loss means the full text is worth a click.
  return text.trim() !== oneLine || oneLine.length > 80 ? text : undefined;
}

/** The approval card's question, naming the risk in its own words. */
export function approvalTitle(name: string): string {
  return name === "run_automation" ? "Run this automation now?" : "Type this into the terminal?";
}

export function parseArguments(call: AiToolCall): Record<string, unknown> {
  try {
    const parsed: unknown = JSON.parse(call.arguments);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : {};
  } catch {
    return {};
  }
}

/** What an executor needs, all of it live at the moment the call runs. */
export interface ToolContext {
  /** The shell's current view, fresh at call time — not the question's snapshot. */
  session(): PanelSessionContext | undefined;
  clientId?: string;
  profileId: string;
  /** Brings a pane on screen; the shell's own navigation. */
  openPane(target: { profileId: string; sessionId: string; paneId: string; source: string }): void;
  /** Types into one pane through the host's input lane. */
  sendInput(paneId: string, text: string, pressEnter: boolean): Promise<void>;
  /**
   * The pane's screen as plain text, newest rows last; `undefined` when no
   * mounted terminal holds it. The rows bound is in lines.
   */
  readScreen(paneId: string, rows: number): string | undefined;
  /** Starts one automation now; resolves to what the host said. */
  runAutomation(id: string): Promise<string>;
}

/** A pane of the current window by index, or the active one when no index is given. */
export function resolvePane(session: PanelSessionContext, index: unknown): Pane | undefined {
  const windowId = session.activeWindowId;
  if (!windowId) return undefined;
  const panes = session.snapshot.panes.filter((pane) => pane.windowId === windowId);
  if (typeof index === "number" && Number.isInteger(index)) {
    return panes.find((pane) => pane.index === index);
  }
  return panes.find((pane) => pane.active) ?? panes[0];
}

function describePanes(session: PanelSessionContext): string {
  const windowId = session.activeWindowId;
  const window = session.snapshot.windows.find((candidate) => candidate.id === windowId);
  if (!window) return "The current tab is gone.";
  const panes = session.snapshot.panes.filter((pane) => pane.windowId === window.id);
  if (panes.length === 0) return `Window "${window.name}" has no panes.`;
  const lines = panes.map((pane) =>
    `#${pane.index}${pane.active ? " (active)" : ""}: ${pane.currentPath || "?"} — running ${pane.currentCommand || "?"}`);
  const agents = session.agents.filter((agent) => agent.present && agent.windowId === window.id);
  for (const agent of agents) {
    lines.push(`agent ${agent.displayName || agent.adapterId} in pane of "${window.name}": ${agent.lifecycle}`);
  }
  return lines.join("\n");
}

/**
 * Run one approved (or approval-free) call and answer the model with plain
 * words. Anything the call cannot do comes back as a refusal the model can
 * read — a tool error is information, not an exception.
 */
export async function executeTool(call: AiToolCall, context: ToolContext): Promise<string> {
  const args = parseArguments(call);
  switch (call.name) {
    case "send_to_pane": {
      const text = typeof args.text === "string" ? args.text : "";
      if (!text) return "Nothing to type: the text was empty.";
      if (text.length > MAX_SEND_CHARS) return `Refused: ${text.length} characters is more than one screen of typing.`;
      const session = context.session();
      const pane = session ? resolvePane(session, args.pane) : undefined;
      if (!pane) return "There is no such pane in the current tab.";
      if (!context.clientId) return "The host is not connected, so nothing can be typed.";
      await context.sendInput(pane.id, text, args.press_enter === true);
      return args.press_enter === true
        ? `Typed and submitted in pane #${pane.index}.`
        : `Typed into pane #${pane.index}; the user has not pressed Enter.`;
    }
    case "read_pane": {
      const session = context.session();
      const pane = session ? resolvePane(session, args.pane) : undefined;
      if (!pane) return "There is no such pane in the current tab.";
      const requested = typeof args.lines === "number" && Number.isFinite(args.lines)
        ? Math.floor(args.lines)
        : DEFAULT_READ_LINES;
      const rows = Math.min(Math.max(1, requested), MAX_READ_LINES);
      const text = context.readScreen(pane.id, rows);
      if (text === undefined) return `Pane #${pane.index} is not on screen, so there is nothing to read.`;
      if (!text.trim()) return `Pane #${pane.index}'s screen is empty.`;
      const tail = text.length > MAX_READ_CHARS
        ? `[…older text dropped…]\n${text.slice(-MAX_READ_CHARS)}`
        : text;
      return `Pane #${pane.index}:\n${tail}`;
    }
    case "list_panes": {
      const session = context.session();
      return session ? describePanes(session) : "The shell state is unavailable.";
    }
    case "show_pane": {
      const session = context.session();
      const pane = session ? resolvePane(session, args.pane) : undefined;
      if (!session || !pane) return "There is no such pane in the current tab.";
      context.openPane({
        profileId: context.profileId,
        sessionId: pane.sessionId,
        paneId: pane.id,
        source: `assistant · ${session.activeWindowId ?? ""}`,
      });
      return `Showing pane #${pane.index}.`;
    }
    case "run_automation": {
      const id = typeof args.automation_id === "string" ? args.automation_id : "";
      if (!id) return "No automation id was given.";
      return await context.runAutomation(id);
    }
    default:
      return `Unknown tool: ${call.name}`;
  }
}
