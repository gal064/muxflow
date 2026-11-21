import type { ReactNode } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import type { AgentDisplayState } from "../../../store/selectors";
import { BackIcon } from "../../../ui/components/MediaIcons";
import { colors, fixedChromeText, metrics, typeScale } from "../../../ui/tokens";
import { useAnimationsAllowed } from "../../../ui/useAnimationsAllowed";
import { AgentMark } from "./AgentMark";

export interface AgentHeaderProps {
  title: string;
  /** The agent's mark with its state, as the agent list draws it. Absent for a pane that runs no agent. */
  mark?: { adapterId: string; state: AgentDisplayState } | undefined;
  /** The line under the title: the agent's state, then what else the screen knows. */
  line?: string | undefined;
  onBack: () => void;
  /** The title block opens this screen's details; absent, it is only a heading. */
  onTitle?: { label: string; onPress: () => void } | undefined;
  /** This screen's buttons, `HeaderButton`s, at the end. */
  children?: ReactNode;
}

/**
 * An agent screen's header, the chat's and the terminal's: Back, the agent's
 * mark, the title over a line with its state, then the screen's buttons.
 */
export function AgentHeader({ title, mark, line, onBack, onTitle, children }: AgentHeaderProps) {
  const animate = useAnimationsAllowed();
  const text = (
    <>
      <Text {...fixedChromeText} accessibilityRole="header" numberOfLines={1} style={styles.title}>{title}</Text>
      {line ? <Text {...fixedChromeText} numberOfLines={1} style={styles.line}>{line}</Text> : null}
    </>
  );
  return (
    <View style={styles.header}>
      <Pressable accessibilityLabel="Back" accessibilityRole="button" onPress={onBack} style={styles.iconButton}>
        <BackIcon color={colors.chromeInkStrong} />
      </Pressable>
      {mark ? <AgentMark adapterId={mark.adapterId} animate={animate} state={mark.state} /> : null}
      {onTitle ? (
        <Pressable
          accessibilityLabel={onTitle.label}
          accessibilityRole="button"
          // The label replaces what the block reads as, so the title and the line are its value.
          accessibilityValue={{ text: line ? `${title}, ${line}` : title }}
          onPress={onTitle.onPress}
          style={({ pressed }) => [styles.titleBlock, pressed && styles.pressed]}
        >
          {text}
        </Pressable>
      ) : (
        <View accessibilityLabel={line ? `${title}, ${line}` : title} accessible style={styles.titleBlock}>{text}</View>
      )}
      {children}
    </View>
  );
}

/** A round icon button at the header's end. */
export function HeaderButton({ label, onPress, children }: { label: string; onPress: () => void; children: ReactNode }) {
  return (
    <Pressable accessibilityLabel={label} accessibilityRole="button" onPress={onPress} style={styles.iconButton}>
      <View style={styles.round}>{children}</View>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  header: { alignItems: "center", backgroundColor: colors.chromeBg, borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, flexDirection: "row", height: metrics.terminalHeaderHeight + 4, paddingHorizontal: 4 },
  iconButton: { alignItems: "center", height: 48, justifyContent: "center", width: 44 },
  titleBlock: { alignSelf: "stretch", flex: 1, gap: 1, justifyContent: "center", minWidth: 0, paddingHorizontal: 8 },
  pressed: { opacity: 0.75 },
  title: { color: colors.chromeInkStrong, fontSize: typeScale.appBarTitle, fontWeight: "600" },
  line: { color: colors.chromeDim, fontSize: typeScale.meta },
  round: { alignItems: "center", borderRadius: 16, height: 32, justifyContent: "center", width: 32 },
});
