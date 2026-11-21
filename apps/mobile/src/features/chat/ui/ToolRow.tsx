import { memo, useCallback, useState } from "react";
import { Pressable, StyleSheet, Text, View, type NativeSyntheticEvent, type StyleProp, type TextLayoutEventData, type TextStyle } from "react-native";

import { ChatItemKind, ChatToolKind, ChatToolStatus, type ChatItem, type ChatTool } from "../../../protocol/gen/chat_pb";
import { colors, fonts, typeScale } from "../../../ui/tokens";
import { figures, startedAt, subagentLine, toolRowText, type ToolRowText } from "../steps";
import { useHeldOpen } from "./timelineOpen";
import { useRan } from "./useRan";

/**
 * One step as a plain row (mobile-chat-mock.html, S2 and S4): a thought on
 * one dim line, or a tool call as verb, subject and outcome, a subagent with
 * a second line under it. A row is a button only when it has more to show.
 */
export const StepRow = memo(function StepRow({ item }: { item: ChatItem }) {
  if (item.kind === ChatItemKind.REASONING) return <ThoughtRow id={item.id} text={item.text} />;
  return item.tool ? <ToolRow item={item} tool={item.tool} /> : null;
});

const TONES: Record<ToolRowText["tone"], string> = { ok: colors.okInk, failed: colors.dangerInk, working: colors.workingInk };
// The line is slimmer than a touch target; the slop makes a row that opens 44 dp tall.
const ROW_HEIGHT = 30;
const ROW_SLOP = { bottom: (44 - ROW_HEIGHT) / 2, top: (44 - ROW_HEIGHT) / 2 } as const;

/**
 * Says whether `children`, drawn on one line across this box, is cut short
 * there. The same text is laid out unseen over the same width on up to two
 * lines, and needs the second exactly when one line cannot hold it all. The
 * drawn line cannot say so itself: it reports the text it was given, whole.
 */
function CutProbe({ children, style, onCut }: { children: string; style: StyleProp<TextStyle>; onCut: (cut: boolean) => void }) {
  const onTextLayout = useCallback((event: NativeSyntheticEvent<TextLayoutEventData>) => onCut(event.nativeEvent.lines.length > 1), [onCut]);
  return (
    <Text accessibilityElementsHidden importantForAccessibility="no-hide-descendants" numberOfLines={2} onTextLayout={onTextLayout} style={[style, styles.probe]}>
      {children}
    </Text>
  );
}

function ToolRow({ item, tool }: { item: ChatItem; tool: ChatTool }) {
  const row = toolRowText(tool);
  const subagent = tool.kind === ChatToolKind.SUBAGENT;
  const running = tool.status === ChatToolStatus.RUNNING;
  // The row shortened its subject itself (a file to its name, a command to its first line), or the screen cut it.
  const shortened = row.full !== row.subject;
  const [cut, onCut] = useState(false);
  const showFull = shortened || cut;
  const expandable = tool.outputPreview !== "" || showFull;
  const [held, toggle] = useHeldOpen("rows", item.id);
  const open = held && expandable;
  // A subagent's time runs from when its row appeared. Nothing says when one ended, so a finished one shows no time.
  const ran = useRan(subagent && running ? startedAt(item.atUnixMillis) : undefined);
  const under = subagent ? (running ? subagentLine(tool, ran) : figures(tool)) : "";
  return (
    <View>
      <Pressable
        accessibilityHint={expandable ? (open ? "Hides the rest" : "Shows the rest") : undefined}
        accessibilityLabel={`${row.verb} ${row.subject}, ${row.outcome}`}
        accessibilityRole={expandable ? "button" : "text"}
        accessibilityState={expandable ? { expanded: open } : undefined}
        disabled={!expandable}
        hitSlop={ROW_SLOP}
        onPress={toggle}
        style={({ pressed }) => [styles.row, pressed && styles.pressed]}
      >
        <View style={styles.line}>
          <Text numberOfLines={1} style={styles.verb}>{row.verb}</Text>
          <View style={styles.subjectBox}>
            <Text numberOfLines={1} style={styles.subject}>{row.subject}</Text>
            {shortened ? null : <CutProbe onCut={onCut} style={styles.subject}>{row.subject}</CutProbe>}
          </View>
          {row.lines ? (
            <Text numberOfLines={1} style={styles.outcome}>
              <Text style={styles.added}>+{row.lines.added}</Text> <Text style={styles.removed}>−{row.lines.removed}</Text>
            </Text>
          ) : (
            <Text numberOfLines={1} style={[styles.outcome, { color: TONES[row.tone] }]}>{row.outcome}</Text>
          )}
        </View>
        {/* Codex says only that its subagent runs, which the outcome has said already. */}
        {under && under !== row.outcome ? <Text numberOfLines={2} style={styles.under}>{under}</Text> : null}
      </Pressable>
      {open ? (
        <View style={styles.detail}>
          {showFull ? <Text selectable style={styles.full}>{row.full}</Text> : null}
          {tool.outputPreview ? <Text selectable style={styles.output}>{tool.outputPreview.replace(/\n+$/, "")}</Text> : null}
          {tool.outputTruncated ? <Text style={styles.note}>Output shortened.</Text> : null}
        </View>
      ) : null}
    </View>
  );
}

