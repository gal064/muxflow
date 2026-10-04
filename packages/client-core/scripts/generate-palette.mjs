import { readFile, writeFile } from "node:fs/promises";
import { colors, stateColors, terminalCssTokens, terminalTheme } from "../src/palette.ts";

const declarations = Object.entries(colors).map(([key, value]) => {
  const token = key.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);
  // Preserve the desktop's existing hex-alpha values; native colors use rgba.
  if (key === "accentWash") value = `${colors.accent}1f`;
  if (key === "dangerWash") value = `${colors.danger}22`;
  return `  --${token}: ${value};`;
});
for (const [key, token] of Object.entries(terminalCssTokens)) {
  declarations.push(`  ${token}: ${terminalTheme[key]};`);
}
for (const [key, value] of Object.entries(stateColors)) {
  declarations.push(`  --state-${key}: ${value};`);
}
const css = `/* GENERATED from src/palette.ts; run pnpm --filter @muxflow/client-core palette:gen. */\n:root {\n${declarations.join("\n")}\n}\n`;
const target = new URL("../palette.css", import.meta.url);
if (process.argv.includes("--check")) {
  if (await readFile(target, "utf8") !== css) throw new Error("Shared palette CSS is stale");
} else {
  await writeFile(target, css);
}
