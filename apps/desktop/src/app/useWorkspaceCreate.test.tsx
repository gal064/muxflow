// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { HostScopeToken } from "../features/shell/hostScope";
import { setWorkspaceDefaults } from "../features/shell/model";
import { defaultAppState, type PersistedAppState } from "../features/shell/types";
import type { CreateSessionOptions } from "./useShellNavigation";
import { useWorkspaceCreate } from "./useWorkspaceCreate";

const scope: HostScopeToken = {
  hostProfileId: "local", connectionKey: "local", connectionEpoch: 1,
  serverIdentity: "server-a", generation: 1,
};
const created = { sessionId: "$7", windowId: "@7", paneId: "%7", topologyGeneration: 5 };

function mount(state: PersistedAppState, hostProfileId = "local") {
  const createSession = vi.fn<(name: string, options?: CreateSessionOptions) => void>();
  const sendInput = vi.fn(async () => undefined);
  const setStatus = vi.fn();
  const clientIdRef = { current: "client-1" as string | undefined };
  const hostScopeRef = { current: scope };
  let create_!: (name: string) => void;
  function Harness() {
    create_ = useWorkspaceCreate({
      appStateRef: { current: state },
      clientIdRef,
      createSession,
      currentHostProfileId: hostProfileId,
      hostScopeRef,
      sendInput,
      setStatus,
    });
    return null;
  }
  return {
    clientIdRef, createSession, hostScopeRef, sendInput, setStatus,
    get create() { return create_; },
    Harness,
  };
}

const withDefaults = (host: string, directory?: string, startupCommand?: string) =>
  setWorkspaceDefaults(defaultAppState, host, { directory, startupCommand });

describe("creating a workspace with this host's defaults", () => {
  it("carries the configured directory and sends the command once to the acked pane", async () => {
    const harness = mount(withDefaults("local", "/work", "npm run dev"));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<harness.Harness />); });

    act(() => harness.create("api"));
    expect(harness.createSession).toHaveBeenCalledWith("api", expect.objectContaining({ directory: "/work" }));

    const options = harness.createSession.mock.calls[0][1]!;
    act(() => options.onCreated!(created, scope));
    expect(harness.sendInput).toHaveBeenCalledExactlyOnceWith("client-1", "%7", "npm run dev\n");
    await act(async () => renderer.unmount());
  });

  it("sends nothing at all when no command is configured", async () => {
    // Not "sends an empty line": with the setting off there is no callback for
    // the create to call, so there is nothing that could reach the pane.
    const harness = mount(withDefaults("local", "/work"));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<harness.Harness />); });

    act(() => harness.create("api"));
    expect(harness.createSession.mock.calls[0][1]?.onCreated).toBeUndefined();
    expect(harness.sendInput).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("never applies another host's directory or command", async () => {
    // The remote profile's `/srv` does not exist on this laptop, and its
    // command is a command for that machine's shell.
    const harness = mount(withDefaults("ssh-remote", "/srv", "tail -f log"));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<harness.Harness />); });

    act(() => harness.create("api"));
    expect(harness.createSession).toHaveBeenCalledWith("api", { directory: undefined, onCreated: undefined });
    await act(async () => renderer.unmount());
  });

  it("refuses to type into a pane the app is no longer connected to", async () => {
    const harness = mount(withDefaults("local", undefined, "npm run dev"));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<harness.Harness />); });

    act(() => harness.create("api"));
    const options = harness.createSession.mock.calls[0][1]!;
    harness.hostScopeRef.current = { ...scope, connectionEpoch: 2 };
    act(() => options.onCreated!(created, scope));
    expect(harness.sendInput).not.toHaveBeenCalled();

    harness.hostScopeRef.current = scope;
    harness.clientIdRef.current = undefined;
    act(() => options.onCreated!(created, scope));
    expect(harness.sendInput).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("creates the workspace born pinned while the list shows pinned only", async () => {
    // Without this, the new workspace is listed only while it is active and
    // drops out of the filtered sidebar on the first switch away.
    const state = withDefaults("local", "/work");
    const harness = mount({ ...state, shell: { ...state.shell, pinnedOnly: true } });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<harness.Harness />); });

    act(() => harness.create("api"));
    expect(harness.createSession).toHaveBeenCalledWith("api", expect.objectContaining({ pinned: true }));
    await act(async () => renderer.unmount());
  });

  it("leaves the pin unset while the full list is shown", async () => {
    const harness = mount(withDefaults("local", "/work"));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<harness.Harness />); });

    act(() => harness.create("api"));
    expect(harness.createSession.mock.calls[0][1]?.pinned).toBeUndefined();
    await act(async () => renderer.unmount());
  });

  it("reports a command that could not be delivered as its own failure", async () => {
    // The workspace exists. Saying "workspace creation failed" here would be a
    // lie the user would act on.
    const harness = mount(withDefaults("local", undefined, "npm run dev"));
    harness.sendInput.mockRejectedValueOnce(new Error("pane is gone") as never);
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<harness.Harness />); });

    act(() => harness.create("api"));
    const options = harness.createSession.mock.calls[0][1]!;
    await act(async () => { options.onCreated!(created, scope); });
    expect(harness.setStatus).toHaveBeenCalledWith(
      "Workspace created, but the startup command could not be sent: Error: pane is gone",
    );
    await act(async () => renderer.unmount());
  });
});
