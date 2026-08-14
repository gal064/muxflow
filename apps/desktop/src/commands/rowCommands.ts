import { useEffect, useSyncExternalStore } from "react";
import type { CommandId } from "./registry";

/**
 * The palette's half of "every removed button becomes a palette command, a
 * context-menu item, and a shortcut".
 *
 * Row actions — rename this file, stage this change, resume this agent — are
 * the one family that could not simply be listed: they need a subject, and the
 * palette has no way to point at a row. This is that missing piece. Each row
 * surface publishes the row it currently means (the last one focused in it) and
 * the actions that row can take right now; the palette lists those actions from
 * the one command registry like any other command, and invoking one calls back
 * into the surface that published it.
 *
 * Three properties this design is built for:
 *
 * - **One registry.** These ids live in `commandRegistry` beside every other
 *   command, so the shortcut editor can bind them and nothing needs a second
 *   list.
 * - **Availability is the surface's answer, not a guess.** A surface publishes
 *   an action only while it would enable that action in its own menu, so the
 *   palette cannot offer "Stage" for a submodule or while the connection is
 *   read-only.
 * - **A closed panel means no row.** Publication is tied to the component's
 *   lifetime, so closing the right panel takes its commands with it rather than
 *   leaving them pointing at a row nobody can see.
 */
export type RowCommandSurface = "files" | "git" | "agents";

export interface RowCommandSource {
  /** What the published actions act on, for the message a run reports. */
  subject: string;
  /** Exactly the actions this surface would enable for that row right now. */
  available: readonly CommandId[];
  run(commandId: CommandId): void;
}

class RowCommandRegistry {
  readonly #sources = new Map<RowCommandSurface, RowCommandSource>();
  readonly #listeners = new Set<() => void>();
  /**
   * `useSyncExternalStore` compares snapshots by identity and re-reads on every
   * render, so the snapshot has to be the *same array* until the set of
   * available ids actually changes. Recomputing it per read would re-render
   * forever.
   */
  #snapshot: readonly CommandId[] = [];

  publish(surface: RowCommandSurface, source: RowCommandSource | undefined): void {
    if (source) this.#sources.set(surface, source);
    else this.#sources.delete(surface);
    const next = [...this.#sources.values()].flatMap((entry) => [...entry.available]);
    if (next.length === this.#snapshot.length && next.every((id, index) => id === this.#snapshot[index])) return;
    this.#snapshot = next;
    for (const listener of this.#listeners) listener();
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => { this.#listeners.delete(listener); };
  };

  available = (): readonly CommandId[] => this.#snapshot;

  /** The surface that published this id, if it is still published. */
  sourceFor(commandId: CommandId): RowCommandSource | undefined {
    return [...this.#sources.values()].find((entry) => entry.available.includes(commandId));
  }

  /** Returns what happened, so the caller can say so on the status channel. */
  run(commandId: CommandId): { ran: true; subject: string } | { ran: false } {
    const source = this.sourceFor(commandId);
    if (!source) return { ran: false };
    source.run(commandId);
    return { ran: true, subject: source.subject };
  }
}

export const rowCommandRegistry = new RowCommandRegistry();

/** What the command context needs: which row commands are live right now. */
export function useRowCommands(): readonly CommandId[] {
  return useSyncExternalStore(rowCommandRegistry.subscribe, rowCommandRegistry.available, rowCommandRegistry.available);
}

/**
 * Publishes a surface's current row for as long as the surface is mounted.
 *
 * `source` must be memoized by the caller; an unmemoized object would republish
 * on every render, which is harmless for correctness (the registry only
 * notifies on a real change) but pointless work.
 */
export function usePublishedRowCommands(surface: RowCommandSurface, source: RowCommandSource | undefined): void {
  useEffect(() => {
    rowCommandRegistry.publish(surface, source);
    return () => rowCommandRegistry.publish(surface, undefined);
  }, [surface, source]);
}
