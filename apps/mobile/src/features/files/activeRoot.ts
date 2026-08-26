// Active-root resolution (design doc §11.2, §7.5 `resolveActiveRoot`).
//
// `RESOLVE_ACTIVE_ROOT` answers with the directory the Explorer should show for
// a pane: the pane's working directory, or the Git worktree root containing it.
// The `rootToken` it returns is the capability every later file request carries;
// nothing else authorises a path.

import type { Response } from "../../protocol/gen/envelope_pb";
import { newOperationId, resolveActiveRoot } from "../../protocol/requests";
import type { RootedPath } from "../../protocol/requests";

export interface ResolvedRoot {
  paneId: string;
  root: string;
  rootToken: string;
  gitWorktree: boolean;
  rootGeneration: bigint;
}

export type ResolveRequestFn = (request: ReturnType<typeof resolveActiveRoot>) => Promise<Response>;

export class ActiveRootError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ActiveRootError";
  }
}

/**
 * Resolves the root for a pane, passing the token we already hold so the host
 * can answer `rootUnchanged` without a second authoritative discovery.
 */
export async function resolveRoot(
  request: ResolveRequestFn,
  paneId: string,
  expectedServerIdentity: string,
  known?: ResolvedRoot | undefined,
): Promise<ResolvedRoot> {
  const knownToken = known?.paneId === paneId ? known.rootToken : "";
  const response = await request(resolveActiveRoot(newOperationId(), paneId, expectedServerIdentity, knownToken));
  if (known && response.file?.rootUnchanged) return known;
  const activeRoot = response.file?.activeRoot;
  if (!activeRoot || !activeRoot.root || !activeRoot.rootToken) {
    throw new ActiveRootError("the host answered without an active root");
  }
  return {
    paneId,
    root: activeRoot.root,
    rootToken: activeRoot.rootToken,
    gitWorktree: activeRoot.gitWorktree,
    rootGeneration: activeRoot.rootGeneration,
  };
}

/** A `RootedPath` for a path inside `root`; `path` is always a string the host produced. */
export function rooted(root: ResolvedRoot, path: string): RootedPath {
  return { root: root.root, rootToken: root.rootToken, path };
}

/** §9.6 step 1: the app bar title is the last segment of the root. */
export function rootTitle(root: string): string {
  const trimmed = root.replace(/\/+$/u, "");
  const slash = trimmed.lastIndexOf("/");
  return slash === -1 || slash === trimmed.length - 1 ? trimmed || "/" : trimmed.slice(slash + 1);
}

/** §9.7: the viewer's subtitle is the path relative to the root. */
export function relativeToRoot(root: string, path: string): string {
  const base = root.replace(/\/+$/u, "");
  if (path === base) return "";
  return path.startsWith(`${base}/`) ? path.slice(base.length + 1) : path;
}
