import { describe, expect, it, vi } from "vitest";
import { COPY_THEN_INTERRUPT_MS, createCopyChordJournal } from "./copyChordJournal";

describe("copy chord journal", () => {
  const copy = { kind: "run", commandId: "terminal.copy" } as const;
  const interrupt = { kind: "yield", commandId: "terminal.copy" } as const;

  const journal = () => {
    let clock = 0;
    const record = vi.fn();
    const note = createCopyChordJournal(record, () => clock);
    return { note, record, advance: (ms: number) => { clock += ms; } };
  };

  it("records each copy and a quick interrupt after it in the same pane", () => {
    const { note, record, advance } = journal();
    note(copy, "%1");
    advance(400);
    note(interrupt, "%1");
    expect(record.mock.calls).toEqual([
      ["terminal.copyChord", { paneId: "%1" }],
      ["terminal.copyThenInterrupt", { paneId: "%1", ms: 400 }],
    ]);
  });

  it("does not count a late interrupt, another pane, or a plain interrupt", () => {
    const { note, record, advance } = journal();
    note(interrupt, "%1");
    note(copy, "%1");
    advance(COPY_THEN_INTERRUPT_MS + 1);
    note(interrupt, "%1");
    note(copy, "%1");
    note(interrupt, "%2");
    expect(record.mock.calls.map(([kind]) => kind)).toEqual(["terminal.copyChord", "terminal.copyChord"]);
  });

  it("ignores every other disposition", () => {
    const { note, record } = journal();
    note({ kind: "run", commandId: "terminal.paste" }, "%1");
    note({ kind: "ignore" }, "%1");
    note({ kind: "swallow" }, "%1");
    expect(record).not.toHaveBeenCalled();
  });
});
