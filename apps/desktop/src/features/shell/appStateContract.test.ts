import { describe, expect, it } from "vitest";
import contract from "./persistedAppState.contract.json";
import {
  clampedPanelWidth, clampedTerminalFontSize, defaultAppState, defaultShellState, normalizePersistedAppState,
  panelWidthForWindow, PANEL_MIN_WIDTH,
  type AppOwnedTab, type PersistedAppState, type WorkspaceUiRecord,
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
    expect(Object.keys(contract.appTabs[0]).sort()).toEqual(Object.keys(tab).sort());
    expect(Object.keys(contract.workspaceUi[0]).sort()).toEqual(Object.keys(workspace).sort());
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
    // Anything else is still a value this build refuses to trust.
    expect(saved({ agentSort: "inbox" }).shell.agentSort).toBe(defaultShellState.agentSort);
    expect(saved({}).shell.agentSort).toBe(defaultShellState.agentSort);
  });

  it("restores the global compact workspace preference and defaults old saves to normal", () => {
    expect(saved({ compactWorkspaces: true }).shell.compactWorkspaces).toBe(true);
    expect(saved({ compactWorkspaces: false }).shell.compactWorkspaces).toBe(false);
    expect(saved({}).shell.compactWorkspaces).toBe(false);
  });

  it("persists copy-on-select and leaves legacy saves explicitly disabled", () => {
    expect(saved({ copyOnSelect: true }).shell.copyOnSelect).toBe(true);
    expect(saved({ copyOnSelect: false }).shell.copyOnSelect).toBe(false);
    expect(saved({}).shell.copyOnSelect).toBe(false);
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
