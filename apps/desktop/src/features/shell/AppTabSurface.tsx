import Editor from "@monaco-editor/react";
import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import { AutosaveController, type AutosaveView } from "../files/autosave";
import { editorFlushRegistry } from "../files/editorFlushRegistry";
import { attachEditorLayout } from "../files/editorLayout";
import { renderSafeMarkdown, renderSafeSvg } from "../files/markdown";
import { IMAGE_PREVIEW_LIMIT_BYTES, TEXT_FILE_LIMIT_BYTES, type ActiveRoot, type DirectoryListing, type FileWorkspaceClient, type FileWorkspaceScope, type OpenFile } from "../files/types";
import { SurfaceError } from "../../ui/SurfaceError";
import type { AppOwnedTab } from "./types";
import { ADE_MONACO_THEME } from "../files/monaco";
import { recordPerfMilestone } from "../../perf/probe";
import { createPaintTicket, type PaintTicket } from "../../perf/paintTicket";

interface Props {
  tab: AppOwnedTab;
  scope?: FileWorkspaceScope;
  activeRoot?: ActiveRoot;
  client: FileWorkspaceClient;
  canWrite: boolean;
  onDownload(path: string, kind: "file" | "folder", root: ActiveRoot): void;
  /**
   * The buffer became dirty. A preview tab stops being disposable here: the
   * one thing that must never happen is the next single click in the Explorer
   * replacing a tab the user has typed into.
   */
  onDirty(): void;
  onStatus(message: string): void;
  onViewMode(mode: "source" | "preview" | "split"): void;
}

const FILE_EDITOR_PAINT = ["workflow.file.editorPaint"] as const;

