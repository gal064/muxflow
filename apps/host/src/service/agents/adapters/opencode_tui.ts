// muxflow-managed owner=__MUXFLOW_MANAGED_OWNER__ adapter=opencode
// Reports this OpenCode session's lifecycle (working, blocked, idle) to muxflow.
// muxflow installs and updates this file and overwrites edits to it. Remove it
// from muxflow, or with `muxflow-host hook uninstall --adapter opencode`.
//
// Only lifecycle names, the session id and the final assistant message of a run
// are sent. Prompts, tool input and tool output never leave OpenCode.
//
// WHY A TUI PLUGIN AND NOT A SERVER PLUGIN
// OpenCode runs one shared `opencode serve` process for every terminal, so a
// server plugin cannot tell which tmux pane a session belongs to. This plugin
// runs inside the TUI process instead, which lives in a pane and inherits its
// TMUX_PANE, exactly like the pi extension. The shared server still streams
// every session's events to every TUI, so this file forwards only the sessions
// the terminal it runs in is actually showing (its focused session and its open
// tabs), and it folds subagent sessions onto their root.
//
// THE TUI EVENT BUS
// ctx.data.listen delivers every event as {name, details}: `name` is the
// durable event's type and the payload sits under `details.data`. The bus
// hands each event to listeners two to five times, so every branch dedups on
// the event id. A run begins at session.execution.started and ends at
// session.execution.succeeded; there is no session.status or session.idle on
// this bus — lifecycle must be read off the execution events.
import { spawn } from "node:child_process";
import { Plugin } from "@opencode/plugin/tui";

const HELPER = __MUXFLOW_HELPER__;
const ARGS = [
  "hook", "ingest", "--adapter", "opencode",
  "--managed-owner", "__MUXFLOW_MANAGED_OWNER__",
  "--managed-version", "__MUXFLOW_MANAGED_VERSION__",
];
// An OpenCode started inside this pane (for example by the bash tool) inherits
// the pane's TMUX_PANE. This names the one TUI process that reports for it.
const OWNER = "MUXFLOW_OPENCODE_OWNER";
// Long enough for the helper to give up on the daemon and write its durable
// fallback mailbox instead; the same allowance the other adapters' hooks get.
const SEND_TIMEOUT_MS = 5000;
// The 32 KiB bound muxflow applies to the final message, applied here first so
// a huge answer never becomes a huge spawn payload.
const MESSAGE_CAP = 32 * 1024;

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

