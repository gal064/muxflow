import { describe, expect, it } from "vitest";
import { fuzzyMatch, fuzzyRank } from "./fuzzy";

describe("palette fuzzy matching", () => {
  it("matches characters in order with gaps, which substring search could not", () => {
    expect(fuzzyMatch("Split pane right", "spl")).toBeDefined();
    expect(fuzzyMatch("Split pane right", "sright")).toBeDefined();
    expect(fuzzyMatch("Split pane right", "spr")).toBeDefined();
    expect(fuzzyMatch("Split pane right", "xyz")).toBeUndefined();
    // Order still matters: the letters have to appear in the order typed.
    expect(fuzzyMatch("Split pane right", "tips")).toBeUndefined();
  });

  it("ranks the command a human meant above one that merely contains the letters", () => {
    const commands = ["Split pane right", "Show command palette", "Scroll terminal to bottom", "Configure keyboard shortcuts"];
    expect(fuzzyRank(commands, "spl", (value) => value)[0]).toBe("Split pane right");
    expect(fuzzyRank(commands, "scroll", (value) => value)[0]).toBe("Scroll terminal to bottom");
    // Word starts beat mid-word hits.
    expect(fuzzyRank(["Terminal reset", "Rename terminal tab"], "rt", (value) => value)[0]).toBe("Rename terminal tab");
  });

  it("returns everything, in its original order, for an empty query", () => {
    const items = ["b", "a", "c"];
    expect(fuzzyRank(items, "", (value) => value)).toEqual(items);
    expect(fuzzyRank(items, "   ", (value) => value)).toEqual(items);
    expect(fuzzyMatch("anything", "")).toEqual({ score: 0, indices: [] });
  });

  it("reports where it matched, so the palette can show why a row is there", () => {
    expect(fuzzyMatch("Split pane", "sp")!.indices).toEqual([0, 1]);
    // Spaces in the query are separators the user typed for readability, not
    // characters to find, so "s p" is the same query as "sp".
    expect(fuzzyMatch("Split pane", "s p")!.indices).toEqual([0, 1]);
    // Greedy leftmost: the second `p` of "Split pane" is never reached,
    // because the first one already satisfied the query.
    expect(fuzzyMatch("Split pane", "spane")!.indices).toEqual([0, 1, 7, 8, 9]);
  });

  it("is case-insensitive in both directions", () => {
    expect(fuzzyMatch("Split Pane", "SPLIT")).toBeDefined();
    expect(fuzzyMatch("SPLIT PANE", "split")).toBeDefined();
  });
});
