import * as Clipboard from "expo-clipboard";
import { createContext, Fragment, memo, useContext, useEffect, useMemo, useRef, useState } from "react";
import { Linking, Pressable, ScrollView, StyleSheet, Text, View, useWindowDimensions, type TextStyle } from "react-native";

import { externalLinkTarget } from "../../../ui/externalLinks";
import { colors, fixedChromeText, fonts, metrics, radii, typeScale } from "../../../ui/tokens";
import {
  columnWidth,
  parseChatMarkdown,
  streamingCodeText,
  TABLE_CELL_PADDING,
  type ChatBlock,
  type ChatCodeBlock,
  type ChatInline,
  type ChatTableBlock,
} from "./chatMarkdownModel";

export type ChatMarkdownProps = {
  source: string;
  /** The reply is still being written: `source` will grow. */
  streaming?: boolean;
  /**
   * Whether a hold on the text selects it, which it does unless told
   * otherwise. Off where a hold on the reply means something else: the two
   * would both answer the same touch.
   */
  selectable?: boolean;
};

/** Whether the text of the reply being drawn can be selected in place. */
const Selectable = createContext(true);

/** How long the copy button reads "Copied". */
const COPIED_MS = 1500;

function openLink(href: string): void {
  const target = externalLinkTarget(href);
  if (target) void Linking.openURL(target).catch(() => {});
}

function runStyle(run: ChatInline, dim: boolean): TextStyle[] {
  const style: TextStyle[] = [];
  if (run.strong) style.push(dim ? styles.strongDim : styles.strong);
  if (run.emphasis) style.push(styles.emphasis);
  if (run.code) style.push(styles.inlineCode);
  if (run.href) style.push(styles.link);
  if (run.strike) style.push(run.href ? styles.strikeLink : styles.strike);
  return style;
}

function Runs({ content, dim }: { content: ChatInline[]; dim: boolean }) {
  return content.map((run, index) => {
    const { href } = run;
    const style = runStyle(run, dim);
    if (href) return <Text accessibilityRole="link" key={index} onPress={() => openLink(href)} style={style}>{run.text}</Text>;
    return style.length > 0 ? <Text key={index} style={style}>{run.text}</Text> : <Fragment key={index}>{run.text}</Fragment>;
  });
}

function CopyButton({ text }: { text: string }) {
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
  return (
    <Pressable
      accessibilityLabel={copied ? "Copied" : "Copy code"}
      accessibilityRole="button"
      hitSlop={COPY_HIT_SLOP}
      onPress={copy}
      style={({ pressed }) => [styles.copy, pressed && styles.pressed]}
    >
      <Text {...fixedChromeText} style={copied ? styles.copiedLabel : styles.copyLabel}>{copied ? "Copied" : "Copy"}</Text>
    </Pressable>
  );
}

/** Code in a box: its language and a copy button on top, lines that scroll sideways instead of wrapping. */
function CodeBlock({ block, tail }: { block: ChatCodeBlock; tail: boolean }) {
  const text = tail ? streamingCodeText(block) : block.text;
  const selectable = useContext(Selectable);
  return (
    <View style={styles.code}>
      <View style={styles.codeTop}>
        <Text {...fixedChromeText} numberOfLines={1} style={styles.codeLanguage}>{block.language}</Text>
        <CopyButton text={text} />
      </View>
      <ScrollView contentContainerStyle={styles.codeScroll} horizontal>
        <Text selectable={selectable} style={styles.codeText}>{text}</Text>
      </ScrollView>
    </View>
  );
}

/**
 * Every row gives a column the same width, so the columns line up. A table
 * narrower than the reply stretches to fill it; a wider one scrolls sideways
 * as a whole.
 */
