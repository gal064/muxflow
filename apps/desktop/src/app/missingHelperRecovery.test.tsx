// @vitest-environment jsdom
import { useReducer, useRef } from "react";
import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  helperUpgradeReducer, initialHelperUpgradeState, type HelperUpgradeState, type RemoteHelperProbe,
} from "../features/shell/helperUpgrade";
import type { TerminalEvent } from "../features/terminal/api";
import type { ConnectionSpec } from "./types";
import { useAppConnectionController } from "./useAppConnectionController";
import { useMissingHelperRecovery } from "./useMissingHelperRecovery";

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
  const observed: { detail: string; helper: HelperUpgradeState } = { detail: "", helper: initialHelperUpgradeState };
  const client = { publishWireEvent: vi.fn(), publishWireSnapshot: vi.fn() } as never;
  // Stable, like the `useState` setter the shell passes: the controller keys
  // its profile load on it, and a fresh function per render re-runs that load
  // forever.
  const setStatus = () => undefined;
  function Harness() {
    const onHandshakeFailure = useRef<(connection: ConnectionSpec) => void>(() => undefined);
    const controller = useAppConnectionController({
      agentClient: client,
      fileClient: client,
      gitClient: client,
      onHandshakeFailure: (failed) => onHandshakeFailure.current(failed),
      setStatus,
    });
    const [helper, dispatchHelper] = useReducer(helperUpgradeReducer, initialHelperUpgradeState);
    onHandshakeFailure.current = useMissingHelperRecovery({
      connection: controller.connection,
      connectionEpoch: controller.connectionEpoch,
      dispatchHelper,
      setConnectionDetail: controller.setConnectionDetail,
    });
    observed.detail = controller.connectionDetail;
    observed.helper = helper;
    return null;
  }
  return { Harness, observed };
}

const probeCalls = () => invokeMock.mock.calls.filter(([command]) => command === "probe_remote_helper");

describe("a handshake failure on a host with no helper", () => {
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
      _sessionId: string, _paneIds: string[], _connection: ConnectionSpec, onEvent: (event: TerminalEvent) => void,
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

  it("says nothing about installing when the host is the newer side", async () => {
    const { observed, publish, renderer } = await connected(async () => probe({
      installed: true, compatible: false, appOutdated: true, helperVersion: "9", expectedHelperVersion: "8",
    }));
    await act(async () => { publish(handshakeFailure); });
    expect(observed.helper.phase).toBe("idle");
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
    expect(observed.helper.phase).toBe("idle");
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
    await act(async () => { local.publish(handshakeFailure); });
    expect(probeCalls()).toHaveLength(0);
    expect(local.observed.helper.phase).toBe("idle");
    await act(async () => local.renderer.unmount());
  });
});
