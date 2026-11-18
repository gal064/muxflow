import { Pressable, StyleSheet, Text, View } from "react-native";

import type { ChatPendingRequest } from "../../../protocol/gen/chat_pb";
import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import { isQuestion, PendingCard, type PendingAnswer } from "./PendingCard";

export interface RequestDockProps {
  request: ChatPendingRequest;
  /** An answer is on its way; the buttons wait for it. */
  busy: boolean;
  onAnswer: (answer: PendingAnswer) => void;
  /** Folded, the card is one line that still says the agent waits. */
  folded: boolean;
  onFold: () => void;
  onUnfold: () => void;
}

/**
 * The request the agent waits on, fixed between the conversation and the
 * reply field (mobile-chat-mock.html, S3), so scrolling and the keyboard
 * cannot hide it. The card takes at most half the screen's height and scrolls
 * inside itself past that. Folded, it stays mounted and hidden: what was
 * already chosen in it is kept.
 */
export function RequestDock({ request, busy, onAnswer, folded, onFold, onUnfold }: RequestDockProps) {
  const says = isQuestion(request) ? "The agent asked a question" : "The agent is asking for permission";
  return (
    <>
      {folded ? (
        <View style={styles.barWrap}>
          <Pressable
            accessibilityHint="Shows the request"
            accessibilityLabel={`${says}. Answer`}
            accessibilityRole="button"
            onPress={onUnfold}
            style={({ pressed }) => [styles.bar, pressed && styles.pressed]}
          >
            <Text {...fixedChromeText} numberOfLines={1} style={styles.says}>{says}</Text>
            <Text {...fixedChromeText} style={styles.answer}>Answer</Text>
          </Pressable>
        </View>
      ) : null}
      <View style={[styles.card, folded && styles.hidden]}>
        <PendingCard busy={busy} onAnswer={onAnswer} onFold={onFold} request={request} />
      </View>
    </>
  );
}

const styles = StyleSheet.create({
  // Half of the chat's own height, which the open keyboard has already taken its share of.
  card: { maxHeight: "50%", paddingBottom: 8, paddingHorizontal: 10 },
  hidden: { display: "none" },
  barWrap: { paddingBottom: 8, paddingHorizontal: 10 },
  bar: { alignItems: "center", backgroundColor: colors.dangerWash, borderColor: colors.danger, borderRadius: 9, borderWidth: 1, flexDirection: "row", gap: 8, minHeight: 44, paddingHorizontal: 12 },
  says: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.rowSecondary, minWidth: 0 },
  answer: { color: colors.accent, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  pressed: { opacity: 0.75 },
});
