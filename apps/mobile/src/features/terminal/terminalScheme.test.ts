import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { lightTerminalTheme, terminalTheme } from "@muxflow/client-core";

import type { Agent, Pane } from "../../store/sessionStore";
import { paneTerminalScheme } from "./paneTerminalScheme";
import { schemeForBackground, terminalThemeFor } from "./terminalScheme";
import { terminalDocument } from "./terminalDocument";

function pane(chatAdapterId = ""): Pane {
  return { id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/", currentCommand: "codex", chatAdapterId };
}

function agent(adapterId: string, overrides: Partial<Agent> = {}): Agent {
  return {
    id: adapterId,
    adapterId,
    nativeSessionId: "",
    displayName: adapterId,
    lifecycle: "idle",
    attentionKind: "",
    stateGeneration: 1n,
    attentionGeneration: 0n,
    seenGeneration: 0n,
    updatedAtMs: 0,
    lifecycleChangedAtMs: 0,
    attentionSeenAtMs: 0,
    present: true,
    route: { sessionId: "$1", sessionNameFallback: "", windowId: "@1", windowNameFallback: "", paneId: "%1", paneIndexFallback: 0, paneTitleFallback: "" },
    ...overrides,
  };
}

describe("a pane's terminal scheme", () => {
  const scheme = (panes: Record<string, Pane>, agents: Record<string, Agent>) => paneTerminalScheme({ panes, agents, terminalScheme: "light" }, "%1");

  it("takes the host's scheme while Codex runs in the pane", () => {
    expect(scheme({ "%1": pane() }, { codex: agent("codex") })).toBe("light");
  });

  it("stays dark for a shell, another agent, or a Codex that has left", () => {
    expect(scheme({ "%1": pane() }, {})).toBe("dark");
    expect(scheme({ "%1": pane() }, { claude: agent("claude") })).toBe("dark");
    expect(scheme({ "%1": pane() }, { codex: agent("codex", { present: false }) })).toBe("dark");
  });

  it("stays dark for Codex in another pane", () => {
    const elsewhere = agent("codex");
    expect(scheme({ "%1": pane() }, { codex: { ...elsewhere, route: { ...elsewhere.route, paneId: "%2" } } })).toBe("dark");
  });

  it("stays dark for a chat pane and for a pane not known yet", () => {
    expect(scheme({ "%1": pane("codex") }, { codex: agent("codex") })).toBe("dark");
    expect(scheme({}, { codex: agent("codex") })).toBe("dark");
  });
});

describe("terminal scheme", () => {
  it("follows the brightness of the background the host reports", () => {
    expect(schemeForBackground(lightTerminalTheme.background)).toBe("light");
    expect(schemeForBackground(terminalTheme.background)).toBe("dark");
    expect(schemeForBackground("#FAFAFA")).toBe("light");
    expect(schemeForBackground("#000000")).toBe("dark");
  });

  it("stays dark for a background it cannot read", () => {
    for (const bad of ["", "fcfcfc", "#fcfcf", "#fcfcfcf", "#gggggg"]) expect(schemeForBackground(bad)).toBe("dark");
  });

  it("draws each scheme in its shared palette", () => {
    expect(terminalThemeFor("light")).toBe(lightTerminalTheme);
    expect(terminalThemeFor("dark")).toBe(terminalTheme);
  });

  it("gives the page the terminal's own background in each scheme", () => {
    // The page shows through the gutter left of the terminal, so a drift from
    // the palette would be a visible strip.
    const page = readFileSync(new URL("../../../webview/terminal/index.html", import.meta.url), "utf8");
    const backgrounds = [...page.matchAll(/--page-bg: (#[0-9a-f]{6});/gi)].map((match) => match[1]);
    expect(backgrounds).toEqual([terminalTheme.background, lightTerminalTheme.background]);
  });

  it("puts the scheme on the page before its first paint", () => {
    expect(terminalDocument("light")).toContain('<html lang="en" data-scheme="light">');
  });
});
