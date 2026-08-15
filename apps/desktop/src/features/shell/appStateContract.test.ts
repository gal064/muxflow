import { describe, expect, it } from "vitest";
import contract from "./persistedAppState.contract.json";
import {
  defaultAppState, defaultShellState, normalizePersistedAppState,
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
    expect(typed.appTabs[0].kind).toBe("gitDiff");
    expect(typed.commands.shortcutOverrides["window.new"]).toBe("Ctrl+T");
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

  it("leaves the rest of a legacy save alone while migrating the ordering", () => {
    // The migration is one field. A save that carries open tabs and a picked
    // workspace must come back with both, not with a fresh default state.
    const tab = { ...contract.appTabs[0] };
    const restored = normalizePersistedAppState({
      schemaVersion: 1,
      appTabs: [tab],
      workspaceUi: [contract.workspaceUi[0]],
      shell: { agentSort: "priority", sidebarWidth: 320, panelOpen: true },
      commands: { shortcutOverrides: { "window.new": "Ctrl+T" } },
    });
    expect(restored.shell.agentSort).toBe("status");
    expect(restored.appTabs).toEqual([tab]);
    expect(restored.workspaceUi).toHaveLength(1);
    expect(restored.shell.sidebarWidth).toBe(320);
    expect(restored.shell.panelOpen).toBe(true);
    expect(restored.commands.shortcutOverrides["window.new"]).toBe("Ctrl+T");
  });
});
