import { getCurrentWebview, type DragDropEvent as DragDropPayload } from "@tauri-apps/api/webview";
import { useEffect, useId, useRef, useState, type ClipboardEvent, type DragEvent, type ReactNode, type RefObject } from "react";
import { useModalDialog } from "../../commands/useModalDialog";
import type { TerminalTransferClient, TerminalTransferProgress, TerminalTransferScope, UploadCollisionPolicy, UploadPreflight } from "./terminalTransfers";
import {
  encodeImageAsPng,
  joinShellEscapedPaths,
  parseCopiedFileList,
  parseFileUriList,
  requiresLargeUploadConfirmation,
  sameTerminalTransferScope,
  supportedClipboardImageType,
  uploadInOriginalOrder,
  validateAgentImagePath,
} from "./terminalTransfers";
import { canCancelTransfer, isTerminalTransferState, transferStateLabel } from "../transfers/transferState";
import { SurfaceError } from "../../ui/SurfaceError";
import { useTerminalTransferRegistry, type TerminalTransferRegistry } from "./terminalTransferRegistry";
import { readInternalPathDrop } from "./internalPathDrag";

interface PendingReview {
  items: UploadPreflight[];
  imagePng: boolean;
  large: boolean;
  collisions: boolean;
}

interface ActiveBatch {
  readonly scope: TerminalTransferScope;
  readonly abortController: AbortController;
  /**
   * Every transfer this batch has been told about. Dismissing a delivered
   * result is scoped to these, so a paste cannot clear the record an *earlier*
   * batch left behind when its own paste was refused.
   */
  readonly transferIds: Set<string>;
}

export interface TerminalTransferSurfaceController {
  pasteClipboard(): Promise<boolean>;
}

/**
 * Whether a Tauri drag-drop point lands on this element.
 *
 * Tauri types the point as a `PhysicalPosition`, and it is one on Windows. On
 * the two platforms this app ships it is not: wry takes the macOS point from
 * `NSDraggingInfo.draggingLocation` against `NSView.frame`, both AppKit points,
 * and the GTK one from `drag-motion`/`drag-drop` widget coordinates. Both are
 * logical — the same units `getBoundingClientRect` reports.
 *
 * Dividing by `devicePixelRatio` therefore halved every drop coordinate on a
 * Retina display, so the hit test rejected the drop and nothing was uploaded:
 * on a split, a right-hand pane needed a cursor position outside the window to
 * pass, and could never be dropped onto at all.
 */
export function pointIsInside(
  element: Pick<HTMLElement, "getBoundingClientRect">,
  point: { x: number; y: number },
): boolean {
  const rect = element.getBoundingClientRect();
  return point.x >= rect.left && point.x <= rect.right && point.y >= rect.top && point.y <= rect.bottom;
}

export function basename(path: string): string {
  const trimmed = path.replace(/\/+$/u, "");
  return trimmed.slice(trimmed.lastIndexOf("/") + 1) || "upload";
}

