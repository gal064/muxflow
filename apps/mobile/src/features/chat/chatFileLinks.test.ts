import { describe, expect, it, vi } from "vitest";

import { HostError, RequestTimeoutError } from "../../protocol/HostConnection";
import { ChatFileLinks, couldNotTell, MAX_PROBES, PROBE_ATTEMPTS, type ProbeAnswer } from "./chatFileLinks";

function deferred() {
  let settle!: (answer: ProbeAnswer) => void;
  const promise = new Promise<ProbeAnswer>((resolve) => { settle = resolve; });
  return { promise, settle };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("chat file links", () => {
  it("asks about each path once, one at a time, and tells who is drawing it", async () => {
    const answers = [deferred(), deferred()];
    const probe = vi.fn((_: string) => answers[probe.mock.calls.length - 1]!.promise);
    const links = new ChatFileLinks(probe, vi.fn());
    const changed = vi.fn();
    links.subscribe("src/a.rs", changed);
    links.ask("src/a.rs");
    links.ask("src/b.rs");
    links.ask("src/a.rs");
    expect(probe.mock.calls.map(([path]) => path)).toEqual(["src/a.rs"]);
    expect(links.get("src/a.rs")).toBe("pending");
    answers[0]!.settle("exists");
    await flush();
    expect(links.get("src/a.rs")).toBe("exists");
    expect(changed).toHaveBeenCalledTimes(1);
    expect(probe.mock.calls.map(([path]) => path)).toEqual(["src/a.rs", "src/b.rs"]);
    answers[1]!.settle("missing");
    await flush();
    expect(links.get("src/b.rs")).toBe("missing");
  });

  it("asks again on reconnecting what it could not ask offline", async () => {
    let online = false;
    const probe = vi.fn(async () => (online ? "exists" : "offline") as ProbeAnswer);
    const links = new ChatFileLinks(probe, vi.fn());
    links.ask("src/a.rs");
    await flush();
    expect(links.get("src/a.rs")).toBe("pending");
    online = true;
    links.resume();
    await flush();
    expect(links.get("src/a.rs")).toBe("exists");
  });

  it("stops asking after as many paths as a conversation may ask about", () => {
    const probe = vi.fn(() => new Promise<ProbeAnswer>(() => undefined));
    const links = new ChatFileLinks(probe, vi.fn());
    for (let index = 0; index <= MAX_PROBES; index += 1) links.ask(`src/${index}.rs`);
    expect(links.get(`src/${MAX_PROBES - 1}.rs`)).toBe("pending");
    expect(links.get(`src/${MAX_PROBES}.rs`)).toBeUndefined();
  });

  it("asks again when the host could not tell, a few times at most", async () => {
    const probe = vi.fn(async (path: string) => (path === "src/a.rs" && probe.mock.calls.length < 2 ? "retry" : path === "src/b.rs" ? "retry" : "exists") as ProbeAnswer);
    const links = new ChatFileLinks(probe, vi.fn());
    links.ask("src/a.rs");
    links.ask("src/b.rs");
    await flush();
    await flush();
    expect(links.get("src/a.rs")).toBe("exists");
    expect(links.get("src/b.rs")).toBe("missing");
    expect(probe.mock.calls.filter(([path]) => path === "src/b.rs")).toHaveLength(PROBE_ATTEMPTS);
  });

  it("asks nothing while the conversation is not on screen, and lets a tap from before it was left open nothing", async () => {
    const probe = vi.fn(async () => "exists" as ProbeAnswer);
    const opener = vi.fn();
    const links = new ChatFileLinks(probe, opener);
    links.hide();
    links.ask("src/a.rs");
    await flush();
    expect(probe).not.toHaveBeenCalled();
    links.show();
    await flush();
    expect(links.get("src/a.rs")).toBe("exists");

    links.open("src/a.rs");
    const first = opener.mock.calls[0]![1] as () => boolean;
    expect(first()).toBe(true);
    links.open("src/b.rs");
    // A later tap stands instead.
    expect(first()).toBe(false);
    const second = opener.mock.calls[1]![1] as () => boolean;
    links.hide();
    expect(second()).toBe(false);
  });

  it("tells a slow or moved host from a file that is not there", () => {
    expect(couldNotTell(new RequestTimeoutError(1n))).toBe(true);
    expect(couldNotTell(new HostError("bad_request", "stale terminal file path request: pane changed during resolution"))).toBe(true);
    expect(couldNotTell(new HostError("bad_request", "/x/a.rs does not exist"))).toBe(false);
  });
});
