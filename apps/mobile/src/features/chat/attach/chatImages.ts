// How a message's chip gets at an image that is only on the host
// (docs/design/mobile-chat-plan.md §3 "Attachments"). The chat screen gives
// the conversation its own through the context; `attachRuntime` has the one
// that reads through the app's connections.

import { createContext } from "react";

export interface ChatImages {
  /** Reads the image at `hostPath` and answers a file on the phone that holds it. Rejects with why not. */
  load: (hostPath: string) => Promise<{ uri: string }>;
  /** The viewer closed: the file `load` answered is no longer needed. */
  release: (uri: string) => void;
}

/** Outside a chat screen nothing can be read from the host. */
export const ChatImagesContext = createContext<ChatImages>({
  load: () => Promise.reject(new Error("Not connected.")),
  release: () => undefined,
});
