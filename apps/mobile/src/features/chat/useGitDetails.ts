// Reads what Details says about the chat's folder from the host
// (docs/design/mobile-chat-plan.md §3 "Git rows"): the folder's root as the
// file screens resolve it, then the branch list and the two summaries, on the
// control connection. The phone has no Git watch, so it reads when Details
// opens and again when a turn ends under it, and never while it is closed.

import { useCallback, useEffect, useRef, useState } from "react";

import type { ChatTurnState } from "../../protocol/gen/chat_pb";
import { GitSummaryBase, type GitBranchSummary, type Request, type Response } from "../../protocol/gen/envelope_pb";
import { HostError } from "../../protocol/HostConnection";
import { gitBranches, gitBranchSummary, newOperationId } from "../../protocol/requests";
import { getConnection } from "../../session/connectionManager";
import { resolveRoot, type ResolvedRoot } from "../files/activeRoot";
import { filesStore } from "../files/filesStore";
import { inRepository, NO_GIT_DETAILS, NOT_A_REPOSITORY, turnEnded, type GitDetails, type GitRead, type GitSummary } from "./gitDetails";

/** What a read needs from a connection. `HostConnection` is one. */
export interface GitConnection {
  readonly serverIdentity: string;
  request(request: Request): Promise<Response>;
}

/** How one answer changes what Details shows. */
export type GitChange = (details: GitDetails) => GitDetails;

const FAILED = { status: "failed" } as const;

function summaryOf(summary: GitBranchSummary): GitSummary {
  return { baseRef: summary.baseRef, files: summary.totalFileCount, added: summary.added, removed: summary.removed };
}

/**
 * One read: the pane's root, then the branch and both summaries at once.
 * `apply` is called as each answer arrives, and answers false once the read
 * is no longer wanted, which ends it before it asks anything more. Answers
 * the root the host gave, or undefined when it gave none.
 */
export async function readGitDetails(connection: GitConnection, paneId: string, known: ResolvedRoot | undefined, apply: (change: GitChange) => boolean): Promise<ResolvedRoot | undefined> {
  const request = connection.request.bind(connection);
  const identity = connection.serverIdentity;
  let root: ResolvedRoot;
  try {
    root = await resolveRoot(request, paneId, identity, known);
  } catch {
    apply(() => NO_GIT_DETAILS);
    return undefined;
  }
  if (!root.gitWorktree) {
    apply(() => NOT_A_REPOSITORY);
    return root;
  }
  if (!apply((details) => inRepository(details, root.root))) return root;

  const part = async <K extends "branch" | "changes" | "uncommitted">(key: K, ask: Request, pick: (response: Response) => GitDetails[K]): Promise<void> => {
    let read: GitDetails[K];
    try {
      read = pick(await request(ask));
    } catch (error) {
      if (error instanceof HostError && error.code === "not_git_repository") {
        apply(() => NOT_A_REPOSITORY);
        return;
      }
      read = FAILED as GitDetails[K];
    }
    // A sibling answer may have said this is no repository after all.
    apply((details) => (details.repository === "git" ? { ...details, [key]: read } : details));
  };
  const summary = (response: Response): GitRead<GitSummary> => {
    const value = response.git?.branchSummary;
    return value ? { status: "ready", value: summaryOf(value) } : FAILED;
  };
  await Promise.all([
    part("branch", gitBranches(newOperationId(), root.root, root.rootToken, identity), (response) => {
      const list = response.git?.branches;
      return list ? { status: "ready", value: list.current } : FAILED;
    }),
    part("changes", gitBranchSummary(newOperationId(), root.root, root.rootToken, identity, GitSummaryBase.UNSPECIFIED), summary),
    part("uncommitted", gitBranchSummary(newOperationId(), root.root, root.rootToken, identity, GitSummaryBase.HEAD), summary),
  ]);
  return root;
}

export interface GitDetailsTarget {
  paneId: string;
  /** The tmux server the pane is on. */
  serverIdentity: string;
  /** The pane's folder. */
  folder: string;
  turn: ChatTurnState;
  /** Details is open. */
  open: boolean;
  connected: boolean;
}

/**
 * The Git details of one chat's folder, for as long as Details shows them.
 * An answer to a read that is no longer the newest (the chat or its folder
 * changed, or Details was closed) is dropped. What was read stays between
 * openings, so opening again shows it until the new answers arrive.
 */
export function useGitDetails({ paneId, serverIdentity, folder, turn, open, connected }: GitDetailsTarget): GitDetails {
  const key = `${serverIdentity}\n${paneId}\n${folder}`;
  const [held, setHeld] = useState({ key, details: NO_GIT_DETAILS });
  /** Counts reads; only the newest one's answers are taken. */
  const newest = useRef(0);

  const read = useCallback(() => {
    const token = ++newest.current;
    const connection = getConnection();
    if (!connection) return;
    const known = filesStore.getState().roots[paneId];
    const apply = (change: GitChange) => {
      if (token !== newest.current) return false;
      setHeld((current) => ({ key, details: change(current.key === key ? current.details : NO_GIT_DETAILS) }));
      return true;
    };
    void readGitDetails(connection, paneId, known, apply).then((root) => {
      // The file screens start from the same root.
      if (root && root !== known && token === newest.current) filesStore.getState().setRoot(root);
    });
  }, [key, paneId]);

  // Details opened, or the chat, its folder or the connection changed under it.
  useEffect(() => {
    if (!open || !connected) return undefined;
    read();
    return () => {
      newest.current += 1;
    };
  }, [connected, open, read]);

  // A turn ended under it: what the agent changed is read.
  const previousTurn = useRef(turn);
  useEffect(() => {
    const before = previousTurn.current;
    previousTurn.current = turn;
    if (open && connected && turnEnded(before, turn)) read();
  }, [connected, open, read, turn]);

  return held.key === key ? held.details : NO_GIT_DETAILS;
}
