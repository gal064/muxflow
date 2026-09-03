// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { act as domAct } from "react";
import { createRoot } from "react-dom/client";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { NewWorkspaceDialog, defaultNewWorkspaceHostId, type NewWorkspaceHostOption } from "./NewWorkspaceDialog";

const hosts: NewWorkspaceHostOption[] = [
  { profileId: "local", label: "Local", letter: "L", phase: "connected", canMutate: true },
  { profileId: "build", label: "Build Server", letter: "B", phase: "connected", canMutate: true },
  { profileId: "staging", label: "Staging", letter: "S", phase: "reconnecting", canMutate: false },
];

describe("NewWorkspaceDialog", () => {
  beforeEach(() => {
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  });

  it("uses the remembered host, then falls back to a writable active or first host", () => {
    expect(defaultNewWorkspaceHostId(hosts, "build", "local")).toBe("build");
    expect(defaultNewWorkspaceHostId(hosts, "removed", "local")).toBe("local");
    expect(defaultNewWorkspaceHostId(hosts, undefined, "staging")).toBe("local");
    expect(defaultNewWorkspaceHostId(hosts.map((host) => ({ ...host, canMutate: false })), undefined, "staging")).toBe("staging");
    expect(defaultNewWorkspaceHostId([], "build", "local")).toBeUndefined();
  });

  it("keeps name entry primary and exposes the remembered host as a checked picker option", async () => {
    const onHost = vi.fn();
    const onSubmit = vi.fn();
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<NewWorkspaceDialog
        hosts={hosts}
        onCancel={vi.fn()}
        onHost={onHost}
        onSubmit={onSubmit}
        selectedHostProfileId="build"
      />, {
        createNodeMock: (element) => element.type === "button"
          ? { focus() {}, isConnected: true, getBoundingClientRect: () => ({ left: 20, bottom: 80, width: 420 }) }
          : element.type === "input" ? { focus() {}, isConnected: true } : null,
      });
    });

    const input = renderer.root.findByType("input");
    expect(input.props.autoFocus).toBe(true);
    const save = renderer.root.findAllByType("button").find((button) => button.props.type === "submit")!;
    expect(save.props.disabled).toBe(true);
    const picker = renderer.root.findByProps({ "aria-label": "Host: Build Server" });
    await act(async () => { picker.props.onClick({ currentTarget: { getBoundingClientRect: () => ({ left: 20, bottom: 80, width: 420 }) } }); });
    const selected = renderer.root.findByProps({ "data-menu-item": "new-workspace-host-build" });
    expect(selected.props["aria-checked"]).toBe(true);
    expect(renderer.root.findByProps({ "data-menu-item": "new-workspace-host-staging" }).props.disabled).toBe(true);
    await act(async () => { renderer.root.findByProps({ "data-menu-item": "new-workspace-host-local" }).props.onClick(); });
    expect(onHost).toHaveBeenCalledWith("local");

    await act(async () => { input.props.onChange({ target: { value: "release-checks" } }); });
    expect(renderer.root.findAllByType("button").find((button) => button.props.type === "submit")!.props.disabled).toBe(false);
    await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault() {} }); });
    expect(onSubmit).toHaveBeenCalledWith("release-checks", "build");
    await act(async () => renderer.unmount());
  });

  it("does not submit while the selected host is unavailable", async () => {
    const onSubmit = vi.fn();
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<NewWorkspaceDialog hosts={hosts} onCancel={vi.fn()} onHost={vi.fn()} onSubmit={onSubmit} selectedHostProfileId="staging" />);
    });
    const input = renderer.root.findByType("input");
    await act(async () => { input.props.onChange({ target: { value: "work" } }); });
    expect(renderer.root.findAllByType("button").find((button) => button.props.type === "submit")!.props.disabled).toBe(true);
    await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault() {} }); });
    expect(onSubmit).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("keeps the background isolated and lets Escape close only the host menu", async () => {
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const onCancel = vi.fn();
    await domAct(async () => {
      root.render(<>
        <button id="background">Background</button>
        <NewWorkspaceDialog hosts={hosts} onCancel={onCancel} onHost={vi.fn()} onSubmit={vi.fn()} selectedHostProfileId="build" />
      </>);
      await Promise.resolve();
    });
    const background = container.querySelector<HTMLElement>("#background")!;
    const picker = container.querySelector<HTMLButtonElement>('.new-workspace-host-picker')!;
    expect(background.inert).toBe(true);
    expect(background.getAttribute("aria-hidden")).toBe("true");

    await domAct(async () => {
      picker.focus();
      picker.click();
      await Promise.resolve();
    });
    expect(background.inert).toBe(true);
    expect(background.getAttribute("aria-hidden")).toBe("true");
    const checked = container.querySelector<HTMLButtonElement>('[data-menu-item="new-workspace-host-build"]')!;
    expect(document.activeElement).toBe(checked);

    await domAct(async () => {
      checked.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key: "Escape" }));
      await Promise.resolve();
    });
    expect(onCancel).not.toHaveBeenCalled();
    expect(container.querySelector('[role="dialog"]')).not.toBeNull();
    expect(container.querySelector('[role="menu"]')).toBeNull();
    expect(document.activeElement).toBe(picker);
    expect(background.inert).toBe(true);

    await domAct(async () => root.unmount());
    container.remove();
  });
});
