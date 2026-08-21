import { useEffect, useRef, useState } from "react";
import { afterNextPaint, closePanePaintSpans, recordPerfCounter, recordPerfMilestone } from "../../perf/probe";
import { recordIncident } from "../../diagnostics/incidents";
import { createPaintTicket } from "../../perf/paintTicket";
import { keyboardEventIsComposing, type Platform } from "../../commands/registry";
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
import { ownTerminalBytes } from "./TerminalBytes";
import { DeferredTerminalOutputQueue } from "./DeferredTerminalOutputQueue";
import { describePaneDegradation, PaneDegradedWatchdog } from "./PaneDegradedWatchdog";
import { awaitWithin } from "./timeBound";
import { REVEAL_RETRY_DELAY_MS, revealFailureAction } from "./revealRetry";
import { TerminalTransferSurface, type TerminalTransferSurfaceController } from "./TerminalTransferSurface";
import type { TerminalTransferRegistry } from "./terminalTransferRegistry";
import type { TerminalTransferClient, TerminalTransferConnectionScope, TerminalTransferScope } from "./terminalTransfers";
import { writeNativeTerminalClipboard } from "./terminalTransferApi";
import {
  copyCompletedTerminalSelection,
  installTerminalCopyOnSelect,
  translateTerminalKey,
} from "./terminalInputPolicy";
export { paneRecoveryPlan } from "./PaneRecovery";
export { copyCompletedTerminalSelection, translateTerminalKey } from "./terminalInputPolicy";

// A pane may remount while its prior renderer is still draining. Serializing
// visibility ownership keeps a late hide from overtaking the new reveal.
const pendingPaneHandoffs = new Map<string, Promise<void>>();
const paneLifecycleVersions = new Map<string, number>();
let nextTransferRenderLifetime = 0;

/**
 * How long a reveal waits behind the previous instance's handoff.
 *
 * The wait exists so a late hide cannot overtake a new reveal, and it was
 * unbounded: a single xterm write completion that never fires left the pane out
 * of the visibility protocol forever. See `revealForCurrentEpoch` for why
 * proceeding after the bound is safe.
 */
const PANE_HANDOFF_TIMEOUT_MS = 2_000;

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

/**
 * Rewraps the terminal to its own box while tmux has not yet answered for it.
 *
 * tmux stays authoritative — `reconcilePaneGrid` above is still what decides
 * the grid — but its answer costs a trailing debounce (`useClientResize`), a
 * host round trip and a topology rediscovery, and `.pane-frame` clips: for that
 * whole window a shrinking pane has its text cut off and a growing one shows a
 * dead band where the old grid ran out. The box itself re-lays-out on the drag
 * frame, so fitting to it locally is what makes a drag look continuous.
 *
 * `gridForBox` is the measurement tmux's current grid was applied for. While
 * the box still measures that, tmux's numbers *are* the numbers for this box
 * and are re-applied unchanged, so the routine divergence this file exists for
 * (a divider column, a rounded percentage) never triggers a local fit — and a
 * drag that ends back where it started restores tmux's grid rather than keeping
 * an intermediate one.
 *
 * It cannot oscillate with `reconcilePaneGrid`: applying a grid resizes the
 * terminal inside the box and never the box, so a local fit cannot provoke the
 * observer callback that produced it, and the caller re-anchors `gridForBox`
 * only where tmux's grid is applied.
 */
export function refitPaneGridToBox(
  renderer: Pick<TerminalRenderer, "setGrid">,
  pane: Pane,
  measured: TerminalSize | undefined,
  gridForBox: TerminalSize | undefined,
): string | undefined {
  if (measured && gridForBox && (measured.columns !== gridForBox.columns || measured.rows !== gridForBox.rows)) {
    // A box that measures nothing usable is not an argument against tmux's
    // grid, so a refused fit falls through to it rather than leaving the pane
    // at whatever it happened to be showing.
    if (renderer.setGrid(measured).kind !== "rejected") return undefined;
  }
  return reconcilePaneGrid(renderer, pane, measured);
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
  appFocused?: boolean;
  clientId?: string;
  pane: Pane;
  hub: TerminalEventHub;
  onInput: (paneId: string, input: TerminalInput) => void;
  /**
   * A physical key was pressed in this pane.
   *
   * Separate from `onInput` because they are not the same event: the terminal
   * also emits input on its own, replying to what a program asked it, and only
   * this one means a human touched the keyboard.
   */
  onKeyActivity?: (paneId: string) => void;
  onFocus: (paneId: string) => void;
  /**
   * Reports what this terminal turns pixels into. It describes a terminal, not
   * this pane, and the tmux client size is computed from it (P12-U006).
   */
  onMeasurements: (measurements: TerminalMeasurements) => void;
  onController: (paneId: string, controller: TerminalPaneController | undefined) => void;
  onDiagnostic?: (message: string) => void;
  onOpenFilePath?: (paneId: string, path: string) => void;
  copyOnSelect?: boolean;
  platform?: Platform;
  transferClient?: TerminalTransferClient;
  transferRegistry?: TerminalTransferRegistry;
  transferScope?: TerminalTransferConnectionScope;
}

