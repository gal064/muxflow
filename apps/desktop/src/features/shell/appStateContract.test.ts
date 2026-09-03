import { describe, expect, it } from "vitest";
import contract from "./persistedAppState.contract.json";
import {
  clampedPanelWidth, clampedTerminalFontSize, defaultAppState, defaultShellState, normalizePersistedAppState,
  panelWidthForWindow, PANEL_MIN_WIDTH,
  type AppOwnedTab, type PersistedAppState, type WorkspaceDefaults, type WorkspaceUiRecord,
} from "./types";

/**
 * The TypeScript end of the `save_app_state` contract.
 *
 * `PersistedAppState` is declared twice — here and in
 * `src-tauri/src/app_state.rs` — and nothing generates one from the other, so
 * the only thing that can keep them together is a fixture both ends read. The
 * Rust end is `app_state_contract` in that file; it deserializes this same
 * file and fails if any field here is one it does not accept.
 *
 * What this catches, and what a passing `tsc` plus 440 green tests did not:
 * a field added or removed on this side without the storage side following.
 */
describe("persisted app state contract", () => {
  it("names exactly the top-level keys the store persists", () => {
    expect(Object.keys(contract).filter((key) => !key.startsWith("_")).sort())
      .toEqual(Object.keys(defaultAppState).sort());
  });

  it("names exactly the shell preferences, including the optional ones", () => {
    // `windowGeometry` is optional, so it is absent from the defaults and must
    // still be in the contract — a field only ever written after a resize is
    // exactly the kind that goes unnoticed when it stops being saved.
    expect(Object.keys(contract.shell).sort())
      .toEqual([...Object.keys(defaultShellState), "windowGeometry"].sort());
  });

  it("names every field of the two record types that also cross the boundary", () => {
    // `appTabs` carries nineteen fields and `workspaceUi` five. Pinning only
    // the shell object would have left the same silent-drop failure open on the
    // two largest structs.
    const tab: Required<AppOwnedTab> = {
      id: "", hostProfileId: "", serverIdentity: "", sessionId: "", sessionName: "",
      kind: "file", resource: "", title: "", order: 0, rootPath: "", rootToken: "",
      preview: false, viewMode: "source", gitRepositoryId: "", gitPath: "", gitOriginalPath: "",
      gitTarget: "staged", gitStatusGeneration: "", gitSourceGeneration: "",
    };
    const workspace: Required<WorkspaceUiRecord> = {
      hostProfileId: "", serverIdentity: "", sessionId: "", sessionName: "", selectedAppTabId: "",
    };
    // The per-host workspace defaults cross the same boundary, and both of its
    // fields are optional — exactly the shape that stops being saved without
    // anything failing.
    const defaults: Required<WorkspaceDefaults> = { directory: "", startupCommand: "" };
    expect(Object.keys(contract.appTabs[0]).sort()).toEqual(Object.keys(tab).sort());
    expect(Object.keys(contract.workspaceUi[0]).sort()).toEqual(Object.keys(workspace).sort());
    expect(Object.keys(contract.workspaceDefaults.local).sort()).toEqual(Object.keys(defaults).sort());
  });

  it("is a value this side would actually produce", () => {
    // Typing the fixture as the real interface is the assertion: a field whose
    // name or type drifted from `PersistedAppState` fails the typecheck rather
    // than this test.
    const typed: PersistedAppState = {
      ...(contract as unknown as PersistedAppState),
      schemaVersion: 1,
    };
    expect(typed.shell.agentSort).toBe("status");
    expect(typed.shell.sidebarWidth).toBe(260);
    expect(typed.shell.panelWidth).toBe(320);
    expect(typed.appTabs[0].kind).toBe("gitDiff");
    expect(typed.commands.shortcutOverrides["window.new"]).toBe("Ctrl+T");
    expect(typed.shell.defaultMarkdownView).toBe("preview");
    expect(typed.workspaceDefaults.local).toEqual({ directory: "/work/projects", startupCommand: "git status" });
  });

  it("persists the pinned-only filter and leaves saves from before it showing everything", () => {
    // The filter is one boolean over the pins that already exist, so the only
    // thing that can go wrong with it is not coming back — and a save written
    // before the field existed must come back showing every workspace rather
    // than an empty sidebar the user cannot explain.
    const shell = (value: unknown) => normalizePersistedAppState({
      schemaVersion: 1, appTabs: [], workspaceUi: [], shell: { pinnedOnly: value },
    }).shell.pinnedOnly;
    expect(shell(true)).toBe(true);
    expect(shell(false)).toBe(false);
    expect(shell(undefined)).toBe(false);
    expect(shell("yes")).toBe(true);
    expect(defaultAppState.shell.pinnedOnly).toBe(false);
    expect((contract as unknown as PersistedAppState).shell.pinnedOnly).toBe(true);
  });

  it("ignores the pin records left behind by the build that kept pins in app state", () => {
    // Pins are the host's now, read off every snapshot. The records that used
    // to be here are not a migration — an unknown key is simply dropped — and
    // the pins themselves are unaffected, because they were never in this file
    // on the host that owns them.
    const loaded = normalizePersistedAppState({
      ...defaultAppState,
      pinnedWorkspaces: [{ hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", sessionName: "project", pinnedAt: 1 }],
      pinnedTabs: [{ hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", tabId: "@3", pinnedAt: 2 }],
    });
    expect(Object.keys(loaded)).toEqual(Object.keys(defaultAppState));
    expect(loaded).toEqual(defaultAppState);
  });

  it("ignores an archivedWorkspaces field left behind by an older build", () => {
    // The archive is gone, and its records are not a migration: an unknown key
    // is simply dropped, and the workspaces it used to hide come back.
    const loaded = normalizePersistedAppState({
      ...defaultAppState,
      archivedWorkspaces: [{ hostProfileId: "local", serverIdentity: "server-a", sessionId: "$2", sessionName: "parked", archivedAt: 1 }],
    });
    expect(Object.keys(loaded)).toEqual(Object.keys(defaultAppState));
    expect(loaded).toEqual(defaultAppState);
  });
});

describe("per-host workspace defaults", () => {
  const saved = (workspaceDefaults: unknown) => normalizePersistedAppState({
    schemaVersion: 1, appTabs: [], workspaceUi: [], shell: {}, workspaceDefaults,
  }).workspaceDefaults;

  it("keeps one host's directory and command apart from another's", () => {
    expect(saved({
      local: { directory: "/work" },
      "ssh-remote-linux": { startupCommand: "tmux list-sessions" },
    })).toEqual({ local: { directory: "/work" }, "ssh-remote-linux": { startupCommand: "tmux list-sessions" } });
  });

  it("drops values that are not usable rather than storing an empty setting", () => {
    // "Cleared" and "never set" have to be the same state: the create path asks
    // one question — is there a directory? — and a stored "" would answer yes.
    expect(saved({ local: { directory: "   ", startupCommand: "" } })).toEqual({});
    expect(saved({ local: { directory: 7, startupCommand: null } })).toEqual({});
    expect(saved({ local: "everything" })).toEqual({});
    expect(saved({ local: { directory: "  /work  " } })).toEqual({ local: { directory: "/work" } });
    expect(saved(undefined)).toEqual({});
    expect(saved(["local"])).toEqual({});
    // An unusable entry is skipped, not a reason to stop reading the rest: `""`
    // is a legal JSON key, and every host after it would otherwise be dropped.
    expect(saved({ "": { directory: "/x" }, local: { directory: "/work" } }))
      .toEqual({ local: { directory: "/work" } });
  });

  it("bounds each value so one long paste cannot freeze every other save", () => {
    // The storage side validates every text field and refuses the *whole*
    // state, so an unbounded command here would stop open tabs, workspace
    // selection and window geometry from persisting at all.
    const long = "x".repeat(20_000);
    expect(saved({ local: { directory: long, startupCommand: long } })).toEqual({
      local: { directory: "x".repeat(2_048), startupCommand: "x".repeat(2_048) },
    });
  });

  it("caps the map the way the host setup decisions are capped", () => {
    const many = Object.fromEntries(Array.from({ length: 1_100 }, (_, index) => [`host-${index}`, { directory: "/work" }]));
    expect(Object.keys(saved(many))).toHaveLength(1_024);
  });

  it("restores the default markdown view and leaves legacy saves on split", () => {
    const shell = (value: unknown) => normalizePersistedAppState({
      schemaVersion: 1, appTabs: [], workspaceUi: [], shell: { defaultMarkdownView: value },
    }).shell.defaultMarkdownView;
    expect(shell("source")).toBe("source");
    expect(shell("preview")).toBe("preview");
    expect(shell("split")).toBe("split");
    expect(shell("reader")).toBe("split");
    expect(shell(undefined)).toBe("split");
  });
});

describe("the right panel's stored width", () => {
  it("keeps a saved width, and refuses one no drag could have produced", () => {
    expect(clampedPanelWidth(420)).toBe(420);
    expect(clampedPanelWidth(420.6)).toBe(421);
    expect(clampedPanelWidth(120)).toBe(PANEL_MIN_WIDTH);
    expect(clampedPanelWidth(9_000)).toBe(4_000);
    expect(clampedPanelWidth(Number.NaN)).toBe(defaultShellState.panelWidth);
  });

  it("falls back to the default rather than the minimum when nothing was saved", () => {
    // The sidebar's default *is* its minimum, so its fallback can be the
    // minimum. The panel's is 300px, and resetting a save written before this
    // field existed to 240px would narrow every panel on upgrade.
    expect(clampedPanelWidth(undefined)).toBe(300);
    expect(normalizePersistedAppState({
      schemaVersion: 1, appTabs: [], workspaceUi: [], shell: {},
    }).shell.panelWidth).toBe(300);
  });

  it("never lets the panel take more than half the window", () => {
    expect(panelWidthForWindow(600, 1_600)).toBe(600);
    expect(panelWidthForWindow(900, 1_600)).toBe(800);
    // Below twice the minimum the floor wins: a panel narrower than 240px is
    // not a panel, it is a scrollbar.
    expect(panelWidthForWindow(300, 400)).toBe(PANEL_MIN_WIDTH);
  });
});

describe("reading state the previous build wrote", () => {
  const saved = (shell: Record<string, unknown>) => normalizePersistedAppState({
    schemaVersion: 1, appTabs: [], workspaceUi: [], shell,
  });

  it("migrates the renamed agent orderings instead of resetting them", () => {
    // The orderings were renamed, not changed. Falling back to the default —
    // which is what an unrecognised value does everywhere else in this
    // function — would have silently moved every user who had picked the other
    // order back onto this one, for a change that was only ever about wording.
    expect(saved({ agentSort: "priority" }).shell.agentSort).toBe("status");
    expect(saved({ agentSort: "grouped" }).shell.agentSort).toBe("workspace");
    expect(saved({ agentSort: "status" }).shell.agentSort).toBe("status");
    expect(saved({ agentSort: "workspace" }).shell.agentSort).toBe("workspace");
    expect(saved({ agentSort: "pinned" }).shell.agentSort).toBe("pinned");
    // Anything else is still a value this build refuses to trust.
    expect(saved({ agentSort: "inbox" }).shell.agentSort).toBe(defaultShellState.agentSort);
    expect(saved({}).shell.agentSort).toBe(defaultShellState.agentSort);
  });

  it("restores the global compact workspace preference and defaults old saves to normal", () => {
    expect(saved({ compactWorkspaces: true }).shell.compactWorkspaces).toBe(true);
    expect(saved({ compactWorkspaces: false }).shell.compactWorkspaces).toBe(false);
    expect(saved({}).shell.compactWorkspaces).toBe(false);
  });

  it("persists terminal copy preferences and enables safe command cleanup for legacy saves", () => {
    expect(saved({ copyOnSelect: true }).shell.copyOnSelect).toBe(true);
    expect(saved({ copyOnSelect: false }).shell.copyOnSelect).toBe(false);
    expect(saved({}).shell.copyOnSelect).toBe(false);
    expect(saved({ cleanWrappedCommands: true }).shell.cleanWrappedCommands).toBe(true);
    expect(saved({ cleanWrappedCommands: false }).shell.cleanWrappedCommands).toBe(false);
    expect(saved({}).shell.cleanWrappedCommands).toBe(true);
    expect(saved({ terminalApplicationClipboard: true }).shell.terminalApplicationClipboard).toBe(true);
    expect(saved({}).shell.terminalApplicationClipboard).toBe(false);
  });

  it("restores a bounded integer terminal font size and defaults legacy saves", () => {
    expect(saved({ terminalFontSize: 17 }).shell.terminalFontSize).toBe(17);
    expect(saved({ terminalFontSize: 12.6 }).shell.terminalFontSize).toBe(13);
    expect(saved({ terminalFontSize: 3 }).shell.terminalFontSize).toBe(10);
    expect(saved({ terminalFontSize: 99 }).shell.terminalFontSize).toBe(20);
    expect(saved({}).shell.terminalFontSize).toBe(13);
    expect(clampedTerminalFontSize(Number.NaN)).toBe(13);
  });

  it("leaves the rest of a legacy save alone while migrating the ordering", () => {
    // The migration is one field. A save that carries open tabs and a picked
    // workspace must come back with both, not with a fresh default state.
    const tab = { ...contract.appTabs[0] };
    const restored = normalizePersistedAppState({
      schemaVersion: 1,
      appTabs: [tab],
      workspaceUi: [contract.workspaceUi[0]],
      shell: { agentSort: "priority", sidebarWidth: 320, panelOpen: true, panelWidth: 420 },
      commands: { shortcutOverrides: { "window.new": "Ctrl+T" } },
    });
    expect(restored.shell.agentSort).toBe("status");
    expect(restored.appTabs).toEqual([tab]);
    expect(restored.workspaceUi).toHaveLength(1);
    expect(restored.shell.sidebarWidth).toBe(320);
    expect(restored.shell.panelWidth).toBe(420);
    expect(restored.shell.panelOpen).toBe(true);
    expect(restored.commands.shortcutOverrides["window.new"]).toBe("Ctrl+T");
  });
});
