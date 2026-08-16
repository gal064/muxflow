import { invoke } from "@tauri-apps/api/core";
import { measurePerfOutcome, measurePerfRequest, recordPerfCounter, recordPerfHighWater, recordPerfJsonBytesDeferred } from "../../perf/probe";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type {
  GitChangeKind,
  GitCommandResult,
  GitDiff,
  GitDiffTarget,
  GitMutationRequest,
  GitRepository,
  GitStatusEntry,
  GitStatusSnapshot,
  GitWorkspaceClient,
  GitWorkspaceEvent,
  GitWatchLease,
} from "./types";

interface WireRepository { repositoryId: string; worktreeRoot: string; initial: boolean; detachedHead: boolean; headName: string; headOid: string }
interface WireStatusEntry {
  path: number[]; displayPath: string; originalPath: number[]; displayOriginalPath: string;
  indexKind: string; worktreeKind: string; indexStatus: string; worktreeStatus: string;
  conflicted: boolean; conflictCode: string; untracked: boolean; ignored: boolean; submodule: boolean;
  submoduleState: string; symlink: boolean; binary: boolean; renameScore: string;
}
interface WireStatus { repository: WireRepository; generation: string; sourceGeneration: string; entries: WireStatusEntry[]; authoritative: boolean; oversized?: boolean; totalEntryCount?: string; error?: string; copyDetectionIncomplete?: boolean }
interface WireDiff {
  repository: WireRepository; target: string; path: number[]; originalPath: number[]; displayPath: string;
  oldContent: number[]; newContent: number[]; patch: number[]; sourceGeneration: string; binary: boolean; tooLarge: boolean;
  oldMissing: boolean; newMissing: boolean; hunkCount: number;
}
interface WireCommand {
  exitCode: number; stdout: number[]; stderr: number[]; applied?: boolean; refreshFailed?: boolean; refreshError?: string; outcome?: string;
  stdoutTruncated?: boolean; stderrTruncated?: boolean; error?: string; preHeadOid?: string; postHeadOid?: string;
  preIndexGeneration?: string; postIndexGeneration?: string; postStateAuthoritative?: boolean; preStatusGeneration?: string; postStatusGeneration?: string; statusOmitted?: boolean;
  status?: WireStatus;
}
interface WireResponse { operationId: string; status?: WireStatus; diff?: WireDiff; confirmation?: { token: string; expiresUnixMillis: string }; command?: WireCommand }
export interface WireGitEvent { watchId?: string; rootToken: string; status?: WireStatus; error?: string }

const decoder = new TextDecoder("utf-8", { fatal: true });
const MAX_SUPERSEDED_RETRIES = 4;

export class TauriGitWorkspaceClient implements GitWorkspaceClient {
  readonly #listeners = new Set<(event: GitWorkspaceEvent) => void>();

  async status(scope: FileWorkspaceScope, root: ActiveRoot, signal?: AbortSignal): Promise<GitStatusSnapshot> {
    recordPerfCounter("git.statusRequests");
    const status = await measurePerfOutcome("git.status", () => retrySuperseded(async () => {
      throwIfAborted(signal);
      const operationId = crypto.randomUUID();
      return await abortable(
        this.#request(scope, root, { operation: "status", operationId }, validateStatus, "git.status.request"),
        signal,
        () => this.#cancelRequest(scope.clientId, operationId),
      );
    }, signal));
    observeStatus(status);
    return status;
  }

