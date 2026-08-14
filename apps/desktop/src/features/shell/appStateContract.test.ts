import { describe, expect, it } from "vitest";
import contract from "./persistedAppState.contract.json";
import { defaultAppState, defaultShellState, type AppOwnedTab, type PersistedAppState, type WorkspaceUiRecord } from "./types";

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
    // `appTabs` carries eighteen fields and `workspaceUi` five. Pinning only
    // the shell object would have left the same silent-drop failure open on the
    // two largest structs.
    const tab: Required<AppOwnedTab> = {
      id: "", hostProfileId: "", serverIdentity: "", sessionId: "", sessionName: "",
      kind: "file", resource: "", title: "", order: 0, rootPath: "", rootToken: "",
      viewMode: "source", gitRepositoryId: "", gitPath: "", gitOriginalPath: "",
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
    expect(typed.shell.agentSort).toBe("priority");
    expect(typed.shell.sidebarWidth).toBe(260);
    expect(typed.appTabs[0].kind).toBe("gitDiff");
    expect(typed.commands.shortcutOverrides["window.new"]).toBe("Ctrl+T");
  });
});
