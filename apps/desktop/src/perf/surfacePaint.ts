import { useCallback, useEffect, useMemo, useRef } from "react";
import { type PaintTicket } from "./paintTicket";
import { recordPerfMilestone } from "./probe";

/**
 * What a rendering surface knows about the editor it is painting into.
 *
 * Live readers rather than captured numbers: a surface can remount between the
 * measurement being armed and the pixels landing, and the decision has to see
 * the generation that is mounted *then*, not the one that was mounted when the
 * surface last spoke.
 */
export interface EditorSurfaceFacts {
  /** The generation this surface will mount into if it has not yet. */
  readonly expected: number;
  /** The generation mounted right now, if any. */
  mounted(): number | undefined;
  /** The generation that has reported itself ready to paint, if any. */
  ready(): number | undefined;
}

/**
 * The half of a request-to-pixels measurement that a rendering surface owns.
 *
 * It owns reporting only. Whether a report actually completes the measurement
 * is never the surface's question — a ticket belonging to a superseded load, or
 * to an editor generation that is not the mounted one, is simply left pending —
 * which is what keeps the deciding in the hook that started the request.
 */
export interface SurfacePaint {
  /** React has committed the load the surface is currently rendering. */
  noteCommitted(): void;
  /**
   * The surface has reached a state it believes is paintable.
   *
   * `editor` is absent for a surface that renders no editor at all.
   */
  notePaintable(editor?: EditorSurfaceFacts, onPaint?: () => void): void;
}

/**
 * The half a loading hook owns: which measurement is pending, and when it is
 * abandoned.
 *
 * Both editor-bearing surfaces in the app had grown their own copy of this
 * reconciliation, against paint tickets whose lifecycle rules are identical and
 * whose bugs therefore had to be found twice.
 */
export interface PaintReporter extends SurfacePaint {
  /** Takes ownership of the measurement this surface will publish next. */
  hold(ticket: PaintTicket): void;
  /**
   * The measurement currently pending, if any.
   *
   * A superseded load compares against it before retiring its own ticket: the
   * interaction it was measuring is still on screen if a newer read has not
   * replaced the ticket, and the newer read is the one that will finish it.
   */
  pending(): PaintTicket | undefined;
  /** Drops the pending measurement, whatever it is, without publishing it. */
  abandon(): void;
  /**
   * Retires one load's ticket, and the pending slot with it if that is what
   * the slot holds.
   *
   * The two halves matter separately: a load that failed after being
   * superseded must retire only its own ticket, never its successor's.
   */
  discard(ticket: PaintTicket): void;
}

/**
 * One surface's pending paint measurement and the rules for publishing it.
 *
 * The identity comparisons below are exact while the perf probe is on, which
 * is the only state in which anything is published at all. With the probe off
 * `createPaintTicket` hands out one inert frozen singleton, so every ticket is
 * the same object and `pending()`/`discard` cannot tell them apart — harmless,
 * because an inert ticket publishes nothing whichever way the comparison goes,
 * and deliberate, because ordinary unmeasured file and diff traffic should not
 * allocate a ticket per event to keep an identity nobody reads.
 */
export function createPaintReporter(generation: () => number): PaintReporter {
  let pending: PaintTicket | undefined;
  let committed = 0;
  const drop = () => {
    pending?.abandon();
    pending = undefined;
    committed = 0;
  };
  return {
    hold: (ticket) => {
      if (pending !== ticket) pending?.abandon();
      pending = ticket;
    },
    pending: () => pending,
    abandon: drop,
    discard: (ticket) => {
      if (pending !== ticket) {
        ticket.abandon();
        return;
      }
      drop();
    },
    noteCommitted: () => {
      committed = generation();
    },
    notePaintable: (editor, onPaint) => {
      const ticket = pending;
      if (!ticket) return;
      if (editor) {
        // Recorded before the bail, not after: a measurement armed while the
        // editor is still mounting has to name the generation it is waiting
        // for, or the mount that follows cannot recognise its own ticket.
        ticket.expectSurface(editor.mounted() ?? editor.expected);
        if (ticket.surfaceGeneration !== editor.mounted()
          || editor.ready() !== editor.mounted()) return;
      }
      pending = undefined;
      ticket.afterPaint((held) => held.lifecycleGeneration === generation()
        && held.lifecycleGeneration === committed
        && (!editor || held.surfaceGeneration === editor.mounted()),
      onPaint);
    },
  };
}

/** What a surface has to give its editor host for the measurement to complete. */
export interface EditorPaint {
  /** Ref callback for the box the editor is mounted into. */
  bindHost(node: Element | null): void;
  /** The mounted editor exists and is about to paint. */
  onReady(): void;
}

const recordEditorPaint = () => recordPerfMilestone("editor.paint");

/**
 * The surface half of an editor paint measurement, for both surfaces that have
 * one.
 *
 * Three things have to happen in a fixed order — the load React committed is
 * noted, the surface reports itself paintable, and the editor reports itself
 * mounted — and the file tab and the Git diff had each written that sequence
 * out by hand. Two copies of an ordering rule is two places for it to drift.
 *
 * `settled` is "this surface is showing the thing it was loading"; `usesEditor`
 * is whether that thing is an editor. A surface that is settled without one —
 * a binary diff — still completes its own paint span, and never claims the
 * editor milestone.
 */
export function useEditorPaint(paint: SurfacePaint, settled: boolean, usesEditor: boolean): EditorPaint {
  const surface = useEditorSurface();
  useEffect(() => {
    if (!settled) return;
    paint.noteCommitted();
    paint.notePaintable(
      usesEditor ? surface.facts : undefined,
      usesEditor ? recordEditorPaint : undefined,
    );
  }, [paint, settled, surface.facts, usesEditor]);
  const onReady = useCallback(() => {
    surface.noteReady();
    paint.notePaintable(surface.facts, recordEditorPaint);
  }, [paint, surface]);
  return useMemo(() => ({ bindHost: surface.bindHost, onReady }), [onReady, surface.bindHost]);
}

/** A rendering surface's live view of its own editor host. */
interface EditorSurface {
  facts: EditorSurfaceFacts;
  bindHost(node: Element | null): void;
  noteReady(): void;
}

/**
 * Tracks which editor generation is mounted and which is ready.
 *
 * A generation rather than a boolean because the editor chunk is fetched
 * lazily: a surface can be armed for an editor that has not been evaluated
 * yet, unmounted while the chunk is still in flight, and remounted for a
 * different file before the first mount ever reports. Only the generation the
 * measurement named may finish it.
 */
function useEditorSurface(): EditorSurface {
  const sequence = useRef(0);
  const mounted = useRef<number | undefined>(undefined);
  const ready = useRef<number | undefined>(undefined);
  const facts = useMemo<EditorSurfaceFacts>(() => ({
    get expected() { return sequence.current + 1; },
    mounted: () => mounted.current,
    ready: () => ready.current,
  }), []);
  const bindHost = useCallback((node: Element | null) => {
    if (node) {
      mounted.current ??= ++sequence.current;
    } else {
      mounted.current = undefined;
      ready.current = undefined;
    }
  }, []);
  const noteReady = useCallback(() => {
    ready.current = mounted.current;
  }, []);
  return useMemo(() => ({ facts, bindHost, noteReady }), [bindHost, facts, noteReady]);
}
