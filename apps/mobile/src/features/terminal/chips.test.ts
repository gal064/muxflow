import { describe, expect, it } from "vitest";
import { CR, KEY_CHIPS, SHIFT_CHIP, pressChip } from "./chips";
import { fromBase64, toBase64, utf8Encode } from "./bytes";
import { parseFromPageMessage } from "./bridgeMessages";

const hex = (bytes: Uint8Array | undefined) => (bytes ? Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join(" ") : undefined);

describe("key chips (§9.5)", () => {
  it("are in the doc's order with the doc's bytes", () => {
    expect(KEY_CHIPS.map((c) => [c.label, hex(c.bytes), hex(c.shifted)])).toEqual([
      ["Esc", "1b", undefined],
      ["⇧", undefined, undefined],
      ["Tab", "09", "1b 5b 5a"],
      ["⇧Tab", "1b 5b 5a", undefined],
      ["Enter", "0d", "1b 5b 31 33 3b 32 75"],
      ["⇧Enter", "1b 5b 31 33 3b 32 75", undefined],
      ["↑", "1b 5b 41", "1b 5b 31 3b 32 41"],
      ["↓", "1b 5b 42", "1b 5b 31 3b 32 42"],
      ["Ctrl-C", "03", undefined],
      ["Ctrl-D", "04", undefined],
      ["y", "79", undefined],
      ["n", "6e", undefined],
    ]);
    expect(hex(CR)).toBe("0d");
  });

  const chip = (label: string) => KEY_CHIPS.find((c) => c.label === label)!;

  it("the Shift chip toggles the armed state and sends nothing", () => {
    expect(pressChip(SHIFT_CHIP, false)).toEqual({ send: undefined, shiftArmed: true });
    expect(pressChip(SHIFT_CHIP, true)).toEqual({ send: undefined, shiftArmed: false });
  });

  it("an armed Shift sends the next chip's shifted bytes and disarms", () => {
    const press = pressChip(chip("Tab"), true);
    expect(hex(press.send)).toBe("1b 5b 5a");
    expect(press.shiftArmed).toBe(false);
  });

  it("an armed Shift on a chip without a shifted form sends its plain bytes and still disarms", () => {
    const press = pressChip(chip("Esc"), true);
    expect(hex(press.send)).toBe("1b");
    expect(press.shiftArmed).toBe(false);
  });

  it("an unarmed chip sends its plain bytes", () => {
    expect(pressChip(chip("Enter"), false)).toEqual({ send: chip("Enter").bytes, shiftArmed: false });
  });
});

describe("byte helpers", () => {
  it("base64-encodes every padding case like the platform would", () => {
    for (const text of ["", "f", "fo", "foo", "foob", "fooba", "foobar", "é中"]) {
      const bytes = new TextEncoder().encode(text);
      expect(toBase64(bytes)).toBe(Buffer.from(bytes).toString("base64"));
      expect(fromBase64(toBase64(bytes))).toEqual(bytes);
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
    expect(parseFromPageMessage('{"t":"input","b64":"G1s8NjQ7MTsyTQ=="}')).toEqual({ t: "input", b64: "G1s8NjQ7MTsyTQ==" });
    expect(parseFromPageMessage('{"t":"scroll","mode":"normal","rows":12.4,"durationMs":81.6,"cancelled":false}')).toEqual({ t: "scroll", mode: "normal", rows: 12, durationMs: 82, cancelled: false });
    expect(parseFromPageMessage('{"t":"input","b64":""}')).toBeUndefined();
    expect(parseFromPageMessage('{"t":"size"}')).toBeUndefined();
    expect(parseFromPageMessage("not json")).toBeUndefined();
    expect(parseFromPageMessage('{"t":"keys"}')).toBeUndefined();
  });
});
