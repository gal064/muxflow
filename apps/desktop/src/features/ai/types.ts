/** The assistant settings, as persisted in the shell preferences. */
export interface AiChatSettings {
  baseUrl: string;
  model: string;
  apiKey: string;
}

/** A tool call the model completed, in the OpenAI echo shape. */
export interface AiToolCall {
  id: string;
  name: string;
  /** The raw argument JSON the model streamed; parsed once, at execution. */
  arguments: string;
}

/** One message of the call, in the OpenAI shape the endpoint expects. */
export interface AiChatMessage {
  role: "system" | "user" | "assistant" | "tool";
  content?: string | null;
  /** Set on a `tool` answer: the call it answers. */
  toolCallId?: string;
  /** Set on an assistant turn that asked for actions. */
  toolCalls?: readonly AiToolCall[];
}

/** One streamed event from the native proxy, mirroring `ai::AiEvent`. */
export type AiEvent =
  | { type: "delta"; text: string }
  | { type: "toolCall"; id: string; name: string; arguments: string }
  /** The tokens this round's answer cost, when the server reports it. */
  | { type: "usage"; completionTokens: number }
  | { type: "done" }
  | { type: "error"; message: string };

/** What the assistant did inside one answer, kept for the transcript and the
 * history: the model is told its own actions in words, not in raw tool wire. */
export interface AiAction {
  name: string;
  /** One human line: `send_to_pane "git status" ⏎`. */
  summary: string;
  outcome: "done" | "denied" | "failed";
}

/** One bubble: what was asked, and what came back. */
export interface AiTurn {
  role: "user" | "assistant";
  text: string;
  actions?: AiAction[];
  /** When the bubble happened, epoch ms: a question is stamped as it is
   * sent, an answer as it finishes. 0 means still filling in, and the panel
   * shows no stamp until the answer lands. */
  createdAt: number;
  /** Tokens the answer cost, summed over the round(s) that produced it.
   * Only when the server reported usage; absent never means zero. */
  tokens?: number;
}

/**
 * A tool call waiting for the user's decision. `decide` is wired by whoever
 * asked — resolving it resumes the assistant's loop; the store only routes
 * the answer from the panel's Allow/Deny buttons.
 */
export interface AiApproval {
  id: string;
  name: string;
  summary: string;
  decide(approved: boolean): void;
}

/**
 * The round budget spent without an answer, waiting on the user. `resume`
 * with true buys the run more rounds; anything else — Stop, Clear, a closed
 * window — ends it. Like an approval, it lives on the conversation so the
 * panel can raise it after a tab switch.
 */
export interface AiContinuation {
  roundsUsed: number;
  resume(keepGoing: boolean): void;
}