export function TerminalTransferSurface({
  target,
  client,
  scope,
  onPaste,
  onDiagnostic,
  onController,
  registry,
  children,
}: {
  target: RefObject<HTMLElement | null>;
  client: TerminalTransferClient;
  scope?: TerminalTransferScope;
  onPaste(value: string): void;
  onDiagnostic?(message: string): void;
  onController?(controller: TerminalTransferSurfaceController | undefined): void;
  registry?: TerminalTransferRegistry;
  children: ReactNode;
}) {
  const [dragging, setDragging] = useState(false);
  const [review, setReview] = useState<PendingReview>();
  const localRegistry = useTerminalTransferRegistry();
  const transferRegistry = registry ?? localRegistry;
  const [error, setError] = useState<string>();
  const busy = useRef(false);
  const mounted = useRef(true);
  const reviewResolve = useRef<((policy?: UploadCollisionPolicy) => void) | undefined>(undefined);
  const scopeRef = useRef(scope);
  const activeBatchRef = useRef<ActiveBatch | undefined>(undefined);
  scopeRef.current = scope;

  const abortBatch = (batch: ActiveBatch) => {
    if (!batch.abortController.signal.aborted) batch.abortController.abort();
    reviewResolve.current?.();
    reviewResolve.current = undefined;
  };

  const batchIsCurrent = (batch: ActiveBatch) => mounted.current
    && activeBatchRef.current === batch
    && !batch.abortController.signal.aborted
    && sameTerminalTransferScope(batch.scope, scopeRef.current);

  const assertCurrentBatch = (batch: ActiveBatch) => {
    if (!batchIsCurrent(batch)) throw new DOMException("Terminal transfer scope changed.", "AbortError");
  };

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (activeBatchRef.current) abortBatch(activeBatchRef.current);
    };
  }, [client]);

  useEffect(() => {
    const batch = activeBatchRef.current;
    if (batch && !sameTerminalTransferScope(batch.scope, scope)) {
      abortBatch(batch);
      setDragging(false);
      setReview(undefined);
      setError(undefined);
    }
  }, [scope?.clientId, scope?.hostProfileId, scope?.serverIdentity, scope?.connectionEpoch, scope?.mode, scope?.paneId, scope?.renderLifetime]);

  const fail = (reason: unknown) => {
    if (reason instanceof DOMException && reason.name === "AbortError") return;
    const message = reason instanceof Error ? reason.message : String(reason);
    if (message === "Upload cancelled.") {
      if (mounted.current) setError(undefined);
      onDiagnostic?.(message);
      return;
    }
    if (mounted.current) setError(message);
    onDiagnostic?.(message);
  };

  const updateProgress = (batch: ActiveBatch, progress: TerminalTransferProgress) => {
    batch.transferIds.add(progress.id);
    transferRegistry.record(batch.scope, progress);
  };

  const requestReview = (batch: ActiveBatch, pending: PendingReview) => new Promise<UploadCollisionPolicy | undefined>((resolve) => {
    assertCurrentBatch(batch);
    reviewResolve.current = resolve;
    setReview(pending);
  });

  const startRemoteBatch = async (batch: ActiveBatch, items: UploadPreflight[], imagePng: boolean) => {
    assertCurrentBatch(batch);
    const large = requiresLargeUploadConfirmation(items);
    const names = new Set<string>();
    let duplicateNames = false;
    for (const item of items) {
      if (names.has(item.name)) duplicateNames = true;
      names.add(item.name);
    }
    const collisions = duplicateNames || items.some((item) => item.collision);
    let collision: UploadCollisionPolicy = "fail";
    if (large || collisions) {
      const reviewed = await requestReview(batch, { items, imagePng, large, collisions });
      assertCurrentBatch(batch);
      if (!reviewed) throw new DOMException("Upload cancelled.", "AbortError");
      collision = reviewed;
    }
    const verified = await uploadInOriginalOrder(
      items,
      (item, _index, onProgress) => client.start(
        batch.scope,
        item.sourcePath,
        item.name,
        { collision, largeUploadConfirmed: large, imagePng },
        onProgress,
      ),
      (id) => client.cancel(id),
      (progress) => updateProgress(batch, progress),
      batch.abortController.signal,
    );
    assertCurrentBatch(batch);
    const destinations = verified.map((item) => item.destination);
    onPaste(imagePng
      ? destinations.map(validateAgentImagePath).join(" ")
      : joinShellEscapedPaths(destinations));
    // Delivered: the paths are in the pane. Only now, and only for a batch that
    // reached this line — a completion whose paste was refused stays visible.
    transferRegistry.dismissDelivered(batch.scope, [...batch.transferIds]);
  };

  const acceptPaths = async (paths: readonly string[], imagePng = false) => {
    if (!scope) throw new Error("File paste requires a live terminal connection.");
    if (busy.current) throw new Error("Wait for the current terminal transfer batch to finish.");
    if (paths.length === 0) return;
    busy.current = true;
    const batch: ActiveBatch = {
      scope: { ...scope },
      abortController: new AbortController(),
      transferIds: new Set(),
    };
    activeBatchRef.current = batch;
    setError(undefined);
    try {
      if (batch.scope.mode === "local" && !imagePng) {
        const inspected = await client.inspectLocalPaths(paths);
        assertCurrentBatch(batch);
        onPaste(joinShellEscapedPaths(inspected.map((item) => item.path)));
        return;
      }
      const items: UploadPreflight[] = [];
      // Preflight uses a short-lived independent bulk connection today, so it
      // is serialized and never competes with the backend's two transfer slots.
      for (const path of paths) {
        items.push(await client.preflight(batch.scope, path, basename(path), {
          // Rename preflight is non-destructive and tells the review UI whether
          // the requested staging basename collides. Start still uses the exact
          // user-reviewed policy below.
          collision: "rename", largeUploadConfirmed: true, imagePng,
        }, (progress) => updateProgress(batch, progress), batch.abortController.signal));
        assertCurrentBatch(batch);
      }
      // The preflights have become uploads; their completions are no longer
      // what the list should be showing.
      transferRegistry.dismissDelivered(batch.scope, [...batch.transferIds]);
      await startRemoteBatch(batch, items, imagePng);
    } catch (reason) {
      if (!batchIsCurrent(batch)) throw new DOMException("Terminal transfer scope changed.", "AbortError");
      throw reason;
    } finally {
      if (activeBatchRef.current === batch) {
        activeBatchRef.current = undefined;
        busy.current = false;
      }
    }
  };

  const acceptImage = async (file: Blob) => {
    if (!scope) throw new Error("Image paste requires a live terminal connection.");
    if (busy.current) throw new Error("Wait for the current terminal transfer batch to finish.");
    busy.current = true;
    const batch: ActiveBatch = {
      scope: { ...scope },
      abortController: new AbortController(),
      transferIds: new Set(),
    };
    activeBatchRef.current = batch;
    setError(undefined);
    try {
      const png = await encodeImageAsPng(file);
      assertCurrentBatch(batch);
      const staged = await client.stageClipboardPng(png);
      assertCurrentBatch(batch);
      if (batch.scope.mode === "local") {
        onPaste(validateAgentImagePath(staged.path));
        return;
      }
      const item = await client.preflight(batch.scope, staged.path, staged.name, {
        collision: "rename", largeUploadConfirmed: true, imagePng: true,
      }, (progress) => updateProgress(batch, progress), batch.abortController.signal);
      assertCurrentBatch(batch);
      transferRegistry.dismissDelivered(batch.scope, [...batch.transferIds]);
      await startRemoteBatch(batch, [item], true);
    } catch (reason) {
      if (!batchIsCurrent(batch)) throw new DOMException("Terminal transfer scope changed.", "AbortError");
      throw reason;
    } finally {
      if (activeBatchRef.current === batch) {
        activeBatchRef.current = undefined;
        busy.current = false;
      }
    }
  };

  const acceptStagedImage = async (staged: { path: string; sizeBytes: string; name: string }) => {
    if (!scope) throw new Error("Image paste requires a live terminal connection.");
    if (busy.current) throw new Error("Wait for the current terminal transfer batch to finish.");
    busy.current = true;
    const batch: ActiveBatch = {
      scope: { ...scope },
      abortController: new AbortController(),
      transferIds: new Set(),
    };
    activeBatchRef.current = batch;
    setError(undefined);
    try {
      if (batch.scope.mode === "local") {
        onPaste(validateAgentImagePath(staged.path));
        return;
      }
      const item = await client.preflight(batch.scope, staged.path, staged.name, {
        collision: "rename", largeUploadConfirmed: true, imagePng: true,
      }, (progress) => updateProgress(batch, progress), batch.abortController.signal);
      assertCurrentBatch(batch);
      transferRegistry.dismissDelivered(batch.scope, [...batch.transferIds]);
      await startRemoteBatch(batch, [item], true);
    } catch (reason) {
      if (!batchIsCurrent(batch)) throw new DOMException("Terminal transfer scope changed.", "AbortError");
      throw reason;
    } finally {
      if (activeBatchRef.current === batch) {
        activeBatchRef.current = undefined;
        busy.current = false;
      }
    }
  };

  // Read through a ref by the one long-lived listener below, and replaced after
  // every commit. The handler closes over `scope` and `acceptPaths`, both new
  // objects on every render, so making it the effect's dependency tore down four
  // Tauri listeners and re-registered four more — over IPC, with a gap in which
  // nothing was listening — on every render of every pane. `useHostLatency`
  // ticks every five seconds, so that ran continuously. Assigned in an effect
  // rather than during render: a render React discards must not leave a handler
  // behind that closes over state it threw away.
  const nativeDragDropRef = useRef<{
    handle(payload: DragDropPayload): void;
    fail(reason: unknown): void;
  }>({ handle: () => undefined, fail: () => undefined });
  const handleNativeDragDrop = (payload: DragDropPayload) => {
    if (!target.current) return;
    if (payload.type === "leave") return setDragging(false);
    // `enter` fires once, when the cursor crosses the *window*, so a drag that
    // begins over one pane and ends over another would light up the pane it
    // entered and leave the pane it landed on dark. Every position the drag
    // reports is re-tested, so the highlight follows the cursor.
    const inside = pointIsInside(target.current, payload.position);
    if (payload.type === "enter" || payload.type === "over") return setDragging(inside);
    setDragging(false);
    if (inside) void acceptPaths(payload.paths).catch(fail);
  };
  useEffect(() => {
    nativeDragDropRef.current = { handle: handleNativeDragDrop, fail };
  });

  useEffect(() => {
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void getCurrentWebview().onDragDropEvent((event) => {
      if (!disposed) nativeDragDropRef.current.handle(event.payload);
    }).then((release) => {
      if (disposed) release();
      else unlisten = release;
    }).catch((reason) => nativeDragDropRef.current.fail(reason));
    return () => { disposed = true; unlisten?.(); };
  }, [target]);

  const onPasteCapture = (event: ClipboardEvent<HTMLElement>) => {
    const copiedFiles = event.clipboardData.getData("x-special/gnome-copied-files");
    const uriList = copiedFiles || event.clipboardData.getData("text/uri-list");
    if (uriList) {
      event.preventDefault();
      try { void acceptPaths(copiedFiles ? parseCopiedFileList(uriList) : parseFileUriList(uriList)).catch(fail); } catch (reason) { fail(reason); }
      return;
    }
    const files = Array.from(event.clipboardData.files);
    const paths = filePaths(files);
    if (paths.length) {
      event.preventDefault();
      void acceptPaths(paths).catch(fail);
      return;
    }
    const image = soleSupportedImage(files, event.clipboardData.items);
    if (image) {
      event.preventDefault();
      void acceptImage(image).catch(fail);
      return;
    }
  };

  useEffect(() => {
    if (!onController) return;
    const controller: TerminalTransferSurfaceController = {
      pasteClipboard: async () => {
        try {
          if (client.readNativeClipboard) {
            const native = await client.readNativeClipboard();
            if (native?.kind === "files") {
              await acceptPaths(parseFileUriList(native.uris.join("\n")));
              return true;
            }
            if (native?.kind === "image") {
              await acceptStagedImage(native.staged);
              return true;
            }
            // WebKit refuses `navigator.clipboard` reads for content the page
            // did not write itself, so without this rung pasting from another
            // application silently did nothing (M10-E054).
            if (native?.kind === "text") {
              onPaste(native.text);
              return true;
            }
          }
          if (!navigator.clipboard?.read) return false;
          let items: ClipboardItems;
          try { items = await navigator.clipboard.read(); } catch { return false; }
          const paths: string[] = [];
          for (const item of items) {
            const uriType = item.types.includes("x-special/gnome-copied-files")
              ? "x-special/gnome-copied-files"
              : item.types.includes("text/uri-list") ? "text/uri-list" : undefined;
            if (uriType) {
              const value = await (await item.getType(uriType)).text();
              paths.push(...(uriType === "x-special/gnome-copied-files" ? parseCopiedFileList(value) : parseFileUriList(value)));
            }
          }
          if (paths.length) {
            await acceptPaths(paths);
            return true;
          }
          const imageItems = items.flatMap((item) => {
            const type = supportedClipboardImageType(item.types);
            return type ? [{ item, type }] : [];
          });
          if (imageItems.length === 1) {
            await acceptImage(await imageItems[0].item.getType(imageItems[0].type));
            return true;
          }
          return false;
        } catch (reason) {
          fail(reason);
          return true;
        }
      },
    };
    onController(controller);
    return () => onController(undefined);
  }, [onController, scope]);

  const onDrop = (event: DragEvent<HTMLElement>) => {
    event.preventDefault();
    setDragging(false);
    const internal = readInternalPathDrop(event.dataTransfer, scope?.serverIdentity);
    if (internal.kind === "accepted") {
      setError(undefined);
      onPaste(internal.shellText);
      return;
    }
    if (internal.kind === "rejected") {
      fail(new Error(internal.reason));
      return;
    }
    const copiedFiles = event.dataTransfer.getData("x-special/gnome-copied-files");
    const uriList = copiedFiles || event.dataTransfer.getData("text/uri-list");
    if (uriList) {
      try { void acceptPaths(copiedFiles ? parseCopiedFileList(uriList) : parseFileUriList(uriList)).catch(fail); } catch (reason) { fail(reason); }
      return;
    }
    const files = Array.from(event.dataTransfer.files);
    const paths = filePaths(files);
    if (paths.length) void acceptPaths(paths).catch(fail);
    else {
      const image = soleSupportedImage(files, event.dataTransfer.items);
      if (image) void acceptImage(image).catch(fail);
      else fail(new Error("The WebView did not provide file paths. Use the native desktop file drop surface."));
    }
  };

  return <div
      className={`terminal-transfer-surface${dragging ? " dragging" : ""}`}
      onDragEnter={(event) => { event.preventDefault(); setDragging(true); }}
      onDragLeave={() => setDragging(false)}
      onDragOver={(event) => event.preventDefault()}
      onDrop={onDrop}
      onPasteCapture={onPasteCapture}
    >
    {children}
    {dragging && <div className="terminal-drop-hint" role="status">Drop files to paste paths</div>}
    {!registry && <TerminalTransferHistory client={client} onError={fail} registry={transferRegistry} />}
    {/* Dismissible, because nothing else clears it: it survives until the next
        transfer starts, and a failed paste is not followed by one. */}
    {error && <SurfaceError className="terminal-transfer-error" detail={error} onDismiss={() => setError(undefined)} />}
    {review && <UploadReviewDialog pending={review} onChoose={(policy) => {
      const resolve = reviewResolve.current;
      reviewResolve.current = undefined;
      setReview(undefined);
      resolve?.(policy);
    }} />}
  </div>;
}

