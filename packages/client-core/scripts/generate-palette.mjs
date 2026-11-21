import { readFile, writeFile } from "node:fs/promises";
import {
  colors, lightColors, lightStateColors, lightTerminalTheme, stateColors, terminalCssTokens, terminalTheme,
} from "../src/palette.ts";

const dark = { chrome: colors, terminal: terminalTheme, state: stateColors };
const light = { chrome: lightColors, terminal: lightTerminalTheme, state: lightStateColors };

// One theme's tokens, as `[name, value]`, in the order both outputs print them.
// The names are the CSS custom properties without their leading dashes.
function tokens({ chrome, terminal, state }) {
  return [
    ...Object.entries(chrome).map(([key, value]) => {
      const token = key.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);
      // Preserve the desktop's existing hex-alpha values; native colors use rgba.
      if (key === "accentWash") value = `${chrome.accent}1f`;
      if (key === "dangerWash") value = `${chrome.danger}22`;
      return [token, value];
    }),
    ...Object.entries(terminalCssTokens).map(([key, token]) => [token.slice(2), terminal[key]]),
    ...Object.entries(state).map(([key, value]) => [`state-${key}`, value]),
  ];
}

const banner = "GENERATED from src/palette.ts; run pnpm --filter @muxflow/client-core palette:gen.";

const block = (selector, theme) => `${selector} {\n${tokens(theme).map(([name, value]) => `  --${name}: ${value};`).join("\n")}\n}\n`;

// Dark is the bare `:root`, so a document with no theme attribute is dark. The
// `data-palette` selectors let one part of the page take the other palette:
// Markdown documents, when Settings gives them an appearance of their own.
const css = `/* ${banner} */\n${[
  block(':root, [data-palette="dark"]', dark),
  block(':root[data-theme="light"], [data-palette="light"]', light),
].join("")}`;

const rustField = (name) => name.replaceAll("-", "_");

// `0xrrggbbaa`, the form GPUI's `rgba()` takes.
function rustColor(name, value) {
  const hex = /^#([0-9a-f]{6})([0-9a-f]{2})?$/.exec(value);
  if (!hex) throw new Error(`Palette token ${name} is not a hex color: ${value}`);
  return `0x${hex[1]}${hex[2] ?? "ff"}`;
}

function rustTheme(fn, theme) {
  const fields = tokens(theme).map(([name, value]) => `        ${rustField(name)}: rgba(${rustColor(name, value)}),`);
  return `pub fn ${fn}() -> Palette {\n    Palette {\n${fields.join("\n")}\n    }\n}\n`;
}

const rust = `// ${banner}

use gpui_kit::{Rgba, rgba};

/// One theme's colors. Field names are the \`palette.css\` custom properties.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
${tokens(dark).map(([name]) => `    pub ${rustField(name)}: Rgba,`).join("\n")}
}

${rustTheme("dark", dark)}
${rustTheme("light", light)}`;

const outputs = [
  [new URL("../palette.css", import.meta.url), css],
  [new URL("../../../apps/desktop-gpui/src/theme/palette.rs", import.meta.url), rust],
];
for (const [target, content] of outputs) {
  if (process.argv.includes("--check")) {
    if (await readFile(target, "utf8") !== content) throw new Error(`Shared palette output is stale: ${target.pathname.split("/").pop()}`);
  } else {
    await writeFile(target, content);
  }
}