function Table({ block, dim }: { block: ChatTableBlock; dim: boolean }) {
  const { fontScale } = useWindowDimensions();
  const selectable = useContext(Selectable);
  const widths = block.chars.map((chars) => columnWidth(chars, fontScale));
  const row = (cells: ChatInline[][], key: number, header: boolean, last: boolean) => (
    <View key={key} style={[styles.tableRow, header && styles.tableHeader, last && styles.tableLastRow]}>
      {cells.map((cell, column) => {
        const width = widths[column] ?? 0;
        return (
          <Text
            key={column}
            selectable={selectable}
            style={[styles.tableCell, header ? styles.tableHeaderCell : dim && styles.dim, { flexGrow: width, textAlign: block.align[column] ?? "left", width }]}
          >
            <Runs content={cell} dim={dim && !header} />
          </Text>
        );
      })}
    </View>
  );
  return (
    <ScrollView contentContainerStyle={styles.tableScroll} horizontal style={styles.table}>
      <View style={styles.tableRows}>
        {row(block.header, -1, true, block.rows.length === 0)}
        {block.rows.map((cells, index) => row(cells, index, false, index === block.rows.length - 1))}
      </View>
    </ScrollView>
  );
}

function TaskBox({ checked }: { checked: boolean }) {
  return (
    <View accessibilityLabel={checked ? "Done" : "Not done"} accessible style={[styles.taskBox, checked && styles.taskBoxChecked]}>
      {checked ? <Text allowFontScaling={false} style={styles.taskCheck}>✓</Text> : null}
    </View>
  );
}

/**
 * A marker column and the item's blocks beside it. A list inside an item
 * starts after that item's marker, which is what indents each level.
 */
function List({ block, dim, tail }: { block: Extract<ChatBlock, { kind: "list" }>; dim: boolean; tail: boolean }) {
  const lastIndex = block.items.length - 1;
  // Every number of the list in one column, sized for the longest.
  const markerWidth = block.ordered ? String(block.start + lastIndex).length * 8 + 12 : 16;
  return (
    <View style={styles.list}>
      {block.items.map((item, index) => (
        <View key={index} style={styles.listItem}>
          {item.checked === undefined
            ? <Text style={[styles.marker, dim && styles.dim, { minWidth: markerWidth }]}>{block.ordered ? `${block.start + index}.` : "•"}</Text>
            : <TaskBox checked={item.checked} />}
          <View style={styles.listItemBody}>
            <Blocks blocks={item.blocks} dim={dim} tail={tail && index === lastIndex} />
          </View>
        </View>
      ))}
    </View>
  );
}

const Block = memo(function Block({ block, dim, tail }: { block: ChatBlock; dim: boolean; tail: boolean }) {
  const selectable = useContext(Selectable);
  switch (block.kind) {
    case "paragraph":
      return <Text selectable={selectable} style={[styles.body, dim && styles.dim]}><Runs content={block.content} dim={dim} /></Text>;
    case "heading":
      return (
        <Text accessibilityRole="header" selectable={selectable} style={[block.level === 1 ? styles.heading1 : block.level === 2 ? styles.heading2 : styles.heading3, dim && styles.dim]}>
          <Runs content={block.content} dim={dim} />
        </Text>
      );
    case "code":
      return <CodeBlock block={block} tail={tail} />;
    case "table":
      return <Table block={block} dim={dim} />;
    case "list":
      return <List block={block} dim={dim} tail={tail} />;
    case "quote":
      return <View style={styles.quote}><Blocks blocks={block.blocks} dim tail={tail} /></View>;
    case "rule":
      return <View style={styles.rule} />;
  }
});

/** `tail` marks the last block of a reply that is still being written, down to the innermost one. */
function Blocks({ blocks, dim, tail }: { blocks: ChatBlock[]; dim: boolean; tail: boolean }) {
  return blocks.map((block, index) => <Block block={block} dim={dim} key={index} tail={tail && index === blocks.length - 1} />);
}

/**
 * An agent's reply in a chat, drawn with native views: code in a box with a
 * copy button, tables as tables, lists with their levels. Parsing is shared
 * and kept by `source` (see `parseChatMarkdown`), and a block whose source
 * did not change is not rendered again, so a long reply can grow line by line.
 */
export const ChatMarkdown = memo(function ChatMarkdown({ source, streaming = false, selectable = true }: ChatMarkdownProps) {
  const blocks = useMemo(() => parseChatMarkdown(source), [source]);
  return (
    <Selectable.Provider value={selectable}>
      <View style={styles.root}><Blocks blocks={blocks} dim={false} tail={streaming} /></View>
    </Selectable.Provider>
  );
});

const COPY_HEIGHT = 30;
// The strip is slimmer than a touch target; the slop makes the button 44 dp tall.
const COPY_HIT_SLOP = { bottom: (44 - COPY_HEIGHT) / 2, left: 12, right: 0, top: (44 - COPY_HEIGHT) / 2 } as const;
// The page a step darker, whatever the page is: #22262d on the dark ground.
const CODE_SURFACE = "rgba(0, 0, 0, 0.14)";

