// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { ShortcutEditorDialog } from "./ShortcutEditorDialog";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("ShortcutEditorDialog", () => {
  it("does not persist shortcut collisions", async () => {
    const onChange = vi.fn();
    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(<ShortcutEditorDialog
        onChange={onChange}
        onClose={vi.fn()}
        overrides={{ "session.new": "Ctrl+Shift+T" }}
        platform="linux"
      />);
    });
    const done = renderer.root.findAllByType("button").find((button) => button.children.includes("Done"));
    expect(done?.props.disabled).toBe(true);
    expect(renderer.root.findByProps({ role: "alert" }).children.join(" ")).toContain("Conflicting shortcuts are disabled");
    await act(async () => { renderer.unmount(); });
  });

  it("shows a removed binding as empty rather than as the default it no longer has", async () => {
    // Collision repair stores an explicit null, which persists. Falling back to
    // the default drew ⌘1 in the field while nothing in the app answered it,
    // and the field is also where the user restores it.
    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(<ShortcutEditorDialog
        onChange={vi.fn()}
        onClose={vi.fn()}
        overrides={{ "workspace.select1": null }}
        platform="mac"
      />);
    });
    const field = renderer.root.findByProps({ "aria-label": "Shortcut for Go to workspace 1" });
    expect(field.props.value).toBe("");
    await act(async () => { renderer.unmount(); });
  });
});