function filePaths(files: readonly File[]): string[] {
  return files
    .map((file) => (file as File & { path?: string }).path)
    .filter((path): path is string => Boolean(path));
}

function soleSupportedImage(files: readonly File[], items?: DataTransferItemList): Blob | undefined {
  if (files.length === 1 && supportedClipboardImageType([files[0].type])) return files[0];
  if (files.length !== 0 || !items) return undefined;
  const candidates = Array.from(items).filter((item) => item.kind === "file" && supportedClipboardImageType([item.type]));
  return candidates.length === 1 ? candidates[0].getAsFile() ?? undefined : undefined;
}

function UploadReviewDialog({ pending, onChoose }: { pending: PendingReview; onChoose(policy?: UploadCollisionPolicy): void }) {
  const [collision, setCollision] = useState<UploadCollisionPolicy>(pending.collisions ? "rename" : "fail");
  const titleId = useId();
  const dialog = useModalDialog<HTMLFormElement>(() => onChoose());
  return <div className="modal-backdrop" role="presentation"><form
    aria-labelledby={titleId}
    aria-modal="true"
    className="confirmation terminal-upload-review"
    onSubmit={(event) => { event.preventDefault(); onChoose(collision); }}
    ref={dialog}
    role="dialog"
  >
    <h2 id={titleId}>Review terminal upload</h2>
    <p>{pending.items.length} file{pending.items.length === 1 ? "" : "s"} will be staged and pasted only after verification.</p>
    <ul>{pending.items.map((item) => <li key={item.sourcePath}><code>{item.name}</code> · {formatBytes(item.sizeBytes)}{item.collision ? " · destination exists" : ""}</li>)}</ul>
    {pending.large && <div className="download-warning" role="alert">At least one upload is larger than 500 MiB. Starting it explicitly confirms the large transfer.</div>}
    {pending.collisions && <label>Destination collision<select aria-label="Upload collision behavior" onChange={(event) => setCollision(event.target.value as UploadCollisionPolicy)} value={collision}>
      <option value="rename">Keep both with a new name</option>
      <option value="fail">Stop without overwriting</option>
      <option value="overwriteConfirmed">Replace after verification</option>
    </select></label>}
    {collision === "overwriteConfirmed" && <div className="download-warning" role="alert">Existing destination files will be replaced only after byte-count and digest verification.</div>}
    <div className="dialog-actions"><button onClick={() => onChoose()} type="button">Cancel</button><button className="primary" type="submit">Start upload</button></div>
  </form></div>;
}

