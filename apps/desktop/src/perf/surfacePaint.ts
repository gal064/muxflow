import { useCallback, useMemo, useRef } from "react";
import { createPaintTicket, type PaintTicket } from "./paintTicket";

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
   * Whether a measurement is pending — that one, if a ticket is named.
   *
   * A superseded load asks by name: the interaction it was measuring is still
   * on screen if a newer read has not replaced the ticket, and retiring it
   * there would lose the measurement of an open that does finish.
   */
  holding(ticket?: PaintTicket): boolean;
  /**
   * Drops a measurement without publishing it.
   *
   * With a ticket, only that ticket is dropped, and the pending slot is left
   * alone unless the ticket is the pending one — a load that failed after being
   * superseded must not silently retire its successor's measurement.
   */
  abandon(ticket?: PaintTicket): void;
}

export function createPaintReporter(generation: () => number): PaintReporter {
  let pending: PaintTicket | undefined;
  let committed = 0;
  return {
    hold: (ticket) => {
      if (pending !== ticket) pending?.abandon();
      pending = ticket;
    },
    holding: (ticket) => (ticket ? pending === ticket : pending !== undefined),
    abandon: (ticket) => {
      if (ticket && pending !== ticket) {
        ticket.abandon();
        return;
      }
      pending?.abandon();
      pending = undefined;
      committed = 0;
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

/** A rendering surface's live view of its own editor host. */
export interface EditorSurface {
  facts: EditorSurfaceFacts;
  /** Ref callback for the box the editor is mounted into. */
  bindHost(node: Element | null): void;
  /** The mounted editor has reported itself ready to paint. */
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
export function useEditorSurface(): EditorSurface {
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