/** The agent's thinking: one dim line, tap for all of it when there is more. */
function ThoughtRow({ id, text }: { id: string; text: string }) {
  const whole = text.trim();
  // Folded, its lines run on as one, so the line is full before it is cut.
  const line = whole.replace(/\s+/g, " ");
  const [cut, onCut] = useState(false);
  const expandable = cut || whole.includes("\n");
  const [held, toggle] = useHeldOpen("rows", id);
  const open = held && expandable;
  return (
    <Pressable
      accessibilityHint={expandable ? (open ? "Collapses it" : "Shows all of it") : undefined}
      accessibilityLabel={`Thinking: ${whole}`}
      accessibilityRole={expandable ? "button" : "text"}
      accessibilityState={expandable ? { expanded: open } : undefined}
      disabled={!expandable}
      hitSlop={ROW_SLOP}
      onPress={toggle}
      style={({ pressed }) => [styles.row, styles.thoughtRow, pressed && styles.pressed]}
    >
      <Text numberOfLines={open ? undefined : 1} style={styles.thought}>{open ? whole : line}</Text>
      {open ? null : <CutProbe onCut={onCut} style={styles.thought}>{line}</CutProbe>}
    </Pressable>
  );
}

const styles = StyleSheet.create({
  row: { justifyContent: "center", minHeight: ROW_HEIGHT },
  pressed: { opacity: 0.6 },
  line: { alignItems: "center", flexDirection: "row", gap: 8 },
  verb: { color: colors.chromeFaint, flexShrink: 0, fontFamily: fonts.mono, fontSize: 12.5, lineHeight: 18 },
  subjectBox: { flex: 1, minWidth: 0 },
  subject: { color: colors.chromeInk, fontFamily: fonts.mono, fontSize: 12.5, lineHeight: 18 },
  // Over the subject, where it takes no room and is not seen.
  probe: { left: 0, opacity: 0, position: "absolute", right: 0, top: 0 },
  // A long result leaves the subject at least half the line.
  outcome: { flexShrink: 0, fontSize: typeScale.meta, lineHeight: 18, maxWidth: "50%" },
  added: { color: colors.okInk },
  removed: { color: colors.dangerInk },
  under: { color: colors.chromeFaint, fontSize: typeScale.meta, lineHeight: 16, marginTop: -1, paddingBottom: 5 },
  detail: { gap: 6, paddingBottom: 8, paddingTop: 2 },
  full: { color: colors.chromeInk, fontFamily: fonts.mono, fontSize: 12.5, lineHeight: 18 },
  output: { backgroundColor: colors.chromeRaised, borderRadius: 5, color: colors.chromeInk, fontFamily: fonts.mono, fontSize: 11, lineHeight: 16, padding: 8 },
  note: { color: colors.chromeDim, fontSize: typeScale.meta, fontStyle: "italic" },
  thoughtRow: { paddingVertical: 6 },
  thought: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, fontStyle: "italic", lineHeight: 18 },
});
