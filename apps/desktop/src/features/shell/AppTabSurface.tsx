import { invoke } from "@tauri-apps/api/core";
import { lazy, Suspense, useCallback, useEffect, useMemo, useState } from "react";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import { useSanitizedMarkdown } from "../files/markdownPreview";
import { renderSafeSvg } from "../files/markdown";
import { useOpenFileTab } from "../files/useOpenFileTab";
import { IMAGE_PREVIEW_LIMIT_BYTES, TEXT_FILE_LIMIT_BYTES, type ActiveRoot, type FileWorkspaceClient, type FileWorkspaceScope, type OpenFile } from "../files/types";
import { SurfaceError } from "../../ui/SurfaceError";
import type { AppOwnedTab } from "./types";
import { recordPerfMilestone } from "../../perf/probe";
import { useEditorSurface } from "../../perf/surfacePaint";

/**
 * The editor bundle, fetched when a text file is actually going to be shown.
 *
 * Nothing above this line imports Monaco, which is the point: the remote read
 * starts on mount, and a binary file, an oversized file, a disconnected tab or
 * a Markdown preview never fetches the editor at all.
 */
const FileEditor = lazy(() => import("../files/FileEditor").then((module) => ({ default: module.FileEditor })));

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

const recordEditorPaint = () => recordPerfMilestone("editor.paint");

export function AppTabSurface(props: Props) {
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
  const mode = props.tab.kind === "markdown" ? props.tab.viewMode ?? "split" : "source";
  const editorSurface = useEditorSurface();
  // Everything about when this file is read, written and reconciled, including
  // the cancellation that spans it. This component owns only what is drawn.
  const file = useOpenFileTab({
    client: props.client,
    scope: props.scope,
    root,
    resource: props.tab.resource,
    tabId: props.tab.id,
    editorVisible: mode !== "preview",
    onDirty: props.onDirty,
    onStatus: props.onStatus,
  });
  const { editorRequested, opened, paint, view } = file;

  useEffect(() => {
    if (!editorRequested) return;
    paint.noteCommitted();
    paint.notePaintable(editorSurface.facts, recordEditorPaint);
  }, [editorRequested, editorSurface.facts, paint]);

  const onEditorReady = useCallback(() => {
    editorSurface.noteReady();
    paint.notePaintable(editorSurface.facts, recordEditorPaint);
  }, [editorSurface, paint]);
  const editFile = file.edit;
  const canWrite = props.canWrite;
  const onEditorChange = useCallback((content: string) => {
    if (canWrite) editFile(content);
  }, [canWrite, editFile]);

  if (!props.scope || !root) return <EmptyTab tab={props.tab} detail="Reconnect and select a terminal pane to reopen this file." />;
  if (file.loading) return <EmptyTab tab={props.tab} detail="Loading file…" />;
  if (file.error && !opened) return <EmptyTab tab={props.tab} detail={file.error} download={() => props.onDownload(props.tab.resource, "file", root)} />;
  if (file.error) return <EmptyTab tab={props.tab} detail={`The file changed or became unavailable: ${file.error}`} download={() => props.onDownload(props.tab.resource, "file", root)} />;
  if (!opened) return <EmptyTab tab={props.tab} detail="File unavailable." />;
  if (opened.kind === "binary") return <BinarySurface file={opened.file} onDownload={() => props.onDownload(props.tab.resource, "file", root)} />;
  if (Number(opened.file.sizeBytes) > TEXT_FILE_LIMIT_BYTES) return <EmptyTab tab={props.tab} detail="This text file is larger than the 10 MiB editor limit." download={() => props.onDownload(props.tab.resource, "file", root)} />;

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
    {editorRequested && <div className="monaco-host" ref={editorSurface.bindHost}>
      <Suspense fallback={<p className="quiet-empty">Loading editor…</p>}>
        <FileEditor
          modelPath={modelPath(props.tab)}
          onChange={onEditorChange}
          onReady={onEditorReady}
          path={props.tab.resource}
          readOnly={!props.canWrite}
          value={source}
          wordWrap={props.tab.kind === "markdown"}
        />
      </Suspense>
    </div>}
    {props.tab.kind === "markdown" && mode !== "source" && <MarkdownPreview source={source} onStatus={props.onStatus} />}
  </section>;
}

function MarkdownPreview({ source, onStatus }: { source: string; onStatus(message: string): void }) {
  const html = useSanitizedMarkdown(source);
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

function formatBytes(value: string): string {
  const bytes = Number(value);
  if (!Number.isFinite(bytes)) return `${value} bytes`;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
}
