import { describe, expect, it, vi } from "vitest";

import { ChatFileLinks, MAX_PROBES, type ProbeAnswer } from "./chatFileLinks";

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
});
