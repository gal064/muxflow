import { memo, useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { ChatToolKind, ChatToolStatus, type ChatTool } from "../../../protocol/gen/chat_pb";
import { colors, fonts, stateColors, typeScale } from "../../../ui/tokens";

const KIND_WORDS: Partial<Record<ChatToolKind, string>> = {
  [ChatToolKind.COMMAND]: "run",
  [ChatToolKind.READ]: "read",
  [ChatToolKind.EDIT]: "edit",
  [ChatToolKind.SEARCH]: "search",
  [ChatToolKind.WEB]: "web",
  [ChatToolKind.MCP]: "mcp",
  [ChatToolKind.SUBAGENT]: "agent",
};

/** The row's three parts: verb, what it acted on, and its outcome with the ink to draw it in. */
export function toolRowParts(tool: ChatTool): { verb: string; subject: string; result: string; resultColor: string } {
  const verb = tool.title || KIND_WORDS[tool.kind] || "tool";
  // A phone row names a file by its name; the whole path shows when the row opens.
  const fileRow = (tool.kind === ChatToolKind.READ || tool.kind === ChatToolKind.EDIT) && tool.path !== "";
  const subject = fileRow ? fileName(tool.path) : tool.detail || tool.path;
  if (tool.status === ChatToolStatus.RUNNING) return { verb, subject, result: "running", resultColor: stateColors.working };
  if (tool.status === ChatToolStatus.FAILED) return { verb, subject, result: tool.resultSummary || "failed", resultColor: colors.dangerInk };
  if (tool.kind === ChatToolKind.EDIT && (tool.added > 0 || tool.removed > 0)) {
    return { verb, subject, result: `+${tool.added} −${tool.removed}`, resultColor: colors.ok };
  }
  return { verb, subject, result: tool.resultSummary || "done", resultColor: colors.ok };
}

export function fileName(path: string): string {
  return path.replace(/\/+$/, "").split("/").pop() || path;
}

/** The text the row cut short, shown whole when it opens. */
function fullSubject(tool: ChatTool): string {
  return tool.path || tool.detail;
}

/** One tool call as a single row; tapping it shows the start of its output. */
export const ToolRow = memo(function ToolRow({ tool }: { tool: ChatTool }) {
  const [open, setOpen] = useState(false);
  const { verb, subject, result, resultColor } = toolRowParts(tool);
  const edit = tool.kind === ChatToolKind.EDIT && tool.status !== ChatToolStatus.RUNNING && (tool.added > 0 || tool.removed > 0);
  const full = fullSubject(tool);
  const showFull = full !== subject || subject.length > 32;
  const expandable = tool.outputPreview !== "" || showFull;
  return (
    <Pressable
      accessibilityHint={expandable ? (open ? "Hides the output" : "Shows the output") : undefined}
      accessibilityLabel={`${verb} ${subject}, ${result}`}
      accessibilityRole={expandable ? "button" : "text"}
      accessibilityState={expandable ? { expanded: open } : undefined}
      disabled={!expandable}
      hitSlop={{ top: 4, bottom: 4 }}
      onPress={() => setOpen((value) => !value)}
      style={({ pressed }) => [styles.row, pressed && styles.pressed]}
    >
      <View style={styles.line}>
        <Text numberOfLines={1} style={styles.verb}>{verb}</Text>
        <Text ellipsizeMode="middle" numberOfLines={1} style={styles.subject}>{subject}</Text>
        {edit ? (
          <Text numberOfLines={1} style={styles.result}>
            <Text style={{ color: colors.ok }}>+{tool.added}</Text> <Text style={{ color: colors.dangerInk }}>−{tool.removed}</Text>
          </Text>
        ) : (
          <Text numberOfLines={1} style={[styles.result, { color: resultColor }]}>{result}</Text>
        )}
      </View>
      {open ? (
        <View style={styles.detail}>
          {showFull ? <Text selectable style={styles.full}>{full}</Text> : null}
          {tool.outputPreview ? <Text selectable style={styles.output}>{tool.outputPreview.replace(/\n+$/, "")}</Text> : null}
          {tool.outputTruncated ? <Text style={styles.note}>Output shortened.</Text> : null}
        </View>
      ) : null}
    </Pressable>
  );
});

const styles = StyleSheet.create({
  row: { backgroundColor: colors.chromeBg, borderColor: colors.chromeHairline, borderRadius: 7, borderWidth: 1, minHeight: 36, justifyContent: "center", paddingHorizontal: 10, paddingVertical: 6 },
  pressed: { backgroundColor: colors.chromeHover },
  line: { alignItems: "center", flexDirection: "row", gap: 8 },
  verb: { color: colors.chromeFaint, flexShrink: 0, fontFamily: fonts.mono, fontSize: 11 },
  subject: { color: colors.chromeInk, flex: 1, fontFamily: fonts.mono, fontSize: 12, minWidth: 0 },
  result: { flexShrink: 0, fontSize: 11 },
  detail: { gap: 6, paddingBottom: 2, paddingTop: 8 },
  full: { color: colors.chromeInk, fontFamily: fonts.mono, fontSize: 12 },
  output: { backgroundColor: colors.chromeRaised, borderRadius: 5, color: colors.chromeInk, fontFamily: fonts.mono, fontSize: 11, lineHeight: 16, padding: 8 },
  note: { color: colors.chromeDim, fontSize: typeScale.meta, fontStyle: "italic" },
});
