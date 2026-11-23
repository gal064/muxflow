import { TERMINAL_HTML } from "./terminalHtml";
import type { TerminalScheme } from "./terminalScheme";

/**
 * The page with `scheme` already on its root element, so its first paint is in
 * the right palette rather than dark until `init` arrives after load.
 */
export function terminalDocument(scheme: TerminalScheme): string {
  return TERMINAL_HTML.replace('<html lang="en">', `<html lang="en" data-scheme="${scheme}">`);
}
