// Reading a react-test-renderer tree in tests: its text, and an HTML drawing
// of it at phone width (React Native styles mapped to CSS) that a reviewer
// can open in a browser. `MUXFLOW_RENDER_DIR` set: `writeRender` saves one
// .html file per call there. Test-only.

import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import type { ReactTestRendererJSON } from "react-test-renderer";

type Node = ReactTestRendererJSON | string;

function children(node: ReactTestRendererJSON): Node[] {
  return (node.children ?? []) as Node[];
}

/**
 * Every string in the tree, in order: one text element's runs joined as it
 * draws them, separate elements with single spaces.
 */
export function textOf(tree: Node | Node[] | null): string {
  const out: string[] = [];
  const inline = (node: Node): string => (typeof node === "string" ? node : children(node).map(inline).join(""));
  const walk = (node: Node | Node[] | null) => {
    if (node === null) return;
    if (Array.isArray(node)) return node.forEach(walk);
    if (typeof node === "string") return void out.push(node);
    if (node.type === "Text") return void out.push(inline(node));
    if (typeof node.props.placeholder === "string" && !node.props.value) out.push(node.props.placeholder);
    if (typeof node.props.value === "string") out.push(node.props.value);
    children(node).forEach(walk);
  };
  walk(tree);
  return out.join(" ").replace(/\s+/g, " ").trim();
}

const UNITLESS = new Set(["flex", "flexGrow", "flexShrink", "fontWeight", "opacity", "zIndex", "aspectRatio"]);

function flatten(style: unknown): Record<string, unknown> {
  if (!style) return {};
  if (Array.isArray(style)) return Object.assign({}, ...style.map(flatten));
  return typeof style === "object" ? (style as Record<string, unknown>) : {};
}

function css(style: Record<string, unknown>): string {
  const out: string[] = [];
  const put = (key: string, value: unknown) => {
    if (value === undefined || value === null || value === false) return;
    const prop = key.replace(/[A-Z]/g, (ch) => `-${ch.toLowerCase()}`);
    out.push(`${prop}:${typeof value === "number" && !UNITLESS.has(key) ? `${value}px` : String(value)}`);
  };
  for (const [key, value] of Object.entries(style)) {
    switch (key) {
      case "paddingHorizontal": put("paddingLeft", value); put("paddingRight", value); break;
      case "paddingVertical": put("paddingTop", value); put("paddingBottom", value); break;
      case "marginHorizontal": put("marginLeft", value); put("marginRight", value); break;
      case "marginVertical": put("marginTop", value); put("marginBottom", value); break;
      case "flex":
        if (typeof value === "number" && value > 0) out.push(`flex:${value} 1 0%`);
        break;
      case "borderWidth": case "borderTopWidth": case "borderBottomWidth": case "borderLeftWidth": case "borderRightWidth":
        put(key, value);
        out.push(`${key.replace("Width", "Style").replace(/[A-Z]/g, (ch) => `-${ch.toLowerCase()}`)}:${style.borderStyle ?? "solid"}`);
        break;
      case "transform": case "shadowColor": case "shadowOffset": case "shadowOpacity": case "shadowRadius": case "elevation":
        break;
      case "fontFamily":
        put(key, value === "monospace" || value === "JetBrains Mono" ? "'JetBrains Mono', ui-monospace, monospace" : "system-ui, sans-serif");
        break;
      default:
        put(key, value);
    }
  }
  return out.join(";");
}

function escape(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

function html(node: Node, inText: boolean): string {
  if (typeof node === "string") return escape(node);
  const style = flatten(node.props.style);
  const inner = (text: boolean) => children(node).map((child) => html(child, text)).join("");
  const label = typeof node.props.accessibilityLabel === "string" ? ` title="${escape(node.props.accessibilityLabel)}"` : "";
  switch (node.type) {
    case "Text": {
      const lines = typeof node.props.numberOfLines === "number" ? `;display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:${node.props.numberOfLines};overflow:hidden` : "";
      return inText ? `<span style="${css(style)}">${inner(true)}</span>` : `<div class="t" style="${css(style)}${lines}"${label}>${inner(true)}</div>`;
    }
    case "TextInput": {
      const value = typeof node.props.value === "string" ? node.props.value : "";
      const placeholder = typeof node.props.placeholder === "string" ? node.props.placeholder : "";
      return `<div class="v input" style="${css(style)}">${value ? escape(value) : `<span style="color:#7f8794">${escape(placeholder)}</span>`}</div>`;
    }
    case "Svg": case "Path": case "Rect": case "Circle": case "G": case "Line": {
      if (node.type !== "Svg") return "";
      const size = (key: string) => (typeof node.props[key] === "number" ? `${node.props[key]}px` : "16px");
      return `<div class="v" style="width:${size("width")};height:${size("height")};border-radius:50%;border:1.5px solid currentColor;opacity:.7;flex-shrink:0"></div>`;
    }
    case "ActivityIndicator":
      return `<div class="v" style="width:20px;height:20px;border-radius:50%;border:2px solid #7aa6da;border-right-color:transparent"></div>`;
    case "Switch":
      return `<div class="v" style="width:36px;height:20px;border-radius:10px;background:${node.props.value ? "#7aa6da" : "#3e4451"}"></div>`;
    case "ScrollView":
      return `<div class="v" style="${css(style)};overflow:hidden"><div class="v" style="${css(flatten(node.props.contentContainerStyle))}">${inner(false)}</div></div>`;
    case "Modal":
      return `<div class="v" style="position:absolute;inset:0;z-index:50">${inner(false)}</div>`;
    default:
      return `<div class="v" style="${css(style)}"${label}>${inner(inText)}</div>`;
  }
}

/** One standalone page drawing `tree` in a 360 dp frame. */
export function renderHtml(tree: Node | Node[] | null, title: string, frameHeight = 760): string {
  const nodes = tree === null ? [] : Array.isArray(tree) ? tree : [tree];
  return `<!doctype html><meta charset="utf-8"><title>${escape(title)}</title>
<style>
body{margin:0;padding:16px;background:#1f2329;font-family:system-ui,-apple-system,Roboto,sans-serif;color:#c4c8c6}
h1{font:600 13px system-ui;color:#a4abb6;margin:0 0 8px}
.frame{position:relative;width:360px;height:${frameHeight}px;overflow:hidden;border:1px solid #3e4451;border-radius:18px;background:#282c34;display:flex;flex-direction:column}
.v{display:flex;flex-direction:column;position:relative;box-sizing:border-box;flex-shrink:0;min-width:0;min-height:0;align-items:stretch}
.t{box-sizing:border-box;white-space:pre-wrap;overflow-wrap:anywhere}
.input{white-space:pre-wrap}
</style>
<h1>${escape(title)}</h1><div class="frame">${nodes.map((node) => html(node, false)).join("")}</div>`;
}

/** Saves `renderHtml(tree)` as `<name>.html` under `MUXFLOW_RENDER_DIR`; does nothing when it is unset. */
export function writeRender(name: string, tree: Node | Node[] | null, frameHeight?: number): void {
  const dir = process.env.MUXFLOW_RENDER_DIR;
  if (!dir) return;
  mkdirSync(dir, { recursive: true });
  writeFileSync(path.join(dir, `${name}.html`), renderHtml(tree, name, frameHeight));
}
