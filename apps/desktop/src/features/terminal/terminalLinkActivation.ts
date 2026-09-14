import type { Platform } from "../../commands/registry";

/** The deliberate modifier that activates a terminal link on each desktop platform. */
export function isTerminalLinkActivation(
  event: Pick<MouseEvent, "ctrlKey" | "metaKey">,
  platform: Platform,
): boolean {
  return platform === "mac" ? event.metaKey : event.ctrlKey;
}
