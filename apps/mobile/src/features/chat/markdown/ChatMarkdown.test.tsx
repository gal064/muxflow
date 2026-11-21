// The chat reply renderer (docs/design/mobile-chat-mock.html, S6), rendered
// with the stubbed React Native. With MUXFLOW_RENDER_DIR set, the mock's reply
// is also drawn as an HTML page at phone width.

import * as Clipboard from "expo-clipboard";
import { Linking, StyleSheet } from "react-native";
import { act, type ReactTestInstance, type ReactTestRenderer, type ReactTestRendererJSON } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { textOf, writeRender } from "../../../testing/renderTree";
import { byLabel, isHost, press, render, rerender } from "../../../testing/render";
import { ChatMarkdown } from "./ChatMarkdown";

vi.mock("expo-clipboard", () => ({ setStringAsync: vi.fn(async () => true) }));

const S6_REPLY = [
  "The join is one function:",
  "",
  "```rust",
  "fn join(rows: &[Row]) -> String {",
  "    rows.iter().filter(|row| row.soft_wrapped || row.indented()).map(Row::text).collect()",
  "}",
  "```",
  "",
  "| Case | Joined | Why |",
  "| :-- | :-: | --: |",
  "| Soft wrap | yes | The terminal marks it |",
  "| Boxed | no | The border ends it |",
  "",
  "1. Strip the border",
  "   - left edge",
  "     - both corners",
  "   - right edge",
  "2. Join the rows",
].join("\n");

type Json = ReactTestRendererJSON | string;

function texts(view: ReactTestRenderer): ReactTestInstance[] {
  return view.root.findAll((node) => isHost(node, "Text"));
}

/** The outermost text element that reads exactly `text`. */
function textElement(view: ReactTestRenderer, text: string): ReactTestInstance {
  const found = texts(view).filter((node) => plain(node) === text);
  if (found.length === 0) throw new Error(`no text reads "${text}"`);
  return found[0]!;
}

function plain(node: ReactTestInstance | string): string {
  return typeof node === "string" ? node : node.children.map(plain).join("");
}

/**
 * Where each text starts from the left edge, in dp, as the row layout puts
 * it: padding and borders of the views around it, plus the siblings before it
 * in a row.
 */
function leftEdges(tree: Json | Json[] | null): Map<string, number> {
  const edges = new Map<string, number>();
  const number = (value: unknown) => (typeof value === "number" ? value : 0);
  const walk = (node: Json, left: number) => {
    if (typeof node === "string") return;
    if (node.type === "Text") return void edges.set(textOf(node), left);
    const style = StyleSheet.flatten(node.props.style);
    let x = left + number(style.paddingLeft ?? style.paddingHorizontal) + number(style.borderLeftWidth ?? style.borderWidth);
    for (const child of (node.children ?? []) as Json[]) {
      walk(child, x);
      if (style.flexDirection !== "row" || typeof child === "string") continue;
      const own = StyleSheet.flatten(child.props.style);
      x += number(own.minWidth ?? own.width) + number(own.marginRight);
    }
  };
  for (const node of tree === null ? [] : Array.isArray(tree) ? tree : [tree]) walk(node, 0);
  return edges;
}

async function settle(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
  });
}

