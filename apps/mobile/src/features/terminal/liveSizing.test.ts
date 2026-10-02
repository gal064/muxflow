// D6 against the real host: two bridge connections on one session, a "laptop"
// that only selects and resizes, and a phone `TerminalController`. The side
// that is used takes the window; a silent side loses it and never drifts back.
// Skipped when cargo or tmux is absent.

import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { liveHostAvailability, startHostHarness, type HostHarness } from "../../../scripts/host-harness";
import { HostConnection } from "../../protocol/HostConnection";
import { attachTerminal, resizeTerminal, resizeTerminalWindow, selectTerminalSession } from "../../protocol/requests";
import { createSessionStore, type SessionStore } from "../../store/sessionStore";
import type { ToPageMessage } from "./bridgeMessages";
import { utf8Encode } from "./bytes";
import { windowGrid } from "./sizing";
import { TAKE_INTERVAL_MS, TerminalController, type AppForeground } from "./TerminalController";
import { TerminalRegistry } from "./terminalRegistry";

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

describe.skipIf(!availability.available)(`live sizing (${availability.reason ?? "cargo + tmux present"})`, () => {
  let harness: HostHarness;
  const log: string[] = [];
  const say = (line: string) => console.log(`live: ${line}`);
  let epoch = 0;
  const side = (name: string, transport: () => ReturnType<HostHarness["dial"]>) => {
    const store: SessionStore = createSessionStore();
    const registry = new TerminalRegistry();
    const connection = new HostConnection({
      dial: async () => transport(),
      nextConnectionEpoch: () => (epoch += 1),
      store,
      terminals: registry,
      onConnected: () => registry.onConnected(),
      log: (line) => log.push(`${name} ${line}`),
    });
    return { store, registry, connection };
  };
  let laptop: ReturnType<typeof side>;
  let phone: ReturnType<typeof side>;

  beforeAll(async () => {
    harness = await startHostHarness();
    laptop = side("laptop", () => harness.transport);
    phone = side("phone", () => harness.dial());
  });

  afterAll(async () => {
    phone?.connection.disconnect();
    laptop?.connection.disconnect();
    await harness?.stop();
  });

  it("the side in use takes the window; a silent side loses it and never drifts back", async () => {
    laptop.connection.connect();
    phone.connection.connect();
    await waitFor("laptop connected", () => (laptop.store.getState().connection.state === "connected" ? true : undefined));
    await waitFor("phone connected", () => (phone.store.getState().connection.state === "connected" ? true : undefined));
    const sessionId = harness.primarySessionId;
    const paneId = await waitFor("primary pane", () => Object.values(phone.store.getState().panes).find((p) => p.sessionId === sessionId)?.id);
    const windowSize = () => harness.tmux(["display-message", "-p", "-t", paneId, "#{window_width}x#{window_height}"]);
    const windowIs = (size: string) => () => (windowSize() === size ? true : undefined);
    const laptopTakes = async () => {
      await laptop.connection.request(resizeTerminal(160, 48));
    };

    // The laptop states its size first.
    await laptop.connection.request(selectTerminalSession(sessionId));
    await laptopTakes();
    await waitFor("window at the laptop's size", windowIs("160x48"));
    say(`laptop select + resize 160x48 → window ${windowSize()}`);

    // The phone opens the pane: select → resize → attach, and the window follows.
    const page: ToPageMessage[] = [];
    let inForeground = true;
    const foregroundListeners = new Set<() => void>();
    const backgroundListeners = new Set<() => void>();
    const foreground: AppForeground = {
      inForeground: () => inForeground,
      onForeground: (listener) => {
        foregroundListeners.add(listener);
        return () => foregroundListeners.delete(listener);
      },
      onBackground: (listener) => {
        backgroundListeners.add(listener);
        return () => backgroundListeners.delete(listener);
      },
    };
    const controller = new TerminalController({
      paneId,
      sessionId,
      store: phone.store,
      registry: phone.registry,
      getConnection: () => phone.connection,
      page: { send: (m) => page.push(m) },
      log: (line) => log.push(line),
      foreground,
    });
    controller.start();
    controller.onPageMessage({ t: "size", cols: 50, rows: 30 });
    await waitFor("seed", () => page.find((m) => m.t === "seed"));
    await waitFor("window at the phone's size", windowIs("50x30"));
    say(`phone attach at 50x30 → window ${windowSize()}`);

    // The laptop is used again: it takes, and the phone — attached, silent —
    // loses. The host's `refresh-client -C` + `switch-client -E` is the take.
    await laptopTakes();
    await waitFor("window back at the laptop's size", windowIs("160x48"), 5_000);
    say(`laptop resize 160x48 again → window ${windowSize()} (phone still attached)`);
    // Long enough for the phone's take interval since its attach to pass, and
    // to show the window stays where the laptop put it.
    await new Promise((resolve) => setTimeout(resolve, TAKE_INTERVAL_MS + 200));
    expect(windowSize()).toBe("160x48");
    expect(phone.store.getState().connection.state).toBe("connected");

    // The phone is used: its input, seeing the window at the laptop's size in
    // the topology, takes it back ahead of the keystroke.
    await waitFor("phone topology shows the laptop's size", () => {
      const pane = phone.store.getState().panes[paneId];
      const size = pane && windowGrid(Object.values(phone.store.getState().panes), pane.windowId);
      return size?.cols === 160 && size.rows === 48 ? true : undefined;
    });
    await controller.sendInput(utf8Encode("x"));
    expect(log.some((line) => line.includes("take: window is 160x48"))).toBe(true);
    await waitFor("window at the phone's size after input", windowIs("50x30"), 5_000);
    say(`phone input → window ${windowSize()}`);

    // And the laptop's next use takes it once more.
    await laptopTakes();
    await waitFor("window at the laptop's size once more", windowIs("160x48"), 5_000);
    say(`laptop resize 160x48 → window ${windowSize()}`);

    // The phone goes into a pocket while its SSH connection and terminal
    // remain attached. Its sizing client yields, so a laptop resize can hold.
    inForeground = false;
    for (const listener of backgroundListeners) listener();
    await waitFor("phone sizing yield", () => log.some((line) => line.includes("sizing.yield.ok")) ? true : undefined);
    expect(phone.store.getState().connection.state).toBe("connected");
    await laptopTakes();
    await waitFor("laptop size with phone connected", windowIs("160x48"), 5_000);
    // Reproduce the reported tab switch: the desktop stops sizing the first
    // session while the still-connected phone is in the background.
    const [otherSessionId, otherPaneId] = harness.tmux([
      "new-session", "-d", "-P", "-F", "#{session_id} #{pane_id}",
      "-s", "other", "-c", harness.workDir, "exec bash --norc --noprofile",
    ]).split(" ");
    await waitFor("other pane in topology", () => laptop.store.getState().panes[otherPaneId!]?.id);
    await laptop.connection.request(attachTerminal(otherSessionId!, otherPaneId!));
    await new Promise((resolve) => setTimeout(resolve, 500));
    expect(windowSize()).toBe("160x48");
    await laptop.connection.request(selectTerminalSession(sessionId));
    await laptopTakes();
    await waitFor("desktop return keeps full width", windowIs("160x48"), 5_000);
    // A background reconnect must also leave the laptop's size alone — not
    // even select, which would rearm the unsized client at 80x24.
    const attachedBefore = log.filter((line) => line.includes(" attached ")).length;
    phone.connection.disconnect();
    phone.connection.connect();
    await waitFor("phone reconnected", () => (phone.store.getState().connection.state === "connected" ? true : undefined));
    await new Promise((resolve) => setTimeout(resolve, 1_500));
    expect(windowSize()).toBe("160x48");
    expect(log.filter((line) => line.includes(" attached ")).length).toBe(attachedBefore);
    say(`phone reconnect in the background → window ${windowSize()}, no attach`);
    // Out of the pocket: step 1 runs and the phone takes.
    inForeground = true;
    for (const listener of foregroundListeners) listener();
    await waitFor("window at the phone's size after the foreground return", windowIs("50x30"), 5_000);
    say(`phone to the foreground → window ${windowSize()}`);

    await controller.stop();
  }, 90_000);

  it("claims each named window across four shared windows without moving the selected window", async () => {
    const sessionId = harness.primarySessionId;
    const first = Object.values(phone.store.getState().windows).find((window) => window.sessionId === sessionId)!;
    const windows = [first.id];
    for (let i = 0; i < 3; i += 1) {
      windows.push(harness.tmux(["new-window", "-d", "-P", "-F", "#{window_id}", "-t", sessionId, "sleep 120"]));
    }
    await waitFor("all four windows", () => windows.every((id) => phone.store.getState().windows[id]) ? true : undefined);
    harness.tmux(["select-window", "-t", `${sessionId}:${windows[3]}`]);
    const selected = () => harness.tmux(["display-message", "-p", "-t", `${sessionId}:`, "#{window_id}"]);
    const size = (id: string) => harness.tmux(["display-message", "-p", "-t", id, "#{window_width}x#{window_height}"]);
    for (const id of windows) await laptop.connection.request(resizeTerminalWindow(sessionId, id, 160, 48));
    for (const id of windows) expect(size(id)).toBe("160x48");

    // Open an idle, nonselected window. No input or prompt causes the resize.
    const paneId = harness.tmux(["display-message", "-p", "-t", windows[0]!, "#{pane_id}"]);
    const page: ToPageMessage[] = [];
    const controller = new TerminalController({ paneId, sessionId, store: phone.store, registry: phone.registry,
      getConnection: () => phone.connection, page: { send: (message) => page.push(message) } });
    try {
      controller.start();
      controller.onPageMessage({ t: "size", cols: 50, rows: 30 });
      const seed = await waitFor("seed for nonselected window", () => page.find((message) => message.t === "seed"));
      expect(seed).toMatchObject({ grid: { cols: 50, rows: 30 } });
      expect(size(windows[0]!)).toBe("50x30");
      expect(selected()).toBe(windows[3]);
      expect(size(windows[1]!)).toBe("160x48");
      await new Promise((resolve) => setTimeout(resolve, 500));
      expect(size(windows[0]!)).toBe("50x30");

      // The same requested dimensions still claim a different window.
      for (const id of windows.slice(1)) {
        await phone.connection.request(resizeTerminalWindow(sessionId, id, 50, 30));
        expect(size(id)).toBe("50x30");
        expect(selected()).toBe(windows[3]);
        await laptop.connection.request(resizeTerminalWindow(sessionId, id, 160, 48));
        expect(size(id)).toBe("160x48");
        expect(selected()).toBe(windows[3]);
      }
      await laptop.connection.request(resizeTerminalWindow(sessionId, windows[0]!, 160, 48));
      expect(size(windows[0]!)).toBe("160x48");
      controller.onPageMessage({ t: "size", cols: 42, rows: 18 });
      await waitFor("changed mobile viewport", () => size(windows[0]!) === "42x18" ? true : undefined);
      expect(selected()).toBe(windows[3]);

      await expect(phone.connection.request(resizeTerminalWindow("$99999", windows[0]!, 80, 24)))
        .rejects.toThrow("window is no longer in the requested session");
      expect(size(windows[0]!)).toBe("42x18");
      expect(size(windows[1]!)).toBe("160x48");
      expect(size(windows[3]!)).toBe("160x48");
      const closed = windows[2]!;
      harness.tmux(["kill-window", "-t", closed]);
      await waitFor("closed window disappears", () => !phone.store.getState().windows[closed] ? true : undefined);
      await expect(phone.connection.request(resizeTerminalWindow(sessionId, closed, 80, 24)))
        .rejects.toThrow("window is no longer in the requested session");
      expect(selected()).toBe(windows[3]);
      expect(size(windows[0]!)).toBe("42x18");
    } finally {
      await controller.stop();
    }
  }, 90_000);

});
