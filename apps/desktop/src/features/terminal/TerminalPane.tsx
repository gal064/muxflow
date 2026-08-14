import { useEffect, useRef, useState } from "react";
import { closePanePaintSpans } from "../../perf/probe";
import { keyboardEventIsComposing } from "../../commands/registry";
import type { Pane } from "../../app/types";
import type { TerminalEventHub } from "./TerminalEventHub";
import {
  XtermRenderer,
  type TerminalInput,
  type TerminalMeasurements,
  type TerminalRenderer,
  type TerminalSize,
  type TerminalViewportState,
} from "./TerminalRenderer";
import { terminalStateCache } from "./TerminalStateCache";
import { outputAfterRecovery, reducePaneReveal, type PaneRevealState } from "./PaneRevealState";
import { prepareTerminalSnapshot, requestTerminalSeed, setTerminalVisibility } from "./api";
import { TerminalTransferSurface, type TerminalTransferSurfaceController } from "./TerminalTransferSurface";
import type { TerminalTransferRegistry } from "./terminalTransferRegistry";
import type { TerminalTransferClient, TerminalTransferConnectionScope, TerminalTransferScope } from "./terminalTransfers";
export { paneRecoveryPlan } from "./PaneRecovery";

// A pane may remount while its prior renderer is still draining. Serializing
// visibility ownership keeps a late hide from overtaking the new reveal.
const pendingPaneHandoffs = new Map<string, Promise<void>>();
const paneLifecycleVersions = new Map<string, number>();
let nextTransferRenderLifetime = 0;

/**
 * Renders a pane at the grid tmux says it has, not the one its CSS box measures.
 *
 * tmux is authoritative: the program in the pane addressed the cursor against
 * tmux's cols and rows, so any other grid puts its text on the wrong lines
 * (P12-U003.1). The two genuinely differ — tmux splits a 100-column client into
 * 50 and 49 with a divider column, while the layout gives each pane a rounded
 * percentage of the container — and only the active pane's measurement is ever
 * sent back to tmux, so every other mounted pane drifts silently. The measured
 * size still decides what client size to ask tmux for; it just no longer decides
 * what the terminal renders at.
 *
 * Falls back to the measurement only when tmux's numbers are unusable, so a pane
 * missing from a topology snapshot still gets a sized terminal rather than
 * xterm's 80x24 default. Returns what a caller should report, or `undefined`
 * when there is nothing to say.
 */
export function reconcilePaneGrid(
  renderer: Pick<TerminalRenderer, "setGrid">,
  pane: Pane,
  measured?: TerminalSize,
): string | undefined {
  const outcome = renderer.setGrid({ columns: pane.width, rows: pane.height });
  if (outcome.kind === "rejected") {
    const fallback = measured && renderer.setGrid(measured);
    return `Pane ${pane.id}: tmux reports no usable grid (${outcome.reason}); ${
      fallback?.kind === "applied" ? "rendering at the measured box size" : "the terminal keeps its current size"
    }.`;
  }
  if (outcome.kind === "unchanged" || !measured) return undefined;
  if (measured.columns === outcome.size.columns && measured.rows === outcome.size.rows) return undefined;
  return `Pane ${pane.id} measured ${measured.columns}x${measured.rows} from its box but tmux reports ${outcome.size.columns}x${outcome.size.rows}; rendering at tmux's grid.`;
}

function visibleSeedDiagnostic(message: string | undefined): string | undefined {
  // These are expected capability limits on supported tmux versions. Keep the
  // pane-scoped diagnostic in the event stream without permanently covering
  // terminal output with an implementation detail the user cannot act on.
  return message?.startsWith("tmux does not expose ") ? undefined : message;
}

export function isForcedLocalSelection(event: Pick<MouseEvent, "shiftKey">): boolean {
  // xterm's cross-platform force-selection contract is Shift. In particular,
  // this bypasses DEC mouse reporting on Linux without altering child modes.
  return event.shiftKey;
}