export default Plugin.define({
  id: "muxflow.agent-status",
  setup(ctx: any) {
    // Only report inside a tmux pane; a TUI with no pane has nothing to bind to.
    if (!process.env.TMUX_PANE) return () => {};
    const owner = process.env[OWNER];
    if (owner && owner !== String(process.pid)) return () => {};
    process.env[OWNER] = String(process.pid);

    const data = ctx.data;
    // The lifecycle name last sent for each root session, so a redundant edge
    // is reported once.
    const lastSent = new Map<string, string>();
    // The bus re-delivers events; this remembers the ids already acted on.
    const seen = new Set<string>();
    // Serialized so the daemon sees events in order; nothing waits on it.
    let queue: Promise<void> = Promise.resolve();

    const once = (id: unknown): boolean => {
      if (typeof id !== "string") return true;
      if (seen.has(id)) return false;
      seen.add(id);
      if (seen.size > 2048) seen.clear();
      return true;
    };

    // Fold a session onto the root its pane owns. The fold API is not present
    // on every OpenCode version; without it a subagent simply never matches
    // the owned-roots filter below, which is the correct outcome anyway.
    const rootOf = (sessionID: string): string => {
      try {
        return data?.session?.root?.(sessionID) ?? sessionID;
      } catch {
        return sessionID;
      }
    };

    // The roots this terminal is responsible for: its focused session and the
    // roots of its open tabs.
    const ownedRoots = (): Set<string> => {
      const roots = new Set<string>();
      try {
        const route = ctx.ui?.router?.current?.();
        if (route?.type === "session" && route.sessionID) {
          roots.add(rootOf(route.sessionID));
        }
        for (const tab of ctx.ui?.tabs?.list?.() ?? []) {
          if (tab?.sessionID) roots.add(rootOf(tab.sessionID));
        }
      } catch {
        // A UI query that throws must not stop reporting; fall back to whatever
        // roots we already know.
      }
      return roots;
    };

    // The final assistant text of a root session, for voice mode and automations.
    const lastAssistantText = (root: string): string | undefined => {
      try {
        const messages: readonly any[] = data?.session?.message?.list?.(root) ?? [];
        for (let index = messages.length - 1; index >= 0; index -= 1) {
          const message = messages[index];
          if (message?.role !== "assistant") continue;
          const parts = Array.isArray(message.content) ? message.content : [];
          const text = parts
            .filter((part: any) => part?.type === "text" && typeof part.text === "string")
            .map((part: any) => part.text)
            .join("");
          if (!text.trim()) continue;
          return text.length > MESSAGE_CAP ? text.slice(0, MESSAGE_CAP) : text;
        }
      } catch {
        // Best effort only; a Stop without a message is still a Stop.
      }
      return undefined;
    };

    const report = (hook_event_name: string, root: string, withMessage = false) => {
      const body: Report = { hook_event_name, session_id: root };
      if (withMessage) {
        const message = lastAssistantText(root);
        if (message) body.last_assistant_message = message;
      }
      queue = queue.then(() => send(body));
    };

    // Report a lifecycle edge for a root only while this terminal owns it, and
    // only when it differs from what we last sent for that root.
    const transition = (hook_event_name: string, rawSessionID: string | undefined, withMessage = false) => {
      if (!rawSessionID) return;
      const root = rootOf(rawSessionID);
      if (!ownedRoots().has(root)) return;
      if (lastSent.get(root) === hook_event_name) return;
      lastSent.set(root, hook_event_name);
      report(hook_event_name, root, withMessage);
    };

    const off = data?.listen?.((event: any) => {
      const name = event?.name;
      if (typeof name !== "string") return;
      const details = event?.details ?? {};
      const sessionID: string | undefined = details?.data?.sessionID;

      if (name === "session.execution.started") {
        // A run is running. The generic working edge ingest understands.
        if (once(details?.id)) transition("UserPromptSubmit", sessionID);
      } else if (name === "session.execution.succeeded") {
        // The clean end of a successful run; carries the final message.
        if (once(details?.id)) transition("Stop", sessionID, true);
      } else if (name === "session.execution.failed" || name === "session.execution.error") {
        if (once(details?.id)) transition("StopFailure", sessionID);
      } else if (name === "session.execution.cancelled" || name === "session.execution.aborted") {
        // An interrupted run is over too; ingest reads Stop as "done".
        if (once(details?.id)) transition("Stop", sessionID, true);
      } else if (name === "session.created" || name === "session.deleted") {
        if (!once(details?.id)) return;
        // A child session starting or ending is not a new pane agent.
        if (details?.data?.parentID) return;
        if (!sessionID) return;
        const root = rootOf(sessionID);
        if (name === "session.created") {
          if (!ownedRoots().has(root)) return;
          lastSent.set(root, "SessionStart");
          report("SessionStart", root);
        } else {
          lastSent.delete(root);
          report("SessionEnd", root);
        }
      } else if (name === "permission.asked" || name === "permission.updated") {
        // A permission request waits on the human.
        if (once(details?.id)) transition("PermissionRequest", sessionID);
      } else if (name === "permission.replied" || name === "permission.deleted") {
        if (!once(details?.id)) return;
        if (!sessionID) return;
        const root = rootOf(sessionID);
        if (!ownedRoots().has(root)) return;
        // Back to working unless the session is verifiably idle; a status API
        // we cannot read must not park a running agent.
        let status = "running";
        try {
          status = String(data?.session?.status?.(root) ?? "running");
        } catch {
          status = "running";
        }
        if (status === "idle") {
          lastSent.set(root, "Stop");
          report("Stop", root, true);
        } else {
          lastSent.set(root, "PostToolUse");
          report("PostToolUse", root);
        }
      }
    });

    return typeof off === "function" ? off : () => {};
  },
});
