// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AgentHostSetupDialog } from "../features/agents/AgentHostSetupDialog";
import { HookReviewDialog } from "../features/agents/HookReviewDialog";
import { AppDialogLayer } from "./AppDialogLayer";
import { useAppHostSettingsActions } from "./useAppHostSettingsActions";
import { helperConnectionKey } from "../features/shell/helperUpgrade";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
beforeEach(() => { invoke.mockReset(); Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true }); });

const connection = { mode: "ssh" as const, profileId: "host", target: "fixture" };
const probe = {
  operatingSystem: "Linux", architecture: "x86_64", tmuxVersion: "tmux 3.5",
  gitVersion: "git 2", installed: true, compatible: false, remotePath: "$HOME/.local/bin/muxflow-host",
};

describe("setup feedback", () => {
  it("keeps the helper confirmation mounted and disables both actions during upgrade", async () => {
    const props = {
      helperState: { phase: "upgrading", connectionKey: "host", operation: "upgrade", probe },
      onHelperCancel: vi.fn(), onHelperConfirm: vi.fn(),
    } as unknown as Parameters<typeof AppDialogLayer>[0];
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const confirming = { ...props, helperState: { ...props.helperState, phase: "confirming" } } as typeof props;
    await act(async () => { root.render(<AppDialogLayer {...confirming} />); });
    const originalDialog = container.querySelector('[role="alertdialog"]');
    await act(async () => { root.render(<AppDialogLayer {...props} />); });
    expect(container.querySelector('[role="alertdialog"]')).toBe(originalDialog);
    expect(container.textContent).toContain("Upgrading remote helper…");
    expect(container.querySelector('[role="status"]')?.textContent).toContain("Uploading and verifying");
    expect([...container.querySelectorAll("button")].every((button) => button.disabled)).toBe(true);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(props.onHelperCancel).not.toHaveBeenCalled();
    await act(async () => { root.unmount(); });
    container.remove();
  });

  it.each([false, true])("surfaces an upgrade failure outside Settings (rejected=%s)", async (rejected) => {
    if (rejected) invoke.mockRejectedValue(new Error("SSH upload timed out"));
    else invoke.mockResolvedValue({ ok: false, message: "SSH upload timed out", rollback: "notNeeded" });
    const setStatus = vi.fn();
    const dispatchHelper = vi.fn();
    const setConnectionEpoch = vi.fn();
    let actions!: ReturnType<typeof useAppHostSettingsActions>;
    function Harness() {
      actions = useAppHostSettingsActions({
        connection,
        helperState: { phase: "confirming", connectionKey: helperConnectionKey(connection), operation: "upgrade", probe },
        currentScope: {}, scopeIsCurrent: () => true, dispatchHelper, setStatus, setConnectionEpoch,
      } as unknown as Parameters<typeof useAppHostSettingsActions>[0]);
      return null;
    }
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<Harness />); });
    await act(async () => { await actions.confirmHelperInstall(); });
    expect(setStatus).toHaveBeenCalledWith(expect.stringContaining("Helper upgrade failed:"));
    expect(setStatus).toHaveBeenCalledWith(expect.stringContaining("SSH upload timed out"));
    expect(dispatchHelper).toHaveBeenCalledWith(expect.objectContaining({ type: "upgradeFailed" }));
    expect(setConnectionEpoch).not.toHaveBeenCalled();
    await act(async () => { renderer.unmount(); });
  });

  it("shows hook setup activity without offering another action", () => {
    const setup = renderToStaticMarkup(<AgentHostSetupDialog adapters={[]} activity="install" hostLabel="Fixture"
      onAccept={vi.fn()} onDecline={vi.fn()} onReview={vi.fn()} />);
    expect(setup).toContain('role="status"');
    expect(setup).toContain("Setting up agent hooks…");
    expect((setup.match(/disabled=""/g) ?? []).length).toBe(3);
    const review = renderToStaticMarkup(<HookReviewDialog applying review={{
      action: "install", adapterId: "codex", revision: "1", managedLabel: "1", changes: [], alreadyInstalled: false,
    }} onCancel={vi.fn()} onConfirm={vi.fn()} />);
    expect(review).toContain("Installing agent hooks…");
    expect((review.match(/disabled=""/g) ?? []).length).toBe(2);
  });
});
