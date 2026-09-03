import type { HostScopeToken } from "../features/shell/hostScope";
import type { AuthoritativePrecondition, TmuxAction } from "../features/tmux/actions";
import { isDestructiveTmuxAction } from "../features/tmux/actions";
import type { CommandId } from "./registry";

export interface PendingTmuxConfirmation {
  commandId: CommandId;
  title: string;
  detail: string;
  targetLabel: string;
  action: TmuxAction;
  precondition: AuthoritativePrecondition;
  /** The host the target lives on; the confirmed action runs against it, not against whichever host is on screen by then. */
  scope: HostScopeToken;
}

/**
 * `action` arrives already carrying `confirmed`, and is stored as given.
 *
 * This used to stamp the flag itself, from which it followed that a close
 * *without* a dialog had to stamp it somewhere else — two owners for the one
 * thing the host contract turns on. The caller stamps it once now, for the
 * dialog path and the immediate path alike, and this only carries what it was
 * handed. The assertion below is what keeps that honest.
 */
export function createTmuxConfirmation(
  commandId: CommandId,
  commandTitle: string,
  targetLabel: string,
  action: TmuxAction,
  precondition: AuthoritativePrecondition,
  scope: HostScopeToken,
): PendingTmuxConfirmation {
  if (!isDestructiveTmuxAction(action)) throw new Error("confirmation requires a destructive tmux action");
  if (!action.confirmed) throw new Error("confirmation requires an action already marked confirmed");
  return {
    commandId,
    title: commandTitle.replace("…", "?"),
    detail: `This will ask tmux to permanently close ${targetLabel}. Running processes in it will be terminated.`,
    targetLabel,
    action: { ...action },
    precondition: { ...precondition },
    scope: { ...scope },
  };
}
