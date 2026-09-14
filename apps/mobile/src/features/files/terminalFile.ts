import type { Response } from "../../protocol/gen/envelope_pb";
import { newOperationId, resolveTerminalFile as resolveTerminalFileRequest } from "../../protocol/requests";
import type { Pane, SessionState } from "../../store/sessionStore";
import type { ResolvedRoot } from "./activeRoot";

export interface TerminalFileCapture {
  pane: Pick<Pane, "id" | "sessionId" | "windowId" | "currentPath">;
  serverIdentity: string;
  topologyGeneration: bigint;
}

export interface TerminalFileResolution {
  path: string;
  name: string;
  root: ResolvedRoot;
  topologyGeneration: bigint;
}

export type TerminalFileRequestFn = (
  request: ReturnType<typeof resolveTerminalFileRequest>,
) => Promise<Response>;

export class TerminalFileResolutionError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "TerminalFileResolutionError";
  }
}

/** Resolves a terminal token through the host's existing route- and capability-guarded operation. */
export async function resolveTerminalFile(
  request: TerminalFileRequestFn,
  candidate: string,
  capture: TerminalFileCapture,
): Promise<TerminalFileResolution> {
  const response = await request(resolveTerminalFileRequest(
    newOperationId(),
    capture.pane.id,
    candidate,
    capture.serverIdentity,
    capture.topologyGeneration,
    {
      sessionId: capture.pane.sessionId,
      windowId: capture.pane.windowId,
      cwd: capture.pane.currentPath,
    },
  ));
  const activeRoot = response.file?.activeRoot;
  const metadata = response.file?.metadata;
  if (!activeRoot?.root || !activeRoot.rootToken || !metadata?.path) {
    throw new TerminalFileResolutionError("the host answered without a resolved terminal file");
  }
  if (activeRoot.serverIdentity !== capture.serverIdentity || activeRoot.paneId !== capture.pane.id) {
    throw new TerminalFileResolutionError("the host resolved the file for another terminal scope");
  }
  if (activeRoot.topologyGeneration < capture.topologyGeneration) {
    throw new TerminalFileResolutionError("the host resolved the file from stale topology");
  }
  return {
    path: metadata.path,
    name: metadata.name || lastSegment(metadata.path),
    topologyGeneration: activeRoot.topologyGeneration,
    root: {
      paneId: activeRoot.paneId,
      root: activeRoot.root,
      rootToken: activeRoot.rootToken,
      gitWorktree: activeRoot.gitWorktree,
      rootGeneration: activeRoot.rootGeneration,
    },
  };
}

/** The answer may navigate only while the same authoritative pane route is still on screen. */
export function terminalFileCaptureIsCurrent(
  capture: TerminalFileCapture,
  state: Pick<SessionState, "serverIdentity" | "topologyGeneration" | "panes">,
  resolvedTopologyGeneration: bigint,
): boolean {
  if (state.serverIdentity !== capture.serverIdentity
    || state.topologyGeneration < resolvedTopologyGeneration) return false;
  const pane = state.panes[capture.pane.id];
  return pane?.sessionId === capture.pane.sessionId
    && pane.windowId === capture.pane.windowId
    && pane.currentPath === capture.pane.currentPath;
}

function lastSegment(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash < 0 ? path : path.slice(slash + 1);
}
