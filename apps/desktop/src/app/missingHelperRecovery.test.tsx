// @vitest-environment jsdom
import { useEffect, useReducer, useRef } from "react";
import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  helperConnectionKey,
  helperUpgradeReducer,
  initialHelperUpgradeState,
  type HelperUpgradeState,
  type RemoteHelperProbe,
} from "../features/shell/helperUpgrade";
import type { TerminalEvent } from "../features/terminal/api";
import type { ConnectionSpec } from "./types";
import { useAppConnectionController } from "./useAppConnectionController";
import { useRemoteHelperRecovery } from "./useMissingHelperRecovery";

const invokeMock = vi.hoisted(() => vi.fn());
const startTerminalMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class { onmessage?: (value: ArrayBuffer) => void },
  invoke: invokeMock,
}));
// The bridge itself is not under test — its one event is. Everything else in
// the module (the bridge key, the scope) stays real, because the controller's
// identity of "which connection is this" is exactly what the guard keys on.
vi.mock("../features/terminal/api", async (importOriginal) => ({
  ...await importOriginal<typeof import("../features/terminal/api")>(),
  startTerminal: startTerminalMock,
  stopTerminal: vi.fn(async () => undefined),
  requestTerminalSeed: vi.fn(async () => undefined),
}));

const probe = (overrides: Partial<RemoteHelperProbe> = {}): RemoteHelperProbe => ({
  operatingSystem: "linux", architecture: "x86_64", tmuxVersion: "3.4", gitVersion: "2.43",
  installed: false, compatible: false, remotePath: "/home/operator/.local/bin/muxflow-host",
  ...overrides,
});

const handshakeFailure: TerminalEvent = {
  kind: "error",
  message: "host closed during handshake: bash: /home/operator/.local/bin/muxflow-host: No such file or directory",
  sequence: 0,
};

/**
 * The shell's own wiring, minus the shell: the connection controller reporting
 * a handshake failure into the recovery hook, into the same reducer the
 * Settings button and the confirmation dialog share.
 */
function harness() {
  const observed: {
    cancel(): void;
    detail: string;
    helper: HelperUpgradeState;
    phase: string;
    probeManually(): void;
    reconnect(): void;
    setConnection(connection: ConnectionSpec): void;
  } = {
    cancel: () => undefined, detail: "", helper: initialHelperUpgradeState, phase: "disconnected",
    probeManually: () => undefined,
    reconnect: () => undefined,
    setConnection: () => undefined,
  };
  const client = { publishWireEvent: vi.fn(), publishWireSnapshot: vi.fn() } as never;
  // Stable, like the `useState` setter the shell passes: the controller keys
  // its profile load on it, and a fresh function per render re-runs that load
  // forever.
  const setStatus = () => undefined;
  function Harness() {
    const onHandshakeFailure = useRef<(connection: ConnectionSpec) => void>(() => undefined);
    const onConnectionStateChanged = useRef<NonNullable<
      Parameters<typeof useAppConnectionController>[0]["onConnectionStateChanged"]
    >>(() => undefined);
    const controller = useAppConnectionController({
      agentClient: client,
      fileClient: client,
      gitClient: client,
      onHandshakeFailure: (failed) => onHandshakeFailure.current(failed),
      onConnectionStateChanged: (changed, state) => onConnectionStateChanged.current(changed, state),
      setStatus,
    });
    const [helper, dispatchHelper] = useReducer(helperUpgradeReducer, initialHelperUpgradeState);
    const connectionKey = helperConnectionKey(controller.connection);
    useEffect(() => dispatchHelper({ type: "reset" }), [connectionKey, controller.connectionEpoch]);
    const recovery = useRemoteHelperRecovery({
      connection: controller.connection,
      connectionEpoch: controller.connectionEpoch,
      dispatchHelper,
      setConnectionDetail: controller.setConnectionDetail,
    });
    onHandshakeFailure.current = recovery.onHandshakeFailure;
    onConnectionStateChanged.current = recovery.onConnectionStateChanged;
    observed.detail = controller.connectionDetail;
    observed.helper = helper;
    observed.phase = controller.hostState.phase;
    observed.cancel = () => dispatchHelper({ type: "cancelUpgrade" });
    observed.probeManually = recovery.probeManually;
    observed.reconnect = () => controller.setConnectionEpoch((value) => value + 1);
    observed.setConnection = controller.setConnection;
    return null;
  }
  return { Harness, observed };
}

const probeCalls = () => invokeMock.mock.calls.filter(([command]) => command === "probe_remote_helper");

