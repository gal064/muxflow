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
}

export function createTmuxConfirmation(
  commandId: CommandId,
  commandTitle: string,
  targetLabel: string,
  action: TmuxAction,
  precondition: AuthoritativePrecondition,
): PendingTmuxConfirmation {
  if (!isDestructiveTmuxAction(action)) throw new Error("confirmation requires a destructive tmux action");
  return {
    commandId,
    title: commandTitle.replace("…", "?"),
    detail: `This will ask tmux to permanently close ${targetLabel}. Running processes in it will be terminated.`,
    targetLabel,
    action: { ...action, confirmed: true },
    precondition: { ...precondition },
  };
}
