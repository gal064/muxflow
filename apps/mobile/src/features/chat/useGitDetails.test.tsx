// The Git read behind Details, against a scripted host: what one read asks
// and makes of the answers, and when the hook reads and which answers it
// drops.

import { create } from "@bufbuild/protobuf";
import { act } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ChatTurnState } from "../../protocol/gen/chat_pb";
import { GitSummaryBase, Operation, ResponseSchema, type Request, type Response } from "../../protocol/gen/envelope_pb";
import { HostError } from "../../protocol/HostConnection";
import { render, rerender } from "../../testing/render";
import { filesStore } from "../files/filesStore";
import { gitRows, NO_GIT_DETAILS, NOT_A_REPOSITORY, type GitDetails } from "./gitDetails";
import { readGitDetails, useGitDetails, type GitChange, type GitDetailsTarget } from "./useGitDetails";

const mocks = vi.hoisted(() => ({ connection: null as unknown }));
vi.mock("../../session/connectionManager", () => ({ getConnection: () => mocks.connection }));

const PANE = "%7";
const ROOT = "/home/operator/dev/muxflow";
const TOKEN = "file-v2:root";

type Answer = (request: Request) => Response | Promise<Response>;

/** A host that answers each operation as scripted and remembers what it was asked. */
class Host {
  serverIdentity = "server-a";
  readonly requests: Request[] = [];
  private readonly answers = new Map<Operation, Answer>();

  answer(operation: Operation, answer: Answer): void {
    this.answers.set(operation, answer);
  }

  async request(request: Request): Promise<Response> {
    this.requests.push(request);
    const answer = this.answers.get(request.operation);
    if (!answer) throw new Error(`unscripted operation ${request.operation}`);
    return answer(request);
  }

  of(operation: Operation): Request[] {
    return this.requests.filter((request) => request.operation === operation);
  }
}

function rootResponse(fields: { root?: string; gitWorktree?: boolean; rootToken?: string } = {}): Response {
  return create(ResponseSchema, {
    ok: true,
    file: { activeRoot: { paneId: PANE, root: fields.root ?? ROOT, rootToken: fields.rootToken ?? TOKEN, gitWorktree: fields.gitWorktree ?? true, rootGeneration: 3n } },
  });
}

function summaryResponse(baseRef: string, files: number, added: number, removed: number): Response {
  return create(ResponseSchema, { ok: true, git: { branchSummary: { baseRef, totalFileCount: BigInt(files), added: BigInt(added), removed: BigInt(removed) } } });
}

function branchesResponse(current: string): Response {
  return create(ResponseSchema, { ok: true, git: { branches: { branches: current ? [current, "main"] : ["main"], current } } });
}

/** A host with the mock's repository: on gpui/chat, 14 files against main, one uncommitted. */
function repositoryHost(): Host {
  const host = new Host();
  host.answer(Operation.RESOLVE_ACTIVE_ROOT, () => rootResponse());
  host.answer(Operation.GIT_BRANCHES, () => branchesResponse("gpui/chat"));
  host.answer(Operation.GIT_BRANCH_SUMMARY, (request) => (request.git?.summaryBase === GitSummaryBase.HEAD ? summaryResponse("HEAD", 1, 3, 1) : summaryResponse("origin/main", 14, 412, 96)));
  return host;
}

