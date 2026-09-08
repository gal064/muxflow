import { Fragment, memo, useMemo } from "react";
import { Linking, StyleSheet, Text, type TextStyle } from "react-native";

import { colors, typeScale } from "../../ui/tokens";
import { parseVoiceMarkdown, type MarkdownInline } from "./markdownModel";

function Inline({ content }: { content: MarkdownInline[] }) {
  return content.map((span, index) => {
    const key = `${index}:${span.kind}`;
    switch (span.kind) {
      case "strong":
        return <Text key={key} style={styles.strong}>{span.text}</Text>;
      case "emphasis":
        return <Text key={key} style={styles.emphasis}>{span.text}</Text>;
      case "code":
        return <Text key={key} style={styles.inlineCode}>{span.text}</Text>;
      case "link":
        return <Text accessibilityRole="link" key={key} onPress={() => void Linking.openURL(span.href).catch(() => {})} style={styles.link}>{span.text}</Text>;
      default:
        return <Fragment key={key}>{span.text}</Fragment>;
    }
  });
}

/** Native, dependency-free Markdown display for voice replies. */
export const VoiceMarkdown = memo(function VoiceMarkdown({ source, numberOfLines }: { source: string; numberOfLines?: number }) {
  const blocks = useMemo(() => parseVoiceMarkdown(source), [source]);
  return (
    <Text numberOfLines={numberOfLines} style={styles.body}>
      {blocks.map((block, blockIndex) => {
        const separator = blockIndex === 0 ? "" : "\n\n";
        if (block.kind === "code") return <Text key={blockIndex}>{separator}<Text style={styles.codeBlock}>{block.text}</Text></Text>;
        if (block.kind === "rule") return <Text key={blockIndex}>{separator}<Text style={styles.rule}>────────</Text></Text>;
        if (block.kind === "list") {
          return (
            <Text key={blockIndex}>
              {separator}
              {block.items.map((item, itemIndex) => (
                <Text key={itemIndex}>{itemIndex === 0 ? "" : "\n"}{block.ordered ? `${itemIndex + 1}. ` : "• "}<Inline content={item} /></Text>
              ))}
            </Text>
          );
        }
        const style: TextStyle = block.kind === "heading"
          ? block.level === 1 ? styles.heading1 : block.level === 2 ? styles.heading2 : styles.heading3
          : block.kind === "quote" ? styles.quote : styles.paragraph;
        return <Text key={blockIndex}>{separator}<Text style={style}>{block.kind === "quote" ? "› " : ""}<Inline content={block.content} /></Text></Text>;
      })}
    </Text>
  );
});

const styles = StyleSheet.create({
  body: { color: colors.chromeInk, fontSize: typeScale.body, lineHeight: 20 },
  paragraph: { color: colors.chromeInk },
  heading1: { color: colors.chromeInkStrong, fontSize: 20, fontWeight: "700", lineHeight: 26 },
  heading2: { color: colors.chromeInkStrong, fontSize: 18, fontWeight: "700", lineHeight: 24 },
  heading3: { color: colors.chromeInkStrong, fontSize: typeScale.body, fontWeight: "700" },
  strong: { color: colors.chromeInkStrong, fontWeight: "700" },
  emphasis: { fontStyle: "italic" },
  quote: { color: colors.chromeDim, fontStyle: "italic" },
  inlineCode: { backgroundColor: colors.chromeBg, color: colors.chromeInkStrong, fontFamily: "monospace", fontSize: 13 },
  codeBlock: { backgroundColor: colors.chromeBg, color: colors.chromeInkStrong, fontFamily: "monospace", fontSize: 13, lineHeight: 18 },
  link: { color: colors.accent, textDecorationLine: "underline" },
  rule: { color: colors.chromeBorder },
});
