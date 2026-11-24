// File links in an agent's reply, as the desktop app has them: a Markdown
// link to a path opens it, and inline code that names a file on the host
// becomes a link to it once the host says the file is there. Both open in the
// file viewer, resolved against the agent's directory as a terminal's paths
// are.

import { HostError, RequestTimeoutError } from "../../protocol/HostConnection";
import { getConnection, toast } from "../../session/connectionManager";
import { sessionStore } from "../../store/sessionStore";
import { displayMessageFor } from "../files/errors";
import { filesStore } from "../files/filesStore";
import { FILE_VIEWER_COPY } from "../files/presentation";
import { resolveTerminalFile, terminalFileCaptureIsCurrent, type TerminalFileCapture, type TerminalFileResolution } from "../files/terminalFile";
import { appForeground } from "../terminal/appForeground";

/** What the host said about an inline code path. */
export type FileProbe = "pending" | "exists" | "missing";

/** How many paths one conversation asks the host about, at most. */
export const MAX_PROBES = 512;

/** How many times a path is asked about when the host could not say. */
export const PROBE_ATTEMPTS = 3;

/** How long a path that could not be asked waits before it is asked again. */
export const OFFLINE_RETRY_MS = 3_000;

/**
 * The host's answer about a path: `offline` when there was no one to ask,
 * `retry` when the host could not say (it took too long, or the pane moved
 * while it looked).
 */
export type ProbeAnswer = "exists" | "missing" | "offline" | "retry";

/**
 * Opens a path. `current` says whether the tap still stands: not once
 * another tap came or the conversation was left.
 */
export type FileOpener = (path: string, current: () => boolean) => void;

/**
 * The paths one conversation has asked the host about, asked one at a time
 * and only while the conversation is on screen. A path asked while the phone
 * was offline is asked again on `resume`.
 */
export class ChatFileLinks {
  private readonly probes = new Map<string, FileProbe>();
  private readonly attempts = new Map<string, number>();
  private readonly queue: string[] = [];
  private readonly listeners = new Map<string, Set<() => void>>();
  private probing = false;
  private shown = true;
  private tap = 0;
  private later: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private readonly probe: (path: string) => Promise<ProbeAnswer>,
    private readonly opener: FileOpener,
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

  /** Opens `path` in the file viewer. */
  open(path: string): void {
    const mine = ++this.tap;
    this.opener(path, () => this.shown && mine === this.tap);
  }

  /** The conversation is on screen: what is waiting is asked now. */
  show(): void {
    this.shown = true;
    void this.pump();
  }

  /** The conversation was left: nothing more is asked, and a tap still on its way opens nothing. */
  hide(): void {
    this.shown = false;
    this.tap += 1;
    clearTimeout(this.later);
    this.later = undefined;
  }

  /** The phone is connected again: what was waiting is asked now. */
  resume(): void {
    void this.pump();
  }

  private async pump(): Promise<void> {
    if (this.probing || !this.shown) return;
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
    if (answer === "offline") {
      // Asked again on reconnecting, or a little later: the pane may only
      // not be known yet.
      if (this.shown && this.later === undefined) {
        this.later = setTimeout(() => {
          this.later = undefined;
          void this.pump();
        }, OFFLINE_RETRY_MS);
      }
      return;
    }
    this.queue.shift();
    const attempts = (this.attempts.get(path) ?? 0) + 1;
    if (answer === "retry" && attempts < PROBE_ATTEMPTS) {
      this.attempts.set(path, attempts);
      this.queue.push(path);
    } else {
      this.attempts.delete(path);
      this.probes.set(path, answer === "exists" ? "exists" : "missing");
      for (const listener of this.listeners.get(path) ?? []) listener();
    }
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

/** Whether a refusal says the host could not tell, rather than that there is no such file. */
export function couldNotTell(error: unknown): boolean {
  if (error instanceof RequestTimeoutError) return true;
  return error instanceof HostError && /\bstale\b/iu.test(error.message);
}

async function probeFor(paneId: string, path: string): Promise<ProbeAnswer> {
  const captured = capture(paneId);
  const connection = getConnection();
  if (!captured || !connection) return "offline";
  try {
    await resolveTerminalFile(connection.request.bind(connection), resolvable(path), captured);
    return "exists";
  } catch (error) {
    if (!capture(paneId)) return "offline";
    return couldNotTell(error) || !(error instanceof HostError) ? "retry" : "missing";
  }
}

/**
 * The file links of the chat in `paneId`. A tap opens the file viewer through
 * `view`, with the root the host resolved, as a terminal's path does.
 */
function chatFileLinksFor(paneId: string, view: (resolved: TerminalFileResolution) => void): ChatFileLinks {
  const opener: FileOpener = (path, current) => {
    const captured = capture(paneId);
    const connection = getConnection();
    if (!captured || !connection) {
      toast("Reconnect before opening a file.");
      return;
    }
    const epoch = connection.connectionEpoch;
    const stands = () => current() && appForeground.inForeground();
    resolveTerminalFile(connection.request.bind(connection), resolvable(path), captured).then((resolved) => {
      if (!stands()) return;
      const live = getConnection();
      if (live !== connection || live.connectionEpoch !== epoch) return;
      if (!terminalFileCaptureIsCurrent(captured, sessionStore.getState(), resolved.topologyGeneration)) return;
      filesStore.getState().setTerminalFileRoot(resolved.path, resolved.root);
      view(resolved);
    }, (error: unknown) => {
      if (stands()) toast(FILE_VIEWER_COPY.error(displayMessageFor(error)));
    });
  };
  return new ChatFileLinks((path) => probeFor(paneId, path), opener);
}

/** How many conversations' answers are kept, so going back to one does not ask the host again. */
const KEPT = 16;
const kept = new Map<string, ChatFileLinks>();

/**
 * The file links of the chat in `paneId` with the agent in `folder` on the
 * host `serverIdentity`: the ones it had when last on screen, while they are
 * kept. The host's answers hold for that host and directory only.
 */
export function chatFileLinks(
  serverIdentity: string,
  paneId: string,
  folder: string,
  view: (resolved: TerminalFileResolution) => void,
): ChatFileLinks {
  const key = `${serverIdentity}\n${paneId}\n${folder}`;
  const links = kept.get(key) ?? chatFileLinksFor(paneId, view);
  kept.delete(key);
  kept.set(key, links);
  for (const old of kept.keys()) {
    if (kept.size <= KEPT) break;
    kept.delete(old);
  }
  return links;
}