export function TerminalPane({
  appFocused = true,
  clientId,
  pane,
  hub,
  onInput,
  onKeyActivity,
  onFocus,
  onMeasurements,
  onController,
  onDiagnostic,
  onOpenFilePath,
  copyOnSelect = false,
  platform = "linux",
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
  const keyActivityRef = useRef(onKeyActivity);
  const focusRef = useRef(onFocus);
  const measurementsRef = useRef(onMeasurements);
  const controllerRef = useRef(onController);
  const diagnosticRef = useRef(onDiagnostic);
  const openFilePathRef = useRef(onOpenFilePath);
  const clientIdRef = useRef(clientId);
  const appFocusedRef = useRef(appFocused);
  const copyOnSelectRef = useRef(copyOnSelect);
  const platformRef = useRef(platform);
  const rendererEpochRef = useRef<number | undefined>(undefined);
  // What the box measured when tmux's grid was last applied to it. The anchor
  // `refitPaneGridToBox` compares against; written only where tmux's grid is
  // applied, so an optimistic fit can never move it.
  const gridForBoxRef = useRef<TerminalSize | undefined>(undefined);
  const lastRevealKeyRef = useRef<string | undefined>(undefined);
  // Bumped only by an explicit re-assertion. The reveal is otherwise still once
  // per (client, epoch); this is what lets the watchdog step past that latch
  // without weakening it, and what makes an in-flight reveal stand down when a
  // newer one supersedes it.
  const revealAttemptRef = useRef(0);
  const reassertVisibilityRef = useRef<(() => void) | undefined>(undefined);
  const watchdogRef = useRef<PaneDegradedWatchdog | undefined>(undefined);
  const revealStateRef = useRef<PaneRevealState>({ ready: false, hasLocalState: false });
  const deferredOutputRef = useRef(new DeferredTerminalOutputQueue());
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
  keyActivityRef.current = onKeyActivity;
  focusRef.current = onFocus;
  measurementsRef.current = onMeasurements;
  controllerRef.current = onController;
  diagnosticRef.current = onDiagnostic;
  openFilePathRef.current = onOpenFilePath;
  clientIdRef.current = clientId;
  copyOnSelectRef.current = copyOnSelect;
  platformRef.current = platform;
  const paneTransferScope: TerminalTransferScope | undefined = transferScope ? {
    ...transferScope,
    paneId: pane.id,
    renderLifetime: transferRenderLifetimeRef.current.value,
  } : undefined;

  useEffect(() => {
    const terminalContainer = container.current;
    if (!terminalContainer) return;
    // Re-asserted rather than left to the JSX default: this element outlives a
    // remount that reuses the DOM node, and a node still carrying "true" from
    // the previous renderer would show the new one's empty first frame.
    terminalContainer.setAttribute("data-painted", "false");
    // xterm defers the parse of whatever it is handed to a timeout and the draw
    // to a later frame, so the frame right after `open` is a fully visible
    // empty grid with a block cursor at (0,0) — a pane that is about to show a
    // screenful of text flashes an empty one first on every remount (a window
    // or tab switch remounts every pane). `data-painted="false"` keeps the
    // terminal itself hidden until content lands; the pane's background is
    // painted by CSS either way, so nothing moves and nothing goes black.
    //
    // Latched, because every content path calls this and a busy pane would
    // otherwise touch the DOM once per output chunk.
    let revealed = false;
    const revealTerminal = () => {
      if (revealed) return;
      revealed = true;
      container.current?.setAttribute("data-painted", "true");
    };
    // The safety net, not the mechanism: content reveals the terminal as it
    // paints, and the paths that produce no content at all (a restore the
    // renderer refused, a host that never answers the seed request) would
    // otherwise leave this pane's terminal hidden indefinitely. Long enough
    // that a warm restore always wins the race and reveals on its own frame,
    // short enough that a stuck pane still shows its cursor.
    //
    // Armed here, first, ahead of everything below that can throw — the
    // renderer construction, and `hub.subscribePane`, which replays this pane's
    // buffered backlog synchronously and rethrows if the replay fails. A
    // fallback armed at the end of this effect is not a fallback for any of
    // that: the pane would stay hidden with no timer to rescue it.
    const revealFallback = setTimeout(revealTerminal, 300);
    const lifecycle = (paneLifecycleVersions.get(pane.id) ?? 0) + 1;
    paneLifecycleVersions.set(pane.id, lifecycle);
    const initialPaint = createPaintTicket([], lifecycle);
    let rendererActive = true;
    let rendererEpoch: number | undefined;
    const renderer = new XtermRenderer({
      paneId: pane.id,
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
      onOpenFilePath: (path) => openFilePathRef.current?.(pane.id, path),
      onClipboardWrite: writeNativeTerminalClipboard,
      onClipboardWriteError: (error) => {
        diagnosticRef.current?.(`Could not copy terminal text to the system clipboard: ${String(error)}`);
      },
      // Rejecting is how this tells the renderer the request did not go out,
      // which reopens its latch so the pane can ask again.
      onResnapshotRequired: async (reason) => {
        if (!rendererActive) return;
        // The renderer asks once and then latches. Record it so the pane keeps
        // asking on a bound if that one request produces nothing.
        watchdogRef.current?.note("rendererReseed");
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
    const commitRendered = (generation: number, terminalEpoch: number | undefined, establishesEpoch = false): boolean => {
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
      if (paneLifecycleVersions.get(pane.id) !== lifecycle) return false;
      hub.markRendered(pane.id, generation, terminalEpoch);
      return true;
    };
    const publishInitialPaint = (
      generation: number,
      terminalEpoch: number | undefined,
      establishesEpoch = false,
    ) => {
      // First, unconditionally, and straight at the DOM. This runs from the
      // renderer's rendered callback — the same task as the write that queued
      // the content draw — so the reveal lands on the frame the content does.
      // `useState` would be batched into a later frame, which is one more frame
      // of the empty grid, and the probe helpers below are no-ops when the perf
      // probe is off, so the reveal cannot live inside them either.
      revealTerminal();
      if (!commitRendered(generation, terminalEpoch, establishesEpoch)) return;
      // The perceived-latency spans (create.*, window.switch, pane.split) end
      // at the frame that shows this pane's content, so they are closed apart
      // from the startup ticket below: that ticket publishes once per mount,
      // which left any span whose content arrived as a later seed or restore
      // on the same instance permanently open — the abandonment Phase 15
      // measured (tests/performance/benchmark/comparability.md). The guard is only "these
      // pixels are still this pane's": a superseded or torn-down instance
      // never painted what it parsed, and its successor reports instead.
      afterNextPaint(() => {
        if (!rendererActive || paneLifecycleVersions.get(pane.id) !== lifecycle) return;
        closePanePaintSpans(clientIdRef.current, pane.id);
      });
      initialPaint.afterPaint(
        (ticket) => ticket.lifecycleGeneration === lifecycle
          && rendererActive
          && paneLifecycleVersions.get(pane.id) === lifecycle,
        () => {
          recordPerfMilestone("startup.terminalPaint");
        },
      );
    };
    rendererRef.current = renderer;
    renderer.open(terminalContainer);
    const reportGrid = (message: string | undefined) => {
      // Divergence is the norm, not a fault the user can act on, so it goes to
      // the console rather than the pane's diagnostic banner.
      if (message) console.warn(message);
    };
    // Before any content: everything below is parsed against this grid.
    const measuredAtOpen = renderer.measure();
    reportGrid(reconcilePaneGrid(renderer, pane, measuredAtOpen));
    gridForBoxRef.current = measuredAtOpen;
    const interceptPaste = (event: ClipboardEvent) => {
      // Native Edit > Paste bypasses the app command and targets xterm's
      // textarea. Own plain text in capture phase so xterm cannot wrap it in a
      // bracketed-paste envelope on its way through.
      interceptTerminalPlainTextPaste(event, (text) => {
        inputRef.current(pane.id, { kind: "text", data: text });
      });
    };
    terminalContainer.addEventListener("paste", interceptPaste, true);
    const handleTerminalKeyDown = (event: KeyboardEvent) => {
      // A bare modifier must not vouch for input: wheel scrolling a TUI also
      // produces terminal input (the wheel is translated into sequences for
      // the program), and a Shift or Cmd pressed around a scroll opened the
      // echo probe's gate for input the user never typed — journalling a
      // five-second "stall" nobody felt when the program had nothing to
      // redraw. Only a key that can become bytes counts as typing.
      if (event.key === "Shift" || event.key === "Meta" || event.key === "Alt" || event.key === "Control") return;
      // Capture phase sees both physical activity and the two app-owned
      // translations before xterm collapses modifiers. Every key the policy
      // does not translate continues to xterm untouched.
      keyActivityRef.current?.(pane.id);
      const translated = translateTerminalKey(event, {
        alternateScreen: renderer.isAlternateScreenActive(),
        currentCommand: paneRef.current.currentCommand,
        platform: platformRef.current,
      });
      if (translated === undefined) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      inputRef.current(pane.id, { kind: "text", data: translated });
    };
    terminalContainer.addEventListener("keydown", handleTerminalKeyDown, true);
    const disposeCopyOnSelect = installTerminalCopyOnSelect({
      container: terminalContainer,
      renderer,
      enabled: () => copyOnSelectRef.current,
      write: writeNativeTerminalClipboard,
      onError: (error) => {
        diagnosticRef.current?.(`Could not copy the terminal selection: ${String(error)}`);
      },
    });
    // What this terminal is known to be showing, while that is exactly one
    // serialized screen with nothing written after it — the mount-time cache
    // restore, or a handshake restore that arrived without a tail. Any byte
    // written on top of it clears this.
    //
    // It exists because a remount is answered with that same screen more than
    // once: the host emits a pane-resource event for the hide, which the hub
    // buffers and replays into the next mount, and then another for the reveal.
    // Both carry this pane's own hide snapshot, and restoring a screen that is
    // already on the terminal is an ESC c and a byte-identical rewrite — a
    // blank frame followed by the text that was already there, which is the
    // flicker a tab switch used to show two or three times.
    //
    // The serialized screen is kept, not just its generation: the generation is
    // the host's claim about which bytes these are, and it is not always a true
    // one. `set_visible(true)` restamps `snapshot_generation` without replacing
    // the snapshot, and the idempotent path in `hide_with_checkpoint` returns
    // the stored snapshot under a checkpoint this side has since re-serialized
    // (both in crates/tmux-control/src/replay.rs). A skip decided on the
    // generation alone keeps a screen the host is trying to replace.
    let screenOnDisplay: { generation: number; terminalEpoch: number | undefined; serialized: string } | undefined;
    const cached = terminalStateCache.get(pane.id);
    const currentCached = cached?.terminalEpoch !== undefined && cached.terminalEpoch === hub.generationEpoch
      ? cached
      : undefined;
    if (currentCached) {
      const cachedEpoch = currentCached.terminalEpoch;
      const restored = renderer.restore(currentCached.serialized, () => {
        publishInitialPaint(currentCached.outputGeneration, cachedEpoch, true);
      }, currentCached.outputGeneration);
      // A fresh terminal cannot refuse a restore today, but a caller that
      // ignores the answer is how the tail-splice bug happened; if it ever
      // does refuse, the cache is not what this pane should show.
      if (restored) {
        screenOnDisplay = {
          generation: currentCached.outputGeneration,
          terminalEpoch: cachedEpoch,
          serialized: currentCached.serialized,
        };
      } else terminalStateCache.delete(pane.id);
    } else if (cached) {
      terminalStateCache.delete(pane.id);
    }
    rendererEpochRef.current = undefined;
    revealStateRef.current = { ready: false, hasLocalState: Boolean(currentCached) };

    const clearDeferredOutput = () => {
      deferredOutputRef.current.reset();
    };
    const flushDeferredOutput = (afterGeneration = -1) => {
      const deferred = outputAfterRecovery(deferredOutputRef.current.drain(), afterGeneration);
      // These bytes land on top of whatever screen is up, so it is no longer
      // the bare snapshot the handshake could be asked to skip.
      if (deferred.length) screenOnDisplay = undefined;
      for (const output of deferred) {
        renderer.write(
          output.data,
          () => {
            // Same rule as live output: bytes that reach xterm are content, and
            // content is what makes this pane worth showing.
            revealTerminal();
            commitRendered(output.generation, output.terminalEpoch);
          },
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

    // The time bound on every one-shot recovery latch this pane can be caught
    // in. It arms only while the pane is degraded, so a pane that is working
    // runs no timers at all.
    const watchdog = new PaneDegradedWatchdog((reason, attempt) => {
      if (!rendererActive) return;
      recordPerfCounter("terminal.pane.watchdogReseeds");
      // Every retry, not once per episode: attempt numbers in the journal are
      // how "stuck for 2 seconds" and "stuck until reconnect" tell apart.
      recordIncident("pane.degraded", { paneId: pane.id, reason, attempt });
      // Once per episode, not once per retry: the point is that a stuck pane
      // stops being silent, not that it becomes noisy.
      if (attempt === 0) {
        setRendererDiagnostic(`${describePaneDegradation(reason)}; retrying…`);
      }
      // Reopen the hub's single-shot conflict latch as part of asking again, so
      // a later conflicting handoff is still able to request recovery.
      hub.retryPaneSeed(pane.id);
      requestFreshSeed(`Pane ${pane.id} stayed degraded (${reason}) past its recovery bound`);
      // A seed cannot help a pane the host believes is hidden, so the other half
      // of a retry is re-asserting visibility.
      reassertVisibilityRef.current?.();
    });
    watchdogRef.current = watchdog;

    const unsubscribeInput = renderer.onInput((input) => inputRef.current(pane.id, input));
    const unsubscribeViewport = renderer.onViewportChange(setViewport);
    const unsubscribeEvents = hub.subscribePane(pane.id, (event) => {
      const transition = reducePaneReveal(revealStateRef.current, event);
      revealStateRef.current = transition.state;
      const effect = transition.effect;
      const generation = "generation" in event ? event.generation : 0;
      const eventEpoch = hub.generationEpoch;
      if (effect.kind === "seed") {
        terminalStateCache.delete(pane.id);
        clearDeferredOutput();
        // A seed replaces the screen wholesale, so whatever was up is not what
        // this terminal shows any more.
        screenOnDisplay = undefined;
        // An authoritative screen is what every degraded state here was waiting
        // for: this pane is working again, and the next fault starts over at the
        // shortest retry delay rather than inheriting this episode's backoff.
        watchdog.noteHealthy();
        renderer.seed(effect.data, () => {
          publishInitialPaint(generation, eventEpoch, true);
        }, generation);
        setRendererDiagnostic(undefined);
        if (seedDiagnosticForNextSeedRef.current) seedDiagnosticForNextSeedRef.current = false;
        else setSeedDiagnostic(undefined);
      } else if (effect.kind === "output") {
        // Output reaching the renderer is proof the pane is not frozen.
        watchdog.noteHealthy();
        screenOnDisplay = undefined;
        renderer.write(effect.data, () => {
          // Output is content, and for some panes it is the only content that
          // ever arrives: a pane already `ready` when it mounted never sees a
          // seed or a restore, so before this its first real screenful was
          // revealed by nothing but the fallback timer. Latched, so the busy
          // case costs one comparison per chunk.
          revealTerminal();
          commitRendered(generation, eventEpoch);
        }, generation);
      } else if (effect.kind === "deferOutput") {
        const admission = deferredOutputRef.current.enqueue({
          data: effect.data,
          generation,
          terminalEpoch: eventEpoch,
        });
        if (admission === "overflow") {
          revealStateRef.current = { ready: false, hasLocalState: false };
          // The queue now refuses everything until it is reset, and the reset is
          // driven by the seed this asks for — one request, no retry, until now.
          watchdog.note("deferredOverflow");
          requestFreshSeed("Output arrived before pane recovery exceeded its byte or record budget");
        }
      } else if (effect.kind === "awaitSeed") {
        terminalStateCache.delete(pane.id);
        clearDeferredOutput();
        screenOnDisplay = undefined;
        // A handshake answer this reducer has no rule for used to fall through
        // to nothing at all. It recovers like any other seed debt now; what it
        // still owes is a record, because the shape itself is the finding.
        if (effect.unusableState) {
          recordIncident("pane.revealDeadEnd", { paneId: pane.id, state: effect.unusableState });
        }
        // Nobody on this side requests the seed when the host said it owns the
        // request (`requestSeed` false). That is exactly the case where a
        // suppressed host seed freezes the pane, so bound the wait either way:
        // this note is what arms the watchdog, and it is deliberately outside
        // the `requestSeed` branch below.
        watchdog.note("paneAwaitingSeed");
        renderer.seed(ownTerminalBytes(new Uint8Array()));
        setRendererDiagnostic(`${effect.reason}; waiting for a fresh terminal seed…`);
        // Deliberately not revealed. This branch's blank RIS is seed debt, not
        // content, and showing it is a whole extra visible repaint on the way to
        // the screen the arriving seed paints (which reveals for itself). The
        // diagnostic banner is a sibling of the terminal, so the user is told
        // what is happening either way, and the fallback armed at the top of
        // this effect owns the case where that seed never comes.
        if (effect.requestSeed) requestFreshSeed(effect.reason);
      } else if (effect.kind === "restore") {
        const markRecoveryRendered = () => {
          publishInitialPaint(effect.tailThroughGeneration, eventEpoch, true);
        };
        // When the host is handing back exactly the screen the cache already
        // painted, restoring it again is an ESC c and a byte-identical rewrite
        // — a blank frame followed by the text that was already there. Only the
        // raw tail is new information then. The generation and epoch have to
        // agree *and* the bytes have to match: the generation is the host's
        // claim about which screen this is, and it is not always a true one
        // (see `screenOnDisplay`). The byte comparison runs only after the two
        // cheap checks have passed, and length settles almost every mismatch
        // before a character is read.
        const skipRedundantRestore = screenOnDisplay !== undefined
          && screenOnDisplay.terminalEpoch === eventEpoch
          && screenOnDisplay.generation === effect.snapshotGeneration
          && screenOnDisplay.serialized.length === effect.serialized.length
          && screenOnDisplay.serialized === effect.serialized;
        // The snapshot and its raw tail are one screen in two pieces. If the
        // snapshot was refused, the tail must not be written onto whatever the
        // terminal happens to be showing, and nothing may be reported as
        // rendered: the renderer has already asked the host for a seed, and
        // this pane waits for it.
        const restored = skipRedundantRestore || renderer.restore(
          effect.serialized,
          effect.rawTail.byteLength ? undefined : markRecoveryRendered,
          effect.rawTail.byteLength ? effect.snapshotGeneration : effect.tailThroughGeneration,
          effect.tailThroughGeneration,
        );
        // Only once the snapshot is down, and never after a refusal. An empty
        // tail is still written on the skipped path: the scheduler queues it as
        // an ordered barrier, and that barrier is what carries the
        // acknowledgement and the reveal the skipped restore would otherwise
        // have carried.
        const writesTail = restored && (skipRedundantRestore || effect.rawTail.byteLength > 0);
        const tailQueued = writesTail
          ? renderer.write(effect.rawTail, markRecoveryRendered, effect.tailThroughGeneration)
          : true;
        if (!restored || !tailQueued) {
          clearDeferredOutput();
          screenOnDisplay = undefined;
          revealStateRef.current = { ready: false, hasLocalState: false };
          // A refused restore has already asked the host for a seed. A refused
          // *tail* has not: the scheduler drops the record and the callback
          // with it, so this pane just lost its acknowledgement and its reveal
          // with nothing said. Say it, and ask.
          if (restored && !tailQueued) {
            watchdog.note("rendererReseed");
            requestFreshSeed(`Pane ${pane.id} could not queue the tail of its recovery screen`);
          }
        } else {
          // A tail-less answer leaves the terminal showing exactly this
          // snapshot, which is what lets the *second* copy of it — the reveal's,
          // after the hide's — be skipped as well.
          screenOnDisplay = effect.rawTail.byteLength === 0
            ? { generation: effect.tailThroughGeneration, terminalEpoch: eventEpoch, serialized: effect.serialized }
            : undefined;
          flushDeferredOutput(effect.tailThroughGeneration);
          // Recovery material laid a real screen down; the pane is whole again.
          watchdog.noteHealthy();
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
    }, (health) => {
      // The hub's two latches, mirrored. While `awaitingSeed` stands the hub
      // drops this pane's events instead of delivering them, so this callback is
      // the only way the pane can find out it has been silenced.
      if (health.awaitingSeed) watchdog.note("hubAwaitingSeed");
      else watchdog.clear("hubAwaitingSeed");
      if (health.conflictReseedRequested) watchdog.note("hubConflictReseed");
      else watchdog.clear("hubConflictReseed");
    });
    // Render-side only, plus the terminal's own metrics. This observer once
    // computed the tmux client size from this pane's box and its share of the
    // topology, which is the defect in P12-U006; what it reports now describes
    // a terminal (cell size and chrome) and nothing about this pane's box.
    const reportMeasurements = () => {
      const measurements = renderer.measurements();
      if (measurements) measurementsRef.current(measurements);
    };
    const reconcileBoxAndMetrics = () => {
      // A drag moves this box every frame while tmux is still a debounce and a
      // round trip away from hearing about it, so re-applying tmux's grid here
      // is a no-op that leaves the pane clipped or short for the whole drag.
      // The refit prefers the box only while the box has moved away from the
      // measurement tmux's grid was applied for; the topology effect below
      // hands authority back the moment tmux answers.
      reportGrid(refitPaneGridToBox(renderer, paneRef.current, renderer.measure(), gridForBoxRef.current));
      // Re-read rather than report once: xterm rounds a cell to whole device
      // pixels, so moving the window between displays of different pixel
      // ratios changes it with no remount.
      reportMeasurements();
    };
    const observer = new ResizeObserver(reconcileBoxAndMetrics);
    observer.observe(terminalContainer);
    // xterm has its own DPR observer. Its render dimensions can therefore
    // change while this fixed CSS box does not, which ResizeObserver cannot
    // report. Follow xterm's metric events through the same guarded path.
    const unsubscribeMeasurements = renderer.onMeasurementsChange(reconcileBoxAndMetrics);

    const controller: TerminalPaneController = {
      focus: () => renderer.focus(),
      copy: async () => {
        if (!renderer.hasSelection()) return false;
        await writeNativeTerminalClipboard(renderer.getSelection());
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
      clearTimeout(revealFallback);
      initialPaint.abandon();
      watchdog.stop();
      if (watchdogRef.current === watchdog) watchdogRef.current = undefined;
      lastRevealKeyRef.current = undefined;
      observer.disconnect();
      unsubscribeMeasurements();
      unsubscribeEvents();
      unsubscribeViewport();
      unsubscribeInput();
      terminalContainer.removeEventListener("paste", interceptPaste, true);
      terminalContainer.removeEventListener("keydown", handleTerminalKeyDown, true);
      disposeCopyOnSelect();
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
          if (snapshotMatchesEpoch) {
            terminalStateCache.set(pane.id, prepared, checkpoint);
          }
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
    let retryTimer: ReturnType<typeof setTimeout> | undefined;
    // `retriesUsed` counts only within one reveal attempt: a retry re-runs the
    // visibility call under the *same* key, so it never gets past the
    // once-per-(client, epoch) latch on its own and never races the reveal a
    // newer epoch or an explicit re-assertion starts.
    const runReveal = async (
      checkpoint: { terminalEpoch: number; outputGeneration: number },
      revealKey: string,
      retriesUsed: number,
    ) => {
      const handoff = pendingPaneHandoffs.get(pane.id);
      if (handoff && (await awaitWithin(handoff, PANE_HANDOFF_TIMEOUT_MS)) === "timeout") {
        // Proceeding is safe, and waiting longer is not. A pending handoff
        // belongs to a *previous* instance of this pane — it is created by the
        // mount effect's cleanup, and React runs that cleanup before the new
        // instance's effects — so its own guard
        // (`paneLifecycleVersions.get(pane.id) !== lifecycle`) already makes
        // everything it does after the drain a no-op: it writes no
        // `terminalStateCache` entry and sends no hide. That guard is what
        // rules out the two things this wait was protecting against, a late
        // hide overtaking this reveal and a stale serialize landing in the
        // cache under this pane's id — the cache is keyed by pane id alone,
        // with the epoch only carried as a field, so a stale `set` would be
        // indistinguishable from a fresh one if it ever ran. The one case
        // where the handoff is still current is a pane that really did go
        // away, and then `active` is false and this reveal stops below.
        // Dropping the map entry keeps the *next* reveal from queueing behind
        // the same wedged promise.
        if (pendingPaneHandoffs.get(pane.id) === handoff) pendingPaneHandoffs.delete(pane.id);
        recordPerfCounter("terminal.pane.handoffTimeouts");
      }
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
        watchdogRef.current?.clear("revealFailed");
      } catch (error) {
        const action = revealFailureAction({
          error,
          current: active && lastRevealKeyRef.current === revealKey,
          retriesUsed,
        });
        if (action === "ignore") return;
        if (action === "rebuild") {
          // The host refused this request's *checkpoint*, not the reveal, and
          // nothing on this side can build a newer one: the epoch comes from
          // the hub, and the hub only learns the new epoch from the frame that
          // is still in flight. So drop the checkpoint and take the one
          // recovery that never carries one — a fresh host seed, which the host
          // answers by forcing this pane visible and capturing it, in a single
          // round trip. Retrying instead spent eight identical attempts over
          // two seconds and left the pane stale until an unrelated fallback
          // rescued it ~16s after wake.
          lastRevealKeyRef.current = undefined;
          // The cached screen was serialized under the same superseded epoch, so
          // a remount must not restore it as if it were current. Nothing already
          // painted is touched: this only invalidates the *next* mount's base.
          terminalStateCache.delete(pane.id);
          // hubEpoch dates the frontend's knowledge at rejection time: a fresh
          // hub epoch here means a stale checkpoint outlived the epoch frame; a
          // stale one means the frame itself was late (blank-panes.md, 04:38).
          recordIncident("pane.revealRebuilt", {
            paneId: pane.id,
            hubEpoch: hub.generationEpoch,
            error: String(error).slice(0, 200),
          });
          // The host still believes this pane is hidden in the epoch it has
          // moved to, and the seed below is what re-asserts otherwise. Keep the
          // time bound armed anyway: if that epoch's frame never reaches this
          // side, the watchdog is the only thing left to re-assert visibility.
          watchdogRef.current?.note("revealFailed");
          if (clientIdRef.current === clientId) {
            void requestTerminalSeed(clientId, pane.id).catch((seedError) => {
              // A reseed that fails is invisible everywhere else, and in the
              // 04:38 episode three reseeds went out with no proof any landed.
              recordIncident("pane.reseedFailed", { paneId: pane.id, error: String(seedError).slice(0, 200) });
              diagnosticRef.current?.(`Could not reseed ${pane.id} after a stale visibility epoch: ${String(seedError)}`);
            });
          }
          return;
        }
        if (action === "retry") {
          // The latch stays held: this key is still the pane's live reveal, and
          // the retry re-runs under it rather than announcing a new attempt.
          // The error text is the whole diagnosis: which of the transient
          // transport errors fired is what separates a resume race from a
          // stale-epoch race in the post-wake journal.
          if (retriesUsed === 0) {
            recordIncident("pane.revealRetry", {
              paneId: pane.id,
              attempt: revealAttemptRef.current,
              error: String(error).slice(0, 200),
            });
          }
          clearTimeout(retryTimer);
          retryTimer = setTimeout(() => {
            retryTimer = undefined;
            void runReveal(checkpoint, revealKey, retriesUsed + 1);
          }, REVEAL_RETRY_DELAY_MS);
          return;
        }
        lastRevealKeyRef.current = undefined;
        // Clearing the latch is not a retry: nothing re-runs this unless the
        // epoch changes, so the host can be left believing a mounted pane is
        // hidden and no output is ever sent for it. The watchdog is what turns
        // that into a bounded series of re-assertions.
        watchdogRef.current?.note("revealFailed");
        // The watchdog's own pane.degraded lines carry only attempt counts;
        // this is the one record that names what the transport actually said.
        recordIncident("pane.revealFailed", {
          paneId: pane.id,
          retriesUsed,
          error: String(error).slice(0, 200),
        });
        diagnosticRef.current?.(`Could not mark ${pane.id} visible: ${String(error)}`);
        if (clientIdRef.current === clientId && hub.generationEpoch === currentCheckpoint.terminalEpoch) {
          void requestTerminalSeed(clientId, pane.id).catch((seedError) => {
            diagnosticRef.current?.(`Could not reseed ${pane.id} after a visibility conflict: ${String(seedError)}`);
          });
        }
      }
    };
    const revealForCurrentEpoch = () => {
      const checkpoint = hub.visibilityCheckpoint(pane.id);
      if (!checkpoint) return;
      const revealKey = `${clientId}:${checkpoint.terminalEpoch}:${revealAttemptRef.current}`;
      if (lastRevealKeyRef.current === revealKey) return;
      lastRevealKeyRef.current = revealKey;
      // Whatever was still queued to retry belongs to the key this supersedes.
      clearTimeout(retryTimer);
      retryTimer = undefined;
      void runReveal(checkpoint, revealKey, 0);
    };
    reassertVisibilityRef.current = () => {
      // Stepping the attempt is what gets past the once-per-(client, epoch)
      // latch, and it also supersedes any reveal still in flight: that one's key
      // no longer matches, so it stands down at its own guard rather than racing
      // this one to the host.
      revealAttemptRef.current += 1;
      revealForCurrentEpoch();
    };
    revealForCurrentEpoch();
    const unsubscribe = hub.subscribeEpoch(() => {
      terminalStateCache.delete(pane.id);
      deferredOutputRef.current.reset();
      rendererEpochRef.current = undefined;
      revealStateRef.current = { ready: false, hasLocalState: false };
      revealForCurrentEpoch();
    });
    return () => {
      active = false;
      clearTimeout(retryTimer);
      reassertVisibilityRef.current = undefined;
      unsubscribe();
    };
  }, [clientId, hub, pane.id]);

  useEffect(() => {
    if (pane.active) rendererRef.current?.focus();
  }, [pane.active]);

  useEffect(() => {
    const regainedFocus = appFocused && !appFocusedRef.current;
    appFocusedRef.current = appFocused;
    // WebKit may leave xterm's hidden textarea unfocused when a foreground
    // window returns. A tab round-trip fixes it only because remounting calls
    // `focus`; do the same on the actual foreground transition. This runs
    // before a pointer's own focus action, so clicking a sidebar control still
    // leaves that control focused.
    if (regainedFocus && pane.active) rendererRef.current?.focus();
  }, [appFocused, pane.active]);

  // tmux resized this pane (a split, a zoom, another client attaching). Follow
  // it immediately rather than at the next ResizeObserver callback, which a
  // pane whose CSS box did not change never gets.
  useEffect(() => {
    const renderer = rendererRef.current;
    if (!renderer) return;
    // The measurement is passed so the "tmux has no usable grid" fallback is
    // available here too; without it that case silently leaves the pane on
    // xterm's default 80x24 and says nothing.
    const measured = renderer.measure();
    const report = reconcilePaneGrid(renderer, pane, measured);
    // tmux has now answered for this box: any fit made optimistically while its
    // answer was in flight is superseded here, and re-anchoring means the next
    // box change is judged against this measurement instead of the pre-drag
    // one. This is the only other place the anchor moves.
    gridForBoxRef.current = measured;
    if (report) console.warn(report);
  }, [pane.id, pane.width, pane.height]);

  const find = (direction: "next" | "previous") => {
    const found = rendererRef.current?.search(query, direction);
    setSearchMiss(!found);
  };

  // No `title`: a tooltip on the whole terminal surface appears wherever the
  // pointer rests and covers the output it is resting on (see 241cc74).
  const terminal = <div
      className="terminal-pane"
      ref={container}
      aria-label={`Terminal pane ${pane.id}, ${pane.currentCommand}`}
      data-pane-id={pane.id}
      // Hides the terminal, not the pane, until it has content to show. The
      // mount effect flips it, and re-asserts this value on the way in.
      data-painted="false"
      data-terminal-surface="true"
      data-local-selection-modifier="Shift"
      onMouseDownCapture={(event) => {
        rendererRef.current?.focus();
        if (isForcedLocalSelection(event.nativeEvent)) event.currentTarget.dataset.localSelectionActive = "true";
      }}
      onMouseUpCapture={(event) => { delete event.currentTarget.dataset.localSelectionActive; }}
      onFocusCapture={() => focusRef.current(pane.id)}
      role="region"
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