export function interceptTerminalPlainTextPaste(
  event: Pick<ClipboardEvent, "defaultPrevented" | "preventDefault" | "stopImmediatePropagation" | "stopPropagation"> & {
    clipboardData: Pick<DataTransfer, "getData"> | null;
  },
  paste: (text: string) => void,
): boolean {
  if (event.defaultPrevented) return false;
  const text = event.clipboardData?.getData("text/plain");
  if (!text) return false;
  event.preventDefault();
  event.stopPropagation();
  event.stopImmediatePropagation();
  paste(text);
  return true;
}


export interface TerminalPaneController {
  focus(): void;
  copy(): Promise<boolean>;
  paste(): Promise<boolean>;
  showSearch(): void;
  scrollToBottom(): void;
}

interface Props {
  clientId?: string;
  pane: Pane;
  hub: TerminalEventHub;
  onInput: (paneId: string, input: TerminalInput) => void;
  onFocus: (paneId: string) => void;
  /**
   * Reports what this terminal turns pixels into. It describes a terminal, not
   * this pane, and the tmux client size is computed from it (P12-U006).
   */
  onMeasurements?: (measurements: TerminalMeasurements) => void;
  onController: (paneId: string, controller: TerminalPaneController | undefined) => void;
  onDiagnostic?: (message: string) => void;
  transferClient?: TerminalTransferClient;
  transferRegistry?: TerminalTransferRegistry;
  transferScope?: TerminalTransferConnectionScope;
}