export function TerminalTransferHistory({ registry, client, onError }: {
  registry: TerminalTransferRegistry;
  client: TerminalTransferClient;
  onError?(error: unknown): void;
}) {
  if (registry.records.length === 0) return null;
  return <section aria-label="Terminal uploads" aria-live="polite" className="terminal-upload-progress">
    <h3>Terminal uploads</h3>
    {registry.records.map((record) => {
      const transfer = record.progress;
      const percent = transfer.totalBytes ? progressPercent(transfer.completedBytes, transfer.totalBytes) : undefined;
      return <div aria-label={`Upload ${transfer.name}: ${transferStateLabel(transfer.state)}`} className={`terminal-upload ${transfer.state}`} key={record.key}>
        <span title={transfer.sourcePath}>{transfer.name}</span><small>{transferStateLabel(transfer.state)}</small>
        <small className="transfer-origin">Profile {record.owner.hostProfileId} · pane {record.owner.paneId} · server {record.owner.serverIdentity} · epoch {record.owner.connectionEpoch}</small>
        <progress aria-label={`Upload progress for ${transfer.name}`} data-completed-bytes={transfer.completedBytes} {...(transfer.totalBytes ? { "data-total-bytes": transfer.totalBytes } : {})} {...(percent === undefined ? {} : { max: 100, value: percent })} />
        <small>{formatProgress(transfer)}</small>
        {canCancelTransfer(transfer.state) && <button aria-label={`Cancel upload ${transfer.name}`} onClick={() => void client.cancel(transfer.id).then((disposition) => {
          if (disposition.disposition === "awaitingAuthoritativeOutcome") registry.markVerifying(record.key);
        }).catch((reason) => onError?.(reason))} type="button">Cancel</button>}
        {/* A finished record is one a delivered success would already have
            removed, so what is left here is a failure, a cancellation or an
            unknown outcome — every one of them the user's to close. The same
            predicate the registry uses, so the button cannot appear on a
            record `dismiss` would refuse. */}
        {isTerminalTransferState(transfer.state) && <button
          aria-label={`Dismiss upload ${transfer.name}`}
          onClick={() => registry.dismiss(record.key)}
          type="button"
        >Dismiss</button>}
        {transfer.state === "verifying" && <small className="transfer-finalizing" role="status">Commit in progress; awaiting the authoritative backend outcome.</small>}
        {transfer.failureKind === "staleScope" && <em role="alert">Upload stopped because the connection scope changed.</em>}
        {transfer.failureKind === "timeout" && <em role="alert">Upload timed out before an authoritative result arrived.</em>}
        {transfer.outcome === "unknown" && <em role="alert">The upload outcome is unknown. Inspect the destination before retrying or pasting.</em>}
        {transfer.error && <em role="alert">{transfer.error}</em>}
        {transfer.cleanupError && <em role="alert">Partial cleanup failed: {transfer.cleanupError}</em>}
        {transfer.cleanupStatus && ["failed", "cancelled"].includes(transfer.state) && <small>Cleanup: {transfer.cleanupStatus}</small>}
      </div>;
    })}
  </section>;
}

