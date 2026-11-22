// New agent | New terminal against the real host (quick-session plan §4.2,
// §5): the first tap creates `random` in the default directory, the next opens
// a window in it, and the window starts in the directory it was given. On a
// host with no tmux server, the create starts one on the same connection.
// Skipped when cargo or tmux is absent.

import { mkdtempSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { liveHostAvailability, startHostHarness, type HostHarness } from "../../../scripts/host-harness";
import { HostConnection } from "../../protocol/HostConnection";
import { createSessionStore } from "../../store/sessionStore";
import { isConnectionScopeCurrent, NO_TMUX_SERVER, openInRandom } from "./createWindow";

const availability = liveHostAvailability();

async function waitFor<T>(label: string, probe: () => T | undefined, timeoutMs = 15_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = probe();
    if (value !== undefined) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

describe.skipIf(!availability.available)(`live random workspace (${availability.reason ?? "cargo + tmux present"})`, () => {
  let harness: HostHarness;
  let connection: HostConnection;
  const store = createSessionStore();
  let epoch = 0;

  beforeAll(async () => {
    harness = await startHostHarness();
    connection = new HostConnection({ dial: async () => harness.transport, nextConnectionEpoch: () => (epoch += 1), store });
  });

  afterAll(async () => {
    connection?.disconnect();
    await harness?.stop();
  });

  it("creates random in the directory, then opens windows in it in theirs", async () => {
    connection.connect();
    await waitFor("connected", () => (store.getState().connection.state === "connected" ? true : undefined));
    // Resolved: tmux reports the pane's path with symlinks (macOS /var) resolved.
    const first = realpathSync(mkdtempSync(path.join(tmpdir(), "mxm-random-a-")));
    const second = realpathSync(mkdtempSync(path.join(tmpdir(), "mxm-random-b-")));
    const panePath = (paneId: string) => harness.tmux(["display-message", "-p", "-t", paneId, "#{pane_current_path}"]);

    const created = await openInRandom(connection, store, { kind: "terminal", directory: first, getCurrentConnection: () => connection });
    expect(created.createdWorkspace).toBe(true);
    expect(harness.tmux(["display-message", "-p", "-t", created.sessionId, "#{session_name}"])).toBe("random");
    expect(panePath(created.paneId)).toBe(first);

    await waitFor("random in the topology", () => Object.values(store.getState().sessions).find((session) => session.name === "random"));
    const opened = await openInRandom(connection, store, { kind: "terminal", directory: second, getCurrentConnection: () => connection });
    expect(opened).toMatchObject({ createdWorkspace: false, sessionId: created.sessionId });
    expect(opened.paneId).not.toBe(created.paneId);
    expect(panePath(opened.paneId)).toBe(second);

    // A directory the host cannot use is refused, not silently replaced.
    await expect(openInRandom(connection, store, { kind: "terminal", directory: path.join(second, "missing"), getCurrentConnection: () => connection }))
      .rejects.toThrow(/does not exist/);
  }, 60_000);
});

// The harness runs its own tmux server on a private socket, so ending it here
// leaves the user's tmux alone.
describe.skipIf(!availability.available)(`live random workspace, no tmux server (${availability.reason ?? "cargo + tmux present"})`, () => {
  let harness: HostHarness;
  let connection: HostConnection;
  const store = createSessionStore();
  let epoch = 0;

  beforeAll(async () => {
    harness = await startHostHarness();
    harness.tmux(["kill-server"]);
    connection = new HostConnection({ dial: async () => harness.transport, nextConnectionEpoch: () => (epoch += 1), store });
  });

  afterAll(async () => {
    connection?.disconnect();
    await harness?.stop();
  });

  it("starts the server with random and still owns the pane it opened", async () => {
    connection.connect();
    await waitFor("connected", () => (store.getState().connection.state === "connected" ? true : undefined));
    expect(connection.serverIdentity).toBe(NO_TMUX_SERVER);

    const opened = await openInRandom(connection, store, { kind: "terminal", directory: harness.workDir, getCurrentConnection: () => connection });
    expect(opened.createdWorkspace).toBe(true);
    await waitFor("the new pane in the topology", () => (store.getState().panes[opened.paneId] ? true : undefined));
    expect(connection.serverIdentity).not.toBe(NO_TMUX_SERVER);
    // What the bar checks before it opens the terminal.
    expect(isConnectionScopeCurrent(opened.scope, () => connection)).toBe(true);
  }, 60_000);
});