export function TerminalPane({
  clientId,
  pane,
  hub,
  onInput,
  onFocus,
  onMeasurements,
  onController,
  onDiagnostic,
  transferClient,
  transferRegistry,
  transferScope,
}: Props) {
  const container = useRef<HTMLDivElement>(null);
  const rendererRef = useRef<TerminalRenderer | undefined>(undefined);
  const transferControllerRef = useRef<TerminalTransferSurfaceController | undefined>(undefined);
  const transferRenderLifetimeRef = useRef<{ hub: TerminalEventHub; paneId: string; value: string } | undefined>(undefined);
  if (!transferRenderLifetimeRef.current
    || transferRenderLifetimeRef.current.hub !== hub
    || transferRenderLifetimeRef.current.paneId !== pane.id) {
    transferRenderLifetimeRef.current = { hub, paneId: pane.id, value: String(++nextTransferRenderLifetime) };
  }
  const paneRef = useRef(pane);
  const inputRef = useRef(onInput);
  const focusRef = useRef(onFocus);
  const measurementsRef = useRef(onMeasurements);
  const controllerRef = useRef(onController);
  const diagnosticRef = useRef(onDiagnostic);
  const clientIdRef = useRef(clientId);
  const rendererEpochRef = useRef<number | undefined>(undefined);
  const lastRevealKeyRef = useRef<string | undefined>(undefined);
  const revealStateRef = useRef<PaneRevealState>({ ready: false, hasLocalState: false });
  const deferredOutputRef = useRef<Array<{ data: Uint8Array; generation: number; terminalEpoch?: number }>>([]);
  const deferredOutputBytesRef = useRef(0);
  const seedDiagnosticForNextSeedRef = useRef(false);
  const [rendererDiagnostic, setRendererDiagnostic] = useState<string>();
  const [seedDiagnostic, setSeedDiagnostic] = useState<string>();
  const [viewport, setViewport] = useState<TerminalViewportState>({ atBottom: true, newOutput: false });
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [searchMiss, setSearchMiss] = useState(false);
  const searchComposing = useRef(false);
  paneRef.current = pane;
  inputRef.current = onInput;
  focusRef.current = onFocus;
  measurementsRef.current = onMeasurements;
  controllerRef.current = onController;
  diagnosticRef.current = onDiagnostic;
  clientIdRef.current = clientId;
  const paneTransferScope: TerminalTransferScope | undefined = transferScope ? {
    ...transferScope,
    paneId: pane.id,
    renderLifetime: transferRenderLifetimeRef.current.value,
  } : undefined;

  useEffect(() => {
    if (!container.current) return;
    const lifecycle = (paneLifecycleVersions.get(pane.id) ?? 0) + 1;
    paneLifecycleVersions.set(pane.id, lifecycle);
    let rendererActive = true;
    let rendererEpoch: number | undefined;
    const renderer = new XtermRenderer({
      onDiagnostic: (message) => {
        if (!rendererActive) return;
        setRendererDiagnostic(message);
        if (message) diagnosticRef.current?.(message);
      },
      onOpenLink: (url) => {
        if (window.confirm(`Open this external link?\n\n${url}`)) {
          window.open(url, "_blank", "noopener,noreferrer");
        }
      },
      // Rejecting is how this tells the renderer the request did not go out,
      // which reopens its latch so the pane can ask again.
      onResnapshotRequired: async (reason) => {
        if (!rendererActive) return;
        terminalStateCache.delete(pane.id);
        const currentClientId = clientIdRef.current;
        if (!currentClientId) throw new Error(`${reason} No connection to request a seed through.`);
        try {
          await requestTerminalSeed(currentClientId, pane.id);
        } catch (error) {
          diagnosticRef.current?.(`${reason} Seed request failed: ${String(error)}`);
          throw error;
        }
      },
    });
    const commitRendered = (generation: number, terminalEpoch: number | undefined, establishesEpoch = false) => {
      if (establishesEpoch && hub.generationEpoch === terminalEpoch) {
        rendererEpoch = terminalEpoch;
        if (rendererActive) rendererEpochRef.current = terminalEpoch;
      }
      // Deliberately not gated on `rendererActive`. Bytes still reach xterm
      // while this effect is tearing down and the drain runs, and silencing
      // the hub for that window made the hide checkpoint (renderer counter)
      // and the reveal checkpoint (hub counter) describe different cutoffs —
      // the stale-splice half of P12-U003.3. A superseded lifecycle is a
      // different pane instance and must stay silent.
      if (paneLifecycleVersions.get(pane.id) !== lifecycle) return;
      hub.markRendered(pane.id, generation, terminalEpoch);
    };
    rendererRef.current = renderer;
    const terminalContainer = container.current;
    renderer.open(terminalContainer);
    const reportGrid = (message: string | undefined) => {
      // Divergence is the norm, not a fault the user can act on, so it goes to
      // the console rather than the pane's diagnostic banner.
      if (message) console.warn(message);
    };
    // Before any content: everything below is parsed against this grid.
    reportGrid(reconcilePaneGrid(renderer, pane, renderer.measure()));
    const interceptPaste = (event: ClipboardEvent) => {
      // Native Edit > Paste bypasses the app command and targets xterm's
      // textarea. Own plain text in capture phase so xterm cannot wrap it in a
      // bracketed-paste envelope on its way through.
      interceptTerminalPlainTextPaste(event, (text) => {
        inputRef.current(pane.id, { kind: "text", data: text });
      });
    };
    terminalContainer.addEventListener("paste", interceptPaste, true);
    const cached = terminalStateCache.get(pane.id);
    const currentCached = cached?.terminalEpoch !== undefined && cached.terminalEpoch === hub.generationEpoch
      ? cached
      : undefined;
    if (currentCached) {
      const cachedEpoch = currentCached.terminalEpoch;
      const restored = renderer.restore(currentCached.serialized, () => {
        closePanePaintSpans();
        commitRendered(currentCached.outputGeneration, cachedEpoch, true);
      }, currentCached.outputGeneration);
      // A fresh terminal cannot refuse a restore today, but a caller that
      // ignores the answer is how the tail-splice bug happened; if it ever
      // does refuse, the cache is not what this pane should show.
      if (!restored) terminalStateCache.delete(pane.id);
    } else if (cached) {
      terminalStateCache.delete(pane.id);
    }
    rendererEpochRef.current = undefined;
    revealStateRef.current = { ready: false, hasLocalState: Boolean(currentCached) };

    const clearDeferredOutput = () => {
      deferredOutputRef.current = [];
      deferredOutputBytesRef.current = 0;
    };
    const flushDeferredOutput = (afterGeneration = -1) => {
      const deferred = outputAfterRecovery(deferredOutputRef.current, afterGeneration);
      clearDeferredOutput();
      for (const output of deferred) {
        renderer.write(
          output.data,
          () => commitRendered(output.generation, output.terminalEpoch),
          output.generation,
        );
      }
    };
    const requestFreshSeed = (reason: string) => {
      const currentClientId = clientIdRef.current;
      if (currentClientId) void requestTerminalSeed(currentClientId, pane.id).catch((error) => {
        diagnosticRef.current?.(`${reason}; fresh seed request failed: ${String(error)}`);
      });
    };

    const unsubscribeInput = renderer.onInput((input) => inputRef.current(pane.id, input));
    const unsubscribeViewport = renderer.onViewportChange(setViewport);
    const unsubscribeEvents = hub.subscribePane(pane.id, (event) => {
      if (!("paneId" in event)) return;
      const transition = reducePaneReveal(revealStateRef.current, event);
      revealStateRef.current = transition.state;
      const effect = transition.effect;
      const generation = "generation" in event ? event.generation : 0;
      const eventEpoch = hub.generationEpoch;
      if (effect.kind === "seed") {
        terminalStateCache.delete(pane.id);
        clearDeferredOutput();
        renderer.seed(effect.data, () => {
          closePanePaintSpans();
          commitRendered(generation, eventEpoch, true);
        }, generation);
        setRendererDiagnostic(undefined);
        if (seedDiagnosticForNextSeedRef.current) seedDiagnosticForNextSeedRef.current = false;
        else setSeedDiagnostic(undefined);
      } else if (effect.kind === "output") {
        renderer.write(effect.data, () => commitRendered(generation, eventEpoch), generation);
      } else if (effect.kind === "deferOutput") {
        if (deferredOutputBytesRef.current + effect.data.byteLength > 1024 * 1024) {
          clearDeferredOutput();
          revealStateRef.current = { ready: false, hasLocalState: false };
          requestFreshSeed("Output arrived before pane recovery exceeded 1 MiB");
        } else {
          deferredOutputRef.current.push({ data: effect.data, generation, terminalEpoch: eventEpoch });
          deferredOutputBytesRef.current += effect.data.byteLength;
        }
      } else if (effect.kind === "awaitSeed") {
        terminalStateCache.delete(pane.id);
        clearDeferredOutput();
        renderer.seed(new Uint8Array());
        setRendererDiagnostic(`${effect.reason}; waiting for a fresh terminal seed…`);
        if (effect.requestSeed) requestFreshSeed(effect.reason);
      } else if (effect.kind === "restore") {
        const markRecoveryRendered = () => {
          closePanePaintSpans();
          commitRendered(effect.tailThroughGeneration, eventEpoch, true);
        };
        // The snapshot and its raw tail are one screen in two pieces. If the
        // snapshot was refused, the tail must not be written onto whatever the
        // terminal happens to be showing, and nothing may be reported as
        // rendered: the renderer has already asked the host for a seed, and
        // this pane waits for it.
        const restored = renderer.restore(
          effect.serialized,
          effect.rawTail.byteLength ? undefined : markRecoveryRendered,
          effect.rawTail.byteLength ? effect.snapshotGeneration : effect.tailThroughGeneration,
          effect.tailThroughGeneration,
        );
        if (!restored) {
          clearDeferredOutput();
          revealStateRef.current = { ready: false, hasLocalState: false };
        } else {
          if (effect.rawTail.byteLength) {
            renderer.write(effect.rawTail, markRecoveryRendered, effect.tailThroughGeneration);
          }
          flushDeferredOutput(effect.tailThroughGeneration);
          setRendererDiagnostic(undefined);
        }
      } else if (effect.kind === "diagnostic") {
        // Seed diagnostics describe fidelity limitations in this pane only.
        // They do not invalidate recovery or escalate to connection status.
        seedDiagnosticForNextSeedRef.current = true;
        setSeedDiagnostic(effect.message);
      } else if (transition.state.ready) {
        flushDeferredOutput();
      }
    });
    // Render-side only, plus the terminal's own metrics. This observer once
    // computed the tmux client size from this pane's box and its share of the
    // topology, which is the defect in P12-U006; what it reports now describes
    // a terminal (cell size and chrome) and nothing about this pane's box.
    const reportMeasurements = () => {
      const measurements = renderer.measurements();
      if (measurements) measurementsRef.current?.(measurements);
    };
    const observer = new ResizeObserver(() => {
      reportGrid(reconcilePaneGrid(renderer, paneRef.current, renderer.measure()));
      // Re-read rather than report once: xterm rounds a cell to whole device
      // pixels, so moving the window between displays of different pixel
      // ratios changes it with no remount.
      reportMeasurements();
    });
    observer.observe(container.current);

    const controller: TerminalPaneController = {
      focus: () => renderer.focus(),
      copy: async () => {
        if (!renderer.hasSelection()) return false;
        await navigator.clipboard.writeText(renderer.getSelection());
        return true;
      },
      paste: async () => {
        if (await transferControllerRef.current?.pasteClipboard()) return true;
        inputRef.current(pane.id, { kind: "text", data: await navigator.clipboard.readText() });
        return true;
      },
      showSearch: () => setSearching(true),
      scrollToBottom: () => renderer.scrollToBottom(),
    };
    controllerRef.current(pane.id, controller);
    reportMeasurements();
    if (pane.active) renderer.focus();

    return () => {
      rendererActive = false;
      lastRevealKeyRef.current = undefined;
      observer.disconnect();
      unsubscribeEvents();
      unsubscribeViewport();
      unsubscribeInput();
      terminalContainer.removeEventListener("paste", interceptPaste, true);
      controllerRef.current(pane.id, undefined);
      const currentClientId = clientIdRef.current;
      const handoff = (async () => {
        try {
          const drained = await renderer.drainAndSerialize();
          if (paneLifecycleVersions.get(pane.id) !== lifecycle) return;
          const currentCheckpoint = hub.visibilityCheckpoint(pane.id);
          if (!currentCheckpoint) return;
          const snapshotMatchesEpoch = rendererEpoch === currentCheckpoint.terminalEpoch;
          const checkpoint = snapshotMatchesEpoch
            ? { ...currentCheckpoint, outputGeneration: drained.outputGeneration }
            : { ...currentCheckpoint, outputGeneration: 0 };
          const prepared = prepareTerminalSnapshot(drained.serialized);
          if (snapshotMatchesEpoch) terminalStateCache.set(pane.id, drained.serialized, checkpoint);
          else terminalStateCache.delete(pane.id);
          if (!prepared.retained && snapshotMatchesEpoch) {
            diagnosticRef.current?.(
              `The ${pane.id} renderer snapshot is ${prepared.originalByteLength} bytes; host recovery will use a fresh seed.`,
            );
          }
          if (!currentClientId || clientIdRef.current !== currentClientId) return;
          try {
            await setTerminalVisibility(
              currentClientId,
              pane.id,
              false,
              snapshotMatchesEpoch ? prepared.data : new Uint8Array(),
              checkpoint,
            );
          } catch (error) {
            diagnosticRef.current?.(`Could not mark ${pane.id} hidden: ${String(error)}`);
            if (clientIdRef.current === currentClientId && hub.generationEpoch === checkpoint.terminalEpoch) {
              // A rejected cutoff is never adjusted or guessed. Recover the
              // affected pane from an authoritative host seed instead.
              await requestTerminalSeed(currentClientId, pane.id).catch(() => undefined);
            }
          }
        } catch (error) {
          diagnosticRef.current?.(`Could not drain ${pane.id} before hiding it: ${String(error)}`);
          if (currentClientId && clientIdRef.current === currentClientId) {
            await requestTerminalSeed(currentClientId, pane.id).catch(() => undefined);
          }
        } finally {
          renderer.disposeGpuRenderer();
          renderer.dispose();
          if (rendererRef.current === renderer) rendererRef.current = undefined;
        }
      })();
      pendingPaneHandoffs.set(pane.id, handoff);
      void handoff.then(
        () => {
          if (pendingPaneHandoffs.get(pane.id) === handoff) pendingPaneHandoffs.delete(pane.id);
          if (paneLifecycleVersions.get(pane.id) === lifecycle) paneLifecycleVersions.delete(pane.id);
        },
        () => {
          if (pendingPaneHandoffs.get(pane.id) === handoff) pendingPaneHandoffs.delete(pane.id);
          if (paneLifecycleVersions.get(pane.id) === lifecycle) paneLifecycleVersions.delete(pane.id);
        },
      );
    };
  }, [hub, pane.id]);

  useEffect(() => {
    if (!clientId) return;
    let active = true;
    const revealForCurrentEpoch = () => {
      const checkpoint = hub.visibilityCheckpoint(pane.id);
      if (!checkpoint) return;
      const revealKey = `${clientId}:${checkpoint.terminalEpoch}`;
      if (lastRevealKeyRef.current === revealKey) return;
      lastRevealKeyRef.current = revealKey;
      void (async () => {
        await pendingPaneHandoffs.get(pane.id)?.catch(() => undefined);
        if (!active || lastRevealKeyRef.current !== revealKey) return;
        const currentCheckpoint = hub.visibilityCheckpoint(pane.id);
        if (!currentCheckpoint || currentCheckpoint.terminalEpoch !== checkpoint.terminalEpoch) return;
        revealStateRef.current = {
          ready: false,
          hasLocalState: rendererEpochRef.current === currentCheckpoint.terminalEpoch,
        };
        const rendererMatchesEpoch = rendererEpochRef.current === currentCheckpoint.terminalEpoch;
        try {
          await setTerminalVisibility(
            clientId,
            pane.id,
            true,
            new Uint8Array(),
            rendererMatchesEpoch ? currentCheckpoint : { ...currentCheckpoint, outputGeneration: 0 },
          );
        } catch (error) {
          if (lastRevealKeyRef.current === revealKey) lastRevealKeyRef.current = undefined;
          diagnosticRef.current?.(`Could not mark ${pane.id} visible: ${String(error)}`);
          if (clientIdRef.current === clientId && hub.generationEpoch === currentCheckpoint.terminalEpoch) {
            void requestTerminalSeed(clientId, pane.id).catch((seedError) => {
              diagnosticRef.current?.(`Could not reseed ${pane.id} after a visibility conflict: ${String(seedError)}`);
            });
          }
        }
      })();
    };
    revealForCurrentEpoch();
    const unsubscribe = hub.subscribe((event) => {
      if (event.kind === "generationEpoch") {
        terminalStateCache.delete(pane.id);
        deferredOutputRef.current = [];
        deferredOutputBytesRef.current = 0;
        rendererEpochRef.current = undefined;
        revealStateRef.current = { ready: false, hasLocalState: false };
        revealForCurrentEpoch();
      }
    });
    return () => {
      active = false;
      unsubscribe();
    };
  }, [clientId, hub, pane.id]);

  useEffect(() => {
    if (pane.active) rendererRef.current?.focus();
  }, [pane.active]);

  // tmux resized this pane (a split, a zoom, another client attaching). Follow
  // it immediately rather than at the next ResizeObserver callback, which a
  // pane whose CSS box did not change never gets.
  useEffect(() => {
    const renderer = rendererRef.current;
    if (!renderer) return;
    // The measurement is passed so the "tmux has no usable grid" fallback is
    // available here too; without it that case silently leaves the pane on
    // xterm's default 80x24 and says nothing.
    const report = reconcilePaneGrid(renderer, pane, renderer.measure());
    if (report) console.warn(report);
  }, [pane.id, pane.width, pane.height]);

  const find = (direction: "next" | "previous") => {
    const found = rendererRef.current?.search(query, direction);
    setSearchMiss(!found);
  };

  const terminal = <div
      className="terminal-pane"
      ref={container}
      aria-label={`Terminal pane ${pane.id}, ${pane.currentCommand}`}
      data-pane-id={pane.id}
      data-terminal-surface="true"
      data-local-selection-modifier="Shift"
      onMouseDownCapture={(event) => {
        if (isForcedLocalSelection(event.nativeEvent)) event.currentTarget.dataset.localSelectionActive = "true";
      }}
      onMouseUpCapture={(event) => { delete event.currentTarget.dataset.localSelectionActive; }}
      onFocusCapture={() => focusRef.current(pane.id)}
      role="region"
      title="Hold Shift while dragging to force local selection in mouse-aware terminal apps"
    />;

  return <>
    {transferClient ? <TerminalTransferSurface
      client={transferClient}
      onDiagnostic={onDiagnostic}
      onController={(controller) => { transferControllerRef.current = controller; }}
      // Pass the raw text. Neither host input path adds bracketed-paste
      // markers — `send-keys -H` sends bytes, and `paste-buffer` brackets only
      // with `-p`, which is never passed — so whatever the payload contains is
      // what the program receives. `xterm.paste` would wrap it in a second
      // envelope, which is how markers used to get doubled across the tmux
      // boundary.
      onPaste={(value) => inputRef.current(pane.id, { kind: "text", data: value })}
      registry={transferRegistry}
      scope={paneTransferScope}
      target={container}
    >{terminal}</TerminalTransferSurface> : terminal}
    {searching && <form aria-label={`Search terminal pane ${pane.id}`} className="terminal-search" onSubmit={(event) => { event.preventDefault(); if (!searchComposing.current) find("next"); }} role="search">
      <input
        autoFocus
        aria-label="Find in terminal"
        onChange={(event) => { setQuery(event.target.value); setSearchMiss(false); }}
        onCompositionEnd={() => { searchComposing.current = false; }}
        onCompositionStart={() => { searchComposing.current = true; }}
        onKeyDown={(event) => { if (keyboardEventIsComposing(event.nativeEvent)) setSearchMiss(false); }}
        placeholder="Find"
        value={query}
      />
      <button aria-label="Previous match" onClick={() => find("previous")} type="button">↑</button>
      <button aria-label="Next match" onClick={() => find("next")} type="submit">↓</button>
      <button aria-label="Close search" onClick={() => { rendererRef.current?.clearSearch(); setSearching(false); }} type="button">×</button>
      {searchMiss && <span className="search-miss">No match</span>}
    </form>}
    {viewport.newOutput && !viewport.atBottom && <button
      className="new-output"
      onClick={() => rendererRef.current?.scrollToBottom()}
      type="button"
    >New output ↓</button>}
    {(visibleSeedDiagnostic(seedDiagnostic) || rendererDiagnostic) && <div className="renderer-diagnostic" role="status">
      {[visibleSeedDiagnostic(seedDiagnostic), rendererDiagnostic].filter(Boolean).join(" · ")}
    </div>}
  </>;
}