const styles = StyleSheet.create({
  root: { alignSelf: "stretch", gap: 10 },
  pressed: { opacity: 0.6 },
  body: { color: colors.chromeInk, fontSize: typeScale.body, lineHeight: 20 },
  dim: { color: colors.chromeDim },
  heading1: { color: colors.chromeInkStrong, fontSize: 20, fontWeight: "700", lineHeight: 26 },
  heading2: { color: colors.chromeInkStrong, fontSize: 18, fontWeight: "700", lineHeight: 24 },
  heading3: { color: colors.chromeInkStrong, fontSize: typeScale.body, fontWeight: "700", lineHeight: 20 },
  strong: { color: colors.chromeInkStrong, fontWeight: "700" },
  strongDim: { fontWeight: "700" },
  emphasis: { fontStyle: "italic" },
  strike: { textDecorationLine: "line-through" },
  strikeLink: { textDecorationLine: "underline line-through" },
  inlineCode: { backgroundColor: colors.chromeRaised, color: colors.chromeInkStrong, fontFamily: fonts.mono, fontSize: 13 },
  link: { color: colors.linkInk, textDecorationLine: "underline" },

  code: { backgroundColor: CODE_SURFACE, borderColor: colors.chromeHairline, borderRadius: radii.card, borderWidth: metrics.hairlineWidth },
  codeTop: {
    alignItems: "center",
    borderBottomColor: colors.chromeHairline,
    borderBottomWidth: metrics.hairlineWidth,
    flexDirection: "row",
    height: COPY_HEIGHT,
    justifyContent: "space-between",
    paddingLeft: 10,
  },
  codeLanguage: { color: colors.chromeFaint, flexShrink: 1, fontSize: typeScale.meta },
  copy: { alignItems: "center", height: COPY_HEIGHT, justifyContent: "center", paddingHorizontal: 10 },
  copyLabel: { color: colors.chromeDim, fontSize: typeScale.meta },
  copiedLabel: { color: colors.okInk, fontSize: typeScale.meta },
  codeScroll: { flexDirection: "row", paddingHorizontal: 10, paddingVertical: 8 },
  codeText: { color: colors.chromeInkStrong, fontFamily: fonts.mono, fontSize: typeScale.terminal, lineHeight: 19 },

  table: { borderColor: colors.chromeHairline, borderRadius: radii.card, borderWidth: metrics.hairlineWidth },
  tableScroll: { flexDirection: "row", flexGrow: 1 },
  tableRows: { flexGrow: 1 },
  tableRow: { borderBottomColor: colors.chromeHairline, borderBottomWidth: metrics.hairlineWidth, flexDirection: "row" },
  tableLastRow: { borderBottomWidth: 0 },
  tableHeader: { backgroundColor: colors.chromeRaised },
  tableCell: { color: colors.chromeInk, flexShrink: 0, fontSize: typeScale.rowSecondary, lineHeight: 18, paddingHorizontal: TABLE_CELL_PADDING, paddingVertical: 5 },
  tableHeaderCell: { color: colors.chromeInkStrong, fontWeight: "600" },

  list: { gap: 4 },
  listItem: { flexDirection: "row" },
  listItemBody: { flex: 1, gap: 4 },
  marker: { color: colors.chromeInk, fontSize: typeScale.body, fontVariant: ["tabular-nums"], lineHeight: 20, paddingRight: 6 },
  taskBox: {
    alignItems: "center",
    borderColor: colors.chromeFaint,
    borderRadius: 3,
    borderWidth: 1.5,
    height: 14,
    justifyContent: "center",
    marginRight: 8,
    marginTop: 3,
    width: 14,
  },
  taskBoxChecked: { backgroundColor: colors.accent, borderColor: colors.accent },
  taskCheck: { color: colors.accentInk, fontSize: 10, fontWeight: "700", lineHeight: 12 },

  quote: { borderLeftColor: colors.chromeBorder, borderLeftWidth: 2, gap: 8, paddingLeft: 10 },
  rule: { backgroundColor: colors.chromeBorder, height: metrics.hairlineWidth, marginVertical: 4 },
});
