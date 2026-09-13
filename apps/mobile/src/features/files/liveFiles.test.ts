// The Files feature against the real `muxflow-host bridge --stdio` (design doc
// §15). Skipped when cargo or tmux is missing. Every observation is logged with
// a `live:` prefix so the run's output is the evidence behind §9.6, §9.7 and
// §11.
//
// Two bridges are dialled on purpose: file bodies are served only on a bulk
// connection (§11.1), and the control lane refuses them.

import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { liveHostAvailability, startHostHarness, type HostHarness } from "../../../scripts/host-harness";
import { HostConnection, HostError } from "../../protocol/HostConnection";
import { newOperationId, openFileStream } from "../../protocol/requests";
import { createSessionStore } from "../../store/sessionStore";
import { resolveRoot, rooted } from "./activeRoot";
import { readFile } from "./fileStream";
import { fetchDirectory } from "./listing";
import { filePresentation } from "./presentation";

const availability = liveHostAvailability();
const decoder = new TextDecoder();

/** Two pages at the phone's page size of 500 (§7.5). */
const CROWDED_ENTRIES = 620;

const README = [
  "# Muxflow plan",
  "",
  "A paragraph with a [link](https://example.com/docs) and `inline code`.",
  "",
  "## Steps",
  "",
  "- [x] read the design doc",
  "- [ ] ship M5",
  "",
  "```rust",
  "fn main() { println!(\"hi\"); }",
  "```",
  "",
  "| field | value |",
  "| --- | --- |",
  "| root | active |",
  "",
].join("\n");

