import { invoke } from "@tauri-apps/api/core";
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import { CLICK_SLOP_PX, useSanitizedMarkdown } from "../files/markdownPreview";
import { renderSafeSvg } from "../files/markdown";
import { useOpenFileTab } from "../files/useOpenFileTab";
import { IMAGE_PREVIEW_LIMIT_BYTES, type ActiveRoot, type BinaryFile, type FileWorkspaceClient, type FileWorkspaceScope } from "../files/types";
import { DelayedLoading } from "../../ui/DelayedLoading";
import { SurfaceError } from "../../ui/SurfaceError";
import type { SaveState } from "../files/autosave";
import { useVisibleSaveState } from "../files/saveStatus";
import type { AppOwnedTab } from "./types";
import { useEditorPaint } from "../../perf/surfacePaint";

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
  const { content, editorRequested, view } = file;
  const editor = useEditorPaint(file.paint, editorRequested, true);
  const editFile = file.edit;
  const canWrite = props.canWrite;
  const onEditorChange = useCallback((typed: string) => {
    if (canWrite) editFile(typed);
  }, [canWrite, editFile]);

  if (!root) return <EmptyTab tab={props.tab} detail="Reconnect and select a terminal pane to reopen this file." />;
  if (!props.scope && content.kind === "loading") {
    return <EmptyTab tab={props.tab} detail="Reconnect and select a terminal pane to reopen this file." />;
  }
  const download = () => props.onDownload(props.tab.resource, "file", root);
  const toolbar = (state: SaveState | undefined) => <EditorToolbar
    canWrite={props.canWrite}
    download={download}
    mode={mode}
    onViewMode={props.onViewMode}
    saveState={state}
    tab={props.tab}
  />;
  switch (content.kind) {
    // The frame the file is about to appear in, not a card in the middle of
    // the tab. Reading a file used to move the tab through a centred card and
    // then into this frame, so every open flickered through two layouts; the
    // toolbar is the real one, and only the content area is still empty.
    case "loading": return <section
      aria-label={props.tab.title}
      className="file-tab-surface"
      data-view-mode={props.tab.kind === "markdown" ? mode : undefined}
      role="tabpanel"
    >
      {toolbar(undefined)}
      {/* Top-left of the content area, the same spot the editor stage's own
          fallback uses: a read that resolves fast shows nothing at all, and a
          slow one shows a single line that does not move when the read
          finishes and the editor chunk takes over the waiting. */}
      <DelayedLoading detail="Loading…" />
    </section>;
    case "failed": return <EmptyTab tab={props.tab} detail={content.detail} download={download} />;
    case "changed": return <EmptyTab tab={props.tab} detail={`The file changed or became unavailable: ${content.detail}`} download={download} />;
    case "unavailable": return <EmptyTab tab={props.tab} detail="File unavailable." />;
    case "binary": return <BinarySurface file={content.file} onDownload={download} />;
    case "tooLarge": return <EmptyTab tab={props.tab} detail="This text file is larger than the 10 MiB editor limit." download={download} />;
  }

  const source = view?.content ?? content.file.content;
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
    {toolbar(view?.state ?? "saved")}
    {view?.error && <SurfaceError className="editor-error" detail={view.error} />}
    {editorRequested && <div className="monaco-host" ref={editor.bindHost}>
      <Suspense fallback={<DelayedLoading detail="Loading…" />}>
        <FileEditor
          modelPath={modelPath(props.tab)}
          onChange={onEditorChange}
          onReady={editor.onReady}
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

/**
 * The bar across the top of a file tab, drawn the same whether the file has
 * arrived or not.
 *
 * `saveState` undefined means the file is still being read: the chip holds its
 * place in the row without claiming a state the tab cannot know yet.
 */
function EditorToolbar({ canWrite, download, mode, onViewMode, saveState, tab }: {
  canWrite: boolean;
  download(): void;
  mode: "source" | "preview" | "split";
  onViewMode(mode: "source" | "preview" | "split"): void;
  saveState: SaveState | undefined;
  tab: AppOwnedTab;
}) {
  const visibleSaveState = useVisibleSaveState(saveState, canWrite);
  return <header className="editor-toolbar">
    <code title={tab.resource}>{tab.resource}</code>
    <span className={`save-state${visibleSaveState ? ` ${visibleSaveState}` : ""}`} role="status">
      {visibleSaveState === "saving" ? "Saving…" : visibleSaveState === "dirty" ? "Unsaved" : visibleSaveState === "error" ? "Save failed" : ""}
    </span>
    {tab.kind === "markdown" && <div aria-label="Markdown view" className="markdown-modes" role="group">
      {(["source", "preview", "split"] as const).map((item) => <button aria-pressed={mode === item} key={item} onClick={() => onViewMode(item)} type="button">{item}</button>)}
    </div>}
    <button onClick={download} type="button">Download…</button>
  </header>;
}

/**
 * The rendered article, which is also ordinary selectable document text.
 *
 * Two things used to fight the user's selection. The preview republishes
 * sanitized HTML whenever the buffer changes, and in split mode the buffer
 * changes on every keystroke and on every disk reload — replacing the subtree
 * out from under a live selection collapses it. And the delegated link handler
 * ran on any click, so releasing a drag that happened to end inside a link
 * swallowed the gesture and opened the link instead of leaving the text
 * selected. The gesture is tracked here and handed to the hook, which parks a
 * finished sanitize until the selection is gone.
 */
export function MarkdownPreview({ source, onStatus }: { source: string; onStatus(message: string): void }) {
  const article = useRef<HTMLElement>(null);
  const [selecting, setSelecting] = useState(false);
  // Where the press landed, so a release far from it reads as a drag.
  const pressedAt = useRef<{ x: number; y: number }>(undefined);
  const html = useSanitizedMarkdown(source, { held: selecting, container: article });
  const [externalUrl, setExternalUrl] = useState<string>();

  // On the document, not the article: a drag that leaves the preview still ends
  // somewhere, and a gesture whose end is never seen would hold the preview
  // frozen for the rest of the session. `blur` is the backstop for an
  // interruption that takes the window without delivering a pointer event.
  useEffect(() => {
    if (!selecting) return;
    const end = () => setSelecting(false);
    document.addEventListener("pointerup", end);
    document.addEventListener("pointercancel", end);
    window.addEventListener("blur", end);
    return () => {
      document.removeEventListener("pointerup", end);
      document.removeEventListener("pointercancel", end);
      window.removeEventListener("blur", end);
    };
  }, [selecting]);

  return <><article className="markdown-preview" ref={article} onPointerDown={(event) => {
    if (event.button !== 0) return;
    pressedAt.current = { x: event.clientX, y: event.clientY };
    setSelecting(true);
  }} onClick={(event) => {
    const pressed = pressedAt.current;
    pressedAt.current = undefined;
    const anchor = (event.target as HTMLElement).closest("a");
    const href = anchor?.getAttribute("href");
    if (!href) return;
    // Unconditional, and before the gesture tests below. A click's default
    // action is activation, not selection — the selection was settled back at
    // pointerup — so suppressing it costs the drag nothing, while letting a
    // relative href through navigates the whole webview off the app and takes
    // every tab, terminal and unsaved buffer with it.
    event.preventDefault();
    // A release that travelled is a selection gesture, even when it ends on a
    // link, and a selection still standing at click time is one the user just
    // made. `detail` is 0 for a keyboard activation, which has no coordinates
    // to compare and must not be measured against a stale press.
    if (event.detail > 0 && pressed && Math.hypot(event.clientX - pressed.x, event.clientY - pressed.y) > CLICK_SLOP_PX) return;
    const selection = document.getSelection?.();
    if (selection && !selection.isCollapsed) return;
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

function BinarySurface({ file, onDownload }: { file: BinaryFile; onDownload(): void }) {
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
