import { FolderIcon } from "../../../ui/components/MediaIcons";
import { colors } from "../../../ui/tokens";
import { AgentHeader, HeaderButton } from "../../agents/ui/AgentHeader";
import { headerLine } from "../chatHeader";
import type { ChatPane } from "../chatModel";
import { InfoIcon } from "./HeaderIcons";

export interface ChatHeaderProps {
  title: string;
  pane: ChatPane | undefined;
  connected: boolean;
  /**
   * The chat's agent: its mark, and whether its last turn is an unseen
   * completion. The mark draws the chat's own state, as the line says it.
   */
  agent?: { adapterId: string; finished: boolean } | undefined;
  onDetails: () => void;
  /** Opens the pane's files; absent hides the button. */
  onFiles?: (() => void) | undefined;
  onBack: () => void;
}

/**
 * The chat's header (mobile-chat-mock.html, S8), the agent screens' shared
 * one: Back, the agent's mark, the title over a line with the state, model
 * and effort, then Files and Details. The title block opens Details too.
 * Voice mode is the reply field's mic (mobile-voice-mock.html).
 */
export function ChatHeader({ title, pane, connected, agent, onDetails, onFiles, onBack }: ChatHeaderProps) {
  const line = headerLine(pane, connected, agent?.finished === true);
  // No state known yet (opening, connecting, not connected): the mark draws none, as for an idle agent.
  const mark = agent ? { adapterId: agent.adapterId, state: line?.state?.mark ?? "idle" } : undefined;
  return (
    <AgentHeader line={line?.text} mark={mark} onBack={onBack} onTitle={{ label: "Chat details", onPress: onDetails }} title={title}>
      {onFiles ? (
        <HeaderButton label="Files" onPress={onFiles}>
          <FolderIcon color={colors.chromeDim} size={20} />
        </HeaderButton>
      ) : null}
      <HeaderButton label="Details" onPress={onDetails}>
        <InfoIcon color={colors.chromeDim} />
      </HeaderButton>
    </AgentHeader>
  );
}
