import * as Clipboard from "expo-clipboard";
import { useEffect, useRef, useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import { replyTime } from "../messageTime";
import { CheckIcon, CopyIcon, ForkIcon } from "./MessageIcons";

/** How long Copy shows its tick. */
const COPIED_MS = 1500;

export interface ReplyActionsProps {
  /** The reply's last text, as the agent wrote it: what Copy copies. */
  text: string;
  /** When it was written (`ChatItem.atUnixMillis`); 0 draws no time. */
  atUnixMillis: bigint;
  /** Fork from here was pressed. Absent when the agent cannot fork, which leaves the button out. */
  onFork?: (() => void) | undefined;
  /** This reply's fork is on its way. */
  forking?: boolean;
  /** Another fork is on its way: this one cannot be asked for yet. */
  forkDisabled?: boolean;
}

/**
 * The quiet row under the newest finished reply (mobile-chat-mock.html, S5):
 * Copy, Fork from here, and when the reply was written. Earlier messages have
 * the same in the sheet a hold opens.
 */
export function ReplyActions({ text, atUnixMillis, onFork, forking = false, forkDisabled = false }: ReplyActionsProps) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const copy = () => {
    void Clipboard.setStringAsync(text).then(() => {
      setCopied(true);
      clearTimeout(timer.current);
      timer.current = setTimeout(() => setCopied(false), COPIED_MS);
    }).catch(() => {});
  };
  const time = replyTime(atUnixMillis, Date.now());
  const off = forking || forkDisabled;
  return (
    <View style={styles.row}>
      <Pressable
        accessibilityLabel={copied ? "Copied" : "Copy message"}
        accessibilityRole="button"
        hitSlop={SLOP}
        onPress={copy}
        style={({ pressed }) => [styles.button, pressed && styles.pressed]}
      >
        {copied ? <CheckIcon color={colors.okInk} size={ICON} /> : <CopyIcon color={colors.chromeFaint} size={ICON} />}
      </Pressable>
      {onFork ? (
        <Pressable
          accessibilityLabel="Fork from here"
          accessibilityRole="button"
          accessibilityState={{ busy: forking, disabled: off }}
          disabled={off}
          hitSlop={SLOP}
          onPress={onFork}
          style={({ pressed }) => [styles.button, pressed && styles.pressed]}
        >
          {/* On its way: in the accent ink until the host answers. */}
          <ForkIcon color={forking ? colors.accent : colors.chromeFaint} size={ICON} />
        </Pressable>
      ) : null}
      {time ? <Text {...fixedChromeText} numberOfLines={1} style={styles.time}>{time}</Text> : null}
    </View>
  );
}

const ICON = 16;
const WIDTH = 32;
const HEIGHT = 28;
// Each button is smaller than a touch target; the slop makes it 44 dp both ways, and the gap keeps two from overlapping.
const SLOP = { bottom: (44 - HEIGHT) / 2, left: (44 - WIDTH) / 2, right: (44 - WIDTH) / 2, top: (44 - HEIGHT) / 2 } as const;

const styles = StyleSheet.create({
  // The first icon lines up with the reply's text, not with its button's edge.
  row: { alignItems: "center", flexDirection: "row", gap: 44 - WIDTH, marginLeft: -(WIDTH - ICON) / 2 },
  button: { alignItems: "center", height: HEIGHT, justifyContent: "center", width: WIDTH },
  pressed: { opacity: 0.6 },
  time: { color: colors.chromeFaint, flexShrink: 1, fontSize: typeScale.meta, fontVariant: ["tabular-nums"] },
});