async function waitFor<T>(label: string, probe: () => T | undefined, timeoutMs = 15_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = probe();
    if (value !== undefined) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

describe.skipIf(!availability.available)(`live files (${availability.reason ?? "cargo + tmux present"})`, () => {
  let harness: HostHarness;
  const store = createSessionStore();
  const bulkStore = createSessionStore();
  let control: HostConnection;
  let bulk: HostConnection;
  let epoch = 0;
  const say = (line: string) => console.log(`live: ${line}`);

  beforeAll(async () => {
    harness = await startHostHarness();
    control = new HostConnection({
      dial: async () => harness.transport,
      nextConnectionEpoch: () => (epoch += 1),
      store,
    });
    // `startHostHarness` may have to `cargo build -p muxflow-host` first.
  }, 600_000);

  afterAll(async () => {
    bulk?.disconnect();
    control?.disconnect();
    await harness?.stop();
  }, 60_000);

  it("resolves a root, pages a crowded directory, and streams files on the bulk lane", async () => {
    // A scratch tree: a README, a nested directory, a directory with more
    // entries than one page, hidden names, a file past the phone's cap, and a
    // file that is not text.
    writeFileSync(path.join(harness.workDir, "README.md"), README);
    mkdirSync(path.join(harness.workDir, "docs"));
    writeFileSync(path.join(harness.workDir, "docs", "notes.markdown"), "# notes\n");
    mkdirSync(path.join(harness.workDir, ".git"));
    writeFileSync(path.join(harness.workDir, ".git", "HEAD"), "ref: refs/heads/main\n");
    mkdirSync(path.join(harness.workDir, "node_modules"));
    writeFileSync(path.join(harness.workDir, ".gitignore"), "target\n");
    writeFileSync(path.join(harness.workDir, "big.log"), "x".repeat(3 * 1024 * 1024));
    writeFileSync(path.join(harness.workDir, "logo.bin"), Buffer.from([0, 1, 2, 3, 0, 255]));
    // An image the host is willing to preview: it streams a body even though
    // §9.7 only ever shows a placeholder for it.
    writeFileSync(path.join(harness.workDir, "icon.png"), Buffer.from("89504e470d0a1a0a0000000d49484452", "hex"));
    const crowded = path.join(harness.workDir, "many");
    mkdirSync(crowded);
    for (let index = 0; index < CROWDED_ENTRIES; index += 1) {
      writeFileSync(path.join(crowded, `entry-${String(index).padStart(4, "0")}.txt`), `${index}\n`);
    }

    control.connect();
    await waitFor("connected", () => (store.getState().connection.state === "connected" ? true : undefined));
    const identity = control.serverIdentity;
    const paneId = Object.values(store.getState().panes).find((pane) => pane.sessionId === harness.primarySessionId)!.id;
    say(`control connected: identity=${identity} pane=${paneId}`);

    const request = control.request.bind(control);

    // §11.2 — RESOLVE_ACTIVE_ROOT, then the same call with the token we hold.
    const root = await resolveRoot(request, paneId, identity);
    say(`RESOLVE_ACTIVE_ROOT → root=${root.root} gitWorktree=${root.gitWorktree} generation=${root.rootGeneration}`);
    expect(root.root).toBe(harness.workDir);
    const again = await resolveRoot(request, paneId, identity, root);
    say(`RESOLVE_ACTIVE_ROOT with knownRootToken → same root=${again.root === root.root} same token=${again.rootToken === root.rootToken}`);
    expect(again.root).toBe(root.root);

    // §9.6 — the root listing: hidden names gone, directories first, Markdown
    // ahead of the other files.
    const top = await fetchDirectory(request, rooted(root, root.root), identity);
    say(`LIST_DIRECTORY ${root.root} → ${top.pages} page(s), rows: ${top.entries.map((entry) => entry.name).join(", ")}`);
    const names = top.entries.map((entry) => entry.name);
    expect(names).not.toContain(".git");
    expect(names).not.toContain("node_modules");
    expect(names.slice(0, 2)).toEqual(["docs", "many"]);
    expect(names[2]).toBe("README.md");
    expect(names).toContain(".gitignore");

    // §9.6 step 2 — a directory that does not fit in one page.
    const many = top.entries.find((entry) => entry.name === "many")!;
    const paged = await fetchDirectory(request, rooted(root, many.path), identity);
    say(`LIST_DIRECTORY ${many.path} → ${paged.entries.length} entries over ${paged.pages} page(s), complete=${!paged.truncated}`);
    expect(paged.pages).toBeGreaterThan(1);
    expect(paged.entries).toHaveLength(CROWDED_ENTRIES);
    expect(paged.entries[0]!.name).toBe("entry-0000.txt");
    expect(paged.entries.at(-1)!.name).toBe(`entry-${String(CROWDED_ENTRIES - 1).padStart(4, "0")}.txt`);

    // §11.1 — the control lane refuses a body.
    const readme = top.entries.find((entry) => entry.name === "README.md")!;
    const refusal = await control
      .request(openFileStream(newOperationId(), rooted(root, readme.path), identity), { onFileStream: () => {} })
      .then(() => "ok", (error: unknown) => (error instanceof HostError ? `${error.code}: ${error.message}` : String(error)));
    say(`OPEN_FILE_STREAM on the control lane → ${refusal}`);
    expect(refusal).toContain("bulk_connection_required");

    // The bulk lane: a second bridge bound to the control connection.
    bulk = new HostConnection({
      dial: async () => harness.dial(),
      nextConnectionEpoch: () => {
        throw new Error("a bulk lane reuses the control connection's epoch");
      },
      store: bulkStore,
      bulk: { expectedServerIdentity: identity, connectionEpoch: control.connectionEpoch },
    });
    bulk.connect();
    await waitFor("bulk connected", () => (bulkStore.getState().connection.state === "connected" ? true : undefined));
    say(`bulk lane connected: epoch=${bulk.serverHello!.connectionEpoch} window=${bulk.serverHello!.terminalOutputWindowBytes}B`);
    const stream = bulk.request.bind(bulk);

    // §9.7 — the Markdown file, streamed to completion and rendered.
    const body = await readFile(stream, rooted(root, readme.path), identity);
    expect(body.kind).toBe("text");
    const text = decoder.decode((body as { bytes: Uint8Array }).bytes);
    say(`OPEN_FILE_STREAM ${readme.name} → ${text.length} bytes assembled, identical to what was written: ${text === README}`);
    expect(text).toBe(README);
    const presentation = filePresentation(body, readme.name);
    say(`presentation for ${readme.name} → ${presentation.kind}`);
    expect(presentation.kind).toBe("markdown");

    // §11.1 — a file past the phone's 2 MiB cap, and a file that is not text.
    const big = top.entries.find((entry) => entry.name === "big.log")!;
    const bigBody = await readFile(stream, rooted(root, big.path), identity);
    say(`OPEN_FILE_STREAM ${big.name} (${big.size} B) → ${JSON.stringify(filePresentation(bigBody, big.name))}`);
    expect(bigBody.kind).toBe("tooLarge");
    const binary = top.entries.find((entry) => entry.name === "logo.bin")!;
    const binaryBody = await readFile(stream, rooted(root, binary.path), identity);
    say(`OPEN_FILE_STREAM ${binary.name} → ${JSON.stringify(filePresentation(binaryBody, binary.name))}`);
    expect(binaryBody.kind).toBe("binary");

    const image = top.entries.find((entry) => entry.name === "icon.png")!;
    const imageBody = await readFile(stream, rooted(root, image.path), identity);
    say(`OPEN_FILE_STREAM ${image.name} → ${JSON.stringify(filePresentation(imageBody, image.name))}`);
    expect(imageBody.kind).toBe("image");

    // §9.6 — a nested directory reached the way the screen reaches it.
    const docs = top.entries.find((entry) => entry.name === "docs")!;
    const nested = await fetchDirectory(request, rooted(root, docs.path), identity);
    say(`LIST_DIRECTORY ${docs.path} → ${nested.entries.map((entry) => `${entry.name}(markdown=${entry.markdown})`).join(", ")}`);
    expect(nested.entries.map((entry) => entry.name)).toEqual(["notes.markdown"]);

    bulk.disconnect();
    control.disconnect();
    say("both lanes disconnected");
  }, 120_000);
});
