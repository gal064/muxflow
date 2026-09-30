// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { ExplorerMutationDialog } from "./ExplorerMutationDialog";

describe("file mutation feedback", () => {
  it.each([false, true])("shows the failure where it remains visible (dismissed=%s)", async (dismissed) => {
    let fail!: (error: Error) => void;
    const onMutate = vi.fn(() => new Promise<void>((_, reject) => { fail = reject; }));
    const onBackgroundError = vi.fn();
    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(<ExplorerMutationDialog
        pending={{ action: "newFile", rootToken: "root", scopeIdentity: "scope" }}
        root={{ token: "root", path: "/work", cwd: "/work", paneId: "%1", revision: "1", gitWorktree: false }}
        scopeIdentity="scope" disabled={false} onClose={vi.fn()}
        onMutate={onMutate} onBackgroundError={onBackgroundError}
      />);
    });
    await act(async () => { renderer.root.findByProps({ autoFocus: true }).props.onChange({ target: { value: "new.txt" } }); });
    await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault: vi.fn() }); });
    expect(onMutate).toHaveBeenCalledWith({ kind: "createFile", parent: "/work", name: "new.txt" });
    if (dismissed) await act(async () => { renderer.unmount(); });
    await act(async () => { fail(new Error("permission denied")); });
    if (dismissed) expect(onBackgroundError).toHaveBeenCalledWith("Error: permission denied");
    else {
      expect(JSON.stringify(renderer.toJSON())).toContain("permission denied");
      expect(onBackgroundError).not.toHaveBeenCalled();
      await act(async () => { renderer.unmount(); });
    }
  });
});
