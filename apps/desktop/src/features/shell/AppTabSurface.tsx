import Editor from "@monaco-editor/react";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef, useState } from "react";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import { AutosaveController, type AutosaveView } from "../files/autosave";
import { editorFlushRegistry } from "../files/editorFlushRegistry";
import { renderSafeMarkdown, renderSafeSvg } from "../files/markdown";
import { IMAGE_PREVIEW_LIMIT_BYTES, TEXT_FILE_LIMIT_BYTES, type ActiveRoot, type FileWorkspaceClient, type FileWorkspaceScope, type OpenFile } from "../files/types";
import { SurfaceError } from "../../ui/SurfaceError";
import type { AppOwnedTab } from "./types";
import { ADE_MONACO_THEME } from "../files/monaco";

interface Props {
  tab: AppOwnedTab;
  scope?: FileWorkspaceScope;
  activeRoot?: ActiveRoot;
  client: FileWorkspaceClient;
  canWrite: boolean;
  onDownload(path: string, kind: "file" | "folder", root: ActiveRoot): void;
  onStatus(message: string): void;
  onViewMode(mode: "source" | "preview" | "split"): void;
}

export function AppTabSurface(props: Props) {
  const [opened, setOpened] = useState<OpenFile>();
  const [view, setView] = useState<AutosaveView>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const controller = useRef<AutosaveController | undefined>(undefined);
  const loadSerial = useRef(0);
  const loadAbort = useRef<AbortController | undefined>(undefined);
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

  const load = async (externalOperationId?: string) => {
    if (!props.scope || !root) return;
    loadAbort.current?.abort();
    const abort = new AbortController();
    loadAbort.current = abort;
    const serial = ++loadSerial.current;
    try {
      const next = await props.client.openFile(props.scope, root, props.tab.resource, abort.signal);
      if (serial !== loadSerial.current) return;
      setOpened(next);
      setError(undefined);
      if (next.kind === "text") {
        const snapshot = { content: next.file.content, generation: next.file.generation, lineEnding: next.file.lineEnding };
        if (controller.current) {
          // Polling directory snapshots also observe our own atomic rename.
          // Preserve a newer local edit when disk still has the generation we
          // already know; any genuinely newer generation remains last-writer.
          if (!externalOperationId && controller.current.current().generation === snapshot.generation) return;
          controller.current.external(snapshot, externalOperationId);
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
      if (abort.signal.aborted) return;
      if (serial === loadSerial.current) setError(String(cause));
    } finally {
      if (serial === loadSerial.current) setLoading(false);
    }
  };

  useEffect(() => {
    setLoading(true);
    setOpened(undefined);
    setView(undefined);
    controller.current?.dispose();
    controller.current = undefined;
    void load();
    return () => {
      loadSerial.current += 1;
      loadAbort.current?.abort();
      if (controller.current) {
        const pending = controller.current.flush();
        editorFlushRegistry.track(pending);
        void pending.catch((saveError) => props.onStatus(`Could not save ${props.tab.resource}: ${String(saveError)}`));
      }
      controller.current?.dispose();
      controller.current = undefined;
    };
  }, [props.client, props.scope?.clientId, props.tab.resource, root?.token]);

  useEffect(() => editorFlushRegistry.register(props.tab.id, async () => {
    await controller.current?.flush();
  }), [props.tab.id]);

  useEffect(() => {
    if (!props.scope || !root) return;
    let disposed = false;
    let release: (() => void) | undefined;
    void props.client.acquireDirectoryWatch(props.scope, root, parentPath(props.tab.resource)).then((next) => {
      if (disposed) next.release();
      else {
        release = next.release;
        // Re-read after the watch is armed. This closes the initial
        // read-before-watch gap without relying on a later filesystem event.
        void load();
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
      if (event.kind === "directoryChanged" && event.rootToken === root.token && event.directory === parentPath(props.tab.resource)) {
        // Directory snapshots are an overflow/fallback signal and do not carry
        // the writer operation. Never let a generic self-save echo replace a
        // newer dirty edit; precise path events below retain last-writer order.
        const state = controller.current?.current().state;
        if (state !== "dirty" && state !== "saving") void load();
        return;
      }
      if (!(event.kind === "fileChanged" || event.kind === "fileDeleted") || event.path !== props.tab.resource) return;
      if (event.kind === "fileDeleted") {
        setError("The file was deleted externally. The tab remains open.");
        return;
      }
      void load(event.operationId);
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
    {mode !== "preview" && <div className="monaco-host">
      <Editor
        language={languageForPath(props.tab.resource)}
        onChange={(content) => { if (props.canWrite && typeof content === "string") controller.current?.edit(content, opened.file.lineEnding); }}
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
