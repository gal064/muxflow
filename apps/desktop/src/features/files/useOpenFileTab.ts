import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useCommittedRef } from "../../commands/useCommittedRef";
import { createPaintTicket } from "../../perf/paintTicket";
import { recordPerfMilestone } from "../../perf/probe";
import { createPaintReporter, type SurfacePaint } from "../../perf/surfacePaint";
import { AutosaveController, type AutosaveView } from "./autosave";
import { editorFlushRegistry } from "./editorFlushRegistry";
import { parentPath } from "./listingModel";
import {
  TEXT_FILE_LIMIT_BYTES,
  type ActiveRoot,
  type BinaryFile,
  type DirectoryListing,
  type DirectoryWatchLease,
  type FileWorkspaceClient,
  type FileWorkspaceScope,
  type OpenFile,
  type TextFile,
} from "./types";

const FILE_EDITOR_PAINT = ["workflow.file.editorPaint"] as const;

export interface OpenFileTabParams {
  client: FileWorkspaceClient;
  scope?: FileWorkspaceScope;
  root?: ActiveRoot;
  /** Absolute path of the file this tab is showing. */
  resource: string;
  /** Identity of the tab, for the flush registry that survives its unmount. */
  tabId: string;
  /** Whether the tab's current view mode renders an editor at all. */
  editorVisible: boolean;
  /** The buffer became dirty. Called once per clean-to-dirty transition. */
  onDirty(): void;
  onStatus(message: string): void;
}

/**
 * What the tab has to show, decided once.
 *
 * The surface used to re-derive this from `loading`, `error`, the open file's
 * kind and its size — the same ladder as the hook's own, with the editor
 * threshold spelled out in both places. There is one ladder now; the surface
 * chooses the words for each case, which is all it should be choosing.
 */
export type OpenFileContent =
  | { kind: "loading" }
  /** The read failed and there is nothing on screen to keep. */
  | { kind: "failed"; detail: string }
  /** Content is on screen, and the file behind it changed or went away. */
  | { kind: "changed"; detail: string }
  /** The read completed with nothing, which no path is expected to produce. */
  | { kind: "unavailable" }
  | { kind: "binary"; file: BinaryFile }
  | { kind: "tooLarge"; file: TextFile }
  | { kind: "text"; file: TextFile };

export interface OpenFileTab {
  content: OpenFileContent;
  view?: AutosaveView;
  /**
   * This tab will render an editor for what it is holding.
   *
   * The one place that decides it, because it is also what decides whether the
   * editor chunk is fetched at all and whether the pending editor-paint
   * measurement can ever be published.
   */
  editorRequested: boolean;
  /** Records a local edit. Ignored unless a text buffer is open. */
  edit(content: string): void;
  paint: SurfacePaint;
}

/**
 * Where the initial read and its parent watch bootstrap have got to.
 *
 * They are started together and either can land first, so the surface has to
 * remember which — and having remembered, must decide exactly once.
 */
type Reconciliation =
  | { kind: "pending" }
  /**
   * The bootstrap arrived before the read did, so its opinion is parked for
   * whichever read is in flight *now* — named by `serial`. A parked opinion
   * with no epoch outlived the read it was waiting for: a later, unrelated
   * load consumed it and re-opened the file against a generation that had
   * described a different read entirely.
   */
  | { kind: "bootstrap"; generation: string | undefined; serial: number }
  | { kind: "done" };

/**
 * One file tab's read, watch, reconciliation and autosave.
 *
 * Everything about *when* the file is read lives here, and nothing about how it
 * is drawn: the single in-flight open and its cancellation, the parent watch
 * lease and the one reconciliation it is entitled to, the event subscription,
 * the autosave controller, and the measurement that spans request to pixels.
 *
 * It is a hook rather than part of the surface because the surface imports
 * Monaco and this must not. A module that pulls the editor in at the top cannot
 * issue its first remote read until that chunk has been fetched *and*
 * evaluated, which put the whole editor bundle in front of every file open —
 * including the opens that turn out to be a binary, an oversized file, or a
 * Markdown preview, and never render an editor at all.
 */