  async watch(scope: FileWorkspaceScope, root: ActiveRoot, signal?: AbortSignal): Promise<GitWatchLease> {
    recordPerfCounter("git.watchRequests");
    const { status, watchId } = await retrySuperseded(async () => {
      throwIfAborted(signal);
      const watchId = crypto.randomUUID();
      const operationId = crypto.randomUUID();
      const status = await abortable(
        this.#request(scope, root, { operation: "watch", operationId, watchId }, validateStatus, "git.watch.request"),
        signal,
        () => this.#cancelRequest(scope.clientId, operationId),
      );
      return { status, watchId };
    }, signal);
    observeStatus(status);
    let released = false;
    return {
      watchId,
      rootToken: root.token,
      connectionEpoch: scope.terminalEpoch,
      status,
      release: () => {
        if (released) return;
        released = true;
        recordPerfCounter("git.watchReleases");
        void this.#request(
          scope, root, { operation: "unwatch", operationId: crypto.randomUUID(), watchId },
          () => undefined, "git.unwatch.request",
        ).catch(() => undefined);
      },
    };
  }

  async diff(
    scope: FileWorkspaceScope,
    root: ActiveRoot,
    repositoryId: string,
    path: string,
    originalPath: string | undefined,
    target: GitDiffTarget,
    expectedStatusGeneration: string,
    signal?: AbortSignal,
  ): Promise<GitDiff> {
    recordPerfCounter("git.diffRequests");
    const diff = await measurePerfOutcome("git.diff", () => retrySuperseded(async () => {
      throwIfAborted(signal);
      const operationId = crypto.randomUUID();
      return await abortable(this.#request(scope, root, {
        operation: "diff", operationId, repositoryId, path: [...fromBase64(path)],
        ...(originalPath ? { originalPath: [...fromBase64(originalPath)] } : {}),
        diffTarget: target, expectedStatusGeneration,
      }, (response) => validateDiff(response, repositoryId, path, originalPath, target), "git.diff.request"), signal,
      () => this.#cancelRequest(scope.clientId, operationId));
    }, signal));
    recordPerfCounter("git.diffPayloadBytes", (diff.oldContent?.byteLength ?? 0) + (diff.newContent?.byteLength ?? 0) + (diff.patch?.byteLength ?? 0));
    return diff;
  }

  async prepareDiscard(scope: FileWorkspaceScope, root: ActiveRoot, repositoryId: string, request: GitMutationRequest): Promise<string> {
    if (request.kind !== "discardFile" && request.kind !== "discardHunk") throw new Error("Only discard operations require confirmation tokens.");
    return this.#request(
      scope, root, mutationCommand("prepareDiscard", repositoryId, request),
      (response) => {
        if (!response.confirmation?.token) throw new Error("Host omitted the discard confirmation token.");
        return response.confirmation.token;
      },
      "git.prepareDiscard.request",
    );
  }

  async mutate(scope: FileWorkspaceScope, root: ActiveRoot, repositoryId: string, request: GitMutationRequest): Promise<GitCommandResult> {
    recordPerfCounter("git.mutationRequests");
    if ((request.kind === "discardFile" || request.kind === "discardHunk") && !request.confirmationToken) {
      throw new Error("Discard requires confirmation.");
    }
    // The host emits this exact rejection only while pre-command status is
    // being established. Once Git starts, uncertainty is returned as a
    // command outcome and must never pass through this retry path.
    return measurePerfOutcome("workflow.git.mutationAck", () => retrySuperseded(
      () => this.#request(
        scope, root, mutationCommand("mutate", repositoryId, request), validateCommand, "git.mutation.request",
      ),
    ));
  }

  async commit(scope: FileWorkspaceScope, root: ActiveRoot, repositoryId: string, expectedStatusGeneration: string, message: string): Promise<GitCommandResult> {
    recordPerfCounter("git.commitRequests");
    if (!message.trim()) throw new Error("Enter a commit message.");
    // See mutate(): post-command outcomes are responses, never retryable
    // superseded-status transport errors.
    return measurePerfOutcome("workflow.git.mutationAck", () => retrySuperseded(() => this.#request(scope, root, {
      operation: "commit", operationId: crypto.randomUUID(), repositoryId, expectedStatusGeneration, commitMessage: message,
    }, validateCommand, "git.commit.request")));
  }

  subscribe(listener: (event: GitWorkspaceEvent) => void): () => void {
    this.#listeners.add(listener);
    recordPerfCounter("git.subscriberAdds");
    recordPerfHighWater("git.subscribers", this.#listeners.size);
    return () => {
      if (this.#listeners.delete(listener)) recordPerfCounter("git.subscriberReleases");
    };
  }

  publishWireEvent(event: WireGitEvent): void {
    if (!event.rootToken) return;
    if (event.status) {
      try { this.#publish({ kind: "status", rootToken: event.rootToken, watchId: event.watchId, status: mapStatus(event.status) }); }
      catch (cause) { this.#publish({ kind: "error", rootToken: event.rootToken, watchId: event.watchId, error: String(cause) }); }
    }
    if (event.error) this.#publish({ kind: "error", rootToken: event.rootToken, watchId: event.watchId, error: event.error });
  }

  #publish(event: GitWorkspaceEvent): void { for (const listener of this.#listeners) listener(event); }

  #cancelRequest(clientId: string, operationId: string): Promise<unknown> {
    const boundary = { clientId, operationId };
    return measurePerfRequest(
      "git.requestCancellation", "git", boundary, (request) => invoke("cancel_git_request", request),
    );
  }

  async #request<T>(
    scope: FileWorkspaceScope,
    root: ActiveRoot,
    command: Record<string, unknown>,
    validate: (response: WireResponse) => T,
    metricName: string,
  ): Promise<T> {
    const boundary = {
      clientId: scope.clientId,
      command: {
        ...command,
        root: root.path,
        rootToken: root.token,
        connectionEpoch: String(scope.terminalEpoch),
        expectedServerIdentity: scope.serverIdentity,
      },
    };
    return measurePerfRequest(metricName, "git", boundary, async (requestBoundary) => {
      const response = await invoke<WireResponse>("git_request", requestBoundary);
      return validate(response);
    }, { byteCounters: ["git.requestBytes"] });
  }
}

function validateStatus(response: WireResponse): GitStatusSnapshot {
  if (!response.status) throw new Error("Host omitted Git status.");
  return mapStatus(response.status);
}

function observeStatus(status: GitStatusSnapshot): void {
  recordPerfCounter("git.statusEntries", status.entries.length);
  recordPerfJsonBytesDeferred("git.statusMappedPayloadBytes", status);
}

function validateDiff(
  response: WireResponse,
  repositoryId: string,
  path: string,
  originalPath: string | undefined,
  target: GitDiffTarget,
): GitDiff {
  if (!response.diff) throw new Error("Host omitted the Git diff.");
  const diff = mapDiff(response.diff);
  if (diff.repository.id !== repositoryId || diff.path !== path || (diff.originalPath ?? "") !== (originalPath ?? "") || diff.target !== target) {
    throw new Error("Host returned a stale Git diff identity.");
  }
  return diff;
}

function validateCommand(response: WireResponse): GitCommandResult {
  if (!response.command) throw new Error("Host omitted the Git mutation result.");
  return mapCommand(response.command);
}

function mutationCommand(operation: "prepareDiscard" | "mutate", repositoryId: string, request: GitMutationRequest): Record<string, unknown> {
  return {
    operation, operationId: crypto.randomUUID(), repositoryId, path: [...fromBase64(request.path)],
    ...(request.originalPath ? { originalPath: [...fromBase64(request.originalPath)] } : {}),
    mutation: request.kind, diffTarget: request.target,
    expectedStatusGeneration: request.expectedStatusGeneration,
    expectedSourceGeneration: request.expectedSourceGeneration,
    ...(request.hunkIndex !== undefined ? { hunkIndex: request.hunkIndex } : {}),
    ...(request.confirmationToken ? { confirmationToken: request.confirmationToken } : {}),
  };
}

function mapRepository(value: WireRepository): GitRepository {
  return {
    id: value.repositoryId, worktreeRoot: value.worktreeRoot, initial: Boolean(value.initial), detachedHead: Boolean(value.detachedHead),
    ...(value.headName ? { headName: value.headName } : {}), ...(value.headOid ? { headOid: value.headOid } : {}),
  };
}

function mapStatus(value: WireStatus): GitStatusSnapshot {
  if (!value.repository?.repositoryId || !Array.isArray(value.entries)) throw new Error("Host returned malformed Git status.");
  requireDecimalU64(value.generation, "Git status generation");
  const totalEntryCount = value.totalEntryCount ?? String(value.entries.length);
  requireDecimalU64(totalEntryCount, "Git status total entry count");
  return {
    repository: mapRepository(value.repository), generation: String(value.generation), sourceGeneration: value.sourceGeneration,
    entries: value.entries.map(mapEntry), authoritative: Boolean(value.authoritative), oversized: Boolean(value.oversized), totalEntryCount,
    copyDetectionIncomplete: Boolean(value.copyDetectionIncomplete),
    ...(value.error ? { error: value.error } : {}),
  };
}

function mapEntry(value: WireStatusEntry): GitStatusEntry {
  requireBytes(value.path, "Git status path");
  requireBytes(value.originalPath, "Git original path");
  return {
    path: toBase64(value.path), displayPath: value.displayPath, ...(value.originalPath.length ? { originalPath: toBase64(value.originalPath) } : {}),
    ...(value.displayOriginalPath ? { displayOriginalPath: value.displayOriginalPath } : {}),
    indexKind: mapChange(value.indexKind), worktreeKind: mapChange(value.worktreeKind), indexStatus: value.indexStatus,
    worktreeStatus: value.worktreeStatus, conflicted: Boolean(value.conflicted), ...(value.conflictCode ? { conflictCode: value.conflictCode } : {}),
    untracked: Boolean(value.untracked), ignored: Boolean(value.ignored), submodule: Boolean(value.submodule),
    ...(value.submoduleState ? { submoduleState: value.submoduleState } : {}), symlink: Boolean(value.symlink), binary: Boolean(value.binary),
    ...(value.renameScore ? { renameScore: value.renameScore } : {}),
  };
}

function mapChange(value: string): GitChangeKind {
  const normalized = value.replace(/^GIT_CHANGE_KIND_/, "").replaceAll("_", "").toLowerCase();
  return ({ unspecified: "none", modified: "modified", added: "added", deleted: "deleted", renamed: "renamed", copied: "copied", typechanged: "typeChanged", unmerged: "unmerged", untracked: "untracked", ignored: "ignored" } as Record<string, GitChangeKind>)[normalized] ?? "none";
}

function mapTarget(value: string): GitDiffTarget {
  const normalized = value.replace(/^GIT_DIFF_TARGET_/, "").toLowerCase();
  if (normalized === "staged" || normalized === "unstaged") return normalized;
  throw new Error("Host returned an unknown Git diff target.");
}

function mapDiff(value: WireDiff): GitDiff {
  requireBytes(value.path, "Git diff path");
  requireBytes(value.originalPath, "Git diff original path");
  requireBytes(value.oldContent, "Git old content");
  requireBytes(value.newContent, "Git new content");
  requireBytes(value.patch, "Git patch");
  if (!Number.isSafeInteger(value.hunkCount) || value.hunkCount < 0) throw new Error("Host returned an invalid Git hunk count.");
  return {
    repository: mapRepository(value.repository), target: mapTarget(value.target), path: toBase64(value.path),
    ...(value.originalPath.length ? { originalPath: toBase64(value.originalPath) } : {}), displayPath: value.displayPath,
    ...(value.oldContent.length ? { oldContent: new Uint8Array(value.oldContent) } : {}), ...(value.newContent.length ? { newContent: new Uint8Array(value.newContent) } : {}),
    ...(value.patch.length ? { patch: new Uint8Array(value.patch) } : {}), sourceGeneration: value.sourceGeneration,
    binary: Boolean(value.binary), tooLarge: Boolean(value.tooLarge), oldMissing: Boolean(value.oldMissing), newMissing: Boolean(value.newMissing), hunkCount: value.hunkCount,
  };
}

function mapCommand(value: WireCommand): GitCommandResult {
  requireBytes(value.stdout, "Git stdout");
  requireBytes(value.stderr, "Git stderr");
  return {
    exitCode: value.exitCode, stdout: decodeOutput(value.stdout), stderr: decodeOutput(value.stderr),
    applied: Boolean(value.applied), refreshFailed: Boolean(value.refreshFailed), refreshError: value.refreshError ?? "",
    outcome: mapCommandOutcome(value.outcome), stdoutTruncated: Boolean(value.stdoutTruncated), stderrTruncated: Boolean(value.stderrTruncated), error: value.error ?? "",
    preHeadOid: value.preHeadOid ?? "", postHeadOid: value.postHeadOid ?? "", preIndexGeneration: value.preIndexGeneration ?? "", postIndexGeneration: value.postIndexGeneration ?? "",
    postStateAuthoritative: Boolean(value.postStateAuthoritative), preStatusGeneration: optionalDecimal(value.preStatusGeneration, "Git pre-status generation"),
    postStatusGeneration: optionalDecimal(value.postStatusGeneration, "Git post-status generation"), statusOmitted: Boolean(value.statusOmitted),
    ...(value.status ? { status: mapStatus(value.status) } : {}),
  };
}

function mapCommandOutcome(value?: string): NonNullable<GitCommandResult["outcome"]> {
  const normalized = (value ?? "").replace(/^GIT_COMMAND_OUTCOME_/, "").replaceAll("_", "").toLowerCase();
  return ({ notapplied: "notApplied", applied: "applied", partialorunknown: "partialOrUnknown" } as Record<string, NonNullable<GitCommandResult["outcome"]>>)[normalized] ?? "unspecified";
}

function optionalDecimal(value: string | undefined, label: string): string {
  if (!value) return "0";
  requireDecimalU64(value, label);
  return value;
}

function fromBase64(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

function decodeOutput(value: number[]): string {
  if (!value.length) return "";
  try { return decoder.decode(new Uint8Array(value)); } catch { return "Git returned non-UTF-8 output."; }
}

function toBase64(value: number[]): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function abortable<T>(promise: Promise<T>, signal?: AbortSignal, cancelRemote?: () => Promise<unknown>): Promise<T> {
  if (!signal) return promise;
  if (signal.aborted) {
    recordPerfCounter("git.cancellations");
    void cancelRemote?.().catch(() => undefined);
    return Promise.reject(new DOMException("Git request was cancelled.", "AbortError"));
  }
  return new Promise((resolve, reject) => {
    const cancel = () => {
      recordPerfCounter("git.cancellations");
      void cancelRemote?.().catch(() => undefined);
      reject(new DOMException("Git request was cancelled.", "AbortError"));
    };
    signal.addEventListener("abort", cancel, { once: true });
    promise.then(resolve, reject).finally(() => signal.removeEventListener("abort", cancel));
  });
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw new DOMException("Git request was cancelled.", "AbortError");
}

async function retrySuperseded<T>(request: () => Promise<T>, signal?: AbortSignal): Promise<T> {
  for (let attempt = 0; ; attempt += 1) {
    try { return await request(); }
    catch (cause) {
      if (attempt >= MAX_SUPERSEDED_RETRIES || !String(cause).includes("Git status refresh superseded by a newer snapshot")) throw cause;
      throwIfAborted(signal);
      await Promise.resolve();
    }
  }
}

function requireDecimalU64(value: string, label: string): void {
  if (!/^(0|[1-9]\d{0,19})$/.test(value)) throw new Error(`${label} is not a decimal u64 string.`);
  if (BigInt(value) > 18_446_744_073_709_551_615n) throw new Error(`${label} exceeds u64.`);
}

function requireBytes(value: number[], label: string): void {
  if (!Array.isArray(value) || value.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) throw new Error(`${label} is not a byte array.`);
}