export function AppTabSurface(props: Props) {
  const [opened, setOpened] = useState<OpenFile>();
  const [view, setView] = useState<AutosaveView>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const controller = useRef<AutosaveController | undefined>(undefined);
  const loadSerial = useRef(0);
  const surfaceLifecycle = useRef(0);
  const loadAbort = useRef<AbortController | undefined>(undefined);
  const detachLayout = useRef<(() => void) | undefined>(undefined);
  const editorSurfaceSequence = useRef(0);
  const mountedEditorSurface = useRef<number | undefined>(undefined);
  const readyEditorSurface = useRef<number | undefined>(undefined);
  const pendingPaint = useRef<PaintTicket | undefined>(undefined);
  /** Generation of the content this surface is currently showing. */
  const openedGeneration = useRef<string | undefined>(undefined);
  /** The parent watch's answer, when it arrived before the read completed. */
  const bootstrapGeneration = useRef<string | undefined>(undefined);
  const bootstrapReconciled = useRef(false);
  const bindEditorHost = useCallback((node: HTMLDivElement | null) => {
    if (node) {
      mountedEditorSurface.current ??= ++editorSurfaceSequence.current;
    } else {
      mountedEditorSurface.current = undefined;
      readyEditorSurface.current = undefined;
    }
  }, []);
  const root = useMemo<ActiveRoot | undefined>(() => {
    if (props.tab.rootPath && props.tab.rootToken) return {
      token: props.tab.rootToken,
      path: props.tab.rootPath,
      paneId: props.scope?.paneId ?? "",
      cwd: props.tab.rootPath,
      gitWorktree: false,
      revision: "0",
    };
    return props.activeRoot;
  }, [props.activeRoot, props.scope?.paneId, props.tab.rootPath, props.tab.rootToken]);
  const viewAllowsEditor = !(props.tab.kind === "markdown" && (props.tab.viewMode ?? "split") === "preview");
  const previousViewAllowsEditor = useRef(viewAllowsEditor);
  const editorRequested = !loading && !error && opened?.kind === "text"
    && Number(opened.file.sizeBytes) <= TEXT_FILE_LIMIT_BYTES
    && viewAllowsEditor;
  useEffect(() => {
    if (!editorRequested) {
      pendingPaint.current?.abandon();
      pendingPaint.current = undefined;
      return;
    }
    recordPerfMilestone("editor.monacoRequest");
  }, [editorRequested, props.tab.id]);
  useEffect(() => {
    const enteringEditor = !previousViewAllowsEditor.current && viewAllowsEditor;
    previousViewAllowsEditor.current = viewAllowsEditor;
    // Only an explicit preview -> source/split request owns a new interaction.
    // Background reads may change content kind but never manufacture one.
    if (enteringEditor && editorRequested && !pendingPaint.current) {
      const paint = createPaintTicket(FILE_EDITOR_PAINT, surfaceLifecycle.current);
      paint.expectSurface(mountedEditorSurface.current ?? editorSurfaceSequence.current + 1);
      pendingPaint.current = paint;
    }
  }, [editorRequested, viewAllowsEditor]);

  const load = async (options: { externalOperationId?: string; measureEditorPaint?: boolean } = {}) => {
    if (!props.scope || !root) return;
    loadAbort.current?.abort();
    const abort = new AbortController();
    loadAbort.current = abort;
    const serial = ++loadSerial.current;
    const paint = options.measureEditorPaint
      ? createPaintTicket(FILE_EDITOR_PAINT, surfaceLifecycle.current)
      : undefined;
    if (paint) {
      pendingPaint.current?.abandon();
      pendingPaint.current = paint;
    }
    try {
      const next = await props.client.openFile(props.scope, root, props.tab.resource, abort.signal);
      if (serial !== loadSerial.current) {
        if (paint && pendingPaint.current !== paint) paint.abandon();
        return;
      }
      setOpened(next);
      openedGeneration.current = next.file.generation;
      // The watch bootstrap can land while the first read is still in flight.
      // It is the authoritative directory listing, so a difference here is a
      // real change rather than a reason to re-read on principle.
      if (!bootstrapReconciled.current && bootstrapGeneration.current !== undefined) {
        const bootstrap = bootstrapGeneration.current;
        bootstrapReconciled.current = true;
        if (bootstrap !== next.file.generation) void load(options);
      }
      const interactionPaint = paint ?? pendingPaint.current;
      if (next.kind === "text"
        && Number(next.file.sizeBytes) <= TEXT_FILE_LIMIT_BYTES
        && viewAllowsEditor) {
        interactionPaint?.expectSurface(
          mountedEditorSurface.current ?? editorSurfaceSequence.current + 1,
        );
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
            if (!props.scope || !root) throw new Error("The file host is disconnected.");
            return props.client.writeText(props.scope, root, {
              path: props.tab.resource,
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
      paint?.abandon();
      if (pendingPaint.current === paint) pendingPaint.current = undefined;
      if (abort.signal.aborted) return;
      if (serial === loadSerial.current) setError(String(cause));
    } finally {
      if (serial === loadSerial.current) setLoading(false);
    }
  };

  useEffect(() => {
    surfaceLifecycle.current += 1;
    setLoading(true);
    setOpened(undefined);
    openedGeneration.current = undefined;
    setView(undefined);
    controller.current?.dispose();
    controller.current = undefined;
    void load({ measureEditorPaint: true });
    return () => {
      surfaceLifecycle.current += 1;
      loadSerial.current += 1;
      pendingPaint.current?.abandon();
      pendingPaint.current = undefined;
      loadAbort.current?.abort();
      detachLayout.current?.();
      detachLayout.current = undefined;
      if (controller.current) {
        const pending = controller.current.flush();
        editorFlushRegistry.track(pending);
        void pending.catch((saveError) => props.onStatus(`Could not save ${props.tab.resource}: ${String(saveError)}`));
      }
      controller.current?.dispose();
      controller.current = undefined;
    };
  }, [props.client, props.scope?.clientId, props.tab.resource, root?.token]);

  useEffect(() => {
    if (loading || error || !opened) return;
    const paint = pendingPaint.current;
    if (!paint || paint.surfaceGeneration !== 0) return;
    pendingPaint.current = undefined;
    // Binary, oversized, and preview-only surfaces never mount Monaco, so
    // they must never publish the specifically named editor-paint span.
    paint.abandon();
  }, [error, loading, opened]);

  useEffect(() => {
    if (!editorRequested || loading || error || !opened) return;
    const paint = pendingPaint.current;
    const surface = mountedEditorSurface.current;
    if (!paint || !surface || paint.surfaceGeneration !== surface || readyEditorSurface.current !== surface) return;
    pendingPaint.current = undefined;
    paint.afterPaint((ticket) => ticket.lifecycleGeneration === surfaceLifecycle.current
      && ticket.surfaceGeneration === mountedEditorSurface.current,
    () => recordPerfMilestone("editor.paint"));
  }, [editorRequested, error, loading, opened]);

  useEffect(() => editorFlushRegistry.register(props.tab.id, async () => {
    await controller.current?.flush();
  }), [props.tab.id]);

  // "The buffer is dirty" is a state this surface already tracks, so the tab
  // hears about it once per clean→dirty transition rather than once per
  // keystroke. `onDirty` is deliberately not a dependency: it closes over
  // render-fresh state and would re-run this on every render.
  useEffect(() => { if (view?.state === "dirty") props.onDirty(); }, [view?.state]);

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
  const reconcileBootstrap = (snapshot: DirectoryListing) => {
    if (bootstrapReconciled.current) return;
    const entry = snapshot.entries.find((candidate) => candidate.path === props.tab.resource);
    if (!entry) {
      // A partial page cannot say the file is gone, only that this page did not
      // reach it; a complete one can.
      if (!snapshot.complete) return;
      bootstrapReconciled.current = true;
      if (openedGeneration.current !== undefined) setError("The file was deleted externally. The tab remains open.");
      return;
    }
    if (openedGeneration.current === undefined) {
      bootstrapGeneration.current = entry.generation;
      return;
    }
    bootstrapReconciled.current = true;
    if (openedGeneration.current !== entry.generation) void load();
  };

  useEffect(() => {
    if (!props.scope || !root) return;
    let disposed = false;
    let release: (() => void) | undefined;
    bootstrapReconciled.current = false;
    bootstrapGeneration.current = undefined;
    void props.client.acquireDirectoryWatch(props.scope, root, parentPath(props.tab.resource)).then((next) => {
      if (disposed) next.release();
      else {
        release = next.release;
        reconcileBootstrap(next.snapshot);
      }
    }).catch((watchError) => { if (!disposed) props.onStatus(`File watch unavailable: ${String(watchError)}`); });
    return () => { disposed = true; release?.(); };
  }, [props.client, props.scope?.clientId, props.scope?.terminalEpoch, props.tab.resource, root?.token]);

  useEffect(() => {
    if (!props.scope) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void props.client.subscribe(props.scope, (event) => {
      if (disposed || !root) return;
      if (event.kind === "directorySnapshot" && event.rootToken === root.token && event.listing.directory === parentPath(props.tab.resource)) {
        // An authoritative rescan carries the directory's contents, so it can
        // say whether *this* file moved. Re-reading because some other entry
        // changed is a remote round trip for nothing. Never let a generic
        // self-save echo replace a newer dirty edit either; the precise path
        // events below retain last-writer order.
        const state = controller.current?.current().state;
        if (state === "dirty" || state === "saving") return;
        const entry = event.listing.entries.find((candidate) => candidate.path === props.tab.resource);
        if (!entry) {
          if (event.listing.complete && openedGeneration.current !== undefined) {
            setError("The file was deleted externally. The tab remains open.");
          }
          return;
        }
        if (openedGeneration.current !== entry.generation) void load();
        return;
      }
      if (!(event.kind === "fileChanged" || event.kind === "fileDeleted") || event.path !== props.tab.resource) return;
      if (event.kind === "fileDeleted") {
        setError("The file was deleted externally. The tab remains open.");
        return;
      }
      if (event.generation && event.generation === openedGeneration.current) return;
      void load({ externalOperationId: event.operationId });
    }).then((unsubscribe) => { if (disposed) unsubscribe(); else stop = unsubscribe; });
    return () => { disposed = true; stop?.(); };
  }, [props.client, props.scope?.clientId, props.tab.resource, root?.token]);

  if (!props.scope || !root) return <EmptyTab tab={props.tab} detail="Reconnect and select a terminal pane to reopen this file." />;
  if (loading) return <EmptyTab tab={props.tab} detail="Loading file…" />;
  if (error && !opened) return <EmptyTab tab={props.tab} detail={error} download={() => props.onDownload(props.tab.resource, "file", root)} />;
  if (error) return <EmptyTab tab={props.tab} detail={`The file changed or became unavailable: ${error}`} download={() => props.onDownload(props.tab.resource, "file", root)} />;
  if (!opened) return <EmptyTab tab={props.tab} detail="File unavailable." />;
  if (opened.kind === "binary") return <BinarySurface file={opened.file} onDownload={() => props.onDownload(props.tab.resource, "file", root)} />;
  if (Number(opened.file.sizeBytes) > TEXT_FILE_LIMIT_BYTES) return <EmptyTab tab={props.tab} detail="This text file is larger than the 10 MiB editor limit." download={() => props.onDownload(props.tab.resource, "file", root)} />;

  const mode = props.tab.kind === "markdown" ? props.tab.viewMode ?? "split" : "source";
  const source = view?.content ?? opened.file.content;
  // The view mode is data, not a class. As a class it was `markdown-${mode}`,
  // and in preview mode that is `markdown-preview` — the preview article's own
  // class — so every rule written for the article landed on the whole tab
  // surface too: 24px of padding, a scroll container around the scroll
  // container, a left border, a reading line-height, and the `code`/`pre`/`img`
  // rules restyling the toolbar's path chip. An attribute value shares no
  // namespace with a class name, so the collision cannot come back.
  return <section
    aria-label={props.tab.title}
    className="file-tab-surface"
    data-view-mode={props.tab.kind === "markdown" ? mode : undefined}
    role="tabpanel"
  >
    <header className="editor-toolbar">
      <code title={props.tab.resource}>{props.tab.resource}</code>
      <span className={`save-state ${view?.state ?? "saved"}`} role="status">{view?.state === "saving" ? "Saving…" : view?.state === "dirty" ? "Unsaved" : view?.state === "error" ? "Save failed" : "Saved"}</span>
      {props.tab.kind === "markdown" && <div aria-label="Markdown view" className="markdown-modes" role="group">
        {(["source", "preview", "split"] as const).map((item) => <button aria-pressed={mode === item} key={item} onClick={() => props.onViewMode(item)} type="button">{item}</button>)}
      </div>}
      <button onClick={() => props.onDownload(props.tab.resource, "file", root)} type="button">Download…</button>
    </header>
    {view?.error && <SurfaceError className="editor-error" detail={view.error} />}
    {mode !== "preview" && <div className="monaco-host" ref={bindEditorHost}>
      <Editor
        language={languageForPath(props.tab.resource)}
        onChange={(content) => { if (props.canWrite && typeof content === "string") controller.current?.edit(content, opened.file.lineEnding); }}
        onMount={(editor) => {
          const paint = pendingPaint.current;
          const surface = mountedEditorSurface.current;
          readyEditorSurface.current = surface;
          if (paint && surface && paint.surfaceGeneration === surface) {
            pendingPaint.current = undefined;
            paint.afterPaint((ticket) => ticket.lifecycleGeneration === surfaceLifecycle.current
              && ticket.surfaceGeneration === mountedEditorSurface.current,
            () => recordPerfMilestone("editor.paint"));
          }
          detachLayout.current?.(); detachLayout.current = attachEditorLayout(editor);
        }}
        options={{ automaticLayout: true, minimap: { enabled: false }, readOnly: !props.canWrite, scrollBeyondLastLine: false, wordWrap: props.tab.kind === "markdown" ? "on" : "off" }}
        path={modelPath(props.tab)}
        saveViewState
        theme={ADE_MONACO_THEME}
        value={source}
      />
    </div>}
    {props.tab.kind === "markdown" && mode !== "source" && <MarkdownPreview source={source} onStatus={props.onStatus} />}
  </section>;
}

function MarkdownPreview({ source, onStatus }: { source: string; onStatus(message: string): void }) {
  const html = useMemo(() => renderSafeMarkdown(source), [source]);
  const [externalUrl, setExternalUrl] = useState<string>();
  return <><article className="markdown-preview" onClick={(event) => {
    const anchor = (event.target as HTMLElement).closest("a");
    const href = anchor?.getAttribute("href");
    if (!href) return;
    event.preventDefault();
    if (/^https?:/i.test(href)) setExternalUrl(href);
    else onStatus(`Markdown link: ${href}`);
  }} dangerouslySetInnerHTML={{ __html: html }} />
  {externalUrl && <ConfirmationDialog
    confirmLabel="Open link"
    destructive={false}
    detail={externalUrl}
    onCancel={() => setExternalUrl(undefined)}
    onConfirm={() => {
      const url = externalUrl;
      setExternalUrl(undefined);
      void invoke("open_external_link", { url, confirmed: true }).catch((error) => onStatus(String(error)));
    }}
    title="Open external link?"
  />}</>;
}

function BinarySurface({ file, onDownload }: { file: Extract<OpenFile, { kind: "binary" }>["file"]; onDownload(): void }) {
  const [previewUrl, setPreviewUrl] = useState<string>();
  useEffect(() => {
    if (file.previewKind !== "image" || !file.previewBytes || Number(file.sizeBytes) > IMAGE_PREVIEW_LIMIT_BYTES) {
      setPreviewUrl(undefined);
      return;
    }
    let body: BlobPart = file.previewBytes.slice().buffer;
    if (file.mime === "image/svg+xml") {
      try { body = renderSafeSvg(new TextDecoder("utf-8", { fatal: true }).decode(file.previewBytes)); }
      catch { setPreviewUrl(undefined); return; }
    }
    const next = URL.createObjectURL(new Blob([body], { type: file.mime }));
    setPreviewUrl(next);
    return () => URL.revokeObjectURL(next);
  }, [file.mime, file.previewBytes, file.previewKind, file.sizeBytes]);
  const canPreview = Boolean(previewUrl);
  return <section className="binary-surface" role="tabpanel" aria-label={file.path}>
    {canPreview ? <img alt={file.path.split("/").at(-1)} src={previewUrl} /> : <div className="binary-icon" aria-hidden="true">01</div>}
    <h1>{file.path.split("/").at(-1)}</h1>
    <dl><dt>Type</dt><dd>{file.mime}</dd><dt>Size</dt><dd>{formatBytes(file.sizeBytes)}</dd><dt>Generation</dt><dd>{file.generation}</dd></dl>
    {file.previewKind === "image" && Number(file.sizeBytes) > IMAGE_PREVIEW_LIMIT_BYTES && <p>Image preview is limited to 25 MiB.</p>}
    <button className="primary" onClick={onDownload} type="button">Download…</button>
  </section>;
}

function EmptyTab({ tab, detail, download }: { tab: AppOwnedTab; detail: string; download?: () => void }) {
  return <section className="app-tab-surface" role="tabpanel" aria-label={tab.title}>
    <div className={`app-tab-icon ${tab.kind}`} aria-hidden="true">{tab.kind === "gitDiff" ? "±" : tab.kind === "markdown" ? "M" : "F"}</div>
    <h1>{tab.title}</h1><code>{tab.resource}</code><p>{detail}</p>{download && <button onClick={download} type="button">Download…</button>}
  </section>;
}

function modelPath(tab: AppOwnedTab): string {
  return `tmux-ide://${encodeURIComponent(tab.hostProfileId)}/${encodeURIComponent(tab.serverIdentity)}/${encodeURIComponent(tab.sessionId)}${tab.resource}`;
}

function languageForPath(path: string): string {
  const extension = path.split(".").at(-1)?.toLowerCase();
  return ({ ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript", rs: "rust", py: "python", md: "markdown", json: "json", css: "css", html: "html", sh: "shell", yml: "yaml", yaml: "yaml", toml: "ini" } as Record<string, string>)[extension ?? ""] ?? "plaintext";
}

function formatBytes(value: string): string {
  const bytes = Number(value);
  if (!Number.isFinite(bytes)) return `${value} bytes`;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
}

function parentPath(path: string): string {
  const index = path.lastIndexOf("/");
  return index <= 0 ? "/" : path.slice(0, index);
}
