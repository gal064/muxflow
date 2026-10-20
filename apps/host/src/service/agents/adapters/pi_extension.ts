// muxflow-managed owner=muxflow adapter=pi
// Reports this pi session's lifecycle (working, blocked, idle) to muxflow.
// muxflow installs and updates this file and overwrites edits to it. Remove it
// from muxflow, or with `muxflow-host hook uninstall --adapter pi`.
//
// Only lifecycle names, the session id and the final assistant message of a
// run are sent. Prompts, tool input and tool output never leave pi.
import { spawn } from "node:child_process";

const HELPER = __MUXFLOW_HELPER__;
const ARGS = [
  "hook", "ingest", "--adapter", "pi",
  "--managed-owner", "__MUXFLOW_MANAGED_OWNER__",
  "--managed-version", "__MUXFLOW_MANAGED_VERSION__",
];
// A pi started inside this pane (for example by the bash tool) inherits the
// pane's TMUX_PANE. This names the one pi process that reports for the pane.
const OWNER = "MUXFLOW_PI_OWNER";
// Long enough for the helper to give up on the daemon and write its durable
// fallback mailbox instead; the same allowance the other adapters' hooks get.
const SEND_TIMEOUT_MS = 5000;

type Report = { hook_event_name: string; session_id: string; last_assistant_message?: string };

function send(report: Report): Promise<void> {
  return new Promise((resolve) => {
    try {
      const child = spawn(HELPER, ARGS, { stdio: ["pipe", "ignore", "ignore"] });
      const timer = setTimeout(() => child.kill(), SEND_TIMEOUT_MS);
      const done = () => {
        clearTimeout(timer);
        resolve();
      };
      child.on("error", done);
      child.on("close", done);
      child.stdin?.on("error", () => {});
      child.stdin?.end(JSON.stringify(report));
    } catch {
      resolve();
    }
  });
}

function assistantText(messages: unknown): string | undefined {
  if (!Array.isArray(messages)) return undefined;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message?.role !== "assistant") continue;
    const text = typeof message.content === "string"
      ? message.content
      : Array.isArray(message.content)
        ? message.content
          .filter((part: any) => part?.type === "text" && typeof part.text === "string")
          .map((part: any) => part.text)
          .join("")
        : "";
    return text.trim() ? text : undefined;
  }
  return undefined;
}

export default function (pi: any) {
  let active = false;
  let sessionId = "";
  let lastMessage: string | undefined;
  let openPrompts = 0;
  // Serialized so the daemon sees events in pi's order. Handlers never wait
  // for it, except shutdown, which pi awaits before it exits.
  let queue: Promise<void> = Promise.resolve();

  const report = (hook_event_name: string, extra: Partial<Report> = {}) => {
    if (!active) return queue;
    const body: Report = { hook_event_name, session_id: sessionId, ...extra };
    queue = queue.then(() => send(body));
    return queue;
  };

  pi.on("session_start", (_event: unknown, ctx: any) => {
    if (ctx?.mode !== "tui" || !process.env.TMUX_PANE) return;
    const owner = process.env[OWNER];
    if (owner && owner !== String(process.pid)) return;
    process.env[OWNER] = String(process.pid);
    active = true;
    sessionId = String(ctx.sessionManager?.getSessionId?.() ?? "");
    lastMessage = undefined;
    openPrompts = 0;
    report("SessionStart");
    // A reload can happen in the middle of a run.
    if (ctx.isIdle?.() === false) report("UserPromptSubmit");
  });

  pi.on("agent_start", () => {
    lastMessage = undefined;
    report("UserPromptSubmit");
  });

  pi.on("agent_end", (event: any) => {
    lastMessage = assistantText(event?.messages) ?? lastMessage;
  });

  // `agent_end` can be followed by a retry, compaction or queued message;
  // settled with an idle agent is the end of the user's turn.
  pi.on("agent_settled", (event: any, ctx: any) => {
    if (ctx?.isIdle?.() !== true) return;
    openPrompts = 0;
    // An aborted run's last text is a partial answer, not the run's reply.
    const message = event?.aborted ? undefined : lastMessage;
    lastMessage = undefined;
    report("Stop", message ? { last_assistant_message: message } : {});
  });

  // An extension dialog during a run waits for the user. One while idle is a
  // command the user is already looking at.
  pi.on("ui_prompt_start", (_event: unknown, ctx: any) => {
    if (ctx?.isIdle?.() !== false) return;
    openPrompts += 1;
    if (openPrompts === 1) report("PermissionRequest");
  });

  pi.on("ui_prompt_end", (_event: unknown, ctx: any) => {
    if (openPrompts === 0) return;
    openPrompts -= 1;
    if (openPrompts === 0 && ctx?.isIdle?.() === false) report("PostToolUse");
  });

  pi.on("session_shutdown", () => {
    const sent = report("SessionEnd");
    active = false;
    return sent;
  });
}