beforeEach(() => {
  vi.mocked(Clipboard.setStringAsync).mockClear();
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("ChatMarkdown", () => {
  it("S6: code in a box, a table as a table, a list with its levels", () => {
    const view = render(<ChatMarkdown source={S6_REPLY} />);
    writeRender("chat-markdown-s6", view.toJSON(), 520);
    const shown = textOf(view.toJSON());
    expect(shown).toContain("The join is one function:");
    expect(shown).not.toContain("```");
    expect(shown).not.toContain("| Case |");
    expect(shown).toContain("1. Strip the border");
    expect(shown).toContain("2. Join the rows");
  });

  it("draws a code block with its language, its text unwrapped and selectable, and a copy button", async () => {
    vi.useFakeTimers();
    const view = render(<ChatMarkdown source={S6_REPLY} />);
    expect(textElement(view, "rust")).toBeDefined();
    const code = textElement(view, "fn join(rows: &[Row]) -> String {\n    rows.iter().filter(|row| row.soft_wrapped || row.indented()).map(Row::text).collect()\n}");
    expect(code.props.selectable).toBe(true);
    expect(StyleSheet.flatten(code.props.style).fontFamily).toBe("JetBrains Mono");
    // A sideways scroller around the text is what keeps a long line on one line.
    const scroller = view.root.findAll((node) => isHost(node, "ScrollView") && node.findAll((child) => child === code).length > 0);
    expect(scroller).toHaveLength(1);
    expect(scroller[0]!.props.horizontal).toBe(true);

    const copy = byLabel(view, "Copy code");
    expect(plain(copy)).toBe("Copy");
    const slop = copy.props.hitSlop as { top: number; bottom: number };
    expect(StyleSheet.flatten(copy.props.style).height + slop.top + slop.bottom).toBeGreaterThanOrEqual(44);
    press(copy);
    await settle();
    expect(Clipboard.setStringAsync).toHaveBeenCalledTimes(1);
    expect(Clipboard.setStringAsync).toHaveBeenCalledWith(plain(code));
    expect(plain(byLabel(view, "Copied"))).toBe("Copied");
    act(() => {
      vi.advanceTimersByTime(1500);
    });
    expect(plain(byLabel(view, "Copy code"))).toBe("Copy");
  });

  it("does not say Copied when the clipboard refuses", async () => {
    vi.mocked(Clipboard.setStringAsync).mockRejectedValueOnce(new Error("no clipboard"));
    const view = render(<ChatMarkdown source={"```\nx\n```"} />);
    press(byLabel(view, "Copy code"));
    await settle();
    expect(plain(byLabel(view, "Copy code"))).toBe("Copy");
  });

  it("draws a table's header and cells in columns of one width, aligned as written", () => {
    const view = render(<ChatMarkdown source={S6_REPLY} />);
    const table = view.root.findAll((node) => isHost(node, "ScrollView") && node.props.horizontal === true)[1]!;
    const rows = table.findAll((node) => isHost(node, "View") && StyleSheet.flatten(node.props.style).flexDirection === "row");
    expect(rows.map((row) => row.findAll((node) => isHost(node, "Text")).map(plain))).toEqual([
      ["Case", "Joined", "Why"],
      ["Soft wrap", "yes", "The terminal marks it"],
      ["Boxed", "no", "The border ends it"],
    ]);
    const cellStyles = rows.map((row) => row.findAll((node) => isHost(node, "Text")).map((cell) => StyleSheet.flatten(cell.props.style)));
    for (const row of cellStyles) {
      expect(row.map((style) => style.width)).toEqual(cellStyles[0]!.map((style) => style.width));
      expect(row.map((style) => style.textAlign)).toEqual(["left", "center", "right"]);
    }
    expect(cellStyles[0]!.map((style) => style.width)).toEqual([88, 65, 178]);
    expect(cellStyles[0]![0]!.fontWeight).toBe("600");
    expect(cellStyles[1]![0]!.fontWeight).toBeUndefined();
    for (const cell of rows.flatMap((row) => row.findAll((node) => isHost(node, "Text")))) expect(cell.props.selectable).toBe(true);
  });

  it("indents each level of a nested list and numbers on from the first number", () => {
    const view = render(<ChatMarkdown source={"9. Strip the border\n   - left edge\n     - both corners\n   - right edge\n10. Join the rows"} />);
    const edges = leftEdges(view.toJSON());
    expect(edges.get("9.")).toBe(0);
    expect(edges.get("10.")).toBe(0);
    // Both numbers share one column, as wide as the longer needs.
    expect(edges.get("Strip the border")).toBe(28);
    expect(edges.get("Join the rows")).toBe(28);
    expect(edges.get("left edge")).toBe(44);
    expect(edges.get("right edge")).toBe(44);
    expect(edges.get("both corners")).toBe(60);
  });

  it("draws a task item with a box, and a list item's code block inside the item", () => {
    const view = render(<ChatMarkdown source={"- [x] shipped\n- [ ] next:\n\n  ```sh\n  pnpm test\n  ```\n"} />);
    expect(byLabel(view, "Done").findAll((node) => isHost(node, "Text")).map(plain)).toEqual(["✓"]);
    expect(byLabel(view, "Not done").findAll((node) => isHost(node, "Text"))).toEqual([]);
    const edges = leftEdges(view.toJSON());
    expect(edges.get("shipped")).toBe(22);
    expect(edges.get("pnpm test")).toBeGreaterThan(22);
    expect(textOf(view.toJSON())).not.toContain("[x]");
  });

  it("opens a link the guard allows, and nothing else", () => {
    const open = vi.spyOn(Linking, "openURL").mockResolvedValue(undefined);
    const view = render(<ChatMarkdown source={"[docs](https://example.com/docs) [js](javascript:alert(1)) [file](file:///etc/passwd) [rel](src/a.ts) ![shot](https://example.com/s.png) https://bare.example"} />);
    const links = view.root.findAll((node) => isHost(node, "Text") && typeof node.props.onPress === "function");
    expect(links.map(plain)).toEqual(["docs", "shot", "https://bare.example"]);
    expect(links.every((link) => link.props.accessibilityRole === "link")).toBe(true);
    for (const link of links) press(link);
    expect(open.mock.calls.map((call) => call[0])).toEqual(["https://example.com/docs", "https://example.com/s.png", "https://bare.example"]);
    // The rejected ones are still read, as plain text.
    expect(textOf(view.toJSON())).toBe("docs js file rel shot https://bare.example");
    expect(view.root.findAll((node) => isHost(node, "Image"))).toEqual([]);
  });

  it("draws headings, a quote with a bar and dim text, a rule, and inline styles", () => {
    const view = render(<ChatMarkdown source={"# Title\n\n### Small\n\n> quoted **text**\n\n---\n\nplain **bold** *em* ~~gone~~ `code`"} />);
    expect(StyleSheet.flatten(textElement(view, "Title").props.style)).toMatchObject({ fontSize: 20, fontWeight: "700" });
    expect(StyleSheet.flatten(textElement(view, "Small").props.style)).toMatchObject({ fontSize: 14, fontWeight: "700" });
    const quoted = textElement(view, "quoted text");
    expect(StyleSheet.flatten(quoted.props.style).color).toBe("#a4abb6");
    const bars = view.root.findAll((node) => isHost(node, "View") && StyleSheet.flatten(node.props.style).borderLeftWidth === 2);
    expect(bars).toHaveLength(1);
    expect(bars[0]!.findAll((node) => node === quoted)).toHaveLength(1);
    expect(view.root.findAll((node) => isHost(node, "View") && StyleSheet.flatten(node.props.style).height === 1)).toHaveLength(1);
    const paragraph = textElement(view, "plain bold em gone code");
    expect(paragraph.props.selectable).toBe(true);
    expect(StyleSheet.flatten(paragraph.props.style)).toMatchObject({ fontSize: 14, lineHeight: 20 });
    const runs = paragraph.findAll((node) => isHost(node, "Text") && node !== paragraph).map((run) => [plain(run), StyleSheet.flatten(run.props.style)]);
    expect(runs).toEqual([
      ["bold", expect.objectContaining({ fontWeight: "700" })],
      ["em", expect.objectContaining({ fontStyle: "italic" })],
      ["gone", expect.objectContaining({ textDecorationLine: "line-through" })],
      ["code", expect.objectContaining({ fontFamily: "JetBrains Mono" })],
    ]);
  });

  it("draws an open fence as a code block while the reply is still being written", () => {
    const view = render(<ChatMarkdown source={"Run:\n\n```sh\npnpm test\n``"} streaming />);
    expect(textElement(view, "sh")).toBeDefined();
    expect(textElement(view, "pnpm test").props.selectable).toBe(true);
    rerender(view, <ChatMarkdown source={"Run:\n\n```sh\npnpm test\n```\n\nDone."} streaming />);
    expect(textOf(view.toJSON())).toBe("Run: sh Copy pnpm test Done.");
    // Once the reply has ended, what it says is drawn as it is.
    rerender(view, <ChatMarkdown source={"Run:\n\n```sh\npnpm test\n``"} />);
    expect(textElement(view, "pnpm test\n``")).toBeDefined();
  });

  it("draws nothing for an empty reply", () => {
    expect(textOf(render(<ChatMarkdown source="" />).toJSON())).toBe("");
  });
});
