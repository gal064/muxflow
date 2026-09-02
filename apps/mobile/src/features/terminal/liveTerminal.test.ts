// The terminal feature's logic (no React) against the real `muxflow-host
// bridge --stdio`: create a window, attach, resize, input, hide, reveal.
// Skipped when cargo or tmux is absent. `live:` lines document what the host did.

import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { liveHostAvailability, startHostHarness, type HostHarness } from "../../../scripts/host-harness";
import { HostConnection } from "../../protocol/HostConnection";
import { createSessionStore } from "../../store/sessionStore";
import type { ToPageMessage } from "./bridgeMessages";
import { createTerminalWindow } from "./createWindow";
import { KEY_CHIPS } from "./chips";
import { utf8Encode } from "./bytes";
import { TerminalController } from "./TerminalController";
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

const pageText = (messages: ToPageMessage[], from = 0) =>
  messages.slice(from).filter((m): m is Extract<ToPageMessage, { t: "seed" | "out" }> => m.t === "seed" || m.t === "out")
    .map((m) => Buffer.from(m.b64, "base64").toString("utf8")).join("");

describe.skipIf(!availability.available)(`live terminal (${availability.reason ?? "cargo + tmux present"})`, () => {
  let harness: HostHarness;
  const store = createSessionStore();
  const registry = new TerminalRegistry();
  const log: string[] = [];
  const say = (line: string) => console.log(`live: ${line}`);
  let epoch = 0;
  let connection: HostConnection;

  beforeAll(async () => {
    harness = await startHostHarness();
    connection = new HostConnection({
      dial: async () => harness.transport,
      appVersion: "0.1.0-live-terminal",
      nextConnectionEpoch: () => (epoch += 1),
      store,
      terminals: registry,
      onConnected: () => registry.onConnected(),
      log: (line) => log.push(line),
    });
  });

  afterAll(async () => {
    connection?.disconnect();
    await harness?.stop();
  });

  it("creates a window, attaches at the page's size, echoes input, hides, and reveals", async () => {
    connection.connect();
    await waitFor("connected", () => (store.getState().connection.state === "connected" ? true : undefined));
    const sessionId = harness.primarySessionId;

    const created = await createTerminalWindow(connection, store, sessionId);
    say(`New terminal → window=${created.windowId} pane=${created.paneId}`);
    await waitFor("new pane in topology", () => store.getState().panes[created.paneId]);

    const page: ToPageMessage[] = [];
    const controller = new TerminalController({
      paneId: created.paneId,
      sessionId,
      store,
      registry,
      getConnection: () => connection,
      page: { send: (m) => page.push(m) },
      log: (line) => log.push(line),
    });
    controller.start();
    expect(page).toEqual([{ t: "init" }]);
    controller.onPageMessage({ t: "ready" });
    controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await waitFor("seed", () => page.find((m) => m.t === "seed"));
    say(`attach at 46x40 → seed ${pageText(page).length} chars; pane is ${harness.tmux(["display-message", "-p", "-t", created.paneId, "#{pane_width}x#{pane_height}"])}`);
    expect(harness.tmux(["display-message", "-p", "-t", created.paneId, "#{pane_width}x#{pane_height}"])).toBe("46x40");
    expect(controller.snapshot.phase).toBe("seeded");

    // Keyboard shows: fewer rows; debounced resize.
    controller.onPageMessage({ t: "size", cols: 46, rows: 18 });
    await waitFor("resize applied", () => (harness.tmux(["display-message", "-p", "-t", created.paneId, "#{pane_height}"]) === "18" ? true : undefined));
    say("size 46x18 (keyboard) → RESIZE_TERMINAL → pane_height=18");

    // Input bar: text + CR, then a chip.
    const before = page.length;
    const text = utf8Encode("echo phone-$((40+3))");
    await controller.sendInput(Uint8Array.from([...text, 0x0d]));
    await waitFor("`phone-43` echoed", () => (pageText(page, before).includes("phone-43") ? true : undefined));
    say("input bar `echo phone-$((40+3))` + Enter → output contained phone-43");
    const ctrlC = KEY_CHIPS.find((c) => c.label === "Ctrl-C")!;
    await controller.sendInput(ctrlC.bytes);
    await waitFor("^C prompt", () => (pageText(page, before).includes("^C") ? true : undefined), 5_000).catch(() => undefined);
    say(`Ctrl-C chip → ok (bash printed ^C: ${pageText(page, before).includes("^C")})`);

    // Hide (blur): focus cleared first, then SET_TERMINAL_VISIBILITY(false).
    const generation = controller.generation;
    await controller.stop();
    expect(store.getState().focusedPaneId).toBeUndefined();
    // A hide is answered with nothing — no bytes, no PANE_RESOURCE; the
    // reveal is what hands a hidden pane's output back (as a seed here).
    expect(log.some((l) => l.includes("hide → ok"))).toBe(true);
    say(`hide (epoch=${connection.connectionEpoch}, cutoff=${generation}) → ok; PANE_RESOURCE events: ${log.filter((l) => l.includes("pane.resource")).length}`);
    expect(log.some((l) => l.includes("seed.request.failed") || l.includes("REQUEST_TERMINAL_SEED"))).toBe(false);
    harness.tmux(["send-keys", "-t", created.paneId, "echo hidden-$((40+4))", "Enter"]);
    await new Promise((resolve) => setTimeout(resolve, 400));
    expect(pageText(page).includes("hidden-44")).toBe(false);
    say("output while hidden was not delivered to the page");

    // Reveal (focus again): a new controller, full step 1, reseed carries the hidden output.
    const page2: ToPageMessage[] = [];
    const again = new TerminalController({
      paneId: created.paneId,
      sessionId,
      store,
      registry,
      getConnection: () => connection,
      page: { send: (m) => page2.push(m) },
      log: (line) => log.push(line),
    });
    again.start();
    again.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await waitFor("reseed", () => page2.find((m) => m.t === "seed"));
    expect(pageText(page2)).toContain("hidden-44");
    say(`reveal → reseed ${pageText(page2).length} chars, contains hidden-44`);
    await again.stop();
    expect(store.getState().connection.state).toBe("connected");
  }, 60_000);
});
