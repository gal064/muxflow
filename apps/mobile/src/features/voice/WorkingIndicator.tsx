import { StyleSheet, Text, View } from "react-native";

import { Spinner } from "../agents/ui/Spinner";
import { useSession } from "../../ui/hooks";
import { colors, metrics, radii, typeScale } from "../../ui/tokens";
import { useAnimationsAllowed } from "../../ui/useAnimationsAllowed";
import { useVoice } from "./voiceHooks";
import { awaitingReply } from "./voiceStore";

/**
 * The agent's working state, in the conversation itself (design.md §9.11): a
 * typing-indicator bubble on the agent side after an utterance went out,
 * while the agent's lifecycle is `working`, gone once the reply lands or the
 * agent goes idle. Subscribes to two booleans of its own, so a lifecycle flip
 * re-renders this bubble and nothing above it; the spinner is the agents
 * tab's stepped one, sharing its clock rather than owning a timer.
 */
export function WorkingIndicator({ agentId }: { agentId: string }) {
  const working = useSession((s) => s.agents[agentId]?.lifecycle === "working");
  const awaiting = useVoice((s) => awaitingReply(s.sessions[agentId]));
  const animate = useAnimationsAllowed();
  if (!working || !awaiting) return null;
  return (
    <View accessibilityLabel="Agent is working" accessibilityLiveRegion="polite" accessibilityRole="text" style={styles.row}>
      <View style={styles.bubble}>
        <Spinner animate={animate} ink={colors.chromeDim} ring={colors.chromeRaised} size={14} />
        <Text style={styles.label}>Working…</Text>
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  row: { flexDirection: "row", justifyContent: "flex-start" },
  /** The agent bubble's surface and hairline (VoiceScreen `bubbleAgent`), sized to its content. */
  bubble: {
    alignItems: "center",
    backgroundColor: colors.chromeRaised,
    borderColor: colors.chromeBorder,
    borderRadius: radii.card,
    borderWidth: metrics.hairlineWidth,
    flexDirection: "row",
    gap: 8,
    paddingHorizontal: 14,
    paddingVertical: 10,
  },
  label: { color: colors.chromeDim, fontSize: typeScale.rowSecondary },
});
