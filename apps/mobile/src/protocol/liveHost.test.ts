// End-to-end against the real `muxflow-host bridge --stdio` (design doc §15,
// and the §17 Q1 answer recorded in §7.6). Skipped when cargo or tmux is
// missing. Every observation is logged with a `live:` prefix so the run's
// output documents what the host actually did.

import { writeFileSync } from "node:fs";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { liveHostAvailability, startHostHarness, type HostHarness } from "../../scripts/host-harness";
import { FileKind, type FileStreamFrame } from "./gen/envelope_pb";
import { HostConnection, HostError } from "./HostConnection";
import {
  attachTerminal,
  createWindow,
  listDirectory,
  newOperationId,
  openFileStream,
  requestTerminalSeed,
  resizeTerminal,
  resolveActiveRoot,
  selectTerminalSession,
  setTerminalVisibility,
  terminalInput,
} from "./requests";
import { createSessionStore } from "../store/sessionStore";

const availability = liveHostAvailability();
const decoder = new TextDecoder();

interface Delivered { kind: "seed" | "output"; paneId: string; text: string; generation: bigint }

async function waitFor<T>(label: string, probe: () => T | undefined, timeoutMs = 15_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = probe();
    if (value !== undefined) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

describe.skipIf(!availability.available)(`live host (${availability.reason ?? "cargo + tmux present"})`, () => {
  let harness: HostHarness;
  const store = createSessionStore();
  const delivered: Delivered[] = [];
  const exits: string[] = [];
  const log: string[] = [];
  let epoch = 0;
  let connection: HostConnection;
  const say = (line: string) => {
    log.push(line);
    console.log(`live: ${line}`);
  };

  beforeAll(async () => {
    harness = await startHostHarness();
    connection = new HostConnection({
      dial: async () => harness.transport,
      appVersion: "0.1.0-live-test",
      nextConnectionEpoch: () => (epoch += 1),
      store,
      terminals: {
        seed: (paneId, bytes, generation) => delivered.push({ kind: "seed", paneId, text: decoder.decode(bytes), generation }),
        output: (paneId, bytes, generation) => delivered.push({ kind: "output", paneId, text: decoder.decode(bytes), generation }),
        exit: (paneId, detail) => exits.push(`${paneId}: ${detail}`),
      },
      log: (line) => log.push(line),
    });
  });

  afterAll(async () => {
    connection?.disconnect();
    await harness?.stop();
  });

  it("handshakes, drives a terminal, streams a file, and detaches cleanly", async () => {
    connection.connect();
    await waitFor("connected", () => (store.getState().connection.state === "connected" ? true : undefined));
    const hello = connection.serverHello!;
    say(`ServerHello helper=${hello.helperVersion} build=${hello.helperBuildDigest.slice(0, 12)} tmux="${hello.tmuxVersion}" identity=${hello.serverIdentity} window=${hello.terminalOutputWindowBytes}B/${hello.terminalOutputWindowRecords}rec readOnly=${hello.readOnly}`);
    expect(hello.readOnly).toBe(false);
    expect(hello.terminalOutputWindowBytes).toBeGreaterThan(0n);
    expect(hello.connectionEpoch).toBe(1n);

    const sessionId = harness.primarySessionId;
    const snapshot = store.getState();
    say(`snapshot generation=${snapshot.topologyGeneration} sessions=${Object.values(snapshot.sessions).map((s) => `${s.id}:${s.name}`).join(",")} panes=${Object.keys(snapshot.panes).join(",")}`);
    expect(snapshot.sessions[sessionId]?.name).toBe("primary");

    // §7.6 step 1 as written (resize first, on a fresh connection): the host
    // refuses because no session is visible yet.
    const resizeFirst = await connection.request(resizeTerminal(40, 20)).then(() => "ok", (error: unknown) => (error instanceof HostError ? `${error.code}: ${error.message}` : String(error)));
    say(`RESIZE_TERMINAL on a fresh connection (before SELECT_TERMINAL_SESSION / any action) → ${resizeFirst}`);
    expect(resizeFirst).toContain("no visible session control client");

    // TMUX_ACTION CREATE_WINDOW (which also selects the session for sizing).
    // `expectedGeneration` is read at call time; a `stale_topology` refusal
    // arrives with a fresh TOPOLOGY_SNAPSHOT, so one retry is enough.
    const create = () => connection.request(createWindow(sessionId, hello.serverIdentity, store.getState().topologyGeneration));
    const created = await create().catch((error: unknown) => {
      if (error instanceof HostError && error.code === "stale_topology") {
        say(`CREATE_WINDOW refused once: ${error.message}; retrying with generation=${store.getState().topologyGeneration}`);
        return create();
      }
      throw error;
    });
    const paneId = created.tmuxActionResult!.paneId;
    const windowId = created.tmuxActionResult!.windowId;
    say(`CREATE_WINDOW → window=${windowId} pane=${paneId} generation=${created.tmuxActionResult!.topologyGeneration}`);
    expect(paneId).toMatch(/^%\d+$/);
    await waitFor("new pane in topology", () => store.getState().panes[paneId]);

    // Corrected order: select the session, then size it, then attach.
    // (CREATE_WINDOW already selected it; SELECT is idempotent.)
    await connection.request(selectTerminalSession(sessionId));
    say("SELECT_TERMINAL_SESSION → ok");
    await connection.request(resizeTerminal(40, 20));
    say("RESIZE_TERMINAL 40x20 → ok");
    store.getState().setFocusedPane(paneId);
    const seedsBeforeAttach = delivered.filter((d) => d.kind === "seed" && d.paneId === paneId).length;
    await connection.request(attachTerminal(sessionId, paneId));
    say("ATTACH_TERMINAL → ok");
    // The attach mounts the pane but photographs nothing when the session's
    // control client already exists (CREATE_WINDOW selected it): the seed is
    // asked for explicitly, as the controller does (§7.6 step 1).
    await connection.request(requestTerminalSeed(paneId));
    say("REQUEST_TERMINAL_SEED after attach → ok");
    const seed = await waitFor("TERMINAL_SEED after attach + seed request", () => delivered.filter((d) => d.kind === "seed" && d.paneId === paneId)[seedsBeforeAttach]);
    say(`TERMINAL_SEED: ${seed.text.length} bytes generation=${seed.generation} (screen-only)`);
    expect(seed.text.length).toBeGreaterThan(0);
    const size = harness.tmux(["display-message", "-p", "-t", paneId, "#{pane_width}x#{pane_height}"]);
    say(`pane size after RESIZE_TERMINAL 40x20: ${size}`);
    expect(size).toBe("40x20");

    // SET_TERMINAL_VISIBILITY(true) after an attach that already made the pane
    // visible: accepted, but it is not what produced the seed.
    const seedsBeforeVisible = delivered.filter((d) => d.kind === "seed" && d.paneId === paneId).length;
    await connection.request(setTerminalVisibility(paneId, true, { terminalEpoch: connection.connectionEpoch, generationCutoff: 0n }));
    await new Promise((resolve) => setTimeout(resolve, 500));
    const extraSeeds = delivered.filter((d) => d.kind === "seed" && d.paneId === paneId).length - seedsBeforeVisible;
    say(`SET_TERMINAL_VISIBILITY(true) after attach → ok; additional seeds within 500 ms: ${extraSeeds}`);

    // Input round trip; the marker never appears in the echoed command line.
    const before = delivered.length;
    await connection.request(terminalInput(paneId, new TextEncoder().encode("echo mobile-$((40+2))\n")));
    await waitFor("`mobile-42` in output", () => {
      const text = delivered.slice(before).filter((d) => d.paneId === paneId).map((d) => d.text).join("");
      return text.includes("mobile-42") ? true : undefined;
    });
    say(`TERMINAL_INPUT echo → output contained "mobile-42" (${delivered.slice(before).filter((d) => d.kind === "output").length} TERMINAL_OUTPUT events)`);

    // Acks were sent for every charged event; the host closes the connection
    // on an invalid one, so a request answered afterwards proves acceptance.
    await new Promise((resolve) => setTimeout(resolve, 100));
    const acked = log.filter((line) => line.includes("reconnect")).length;
    expect(acked).toBe(0);
    await connection.request(requestTerminalSeed(paneId));
    say("acks accepted: host still answers after acknowledging delivered output (REQUEST_TERMINAL_SEED → ok)");
    expect(store.getState().connection.state).toBe("connected");

    // Files: active root, listing, stream.
    writeFileSync(path.join(harness.workDir, "hello.md"), "# hello\n\nfrom the phone\n");
    const rootResponse = await connection.request(resolveActiveRoot(newOperationId(), paneId, hello.serverIdentity));
    const activeRoot = rootResponse.file!.activeRoot!;
    say(`RESOLVE_ACTIVE_ROOT → root=${activeRoot.root} gitWorktree=${activeRoot.gitWorktree} token=${activeRoot.rootToken.slice(0, 8)}…`);
    expect(activeRoot.root).toBe(harness.workDir);
    const listing = await connection.request(listDirectory(newOperationId(), { root: activeRoot.root, rootToken: activeRoot.rootToken, path: activeRoot.root }, hello.serverIdentity));
    const entries = listing.file!.directory!.entries;
    say(`LIST_DIRECTORY → ${entries.map((e) => `${e.name}(${FileKind[e.kind]},${e.size}B)`).join(", ")} complete=${listing.file!.directory!.complete}`);
    const hello_md = entries.find((e) => e.name === "hello.md")!;
    expect(hello_md).toBeDefined();
    // File bodies are refused on the control lane; the host serves them only
    // on a bulk connection bound to this one.
    const target = { root: activeRoot.root, rootToken: activeRoot.rootToken, path: hello_md.path };
    const onControl = await connection.request(openFileStream(newOperationId(), target, hello.serverIdentity), { onFileStream: () => {} })
      .then(() => "ok", (error: unknown) => (error instanceof HostError ? `${error.code}: ${error.message}` : String(error)));
    say(`OPEN_FILE_STREAM on the control connection → ${onControl}`);
    expect(onControl).toBe("bulk_connection_required: file bodies are allowed only on an independent bulk connection");
    const bulkStore = createSessionStore();
    const bulk = new HostConnection({
      dial: async () => harness.dial(),
      appVersion: "0.1.0-live-test",
      nextConnectionEpoch: () => { throw new Error("bulk lanes reuse the control epoch"); },
      store: bulkStore,
      bulk: { expectedServerIdentity: hello.serverIdentity, connectionEpoch: connection.connectionEpoch },
      log: (line) => log.push(`bulk ${line}`),
    });
    bulk.connect();
    await waitFor("bulk connected", () => (bulkStore.getState().connection.state === "connected" ? true : undefined));
    say(`bulk ClientHello{bulkConnection=true, expectedServerIdentity, connectionEpoch=${connection.connectionEpoch}} → ServerHello readOnly=${bulk.serverHello!.readOnly} epoch=${bulk.serverHello!.connectionEpoch} window=${bulk.serverHello!.terminalOutputWindowBytes}B`);
    const frames: FileStreamFrame[] = [];
    const streamed = await bulk.request(openFileStream(newOperationId(), target, hello.serverIdentity), { onFileStream: (frame) => frames.push(frame) });
    const header = frames[0]!.header!;
    const body = frames.slice(1).map((f) => decoder.decode(f.data)).join("");
    say(`OPEN_FILE_STREAM on the bulk connection → header kind=${header.contentKind} total=${header.totalBytes} streaming=${header.contentStreaming}; ${frames.length - 1} body frame(s), eof=${frames.at(-1)!.eof}, blake3=${frames.at(-1)!.blake3.slice(0, 8)}…; response ok=${streamed.ok} content.kind=${streamed.file?.content?.kind}`);
    expect(body).toBe("# hello\n\nfrom the phone\n");
    expect(frames.at(-1)!.eof).toBe(true);
    bulk.disconnect();
    say("bulk disconnect → idle");

    // §17 Q1: hide with empty data. Requires a non-zero terminalEpoch.
    const zeroEpoch = await connection.request(setTerminalVisibility(paneId, false, { terminalEpoch: 0n, generationCutoff: 0n }))
      .then(() => "ok", (error: unknown) => (error instanceof HostError ? `${error.code}: ${error.message}` : String(error)));
    say(`SET_TERMINAL_VISIBILITY(false, data=empty, terminalEpoch=0) → ${zeroEpoch}`);
    expect(zeroEpoch).toContain("epoch must be non-zero");
    // Unmount order (§7.6 step 4): drop focus first, then hide. A PANE_RESOURCE
    // with requiresSeed for a still-focused pane would trigger
    // REQUEST_TERMINAL_SEED, which the host treats as a reveal.
    store.getState().setFocusedPane(undefined);
    await connection.request(setTerminalVisibility(paneId, false, { terminalEpoch: connection.connectionEpoch, generationCutoff: 0n }));
    say(`SET_TERMINAL_VISIBILITY(false, data=empty, terminalEpoch=${connection.connectionEpoch}) → ok`);
    // A hide answers with no bytes and no PANE_RESOURCE at all (host
    // `set_visibility`: the store keeps the tail, the *reveal* hands it back).
    // The phone never held a renderer snapshot, so its reveal is answered
    // with a seed.
    await new Promise((resolve) => setTimeout(resolve, 300));
    const hideEvents = log.filter((l) => l.includes("pane.resource"));
    say(`PANE_RESOURCE events after hide: ${hideEvents.length} (none expected: a hide carries nothing)`);
    // Output produced while hidden is not delivered.
    const hiddenBefore = delivered.length;
    await connection.request(terminalInput(paneId, new TextEncoder().encode("echo hidden-$((40+3))\n")));
    await new Promise((resolve) => setTimeout(resolve, 700));
    const leaked = delivered.slice(hiddenBefore).filter((d) => d.paneId === paneId && d.text.includes("hidden-43"));
    say(`output while hidden delivered to the client: ${leaked.length} event(s)`);
    expect(leaked).toHaveLength(0);
    // Reveal: the host reseeds (the hide released the pane), and the seed has the hidden output.
    const seedsBeforeReveal = delivered.filter((d) => d.kind === "seed" && d.paneId === paneId).length;
    store.getState().setFocusedPane(paneId);
    await connection.request(setTerminalVisibility(paneId, true, { terminalEpoch: connection.connectionEpoch, generationCutoff: 0n }));
    const reseed = await waitFor("TERMINAL_SEED after reveal", () => delivered.filter((d) => d.kind === "seed" && d.paneId === paneId)[seedsBeforeReveal]);
    say(`SET_TERMINAL_VISIBILITY(true) after hide → TERMINAL_SEED ${reseed.text.length} bytes generation=${reseed.generation}, contains hidden output: ${reseed.text.includes("hidden-43")}`);
    expect(reseed.text).toContain("hidden-43");

    // Detach cleanly: stdin closes, the bridge exits on its own.
    say(`delivered to the terminal sink: ${delivered.filter((d) => d.kind === "seed").length} seeds, ${delivered.filter((d) => d.kind === "output").length} output events; ${log.filter((l) => l.includes("seed.diagnostic")).length} seed diagnostics; helper stderr lines: ${harness.stderr.length}`);
    connection.disconnect();
    expect(store.getState().connection.state).toBe("idle");
    expect(exits).toEqual([]);
    await harness.stop();
    say("disconnect → idle; bridge stdin closed and the process exited");
  }, 60_000);
});
