// The slash list's entries for a terminal's agent: the host reads them from
// the agent's own program (CHAT_ACTION_TERMINAL_COMMANDS), as a chat's agent
// lists them itself.

import { create } from "@bufbuild/protobuf";
import { useEffect, useState } from "react";

import { ChatCatalogSchema, type ChatCatalog } from "../../protocol/gen/chat_pb";
import { chatTerminalCommands } from "../../protocol/requests";
import { getConnection } from "../../session/connectionManager";
import { log } from "../../session/log";

/**
 * Asks once per agent, while connected. Nothing is listed until the answer,
 * nor when the host cannot say (a host from before the action, an agent whose
 * commands it cannot read).
 */
export function useTerminalCommands(paneId: string, serverIdentity: string, agentKey: string | undefined, connected: boolean): ChatCatalog | undefined {
  const [catalog, setCatalog] = useState<ChatCatalog | undefined>(undefined);
  useEffect(() => {
    setCatalog(undefined);
    const connection = getConnection();
    if (!agentKey || !connected || !connection) return undefined;
    let live = true;
    connection.request(chatTerminalCommands({ paneId, expectedServerIdentity: serverIdentity })).then(
      (response) => {
        if (live) setCatalog(create(ChatCatalogSchema, { commands: response.chat?.commands ?? [] }));
      },
      (error: unknown) => log(`[muxflow] terminal.commands pane=${paneId} failed=${error instanceof Error ? error.message : String(error)}`),
    );
    return () => {
      live = false;
    };
  }, [agentKey, connected, paneId, serverIdentity]);
  return catalog;
}
