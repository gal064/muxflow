import { Channel, invoke } from "@tauri-apps/api/core";
import type { AiChatMessage, AiChatSettings, AiEvent } from "./types";

/** Ids only have to separate this window's own in-flight calls. */
let nextRequestId = 1;

export interface AiCall {
  /** Settles when the answer ended, however it ended. */
  done: Promise<void>;
  /** Stops the generation behind this call; a partial answer stays shown. */
  cancel(): void;
}

/** The camelCase mirror of `ai::ChatMessage`; the native side puts it back
 * into the server's snake_case, but the echoed calls arrive in the OpenAI
 * shape, so that is what travels here. */
function wireMessage(message: AiChatMessage) {
  return {
    role: message.role,
    ...(message.content != null ? { content: message.content } : {}),
    ...(message.toolCallId ? { toolCallId: message.toolCallId } : {}),
    ...(message.toolCalls
      ? {
        toolCalls: message.toolCalls.map((call) => ({
          id: call.id,
          type: "function",
          function: { name: call.name, arguments: call.arguments },
        })),
      }
      : {}),
  };
}

/**
 * Ask the configured model, streaming the answer into `onEvent`.
 *
 * The request leaves from Rust, not from here: the content security policy
 * lets the webview speak only to this process. Exactly one terminal event —
 * `done` or `error` — reaches `onEvent`, however the call ends: a rejected
 * invoke with no terminal event is turned into one, so the panel never
 * watches a spinner that will never stop. `tools` is the OpenAI tool table
 * the assistant may use; a completed call arrives as one `toolCall` event
 * after the text that preceded it.
 */
export function aiChat(
  settings: AiChatSettings,
  messages: readonly AiChatMessage[],
  tools: readonly object[],
  onEvent: (event: AiEvent) => void,
): AiCall {
  const requestId = nextRequestId++;
  const channel = new Channel<AiEvent>();
  let finished = false;
  const settle = (event: AiEvent) => {
    if (event.type === "delta" || event.type === "toolCall" || event.type === "usage") {
      if (!finished) onEvent(event);
      return;
    }
    if (finished) return;
    finished = true;
    onEvent(event);
  };
  channel.onmessage = settle;
  const done = invoke("ai_chat", {
    request: {
      requestId,
      baseUrl: settings.baseUrl.trim(),
      model: settings.model.trim(),
      apiKey: settings.apiKey,
      messages: messages.map((message) => wireMessage(message)),
      ...(tools.length > 0 ? { tools: tools.map((tool) => ({ ...tool })) } : {}),
    },
    onEvent: channel,
  }).then(
    () => settle({ type: "done" }),
    (reason: unknown) => settle({ type: "error", message: String(reason).replace(/^Error: /, "") }),
  );
  return {
    done,
    cancel: () => {
      void invoke("ai_cancel", { requestId }).catch(() => undefined);
    },
  };
}

/**
 * Ask the configured server which models it serves, for Settings to offer
 * instead of a hand-typed id. Rejects with the server's own refusal, which is
 * the message that tells the user their endpoint or key is wrong.
 */
export function fetchAiModels(baseUrl: string, apiKey: string): Promise<string[]> {
  return invoke("ai_models", { request: { baseUrl: baseUrl.trim(), apiKey } });
}
