import { Pressable, StyleSheet, Text, View } from "react-native";

import { BackIcon } from "../../../ui/components/MediaIcons";
import { colors, fixedChromeText, metrics, stateColors, typeScale } from "../../../ui/tokens";
import { headerLine, type HeaderTone } from "../chatHeader";
import type { ChatPane } from "../chatModel";
import { InfoIcon } from "./HeaderIcons";

export interface ChatHeaderProps {
  title: string;
  pane: ChatPane | undefined;
  connected: boolean;
  onDetails: () => void;
  onBack: () => void;
}

const DOT: Record<HeaderTone, string> = {
  working: stateColors.working,
  waiting: colors.danger,
  quiet: colors.chromeFaint,
};

/**
 * The chat's header (mobile-chat-mock.html, S8): Back, the title over a line
 * with the state, model and effort, then Details. The title block opens
 * Details too. Voice mode is the reply field's mic (mobile-voice-mock.html).
 */
export function ChatHeader({ title, pane, connected, onDetails, onBack }: ChatHeaderProps) {
  const line = headerLine(pane, connected);
  return (
    <View style={styles.header}>
      <Pressable accessibilityLabel="Back" accessibilityRole="button" onPress={onBack} style={styles.iconButton}>
        <BackIcon color={colors.chromeInkStrong} />
      </Pressable>
      <Pressable
        accessibilityLabel="Chat details"
        accessibilityRole="button"
        // The label replaces what the block reads as, so the title and the line are its value.
        accessibilityValue={{ text: line ? `${title}, ${line.text}` : title }}
        onPress={onDetails}
        style={({ pressed }) => [styles.titleBlock, pressed && styles.pressed]}
      >
        <Text {...fixedChromeText} accessibilityRole="header" numberOfLines={1} style={styles.title}>{title}</Text>
        {line ? (
          <View style={styles.line}>
            {line.state ? <View style={[styles.dot, { backgroundColor: DOT[line.state.tone] }]} /> : null}
            <Text {...fixedChromeText} numberOfLines={1} style={styles.lineText}>{line.text}</Text>
          </View>
        ) : null}
      </Pressable>
      <Pressable accessibilityLabel="Details" accessibilityRole="button" onPress={onDetails} style={styles.iconButton}>
        <View style={styles.round}>
          <InfoIcon color={colors.chromeDim} />
        </View>
      </Pressable>
    </View>
  );
}

const styles = StyleSheet.create({
  header: { alignItems: "center", backgroundColor: colors.chromeBg, borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, flexDirection: "row", height: metrics.terminalHeaderHeight + 4, paddingHorizontal: 4 },
  iconButton: { alignItems: "center", height: 48, justifyContent: "center", width: 44 },
  titleBlock: { alignSelf: "stretch", flex: 1, gap: 1, justifyContent: "center", minWidth: 0, paddingHorizontal: 4 },
  pressed: { opacity: 0.75 },
  title: { color: colors.chromeInkStrong, fontSize: typeScale.appBarTitle, fontWeight: "600" },
  line: { alignItems: "center", flexDirection: "row", gap: 6 },
  dot: { borderRadius: 4, height: 7, width: 7 },
  lineText: { color: colors.chromeDim, flexShrink: 1, fontSize: typeScale.meta },
  round: { alignItems: "center", borderRadius: 16, height: 32, justifyContent: "center", width: 32 },
});
