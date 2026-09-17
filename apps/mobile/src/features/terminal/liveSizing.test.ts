// D6 against the real host: two bridge connections on one session, a "laptop"
// that only selects and resizes, and a phone `TerminalController`. The side
// that is used takes the window; a silent side loses it and never drifts back.
// Skipped when cargo or tmux is absent.

import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { liveHostAvailability, startHostHarness, type HostHarness } from "../../../scripts/host-harness";
import { HostConnection } from "../../protocol/HostConnection";
import { resizeTerminal, selectTerminalSession } from "../../protocol/requests";
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
    const foreground: AppForeground = {
      inForeground: () => inForeground,
      onForeground: (listener) => {
        foregroundListeners.add(listener);
        return () => foregroundListeners.delete(listener);
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

    // The phone goes into a pocket with the screen mounted and its link
    // flaps: the reconnect must not touch the window (§7.6 step 5) — not
    // even select, which alone would size it from tmux's 80x24 default.
    inForeground = false;
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
});
