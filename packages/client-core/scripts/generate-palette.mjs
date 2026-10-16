import { readFile, writeFile } from "node:fs/promises";
import {
  colors, lightColors, lightStateColors, lightTerminalTheme, stateColors, terminalCssTokens, terminalTheme,
} from "../src/palette.ts";

function chromeDeclarations(palette) {
  return Object.entries(palette).map(([key, value]) => {
    const token = key.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);
    // Preserve the desktop's existing hex-alpha values; native colors use rgba.
    if (key === "accentWash") value = `${palette.accent}1f`;
    if (key === "dangerWash") value = `${palette.danger}22`;
    return `  --${token}: ${value};`;
  });
}

function terminalDeclarations(theme) {
  return Object.entries(terminalCssTokens).map(([key, token]) => `  ${token}: ${theme[key]};`);
}

function stateDeclarations(state) {
  return Object.entries(state).map(([key, value]) => `  --state-${key}: ${value};`);
}

const block = (selector, declarations) => `${selector} {\n${declarations.join("\n")}\n}\n`;

// Dark is the bare `:root`, so a document with no theme attribute is dark. The
// `data-palette` selectors let one part of the page take the other palette:
// Markdown documents, when Settings gives them an appearance of their own.
const css = `/* GENERATED from src/palette.ts; run pnpm --filter @muxflow/client-core palette:gen. */\n${[
  block(':root, [data-palette="dark"]', [...chromeDeclarations(colors), ...terminalDeclarations(terminalTheme), ...stateDeclarations(stateColors)]),
  block(':root[data-theme="light"], [data-palette="light"]', [
    ...chromeDeclarations(lightColors), ...terminalDeclarations(lightTerminalTheme), ...stateDeclarations(lightStateColors),
  ]),
].join("")}`;
const target = new URL("../palette.css", import.meta.url);
if (process.argv.includes("--check")) {
  if (await readFile(target, "utf8") !== css) throw new Error("Shared palette CSS is stale");
} else {
  await writeFile(target, css);
}
