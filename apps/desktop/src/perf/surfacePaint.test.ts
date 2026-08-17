import { afterEach, describe, expect, it, vi } from "vitest";
import { enablePerfProbe, resetPerfProbe } from "./probe";
import { createPaintTicket } from "./paintTicket";
import { createPaintReporter, type EditorSurfaceFacts } from "./surfacePaint";

function facts(overrides: Partial<{ expected: number; mounted?: number; ready?: number }> = {}): EditorSurfaceFacts {
  const state = { expected: 1, mounted: undefined as number | undefined, ready: undefined as number | undefined, ...overrides };
  return {
    get expected() { return state.expected; },
    mounted: () => state.mounted,
    ready: () => state.ready,
  };
}

describe("createPaintReporter", () => {
  afterEach(() => { resetPerfProbe(); });

  function armed(generation: () => number) {
    // The probe has to be on: the disabled path is one inert singleton, which
    // is the right allocation behaviour and the wrong thing to reason about.
    enablePerfProbe(async () => undefined);
    const reporter = createPaintReporter(generation);
    const ticket = createPaintTicket(["surface.test"], generation());
    reporter.hold(ticket);
    return { reporter, ticket };
  }

  it("publishes once the surface has committed and its editor is ready", async () => {
    const { reporter } = armed(() => 3);
    const editor = facts({ mounted: 1, ready: 1 });
    reporter.noteCommitted();
    const onPaint = vi.fn();
    reporter.notePaintable(editor, onPaint);
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(onPaint).toHaveBeenCalledTimes(1);
  });

  it("leaves the measurement pending until the editor generation it named is ready", async () => {
    const { reporter, ticket } = armed(() => 1);
    const editor = facts({ expected: 4 });
    reporter.noteCommitted();
    const onPaint = vi.fn();
    reporter.notePaintable(editor, onPaint);
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(onPaint, "a measurement published before its editor existed").not.toHaveBeenCalled();
    // It named the generation it is waiting for, so the mount can recognise it.
    expect(ticket.surfaceGeneration).toBe(4);
    expect(reporter.pending()).toBe(ticket);
  });

  it("keeps a superseded load's measurement when a newer load has taken it over", () => {
    const { reporter, ticket } = armed(() => 1);
    const replacement = createPaintTicket(["surface.test"], 2);
    reporter.hold(replacement);
    expect(reporter.pending(), "the older ticket was still the pending one").toBe(replacement);
    // Discarding by name drops only that ticket, never the pending successor.
    reporter.discard(ticket);
    expect(reporter.pending()).toBe(replacement);
  });

  it("publishes nothing after the lifetime that armed it has moved on", async () => {
    let generation = 1;
    const { reporter } = armed(() => generation);
    reporter.noteCommitted();
    generation = 2;
    const onPaint = vi.fn();
    reporter.notePaintable(undefined, onPaint);
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(onPaint).not.toHaveBeenCalled();
  });
});
