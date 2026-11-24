// File links in an agent's reply, as the desktop app has them: a Markdown
// link to a path opens it, and inline code that names a file on the host
// becomes a link to it once the host says the file is there. Both open in the
// file viewer, resolved against the agent's directory as a terminal's paths
// are.

import { getConnection, toast } from "../../session/connectionManager";
import { sessionStore } from "../../store/sessionStore";
import { displayMessageFor } from "../files/errors";
import { filesStore } from "../files/filesStore";
import { FILE_VIEWER_COPY } from "../files/presentation";
import { resolveTerminalFile, terminalFileCaptureIsCurrent, type TerminalFileCapture, type TerminalFileResolution } from "../files/terminalFile";

/** What the host said about an inline code path. */
export type FileProbe = "pending" | "exists" | "missing";

/** How many paths one conversation asks the host about, at most. */
export const MAX_PROBES = 512;

/** The host's answer about a path; `offline` when there was no one to ask. */
export type ProbeAnswer = "exists" | "missing" | "offline";

/**
 * The paths one conversation has asked the host about, asked one at a time.
 * A path asked while the phone was offline is asked again on `resume`.
 */
export class ChatFileLinks {
  private readonly probes = new Map<string, FileProbe>();
  private readonly queue: string[] = [];
  private readonly listeners = new Map<string, Set<() => void>>();
  private probing = false;

  constructor(
    private readonly probe: (path: string) => Promise<ProbeAnswer>,
    /** Opens `path` in the file viewer. */
    readonly open: (path: string) => void,
  ) {}

  get(path: string): FileProbe | undefined {
    return this.probes.get(path);
  }

  subscribe(path: string, listener: () => void): () => void {
    let listeners = this.listeners.get(path);
    if (!listeners) this.listeners.set(path, (listeners = new Set()));
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
      if (listeners.size === 0) this.listeners.delete(path);
    };
  }

  /** Asks the host about `path`, once. */
  ask(path: string): void {
    if (this.probes.has(path) || this.probes.size >= MAX_PROBES) return;
    this.probes.set(path, "pending");
    this.queue.push(path);
    void this.pump();
  }

  /** The phone is connected again: what was waiting is asked now. */
  resume(): void {
    void this.pump();
  }

  private async pump(): Promise<void> {
    if (this.probing) return;
    const path = this.queue[0];
    if (path === undefined) return;
    this.probing = true;
    let answer: ProbeAnswer;
    try {
      answer = await this.probe(path);
    } catch {
      answer = "missing";
    }
    this.probing = false;
    if (answer === "offline") return;
    this.queue.shift();
    this.probes.set(path, answer);
    for (const listener of this.listeners.get(path) ?? []) listener();
    void this.pump();
  }
}

/** `path` as the host's resolution takes it: absolute, from home, or relative with a separator. */
function resolvable(path: string): string {
  return path.includes("/") ? path : `./${path}`;
}

/** The pane as it is now, when the phone can ask the host about it. */
function capture(paneId: string): TerminalFileCapture | undefined {
  const connection = getConnection();
  const state = sessionStore.getState();
  const pane = state.panes[paneId];
  if (!connection || state.connection.state !== "connected" || !pane || connection.serverIdentity !== state.serverIdentity) return undefined;
  return {
    pane: { id: pane.id, sessionId: pane.sessionId, windowId: pane.windowId, currentPath: pane.currentPath },
    serverIdentity: state.serverIdentity,
    topologyGeneration: state.topologyGeneration,
  };
}

async function resolve(paneId: string, path: string): Promise<{ resolved: TerminalFileResolution; captured: TerminalFileCapture } | undefined> {
  const captured = capture(paneId);
  const connection = getConnection();
  if (!captured || !connection) return undefined;
  const resolved = await resolveTerminalFile(connection.request.bind(connection), resolvable(path), captured);
  return { resolved, captured };
}

/**
 * The file links of the chat in `paneId`. A tap opens the file viewer through
 * `show`, with the root the host resolved, as a terminal's path does.
 */
export function chatFileLinks(paneId: string, show: (resolved: TerminalFileResolution) => void): ChatFileLinks {
  let tap = 0;
  const probe = async (path: string): Promise<ProbeAnswer> => {
    try {
      return (await resolve(paneId, path)) ? "exists" : "offline";
    } catch {
      return capture(paneId) ? "missing" : "offline";
    }
  };
  const open = (path: string) => {
    const mine = ++tap;
    resolve(paneId, path).then((answer) => {
      if (mine !== tap) return;
      if (!answer) {
        toast("Reconnect before opening a file.");
        return;
      }
      const { resolved, captured } = answer;
      if (!terminalFileCaptureIsCurrent(captured, sessionStore.getState(), resolved.topologyGeneration)) return;
      filesStore.getState().setTerminalFileRoot(resolved.path, resolved.root);
      show(resolved);
    }, (error: unknown) => {
      if (mine === tap) toast(FILE_VIEWER_COPY.error(displayMessageFor(error)));
    });
  };
  return new ChatFileLinks(probe, open);
}
