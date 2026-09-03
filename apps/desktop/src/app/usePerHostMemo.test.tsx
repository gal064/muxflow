// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { usePerHostMemo, type PerHostEntry } from "./usePerHostMemo";

async function memo() {
  let latest!: ReadonlyMap<string, string>;
  function Harness({ entries }: { entries: readonly PerHostEntry<string>[] }) {
    latest = usePerHostMemo(entries);
    return null;
  }
  let renderer!: ReturnType<typeof create>;
  const render = async (entries: readonly PerHostEntry<string>[]) => {
    await act(async () => {
      if (renderer) renderer.update(<Harness entries={entries} />);
      else renderer = create(<Harness entries={entries} />);
    });
    return latest;
  };
  return { render, unmount: () => act(async () => renderer.unmount()) };
}

describe("usePerHostMemo", () => {
  it("rebuilds only the host whose deps changed, and keeps the map when nothing did", async () => {
    const build = vi.fn((key: string, version: number) => `${key}:${version}`);
    const entries = (local: number, peer: number): PerHostEntry<string>[] => [
      { key: "local", deps: [local], build: () => build("local", local) },
      { key: "peer", deps: [peer], build: () => build("peer", peer) },
    ];
    const { render, unmount } = await memo();
    const first = await render(entries(1, 1));
    expect([...first]).toEqual([["local", "local:1"], ["peer", "peer:1"]]);
    expect(build).toHaveBeenCalledTimes(2);

    const same = await render(entries(1, 1));
    expect(same).toBe(first);
    expect(build).toHaveBeenCalledTimes(2);

    const peerChanged = await render(entries(1, 2));
    expect(peerChanged).not.toBe(first);
    expect([...peerChanged]).toEqual([["local", "local:1"], ["peer", "peer:2"]]);
    expect(build).toHaveBeenCalledTimes(3);
    expect(build).toHaveBeenLastCalledWith("peer", 2);
    await unmount();
  });

  it("drops a host that left, and follows the order the hosts are given in", async () => {
    const entry = (key: string): PerHostEntry<string> => ({ key, deps: [key], build: () => key.toUpperCase() });
    const { render, unmount } = await memo();
    const both = await render([entry("local"), entry("peer")]);
    const reordered = await render([entry("peer"), entry("local")]);
    expect(reordered).not.toBe(both);
    expect([...reordered.keys()]).toEqual(["peer", "local"]);
    const one = await render([entry("peer")]);
    expect([...one.keys()]).toEqual(["peer"]);
    // Coming back is a fresh build, not a resurrected value.
    const back = await render([entry("peer"), entry("local")]);
    expect(back.get("local")).toBe("LOCAL");
    await unmount();
  });
});