export function progressPercent(completed: string, total: string): number {
  const numerator = BigInt(completed);
  const denominator = BigInt(total);
  if (denominator <= 0n) return 0;
  return Number((numerator > denominator ? denominator : numerator) * 1000n / denominator) / 10;
}

export function formatBytes(value: string): string {
  const bytes = BigInt(value);
  const units = [[1024n ** 4n, "TiB"], [1024n ** 3n, "GiB"], [1024n ** 2n, "MiB"], [1024n, "KiB"]] as const;
  for (const [size, label] of units) {
    if (bytes >= size) {
      const tenths = bytes * 10n / size;
      return `${tenths / 10n}.${tenths % 10n} ${label}`;
    }
  }
  return `${bytes} B`;
}

function formatProgress(transfer: TerminalTransferProgress): string {
  const bytes = transfer.totalBytes
    ? `${formatBytes(transfer.completedBytes)} / ${formatBytes(transfer.totalBytes)}`
    : `${formatBytes(transfer.completedBytes)} transferred`;
  const speed = transfer.bytesPerSecond ? ` · ${formatBytes(transfer.bytesPerSecond)}/s` : "";
  const eta = transfer.etaSeconds !== undefined && transfer.etaSeconds > 0 ? ` · ${Math.ceil(transfer.etaSeconds)}s remaining` : "";
  return `${bytes}${speed}${eta}`;
}
