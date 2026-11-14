import { HostError } from "../../protocol/HostConnection";

/** A refusal's own words, with the codes a person can act on said plainly. */
export function refusalText(error: unknown): string {
  if (error instanceof HostError) {
    switch (error.code) {
      case "chat_turn_busy":
        return "The agent is busy. Stop it first, or wait for the turn to end.";
      case "chat_request_not_pending":
        return "That request was already answered.";
      case "chat_side_fork_unavailable":
        return "Side questions aren't available for this agent yet.";
      case "history_unavailable":
        return "History isn't available on this host yet.";
      case "chat_message_too_large":
        return "That message is too long to send.";
      default:
        return error.message || "The host refused that.";
    }
  }
  return error instanceof Error ? error.message : String(error);
}
