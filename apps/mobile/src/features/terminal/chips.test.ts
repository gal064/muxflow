import { describe, expect, it } from "vitest";
import { CR, KEY_CHIPS } from "./chips";
import { toBase64, utf8Encode } from "./bytes";
import { parseFromPageMessage } from "./bridgeMessages";

const hex = (bytes: Uint8Array) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join(" ");

describe("key chips (§9.5)", () => {
  it("are in the doc's order with the doc's bytes", () => {
    expect(KEY_CHIPS.map((c) => [c.label, hex(c.bytes)])).toEqual([
      ["Esc", "1b"],
      ["Tab", "09"],
      ["↑", "1b 5b 41"],
      ["↓", "1b 5b 42"],
      ["Enter", "0d"],
      ["Ctrl-C", "03"],
      ["Ctrl-D", "04"],
      ["y", "79"],
      ["n", "6e"],
    ]);
    expect(hex(CR)).toBe("0d");
  });
});

describe("byte helpers", () => {
  it("base64-encodes every padding case like the platform would", () => {
    for (const text of ["", "f", "fo", "foo", "foob", "fooba", "foobar", "é中"]) {
      const bytes = new TextEncoder().encode(text);
      expect(toBase64(bytes)).toBe(Buffer.from(bytes).toString("base64"));
    }
  });

  it("utf8-encodes text like TextEncoder, including astral characters", () => {
    const encoder = (globalThis as { TextEncoder?: unknown }).TextEncoder;
    try {
      (globalThis as { TextEncoder?: unknown }).TextEncoder = undefined;
      for (const text of ["echo hi", "héllo", "中文", "😀 done"]) {
        expect(Array.from(utf8Encode(text))).toEqual(Array.from(Buffer.from(text, "utf8")));
      }
    } finally {
      (globalThis as { TextEncoder?: unknown }).TextEncoder = encoder;
    }
  });
});

describe("bridge messages (§10.2)", () => {
  it("parses the page's messages and rejects anything else", () => {
    expect(parseFromPageMessage('{"t":"size","cols":46,"rows":40,"cellWidth":7.8,"cellHeight":15.6}')).toEqual({ t: "size", cols: 46, rows: 40, cellWidth: 7.8, cellHeight: 15.6 });
    expect(parseFromPageMessage('{"t":"ready"}')).toEqual({ t: "ready" });
    expect(parseFromPageMessage('{"t":"written","bytes":12}')).toEqual({ t: "written", bytes: 12 });
    expect(parseFromPageMessage('{"t":"size"}')).toBeUndefined();
    expect(parseFromPageMessage("not json")).toBeUndefined();
    expect(parseFromPageMessage('{"t":"keys"}')).toBeUndefined();
  });
});