/** Runs one read, keeping every state it passed through. */
async function read(host: Host, from: GitDetails = NO_GIT_DETAILS, known?: Parameters<typeof readGitDetails>[2]) {
  const seen: GitDetails[] = [];
  let details = from;
  const apply = (change: GitChange) => {
    details = change(details);
    seen.push(details);
    return true;
  };
  const root = await readGitDetails(host, PANE, known, apply);
  return { root, details, seen };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

describe("one read of the Git details", () => {
  it("resolves the pane's root, then asks for the branch and both summaries under it", async () => {
    const host = repositoryHost();
    const { root, details, seen } = await read(host);
    expect(root).toMatchObject({ paneId: PANE, root: ROOT, rootToken: TOKEN, gitWorktree: true });
    expect(host.of(Operation.RESOLVE_ACTIVE_ROOT)[0]?.file).toMatchObject({ paneId: PANE, expectedServerIdentity: "server-a", knownRootToken: "" });
    for (const request of [...host.of(Operation.GIT_BRANCHES), ...host.of(Operation.GIT_BRANCH_SUMMARY)]) {
      expect(request.git).toMatchObject({ root: ROOT, rootToken: TOKEN, expectedServerIdentity: "server-a" });
      expect(request.git?.operationId).not.toBe("");
    }
    expect(host.of(Operation.GIT_BRANCHES)).toHaveLength(1);
    expect(host.of(Operation.GIT_BRANCH_SUMMARY).map((request) => request.git?.summaryBase)).toEqual([GitSummaryBase.UNSPECIFIED, GitSummaryBase.HEAD]);
    // The repository is known before any count is: the rows show … meanwhile.
    expect(seen[0]).toEqual({ ...NO_GIT_DETAILS, repository: "git", root: ROOT });
    expect(details).toEqual({
      repository: "git",
      root: ROOT,
      branch: { status: "ready", value: "gpui/chat" },
      changes: { status: "ready", value: { baseRef: "origin/main", files: 14n, added: 412n, removed: 96n } },
      uncommitted: { status: "ready", value: { baseRef: "HEAD", files: 1n, added: 3n, removed: 1n } },
    });
  });

  it("asks Git nothing for a folder outside a repository", async () => {
    const host = new Host();
    host.answer(Operation.RESOLVE_ACTIVE_ROOT, () => rootResponse({ root: "/srv/app", gitWorktree: false }));
    const { root, details } = await read(host);
    expect(root?.gitWorktree).toBe(false);
    expect(details).toEqual(NOT_A_REPOSITORY);
    expect(host.requests).toHaveLength(1);
  });

  it("shows nothing when the root cannot be resolved", async () => {
    const host = new Host();
    host.answer(Operation.RESOLVE_ACTIVE_ROOT, () => {
      throw new HostError("stale_request", "active pane no longer exists");
    });
    const held = (await read(repositoryHost())).details;
    const { root, details } = await read(host, held);
    expect(root).toBeUndefined();
    expect(details).toEqual(NO_GIT_DETAILS);
    expect(host.requests).toHaveLength(1);
  });

  it("starts from the root the phone already holds", async () => {
    const host = repositoryHost();
    const known = { paneId: PANE, root: ROOT, rootToken: TOKEN, gitWorktree: true, rootGeneration: 3n };
    host.answer(Operation.RESOLVE_ACTIVE_ROOT, () => create(ResponseSchema, { ok: true, file: { rootUnchanged: true } }));
    const { root } = await read(host, NO_GIT_DETAILS, known);
    expect(root).toBe(known);
    expect(host.of(Operation.RESOLVE_ACTIVE_ROOT)[0]?.file?.knownRootToken).toBe(TOKEN);
    expect(host.of(Operation.GIT_BRANCHES)[0]?.git).toMatchObject({ root: ROOT, rootToken: TOKEN });
  });

  it("leaves out only the row whose read failed", async () => {
    const host = repositoryHost();
    host.answer(Operation.GIT_BRANCHES, () => {
      throw new HostError("git_timeout", "Git timed out");
    });
    const { details } = await read(host);
    expect(details.branch).toEqual({ status: "failed" });
    expect(gitRows(details)).toMatchObject({ changes: { title: "Changes vs main" }, uncommitted: { title: "Uncommitted" } });
    expect(gitRows(details).branch).toBeUndefined();
  });

  it("treats an answer without its payload as a failed read", async () => {
    const host = repositoryHost();
    host.answer(Operation.GIT_BRANCH_SUMMARY, () => create(ResponseSchema, { ok: true }));
    const { details } = await read(host);
    expect(details.changes).toEqual({ status: "failed" });
    expect(details.uncommitted).toEqual({ status: "failed" });
    expect(details.branch).toEqual({ status: "ready", value: "gpui/chat" });
  });

  it("draws no Git row once the host says the folder is no repository, whatever else it answers", async () => {
    const host = repositoryHost();
    const branches = deferred<Response>();
    host.answer(Operation.GIT_BRANCH_SUMMARY, () => {
      throw new HostError("not_git_repository", "not a git repository");
    });
    host.answer(Operation.GIT_BRANCHES, () => branches.promise);
    const reading = read(host);
    // The branch list answers after the summaries were refused.
    await Promise.resolve();
    branches.resolve(branchesResponse("main"));
    const { details } = await reading;
    expect(details).toEqual(NOT_A_REPOSITORY);
    expect(gitRows(details)).toEqual({});
  });

  it("keeps what a previous released host answers for HEAD, which the rows then leave out", async () => {
    // That host ignores `summary_base` and answers both with the default-branch summary.
    const host = repositoryHost();
    host.answer(Operation.GIT_BRANCH_SUMMARY, () => summaryResponse("origin/main", 14, 412, 96));
    const { details } = await read(host);
    expect(details.uncommitted).toEqual({ status: "ready", value: { baseRef: "origin/main", files: 14n, added: 412n, removed: 96n } });
    const rows = gitRows(details);
    expect(rows.uncommitted).toBeUndefined();
    expect(rows.changes?.title).toBe("Changes vs main");
  });

  it("keeps the last counts on screen while the same root is read again", async () => {
    const host = repositoryHost();
    const first = await read(host);
    const second = await read(host, first.details);
    expect(second.seen[0]).toBe(first.details);
    expect(second.details).toEqual(first.details);
  });
});

// ---- the hook -------------------------------------------------------------------

const target = (fields: Partial<GitDetailsTarget> = {}): GitDetailsTarget => ({
  paneId: PANE,
  serverIdentity: "server-a",
  folder: `${ROOT}/apps/mobile`,
  turn: ChatTurnState.IDLE,
  open: true,
  connected: true,
  ...fields,
});

function Probe({ at, seen }: { at: GitDetailsTarget; seen: { details: GitDetails } }) {
  seen.details = useGitDetails(at);
  return null;
}

/** Lets every answer that is ready arrive. */
async function settle(): Promise<void> {
  await act(async () => {
    for (let turn = 0; turn < 20; turn += 1) await Promise.resolve();
  });
}

describe("when Details reads", () => {
  beforeEach(() => {
    filesStore.getState().clearAll();
    mocks.connection = null;
  });

  it("reads when it opens, and never while closed", async () => {
    const host = repositoryHost();
    mocks.connection = host;
    const seen = { details: NO_GIT_DETAILS };
    const view = render(<Probe at={target({ open: false })} seen={seen} />);
    await settle();
    // A turn ending under a closed sheet reads nothing either.
    rerender(view, <Probe at={target({ open: false, turn: ChatTurnState.WORKING })} seen={seen} />);
    rerender(view, <Probe at={target({ open: false })} seen={seen} />);
    await settle();
    expect(host.requests).toHaveLength(0);
    expect(seen.details).toBe(NO_GIT_DETAILS);

    rerender(view, <Probe at={target()} seen={seen} />);
    await settle();
    expect(host.requests).toHaveLength(4);
    expect(gitRows(seen.details)).toMatchObject({ branch: "gpui/chat", changes: { title: "Changes vs main" }, uncommitted: { title: "Uncommitted" } });
    // The file screens start from the same root.
    expect(filesStore.getState().roots[PANE]).toMatchObject({ root: ROOT, rootToken: TOKEN });
  });

  it("reads again when a turn ends under it, and not when one starts or asks", async () => {
    const host = repositoryHost();
    mocks.connection = host;
    const seen = { details: NO_GIT_DETAILS };
    const view = render(<Probe at={target()} seen={seen} />);
    await settle();
    expect(host.of(Operation.GIT_BRANCHES)).toHaveLength(1);

    rerender(view, <Probe at={target({ turn: ChatTurnState.WORKING })} seen={seen} />);
    rerender(view, <Probe at={target({ turn: ChatTurnState.WAITING })} seen={seen} />);
    await settle();
    expect(host.of(Operation.GIT_BRANCHES)).toHaveLength(1);

    host.answer(Operation.GIT_BRANCH_SUMMARY, (request) => (request.git?.summaryBase === GitSummaryBase.HEAD ? summaryResponse("HEAD", 2, 9, 1) : summaryResponse("origin/main", 15, 418, 96)));
    rerender(view, <Probe at={target()} seen={seen} />);
    await settle();
    expect(host.of(Operation.GIT_BRANCHES)).toHaveLength(2);
    // The second read starts from the root the first one kept.
    expect(host.of(Operation.RESOLVE_ACTIVE_ROOT)[1]?.file?.knownRootToken).toBe(TOKEN);
    expect(gitRows(seen.details).uncommitted?.label).toBe("Uncommitted: +9 −1, 2 files");
  });

  it("drops an answer that arrives after Details closed, and reads afresh when it opens again", async () => {
    const host = repositoryHost();
    mocks.connection = host;
    const late = deferred<Response>();
    host.answer(Operation.RESOLVE_ACTIVE_ROOT, () => late.promise);
    const seen = { details: NO_GIT_DETAILS };
    const view = render(<Probe at={target()} seen={seen} />);
    await settle();
    rerender(view, <Probe at={target({ open: false })} seen={seen} />);
    late.resolve(rootResponse());
    await settle();
    expect(seen.details).toBe(NO_GIT_DETAILS);
    // The read that was dropped asks nothing more.
    expect(host.requests).toHaveLength(1);
    expect(filesStore.getState().roots[PANE]).toBeUndefined();

    host.answer(Operation.RESOLVE_ACTIVE_ROOT, () => rootResponse());
    rerender(view, <Probe at={target()} seen={seen} />);
    await settle();
    expect(host.of(Operation.RESOLVE_ACTIVE_ROOT)).toHaveLength(2);
    expect(gitRows(seen.details).branch).toBe("gpui/chat");
  });

  it("drops the answers for a folder the chat has left", async () => {
    const host = repositoryHost();
    mocks.connection = host;
    const slow = deferred<Response>();
    host.answer(Operation.GIT_BRANCHES, () => slow.promise);
    const seen = { details: NO_GIT_DETAILS };
    const view = render(<Probe at={target()} seen={seen} />);
    await settle();
    expect(gitRows(seen.details)).toMatchObject({ branch: "…", changes: { title: "Changes vs main" } });

    // The pane moved to a folder outside any repository.
    host.answer(Operation.RESOLVE_ACTIVE_ROOT, () => rootResponse({ root: "/srv/app", gitWorktree: false, rootToken: "file-v2:other" }));
    rerender(view, <Probe at={target({ folder: "/srv/app" })} seen={seen} />);
    // Nothing of the old folder is shown, even before the new answer.
    expect(seen.details).toBe(NO_GIT_DETAILS);
    await settle();
    expect(seen.details).toEqual(NOT_A_REPOSITORY);
    slow.resolve(branchesResponse("gpui/chat"));
    await settle();
    expect(seen.details).toEqual(NOT_A_REPOSITORY);
  });

  it("shows what it read last when it opens again, until the new answers arrive", async () => {
    const host = repositoryHost();
    mocks.connection = host;
    const seen = { details: NO_GIT_DETAILS };
    const view = render(<Probe at={target()} seen={seen} />);
    await settle();
    rerender(view, <Probe at={target({ open: false })} seen={seen} />);
    const again = deferred<Response>();
    host.answer(Operation.GIT_BRANCH_SUMMARY, (request) => (request.git?.summaryBase === GitSummaryBase.HEAD ? again.promise : summaryResponse("origin/main", 14, 412, 96)));
    rerender(view, <Probe at={target()} seen={seen} />);
    await settle();
    expect(gitRows(seen.details).uncommitted?.label).toBe("Uncommitted: +3 −1, 1 file");
    again.resolve(summaryResponse("HEAD", 0, 0, 0));
    await settle();
    expect(gitRows(seen.details).uncommitted?.label).toBe("Uncommitted: none");
  });

  it("waits for the connection, and reads once the phone is connected", async () => {
    const host = repositoryHost();
    mocks.connection = host;
    const seen = { details: NO_GIT_DETAILS };
    const view = render(<Probe at={target({ connected: false })} seen={seen} />);
    await settle();
    expect(host.requests).toHaveLength(0);
    rerender(view, <Probe at={target()} seen={seen} />);
    await settle();
    expect(gitRows(seen.details).branch).toBe("gpui/chat");
  });

  it("shows no Git row with no connection to ask", async () => {
    const seen = { details: NO_GIT_DETAILS };
    render(<Probe at={target()} seen={seen} />);
    await settle();
    expect(seen.details).toBe(NO_GIT_DETAILS);
  });
});