export function useOpenFileTab(params: OpenFileTabParams): OpenFileTab {
  const { client, editorVisible, resource, root, scope, tabId } = params;
  const [opened, setOpened] = useState<OpenFile>();
  const [view, setView] = useState<AutosaveView>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const controller = useRef<AutosaveController | undefined>(undefined);
  const loadSerial = useRef(0);
  const surfaceLifecycle = useRef(0);
  const loadAbort = useRef<AbortController | undefined>(undefined);
  const paint = useMemo(() => createPaintReporter(() => surfaceLifecycle.current), []);
  /**
   * Generation of a non-text file on screen.
   *
   * Text has a better answer — the autosave controller owns the generation and
   * advances it on every save — and a second copy of that fact went stale the
   * moment anything was written, which made every self-save echo look like an
   * external change and cost a full remote re-open.
   */
  const shownBinaryGeneration = useRef<string | undefined>(undefined);
  /** Where the read and its parent watch have got to relative to each other. */
  const reconciliation = useRef<Reconciliation>({ kind: "pending" });
  // Reached through a committed ref: neither belongs to the connection
  // lifetime that keys the effects calling them, and listing either as a
  // dependency would re-arm a watch and a subscription on every render.
  const notify = useCommittedRef(params.onStatus);
  const notifyDirty = useCommittedRef(params.onDirty);

  const content = useMemo<OpenFileContent>(() => {
    if (loading) return { kind: "loading" };
    if (error) return opened ? { kind: "changed", detail: error } : { kind: "failed", detail: error };
    if (!opened) return { kind: "unavailable" };
    if (opened.kind === "binary") return { kind: "binary", file: opened.file };
    return Number(opened.file.sizeBytes) > TEXT_FILE_LIMIT_BYTES
      ? { kind: "tooLarge", file: opened.file }
      : { kind: "text", file: opened.file };
  }, [error, loading, opened]);
  const editorRequested = content.kind === "text" && editorVisible;
  const previousEditorVisible = useRef(editorVisible);

  useEffect(() => {
    if (editorRequested) {
      recordPerfMilestone("editor.monacoRequest");
      return;
    }
    // Binary, oversized, disconnected and preview-only surfaces never mount an
    // editor, so they must never publish the specifically named editor-paint
    // span. Not while the read is still in flight, though: that is the one
    // state where the editor is still expected.
    if (!loading) paint.abandon();
  }, [editorRequested, loading, paint]);

  useEffect(() => {
    const enteringEditor = !previousEditorVisible.current && editorVisible;
    previousEditorVisible.current = editorVisible;
    // Only an explicit preview -> source/split request owns a new interaction.
    // Background reads may change content kind but never manufacture one.
    if (enteringEditor && editorRequested && !paint.pending()) {
      paint.hold(createPaintTicket(FILE_EDITOR_PAINT, surfaceLifecycle.current));
    }
  }, [editorRequested, editorVisible, paint]);

  const load = async (options: { externalOperationId?: string; measureEditorPaint?: boolean } = {}) => {
    if (!scope || !root) return;
    loadAbort.current?.abort();
    const abort = new AbortController();
    loadAbort.current = abort;
    const serial = ++loadSerial.current;
    const ticket = options.measureEditorPaint
      ? createPaintTicket(FILE_EDITOR_PAINT, surfaceLifecycle.current)
      : undefined;
    if (ticket) paint.hold(ticket);
    try {
      const next = await client.openFile(scope, root, resource, abort.signal);
      if (serial !== loadSerial.current) {
        // Only if a newer read has not adopted it: the tab-open interaction
        // this ticket measures is still on screen, and the read that
        // superseded this one is the one that will finish it.
        if (ticket && paint.pending() !== ticket) ticket.abandon();
        return;
      }
      setOpened(next);
      shownBinaryGeneration.current = next.kind === "binary" ? next.file.generation : undefined;
      if (next.kind !== "text") {
        // A file that stopped being text has no editor and no autosave state.
        // Leaving the previous controller in place would make it the answer to
        // "what generation is on screen" forever after.
        controller.current?.dispose();
        controller.current = undefined;
        setView(undefined);
      }
      // The watch bootstrap can land while the first read is still in flight.
      // It is the authoritative directory listing, so a difference here is a
      // real change rather than a reason to re-read on principle. The reload is
      // queued rather than called: re-entering `load` from inside its own
      // success path invalidates the serial of the invocation still running.
      const arrived = reconciliation.current;
      if (arrived.kind === "bootstrap" && arrived.serial === serial) {
        reconciliation.current = { kind: "done" };
        if (arrived.generation !== undefined && arrived.generation !== next.file.generation) {
          queueMicrotask(() => { if (serial === loadSerial.current) void load(options); });
        }
      }
      setError(undefined);
      if (next.kind === "text") {
        const snapshot = { content: next.file.content, generation: next.file.generation, lineEnding: next.file.lineEnding };
        if (controller.current) {
          // Polling directory snapshots also observe our own atomic rename.
          // Preserve a newer local edit when disk still has the generation we
          // already know; any genuinely newer generation remains last-writer.
          if (!options.externalOperationId && controller.current.current().generation === snapshot.generation) return;
          controller.current.external(snapshot, options.externalOperationId);
        }
        else {
          const autosave = new AutosaveController(snapshot, async (saving, operationId) => {
            if (!scope || !root) throw new Error("The file host is disconnected.");
            return client.writeText(scope, root, {
              path: resource,
              content: saving.content,
              baseGeneration: saving.generation,
              operationId,
              lineEnding: saving.lineEnding,
            });
          }, setView);
          controller.current = autosave;
          setView(autosave.current());
        }
      }
    } catch (cause) {
      if (abort.signal.aborted && serial !== loadSerial.current) return;
      if (ticket) paint.discard(ticket);
      if (abort.signal.aborted) return;
      if (serial === loadSerial.current) setError(String(cause));
    } finally {
      if (serial === loadSerial.current) setLoading(false);
    }
  };

  // Declared before the read below, because effects run in the order they are
  // written: the listener has to exist before the read starts, or a change
  // landing between the two is described to nobody and the tab shows content
  // it will never be told is stale.
  useEffect(() => {
    if (!scope) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void client.subscribe(scope, (event) => {
      if (disposed || !root) return;
      if (event.kind === "directorySnapshot" && event.rootToken === root.token && event.listing.directory === parentPath(resource)) {
        // An authoritative rescan carries the directory's contents, so it can
        // say whether *this* file moved. Re-reading because some other entry
        // changed is a remote round trip for nothing. Never let a generic
        // self-save echo replace a newer dirty edit either; the precise path
        // events below retain last-writer order.
        const generation = listingOpinion(event.listing);
        if (generation === undefined) return;
        const shown = shownGeneration();
        if (shown === undefined) {
          // Nothing is on screen yet, so the read that will put it there is
          // still in flight — and it is already fetching this generation or a
          // newer one. Reloading here aborts it, and on a file whose read
          // takes longer than the gap between rescans it aborts *every*
          // attempt: the read restarts once per rescan and never advances.
          // That is the 5.2 MB file that never opened
          // (tests/phase15/large-file-open-bug.md) — 171 attempts, 0
          // successes. "Nothing shown" is not "stale"; it is "not yet".
          //
          // The opinion is parked rather than dropped, so the read is still
          // reconciled against it once it lands. Exactly what
          // `reconcileBootstrap` does with the same question.
          reconciliation.current = { kind: "bootstrap", generation, serial: loadSerial.current };
          return;
        }
        if (generation !== shown) reloadFromDisk();
        return;
      }
      if (!(event.kind === "fileChanged" || event.kind === "fileDeleted") || event.path !== resource) return;
      if (event.kind === "fileDeleted") {
        setError("The file was deleted externally. The tab remains open.");
        return;
      }
      if (event.generation && event.generation === shownGeneration()) return;
      void load({ externalOperationId: event.operationId });
    }).then((unsubscribe) => { if (disposed) unsubscribe(); else stop = unsubscribe; });
    return () => { disposed = true; stop?.(); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, scope?.clientId, scope?.terminalEpoch, resource, root?.token]);

  useEffect(() => {
    surfaceLifecycle.current += 1;
    setLoading(true);
    setOpened(undefined);
    shownBinaryGeneration.current = undefined;
    setView(undefined);
    controller.current?.dispose();
    controller.current = undefined;
    void load({ measureEditorPaint: true });
    return () => {
      surfaceLifecycle.current += 1;
      loadSerial.current += 1;
      paint.abandon();
      loadAbort.current?.abort();
      if (controller.current) {
        const pending = controller.current.flush();
        editorFlushRegistry.track(pending);
        void pending.catch((saveError) => notify.current(`Could not save ${resource}: ${String(saveError)}`));
      }
      controller.current?.dispose();
      controller.current = undefined;
    };
    // The read, the parent watch, and the event subscription all belong to one
    // connection generation. Leaving `terminalEpoch` out of this one meant an
    // epoch bump re-armed the watch and its reconciliation against a read that
    // had never restarted.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, scope?.clientId, scope?.terminalEpoch, resource, root?.token]);

  useEffect(() => editorFlushRegistry.register(tabId, async () => {
    await controller.current?.flush();
  }), [tabId]);

  // "The buffer is dirty" is a state this hook already tracks, so the tab hears
  // about it once per clean-to-dirty transition rather than once per keystroke.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => { if (view?.state === "dirty") notifyDirty.current(); }, [view?.state]);

  /**
   * The generation of the content on screen.
   *
   * Derived, never mirrored: for text the autosave controller already owns it
   * and advances it on every save, so anything that kept a second copy would
   * disagree with disk from the first write onwards.
   */
  const shownGeneration = () => controller.current?.current().generation ?? shownBinaryGeneration.current;

  /**
   * The listing entry that describes this file, when the listing is entitled to
   * an opinion about it.
   *
   * A symlink's entry describes the *link*, whose identity does not move when
   * its target is rewritten, while the open describes the bytes. Comparing the
   * two would report a change on every single open of every symlinked file, so
   * a symlink is simply not reconciled from its parent's listing.
   */
  const listingOpinion = (snapshot: DirectoryListing) => {
    const entry = snapshot.entries.find((candidate) => candidate.path === resource);
    if (entry) return entry.kind === "symlink" ? undefined : entry.generation;
    // Absence proves nothing here. A partial page has simply not reached the
    // file, and even a complete listing omits names the host never reports.
    // Only an explicit delete event may retire this tab.
    return undefined;
  };

  /**
   * Reconciles the file this surface read against an authoritative listing of
   * its parent directory.
   *
   * The watch bootstrap already carries the directory's exact contents, so it
   * can answer "did the file change between the read and the watch being
   * armed?" without asking again. It previously re-read unconditionally, which
   * on the remote link is a second full open per tab that almost always
   * confirmed what had just arrived. A reload happens only on a real
   * generation mismatch, and only once per bootstrap.
   */
  const reconcileBootstrap = (lease: DirectoryWatchLease) => {
    if (reconciliation.current.kind === "done") return;
    if (!lease.fresh) {
      // The watch was already armed — by the Explorer showing this folder —
      // so its bootstrap describes the directory as of whenever that happened
      // and has no opinion about a file read just now. Acting on it re-opened
      // the file remotely on the strength of an arbitrarily old row. Nothing is
      // lost by declining: changes since that watch was armed have already
      // arrived as events, and changes after this read arrive as events too.
      reconciliation.current = { kind: "done" };
      return;
    }
    const generation = listingOpinion(lease.snapshot);
    const shown = shownGeneration();
    if (shown === undefined) {
      reconciliation.current = { kind: "bootstrap", generation, serial: loadSerial.current };
      return;
    }
    reconciliation.current = { kind: "done" };
    if (generation !== undefined && generation !== shown) reloadFromDisk();
  };

  /**
   * Re-reads the file because an authoritative listing says it moved.
   *
   * One guard, one caller shape: a buffer the person is still typing into, or
   * one whose save is in flight, is never replaced by disk. The reload path
   * hands the content to `AutosaveController.external`, which overwrites the
   * view outright, so an unguarded caller silently discards unsaved work.
   */
  const reloadFromDisk = () => {
    const state = controller.current?.current().state;
    if (state === "dirty" || state === "saving") return;
    void load();
  };

  useEffect(() => {
    if (!scope || !root) return;
    let disposed = false;
    let release: (() => void) | undefined;
    // The bootstrap *is* a full directory listing, so a tab closed while it is
    // in flight must stop it rather than pay for it and throw it away. Only
    // this subscriber is abandoned: the watch itself survives for whoever else
    // holds it.
    const abandon = new AbortController();
    reconciliation.current = { kind: "pending" };
    void client.acquireDirectoryWatch(scope, root, parentPath(resource), {
      signal: abandon.signal,
    }).then((next) => {
      if (disposed) next.release();
      else {
        release = next.release;
        reconcileBootstrap(next);
      }
    }).catch((watchError) => {
      if (disposed || (watchError instanceof DOMException && watchError.name === "AbortError")) return;
      notify.current(`File watch unavailable: ${String(watchError)}`);
    });
    return () => {
      disposed = true;
      if (release) release();
      else abandon.abort();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, scope?.clientId, scope?.terminalEpoch, resource, root?.token]);

  const lineEnding = opened?.kind === "text" ? opened.file.lineEnding : undefined;
  const edit = useCallback((content: string) => {
    if (lineEnding === undefined) return;
    controller.current?.edit(content, lineEnding);
  }, [lineEnding]);

  return { content, view, editorRequested, edit, paint };
}
