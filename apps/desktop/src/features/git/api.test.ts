import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { enablePerfProbe, perfCounterSnapshot, resetPerfProbe } from "../../perf/probe";
import { TauriGitWorkspaceClient } from "./api";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const scope: FileWorkspaceScope = { clientId: "client", hostProfileId: "local", serverIdentity: "server", generation: 7, terminalEpoch: 41, sessionId: "$1", paneId: "%1" };
const root: ActiveRoot = { token: "root-token", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "9" };

beforeEach(() => invokeMock.mockReset());
afterEach(() => resetPerfProbe());

describe("TauriGitWorkspaceClient", () => {
  it("preserves opaque path bytes and decimal u64 generations", async () => {
    enablePerfProbe(async () => undefined);
    invokeMock.mockResolvedValueOnce({ operationId: "status", status: wireStatus() });
    const status = await new TauriGitWorkspaceClient().status(scope, root);
    expect(status).toMatchObject({ generation: "18446744073709551615", totalEntryCount: "1", copyDetectionIncomplete: true, entries: [{ path: "LS1hIGZpbGUJeAo=", displayPath: "--a file\\tx\\n" }] });
    expect(invokeMock).toHaveBeenCalledWith("git_request", { clientId: "client", command: expect.objectContaining({
      operation: "status", root: "/repo", rootToken: "root-token", connectionEpoch: "41", expectedServerIdentity: "server",
    }) });
    const boundary = invokeMock.mock.calls[0][1];
    const exactBoundaryBytes = new TextEncoder().encode(JSON.stringify(boundary)).byteLength;
    expect(perfCounterSnapshot()).toMatchObject({
      "desktop.hostRequestBytes": exactBoundaryBytes,
      "git.hostRequestBytes": exactBoundaryBytes,
      "git.requestBytes": exactBoundaryBytes,
    });
  });

  it("addresses mutations by opaque path and never by the lossy display path", async () => {
    invokeMock.mockResolvedValueOnce({ operationId: "mutation", command: { exitCode: 0, stdout: [], stderr: [], applied: false, refreshFailed: false, refreshError: "", outcome: "partialOrUnknown", stdoutTruncated: true, stderrTruncated: false, error: "cancelled after index changed", preHeadOid: "a", postHeadOid: "a", preIndexGeneration: "i1", postIndexGeneration: "i2", postStateAuthoritative: true, preStatusGeneration: "7", postStatusGeneration: "8", status: wireStatus() } });
    const client = new TauriGitWorkspaceClient();
    const result = await client.mutate(scope, root, "repo-id", {
      kind: "stageFile", path: "LS1hIGZpbGUJeAo=", target: "unstaged", expectedStatusGeneration: "18446744073709551615", expectedSourceGeneration: "source",
    });
    expect(invokeMock.mock.calls[0][1]).toMatchObject({ command: { repositoryId: "repo-id", path: [...new TextEncoder().encode("--a file\tx\n")], mutation: "stageFile" } });
    expect(JSON.stringify(invokeMock.mock.calls[0][1])).not.toContain("--a file");
    expect(result).toMatchObject({ outcome: "partialOrUnknown", stdoutTruncated: true, error: "cancelled after index changed", preIndexGeneration: "i1", postIndexGeneration: "i2", preStatusGeneration: "7", postStatusGeneration: "8" });
  });

  it("refuses discard without a host-bound confirmation token", async () => {
    const client = new TauriGitWorkspaceClient();
    await expect(client.mutate(scope, root, "repo-id", {
      kind: "discardHunk", path: "YQ==", target: "unstaged", expectedStatusGeneration: "7", expectedSourceGeneration: "source", hunkIndex: 0,
    })).rejects.toThrow("Discard requires confirmation");
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("rejects stale or mismatched diff identity returned by the host", async () => {
    invokeMock.mockResolvedValueOnce({ operationId: "diff", diff: { repository: wireRepository(), target: "staged", path: [...new TextEncoder().encode("different")], originalPath: [], displayPath: "a", oldContent: [], newContent: [], patch: [], sourceGeneration: "s", binary: false, tooLarge: false, oldMissing: false, newMissing: false, hunkCount: 1 } });
    await expect(new TauriGitWorkspaceClient().diff(scope, root, "repo-id", "YQ==", undefined, "staged", "7")).rejects.toThrow("stale Git diff identity");
  });

  it("sends and verifies opaque rename provenance for diffs", async () => {
    invokeMock.mockResolvedValueOnce({ operationId: "diff", diff: { repository: wireRepository(), target: "staged", path: [...new TextEncoder().encode("new")], originalPath: [...new TextEncoder().encode("old")], displayPath: "new", oldContent: [], newContent: [], patch: [], sourceGeneration: "s", binary: false, tooLarge: false, oldMissing: false, newMissing: false, hunkCount: 0 } });
    await new TauriGitWorkspaceClient().diff(scope, root, "repo-id", "bmV3", "b2xk", "staged", "7");
    expect(invokeMock).toHaveBeenCalledWith("git_request", { clientId: "client", command: expect.objectContaining({
      path: [...new TextEncoder().encode("new")], originalPath: [...new TextEncoder().encode("old")], diffTarget: "staged",
    }) });
  });

  it("routes only root-token-bound status events", () => {
    const client = new TauriGitWorkspaceClient();
    const events: unknown[] = [];
    client.subscribe((event) => events.push(event));
    client.publishWireEvent({ rootToken: "", status: wireStatus() });
    client.publishWireEvent({ rootToken: "root-token", watchId: "watch", status: wireStatus() });
    expect(events).toHaveLength(1);
    expect(events[0]).toMatchObject({ kind: "status", rootToken: "root-token", watchId: "watch", status: { repository: { id: "repo-id" } } });
  });

  it("retries a superseded watch bootstrap without surfacing an internal freshness race", async () => {
    invokeMock
      .mockRejectedValueOnce("git_rejected: Git status refresh superseded by a newer snapshot")
      .mockRejectedValueOnce("git_rejected: Git status refresh superseded by a newer snapshot")
      .mockResolvedValueOnce({ operationId: "watch", status: wireStatus() })
      .mockResolvedValue({});
    const lease = await new TauriGitWorkspaceClient().watch(scope, root);
    const requests = invokeMock.mock.calls.filter(([command]) => command === "git_request");
    expect(requests).toHaveLength(3);
    expect(new Set(requests.map(([, args]) => args.command.watchId)).size).toBe(3);
    expect(lease.status.repository.id).toBe("repo-id");
    lease.release();
  });

  it("retries only a pre-command superseded commit request with a fresh operation identity", async () => {
    invokeMock
      .mockRejectedValueOnce("git_rejected: Git status refresh superseded by a newer snapshot")
      .mockResolvedValueOnce({ operationId: "commit", command: {
        exitCode: 0, stdout: [], stderr: [], applied: true, refreshFailed: false, refreshError: "", outcome: "applied",
      } });
    const result = await new TauriGitWorkspaceClient().commit(scope, root, "repo-id", "7", "message");
    expect(result.outcome).toBe("applied");
    const requests = invokeMock.mock.calls.filter(([command]) => command === "git_request");
    expect(requests).toHaveLength(2);
    expect(new Set(requests.map(([, args]) => args.command.operationId)).size).toBe(2);
  });

  it("turns malformed event generations and byte arrays into a scoped error instead of crashing the shell", () => {
    const client = new TauriGitWorkspaceClient();
    const events: unknown[] = [];
    client.subscribe((event) => events.push(event));
    const malformed = wireStatus() as ReturnType<typeof wireStatus> & { generation: string };
    malformed.generation = "9e99";
    malformed.entries[0].path = [999];
    client.publishWireEvent({ rootToken: "root-token", status: malformed });
    expect(events).toEqual([expect.objectContaining({ kind: "error", rootToken: "root-token", error: expect.stringContaining("decimal u64") })]);
  });

  it("propagates AbortSignal cancellation to the exact live protocol request", async () => {
    invokeMock.mockImplementation((command: string) => command === "git_request" ? new Promise(() => undefined) : Promise.resolve());
    const controller = new AbortController();
    const request = new TauriGitWorkspaceClient().status(scope, root, controller.signal);
    controller.abort();
    await expect(request).rejects.toMatchObject({ name: "AbortError" });
    const gitCommand = invokeMock.mock.calls.find(([command]) => command === "git_request")?.[1].command;
    expect(invokeMock).toHaveBeenCalledWith("cancel_git_request", { clientId: "client", operationId: gitCommand.operationId });
  });

  it("does not invoke the bridge when a Git request is already aborted", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(new TauriGitWorkspaceClient().status(scope, root, controller.signal)).rejects.toMatchObject({ name: "AbortError" });
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

function wireRepository() { return { repositoryId: "repo-id", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main", headOid: "abc" }; }
function wireStatus() {
  return {
    repository: wireRepository(), generation: "18446744073709551615", sourceGeneration: "source", authoritative: true, totalEntryCount: "1", copyDetectionIncomplete: true,
    entries: [{ path: [...new TextEncoder().encode("--a file\tx\n")], displayPath: "--a file\\tx\\n", originalPath: [], displayOriginalPath: "", indexKind: "unspecified", worktreeKind: "modified", indexStatus: ".", worktreeStatus: "M", conflicted: false, conflictCode: "", untracked: false, ignored: false, submodule: false, submoduleState: "", symlink: false, binary: false, renameScore: "" }],
  };
}