describe("remote helper reconciliation", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    startTerminalMock.mockReset();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  });

  const sshProfile = { id: "remote-a", label: "qa", connection: { mode: "ssh", profileId: "remote-a", target: "qa-host" } };
  const localProfile = { id: "local", label: "Local", connection: { mode: "local" } };

  /** Mounts the harness on a saved host and hands back its event sink. */
  async function connected(probeResult: () => Promise<RemoteHelperProbe>, profile: unknown = sshProfile) {
    let publish!: (event: TerminalEvent) => void;
    startTerminalMock.mockImplementation(async (
      _sessionId: string, _paneIds: string[], _connection: ConnectionSpec, _attach: boolean, onEvent: (event: TerminalEvent) => void,
    ) => {
      publish = onEvent;
      return "client-1";
    });
    invokeMock.mockImplementation((command: string) => {
      if (command === "list_host_profiles") {
        return Promise.resolve({
          schemaVersion: 1,
          lastProfileId: (profile as { id: string }).id,
          profiles: [profile],
        });
      }
      if (command === "probe_remote_helper") return probeResult();
      return Promise.resolve(undefined);
    });
    const { Harness, observed } = harness();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    return { observed, renderer, publish: (event: TerminalEvent) => publish(event) };
  }

  it("asks the host once, and opens the install the user would have had to find in Settings", async () => {
    const { observed, publish, renderer } = await connected(async () => probe());
    await act(async () => { publish(handshakeFailure); });

    expect(probeCalls()).toHaveLength(1);
    expect(probeCalls()[0][1]).toMatchObject({ connection: { mode: "ssh", target: "qa-host" } });
    expect(observed.helper.phase).toBe("confirming");
    expect(observed.helper).toMatchObject({ operation: "install" });
    // The connection error still stands behind the dialog: cancelling leaves
    // the strip saying what actually happened.
    expect(observed.detail).toContain("No such file or directory");

    // The bridge supervisor retries in a loop, so this event arrives again and
    // again for one missing helper. One question per connection, not per retry.
    await act(async () => { publish(handshakeFailure); publish(handshakeFailure); });
    expect(probeCalls()).toHaveLength(1);
    await act(async () => renderer.unmount());
  });

  it("checks a successful SSH connection once and prompts for a same-version digest mismatch", async () => {
    const { observed, publish, renderer } = await connected(async () => probe({
      installed: true, compatible: false, helperVersion: "0.2.0", expectedHelperVersion: "0.2.0",
    }));
    await act(async () => {
      publish({ kind: "connectionState", state: "connected", sequence: 0 });
    });

    expect(probeCalls()).toHaveLength(1);
    expect(observed.helper).toMatchObject({ phase: "confirming", operation: "upgrade" });

    await act(async () => {
      observed.cancel();
      publish({ kind: "connectionState", state: "connected", sequence: 1 });
    });
    expect(probeCalls()).toHaveLength(1);
    expect(observed.helper.phase).toBe("ready");
    await act(async () => renderer.unmount());
  });

  it("probes a contract-refused read-only helper and offers the compatible replacement", async () => {
    const { observed, publish, renderer } = await connected(async () => probe({
      installed: true, compatible: false, helperVersion: "0.1.0", expectedHelperVersion: "0.2.0",
    }));
    await act(async () => {
      publish({ kind: "error", message: "host helper is missing required capabilities: terminal-output-credit", sequence: 0 });
      publish({ kind: "connectionState", state: "readOnly", sequence: 1 });
    });

    expect(probeCalls()).toHaveLength(1);
    expect(observed.phase).toBe("readOnly");
    expect(observed.helper).toMatchObject({ phase: "confirming", operation: "upgrade" });
    await act(async () => renderer.unmount());
  });

  it("re-probes a native in-place reconnect and ignores the old transport's pending answer", async () => {
    let resolveOld!: (value: RemoteHelperProbe) => void;
    let resolveFresh!: (value: RemoteHelperProbe) => void;
    const oldProbe = new Promise<RemoteHelperProbe>((resolve) => { resolveOld = resolve; });
    const freshProbe = new Promise<RemoteHelperProbe>((resolve) => { resolveFresh = resolve; });
    let call = 0;
    const { observed, publish, renderer } = await connected(() => [oldProbe, freshProbe][call++]);

    await act(async () => { publish({ kind: "connectionState", state: "connected", sequence: 0 }); });
    expect(observed.helper.phase).toBe("probing");
    await act(async () => {
      publish({ kind: "connectionState", state: "disconnected", sequence: 1 });
      publish({ kind: "connectionState", state: "reconnecting", sequence: 2 });
      publish({ kind: "connectionState", state: "connected", sequence: 3 });
    });
    expect(probeCalls()).toHaveLength(2);

    await act(async () => {
      resolveOld(probe({ installed: true, compatible: false }));
      await oldProbe;
    });
    expect(observed.helper.phase).toBe("probing");
    const matching = probe({ installed: true, compatible: true });
    await act(async () => {
      resolveFresh(matching);
      await freshProbe;
    });
    expect(observed.helper).toMatchObject({ phase: "ready", probe: matching });
    await act(async () => renderer.unmount());
  });

  it("withdraws a settled automatic confirmation while its transport reconnects", async () => {
    const mismatch = probe({ installed: true, compatible: false });
    const { observed, publish, renderer } = await connected(async () => mismatch);
    await act(async () => { publish({ kind: "connectionState", state: "connected", sequence: 0 }); });
    expect(observed.helper.phase).toBe("confirming");

    await act(async () => { publish({ kind: "connectionState", state: "disconnected", sequence: 1 }); });
    expect(observed.helper.phase).toBe("idle");
    await act(async () => {
      publish({ kind: "connectionState", state: "reconnecting", sequence: 2 });
      publish({ kind: "connectionState", state: "connected", sequence: 3 });
    });
    expect(probeCalls()).toHaveLength(2);
    expect(observed.helper.phase).toBe("confirming");
    await act(async () => renderer.unmount());
  });

  it("coalesces a concurrent Settings check without weakening the automatic upgrade decision", async () => {
    let resolveProbe!: (value: RemoteHelperProbe) => void;
    const pending = new Promise<RemoteHelperProbe>((resolve) => { resolveProbe = resolve; });
    const { observed, publish, renderer } = await connected(() => pending);
    await act(async () => {
      publish({ kind: "connectionState", state: "connected", sequence: 0 });
      observed.probeManually();
    });
    expect(probeCalls()).toHaveLength(1);

    await act(async () => {
      resolveProbe(probe({
        installed: true, compatible: false, helperVersion: "0.2.0", expectedHelperVersion: "0.2.0",
      }));
      await pending;
    });
    expect(observed.helper).toMatchObject({ phase: "confirming", operation: "upgrade" });
    await act(async () => renderer.unmount());
  });

  it("does not let a coalesced Settings check keep an old transport probe alive", async () => {
    let resolveProbe!: (value: RemoteHelperProbe) => void;
    const pending = new Promise<RemoteHelperProbe>((resolve) => { resolveProbe = resolve; });
    const { observed, publish, renderer } = await connected(() => pending);
    await act(async () => {
      publish({ kind: "connectionState", state: "connected", sequence: 0 });
      observed.probeManually();
      publish({ kind: "connectionState", state: "disconnected", sequence: 1 });
    });
    expect(observed.helper.phase).toBe("idle");

    await act(async () => {
      resolveProbe(probe({ installed: true, compatible: false }));
      await pending;
    });
    expect(observed.helper.phase).toBe("idle");
    await act(async () => renderer.unmount());
  });

  it("gives a settled transport its own probe instead of reusing the failed attempt's answer", async () => {
    let resolveFailedAttempt!: (value: RemoteHelperProbe) => void;
    let resolveSettledTransport!: (value: RemoteHelperProbe) => void;
    const failedAttempt = new Promise<RemoteHelperProbe>((resolve) => { resolveFailedAttempt = resolve; });
    const settledTransport = new Promise<RemoteHelperProbe>((resolve) => { resolveSettledTransport = resolve; });
    let call = 0;
    const { observed, publish, renderer } = await connected(() => call++ === 0 ? failedAttempt : settledTransport);
    await act(async () => {
      publish(handshakeFailure);
      publish({ kind: "connectionState", state: "connected", sequence: 1 });
    });
    expect(probeCalls()).toHaveLength(2);

    await act(async () => {
      resolveFailedAttempt(probe({ installed: true, compatible: false }));
      await failedAttempt;
    });
    expect(observed.helper.phase).toBe("probing");
    const matching = probe({ installed: true, compatible: true });
    await act(async () => {
      resolveSettledTransport(matching);
      await settledTransport;
    });
    expect(observed.helper).toMatchObject({ phase: "ready", probe: matching });
    await act(async () => renderer.unmount());
  });

  it("records a matching successful-connection probe without prompting", async () => {
    const matching = probe({ installed: true, compatible: true, helperVersion: "0.2.0" });
    const { observed, publish, renderer } = await connected(async () => matching);
    await act(async () => {
      publish({ kind: "connectionState", state: "connected", sequence: 0 });
    });

    expect(probeCalls()).toHaveLength(1);
    expect(observed.helper).toMatchObject({ phase: "ready", probe: matching });
    await act(async () => renderer.unmount());
  });

  it("keeps the Settings check informational instead of opening an upgrade prompt", async () => {
    const mismatched = probe({
      installed: true, compatible: false, helperVersion: "0.2.0", expectedHelperVersion: "0.2.0",
    });
    const { observed, renderer } = await connected(async () => mismatched);
    await act(async () => { observed.probeManually(); });

    expect(probeCalls()).toHaveLength(1);
    expect(observed.helper).toMatchObject({ phase: "ready", probe: mismatched });
    await act(async () => renderer.unmount());
  });

  it("keeps a successful connection live when its secondary helper probe fails", async () => {
    const { observed, publish, renderer } = await connected(async () => {
      throw new Error("secondary SSH channel refused");
    });
    await act(async () => {
      publish({ kind: "connectionState", state: "connected", sequence: 0 });
    });

    expect(observed.phase).toBe("connected");
    expect(observed.detail).toBe("");
    expect(observed.helper).toMatchObject({ phase: "failed", message: "Error: secondary SSH channel refused" });
    await act(async () => renderer.unmount());
  });

  it("discards a successful probe response after the app switches hosts", async () => {
    let resolveProbe!: (value: RemoteHelperProbe) => void;
    const pending = new Promise<RemoteHelperProbe>((resolve) => { resolveProbe = resolve; });
    const { observed, publish, renderer } = await connected(async () => pending);
    await act(async () => {
      publish({ kind: "connectionState", state: "connected", sequence: 0 });
    });
    expect(observed.helper.phase).toBe("probing");

    await act(async () => {
      observed.setConnection({ mode: "ssh", profileId: "remote-b", target: "other-host" });
    });
    await act(async () => {
      resolveProbe(probe({ installed: true, compatible: false }));
      await pending;
    });

    expect(observed.helper.phase).toBe("idle");
    await act(async () => renderer.unmount());
  });

  it("abandons a pending probe when an explicit reconnect replaces its epoch", async () => {
    let resolveProbe!: (value: RemoteHelperProbe) => void;
    const pending = new Promise<RemoteHelperProbe>((resolve) => { resolveProbe = resolve; });
    const { observed, publish, renderer } = await connected(() => pending);
    await act(async () => { publish({ kind: "connectionState", state: "connected", sequence: 0 }); });
    expect(observed.helper.phase).toBe("probing");

    await act(async () => { observed.reconnect(); });
    expect(observed.helper.phase).toBe("idle");
    await act(async () => {
      resolveProbe(probe({ installed: true, compatible: false }));
      await pending;
    });
    expect(observed.helper.phase).toBe("idle");
    await act(async () => renderer.unmount());
  });

  it("reconciles again after leaving and returning to the same profile in one epoch", async () => {
    const matching = probe({ installed: true, compatible: true });
    const { observed, publish, renderer } = await connected(async () => matching);
    await act(async () => { publish({ kind: "connectionState", state: "connected", sequence: 0 }); });
    expect(probeCalls()).toHaveLength(1);

    await act(async () => { observed.setConnection({ mode: "local" }); });
    await act(async () => { observed.setConnection(sshProfile.connection as ConnectionSpec); });
    await act(async () => { publish({ kind: "connectionState", state: "connected", sequence: 1 }); });
    expect(probeCalls()).toHaveLength(2);
    expect(observed.helper).toMatchObject({ phase: "ready", probe: matching });
    await act(async () => renderer.unmount());
  });

  it("says nothing about installing when the host is the newer side", async () => {
    const { observed, publish, renderer } = await connected(async () => probe({
      installed: true, compatible: false, appOutdated: true, helperVersion: "9", expectedHelperVersion: "8",
    }));
    await act(async () => { publish(handshakeFailure); });
    expect(observed.helper.phase).toBe("ready");
    expect(observed.detail).toContain("This host runs a newer helper (9) than this app expects (8)");
    await act(async () => renderer.unmount());
  });

  it("leaves an unreachable host with the error it actually gave", async () => {
    // A refused key, a password prompt and a host that is simply not there all
    // fail the probe too, and for those the connection error is the truth.
    const { observed, publish, renderer } = await connected(async () => {
      throw new Error("ssh: connect to host qa-host port 22: Connection refused");
    });
    await act(async () => { publish(handshakeFailure); });
    expect(probeCalls()).toHaveLength(1);
    expect(observed.helper.phase).toBe("failed");
    expect(observed.detail).toContain("No such file or directory");
    await act(async () => renderer.unmount());
  });

  it("ignores an error that is not a handshake, and one from a local bridge", async () => {
    const { observed, publish, renderer } = await connected(async () => probe());
    await act(async () => { publish({ kind: "error", message: "tmux server exited", sequence: 0 }); });
    expect(probeCalls()).toHaveLength(0);
    expect(observed.helper.phase).toBe("idle");
    await act(async () => renderer.unmount());

    // There is no helper to install on the machine the app is running on, and
    // no SSH connection to probe over.
    const local = await connected(async () => probe(), localProfile);
    await act(async () => {
      local.publish({ kind: "connectionState", state: "connected", sequence: 0 });
      local.publish(handshakeFailure);
    });
    expect(probeCalls()).toHaveLength(0);
    expect(local.observed.helper.phase).toBe("idle");
    await act(async () => local.renderer.unmount());
  });
});
