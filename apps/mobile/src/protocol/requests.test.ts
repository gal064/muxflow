import { describe, expect, it } from "vitest";
import { ChatAction, ChatPermissionMode, ChatSpeed } from "./gen/chat_pb";
import { CollisionPolicy, GitSummaryBase, Operation, TmuxActionKind, VoiceProvider } from "./gen/envelope_pb";
import { HistoryAction } from "./gen/history_pb";
import {
  agentDiagnostics, cancelTerminalUpload, chatAnswer, chatClose, chatInterrupt, chatLoadOlder, chatOpen, chatRewind, chatSend, chatSetOptions, chatSideFork,
  closeTmuxTarget, commitTerminalUpload, createSession, createWindow, gitBranches, gitBranchSummary, historyList, historyResume, historySnooze, historyWake, prepareTerminalUpload, resolveTerminalFile, setPinnedTarget,
  UPLOAD_CHUNK_BYTES, voiceProvision, voiceSession, voiceSpeak, voiceStatus, voiceTranscribe, writeTerminalUploadChunk,
} from "./requests";

describe("terminal file request builder", () => {
  it("binds the candidate to the exact pane route and host generation", () => {
    const request = resolveTerminalFile("op-file", "%7", "../README.md", "server-a", 42n, {
      sessionId: "$2",
      windowId: "@5",
      cwd: "/home/dev/project/src",
    });

    expect(request.operation).toBe(Operation.RESOLVE_TERMINAL_FILE);
    expect(request.file).toMatchObject({
      operationId: "op-file",
      paneId: "%7",
      path: "../README.md",
      expectedServerIdentity: "server-a",
      expectedTopologyGeneration: 42n,
      expectedSessionId: "$2",
      expectedWindowId: "@5",
      expectedCwd: "/home/dev/project/src",
    });
  });
});

// Field usage against the host's filesystem dispatcher and the desktop's
// upload client: the transfer id is both ids, and nothing but the fields each
// step reads is set.
describe("terminal upload request builders", () => {
  const id = "3e0f7a52-6f0b-4c36-9d55-0b1f5f1d2a77";

  it("prepares under the file's own name, renaming on a collision", () => {
    const request = prepareTerminalUpload(id, "shot.png", 2_621_440n);
    expect(request.operation).toBe(Operation.PREPARE_TERMINAL_UPLOAD);
    expect(request.file).toMatchObject({
      operationId: id,
      transferId: id,
      destination: "shot.png",
      sourceName: "shot.png",
      totalBytes: 2_621_440n,
      collisionPolicy: CollisionPolicy.RENAME,
      largeUploadConfirmed: false,
      imagePng: false,
    });
  });

  it("writes a chunk at its offset", () => {
    const content = new Uint8Array([1, 2, 3]);
    const request = writeTerminalUploadChunk(id, 1_048_576n, 2_621_440n, content);
    expect(request.operation).toBe(Operation.WRITE_TERMINAL_UPLOAD_CHUNK);
    expect(request.file).toMatchObject({ operationId: id, transferId: id, offset: 1_048_576n, totalBytes: 2_621_440n });
    expect(request.file?.content).toEqual(content);
    expect(request.data.length).toBe(0);
    expect(UPLOAD_CHUNK_BYTES).toBe(1024 * 1024);
  });

  it("commits with the size and the digest, and cancels by id alone", () => {
    const commit = commitTerminalUpload(id, 2_621_440n, "af13");
    expect(commit.operation).toBe(Operation.COMMIT_TERMINAL_UPLOAD);
    expect(commit.file).toMatchObject({ operationId: id, transferId: id, totalBytes: 2_621_440n, blake3: "af13" });

    const cancel = cancelTerminalUpload(id);
    expect(cancel.operation).toBe(Operation.CANCEL_TERMINAL_UPLOAD);
    expect(cancel.file).toMatchObject({ operationId: id, transferId: id, totalBytes: 0n, blake3: "", destination: "" });
  });
});

// Field usage against the host's Git service (apps/host/src/service/git/branch.rs):
// a read names the root, its token and the server, and nothing a mutation needs.
describe("git request builders", () => {
  it("asks for the summary against the default branch, or against HEAD", () => {
    const branch = gitBranchSummary("op-1", "/work/repo", "file-v2:root", "server-a");
    expect(branch.operation).toBe(Operation.GIT_BRANCH_SUMMARY);
    expect(branch.git).toMatchObject({
      operationId: "op-1",
      root: "/work/repo",
      rootToken: "file-v2:root",
      expectedServerIdentity: "server-a",
      summaryBase: GitSummaryBase.UNSPECIFIED,
      repositoryId: "",
      connectionEpoch: 0n,
      expectedStatusGeneration: 0n,
      watchId: "",
    });
    expect(branch.file).toBeUndefined();

    const uncommitted = gitBranchSummary("op-2", "/work/repo", "file-v2:root", "server-a", GitSummaryBase.HEAD);
    expect(uncommitted.operation).toBe(Operation.GIT_BRANCH_SUMMARY);
    expect(uncommitted.git).toMatchObject({ operationId: "op-2", root: "/work/repo", rootToken: "file-v2:root", expectedServerIdentity: "server-a", summaryBase: GitSummaryBase.HEAD });
  });

  it("asks for the branch list", () => {
    const request = gitBranches("op-3", "/work/repo", "file-v2:root", "server-a");
    expect(request.operation).toBe(Operation.GIT_BRANCHES);
    expect(request.git).toMatchObject({ operationId: "op-3", root: "/work/repo", rootToken: "file-v2:root", expectedServerIdentity: "server-a", summaryBase: GitSummaryBase.UNSPECIFIED, branch: "" });
  });
});

// Field usage of the voice builders against docs/mobile/voice-mode-plan.md §3:
// everything rides in `Request.voice`, nothing in `scope` or `data`.
describe("voice request builders", () => {
  it("requests host lifecycle diagnostics without a normal-operation payload", () => {
    const request = agentDiagnostics();
    expect(request.operation).toBe(Operation.AGENT_DIAGNOSTICS);
    expect(request.agent).toBeDefined();
  });

  it("voiceStatus probes readiness and only warms when asked", () => {
    const cold = voiceStatus("op-1");
    expect(cold.operation).toBe(Operation.VOICE_STATUS);
    expect(cold.voice?.operationId).toBe("op-1");
    expect(cold.voice?.warm).toBe(false);
    expect(cold.voice?.confirmed).toBe(false);
    expect(cold.scope).toBe("");
    expect(cold.data.length).toBe(0);

    expect(voiceStatus("op-2", true).voice?.warm).toBe(true);
  });

  it("voiceProvision carries the user's consent, which the host refuses without", () => {
    const request = voiceProvision("op-3");
    expect(request.operation).toBe(Operation.VOICE_PROVISION);
    expect(request.voice?.operationId).toBe("op-3");
    expect(request.voice?.confirmed).toBe(true);
    expect(request.voice?.warm).toBe(false);
  });

  it("voiceTranscribe puts the whole recording and its mime in the voice payload, not Request.data", () => {
    const audio = new Uint8Array([0, 0, 0, 0x1c, 0x66, 0x74, 0x79, 0x70, 0xff]);
    const request = voiceTranscribe("op-4", audio, "audio/mp4");
    expect(request.operation).toBe(Operation.VOICE_TRANSCRIBE);
    expect(request.voice?.operationId).toBe("op-4");
    expect(request.voice?.audio).toBe(audio);
    expect(request.voice?.audioMime).toBe("audio/mp4");
    expect(request.data.length).toBe(0);
  });

  it("voiceSpeak sends the text, an optional voice, and leaves the provider to the host default", () => {
    const request = voiceSpeak("op-5", "Done. Two files changed.");
    expect(request.operation).toBe(Operation.VOICE_SPEAK);
    expect(request.voice?.operationId).toBe("op-5");
    expect(request.voice?.text).toBe("Done. Two files changed.");
    expect(request.voice?.voice).toBe("");
    expect(request.voice?.provider).toBe(VoiceProvider.UNSPECIFIED);

    expect(voiceSpeak("op-6", "Hi", "en-GB-SoniaNeural").voice?.voice).toBe("en-GB-SoniaNeural");
  });

  it("voiceSession registers a pane id and clears it with the empty string", () => {
    const register = voiceSession("%7", "server-a");
    expect(register.operation).toBe(Operation.VOICE_SESSION);
    expect(register.voice?.paneId).toBe("%7");
    expect(register.voice?.expectedServerIdentity).toBe("server-a");
    expect(register.voice?.operationId).toBe("");

    expect(voiceSession("").voice?.paneId).toBe("");
  });
});

// The host refuses an unconfirmed close (`require_confirmation` in
// apps/host/src/service/tmux_actions.rs), and a pinned generation would come
// back stale_topology under an animating pane title.
describe("create request builders", () => {
  it("creates a named session whose first pane starts in the directory", () => {
    const request = createSession("random", "~/work", "server-a", 7n);
    expect(request.operation).toBe(Operation.TMUX_ACTION);
    expect(request.tmuxAction).toMatchObject({ kind: TmuxActionKind.CREATE_SESSION, name: "random", directory: "~/work", sessionId: "", windowId: "", pinned: false, expectedServerIdentity: "server-a", expectedGeneration: 7n });
  });

  it("creates a window in a session, in the directory when one is given", () => {
    const request = createWindow("$4", "server-a", 8n, "/srv");
    expect(request.operation).toBe(Operation.TMUX_ACTION);
    expect(request.tmuxAction).toMatchObject({ kind: TmuxActionKind.CREATE_WINDOW, sessionId: "$4", directory: "/srv", name: "", expectedServerIdentity: "server-a", expectedGeneration: 8n });
    expect(createWindow("$4", "server-a", 8n).tmuxAction).toMatchObject({ kind: TmuxActionKind.CREATE_WINDOW, sessionId: "$4", directory: "" });
  });
});

describe("close request builder", () => {
  it("sends a confirmed, generation-free close for each target kind", () => {
    expect(closeTmuxTarget({ kind: "pane", sessionId: "$1", windowId: "@2", paneId: "%3" }, "server-a").tmuxAction)
      .toMatchObject({ kind: TmuxActionKind.CLOSE_PANE, sessionId: "$1", windowId: "@2", paneId: "%3", expectedServerIdentity: "server-a", expectedGeneration: 0n, confirmed: true });
    expect(closeTmuxTarget({ kind: "window", sessionId: "$1", windowId: "@2" }, "server-a").tmuxAction)
      .toMatchObject({ kind: TmuxActionKind.CLOSE_WINDOW, sessionId: "$1", windowId: "@2", paneId: "", confirmed: true });
    const session = closeTmuxTarget({ kind: "session", sessionId: "$1" }, "server-a");
    expect(session.operation).toBe(Operation.TMUX_ACTION);
    expect(session.tmuxAction).toMatchObject({ kind: TmuxActionKind.CLOSE_SESSION, sessionId: "$1", windowId: "", paneId: "", confirmed: true });
  });
});

describe("pin request builder", () => {
  it("writes the given pin for a workspace, or for a tab inside it, without a generation", () => {
    expect(setPinnedTarget({ kind: "session", sessionId: "$1" }, true, "server-a").tmuxAction)
      .toMatchObject({ kind: TmuxActionKind.SET_PINNED, sessionId: "$1", windowId: "", pinned: true, expectedServerIdentity: "server-a", expectedGeneration: 0n });
    expect(setPinnedTarget({ kind: "window", sessionId: "$1", windowId: "@2" }, false, "server-a").tmuxAction)
      .toMatchObject({ kind: TmuxActionKind.SET_PINNED, sessionId: "$1", windowId: "@2", pinned: false });
  });
});

// Field usage against apps/host/src/service/requests/chat_dispatch.rs: every
// pane-scoped action names the pane and the server it was seen on, in
// `Request.chat`, and nothing rides in `scope` or `data`.
describe("chat request builders", () => {
  const target = { paneId: "%7", expectedServerIdentity: "server-a" };

  it("names the pane, the server and the action on every request", () => {
    const requests = [
      [chatOpen(target), ChatAction.OPEN],
      [chatClose(target), ChatAction.CLOSE],
      [chatSend(target, "hello", "m1"), ChatAction.SEND],
      [chatInterrupt(target), ChatAction.INTERRUPT],
      [chatAnswer(target, "q1", { skipped: true }), ChatAction.ANSWER],
      [chatRewind(target, "m1"), ChatAction.REWIND],
      [chatSetOptions(target, { model: "m" }), ChatAction.SET_OPTIONS],
      [chatLoadOlder(target, "c1"), ChatAction.LOAD_OLDER],
      [chatSideFork(target, "why?"), ChatAction.SIDE_FORK],
    ] as const;
    for (const [request, action] of requests) {
      expect(request.operation).toBe(Operation.CHAT);
      expect(request.chat).toMatchObject({ action, paneId: "%7", expectedServerIdentity: "server-a" });
      expect(request.scope).toBe("");
      expect(request.data.length).toBe(0);
    }
  });

  it("carries each action's own fields", () => {
    expect(chatSend(target, "hello", "m1").chat).toMatchObject({ text: "hello", clientMessageId: "m1" });
    expect(chatSend(target, "hello", "m1").chat?.attachments).toEqual([]);
    expect(chatRewind(target, "m1").chat?.itemId).toBe("m1");
    expect(chatLoadOlder(target, "c1").chat?.olderCursor).toBe("c1");
    expect(chatSideFork(target, "why?").chat?.text).toBe("why?");

    const answer = chatAnswer(target, "q1", { answers: [{ questionId: "scope", optionIds: ["a", "b"], otherText: "c" }] }).chat;
    expect(answer?.requestId).toBe("q1");
    expect(answer?.answer?.answers[0]).toMatchObject({ questionId: "scope", optionIds: ["a", "b"], otherText: "c" });
    expect(chatAnswer(target, "p1", { allow: true }).chat?.answer).toMatchObject({ allow: true, skipped: false });

    // Unset options stay empty or unspecified, which the host reads as "keep".
    const options = chatSetOptions(target, { speed: ChatSpeed.FAST }).chat?.options;
    expect(options).toMatchObject({ model: "", effort: "", permission: ChatPermissionMode.UNSPECIFIED, speed: ChatSpeed.FAST });
  });
});

describe("chat send with attachments", () => {
  const target = { paneId: "%7", expectedServerIdentity: "server-a" };

  it("names each uploaded file by its path and name, and leaves type and size to the host", () => {
    const request = chatSend(target, "", "m2", [
      { path: "/home/dev/.cache/muxflow/uploads/shot (1).png", name: "shot.png" },
      { path: "/home/dev/.cache/muxflow/uploads/notes.txt", name: "notes.txt" },
    ]);
    expect(request.operation).toBe(Operation.CHAT);
    expect(request.chat).toMatchObject({ action: ChatAction.SEND, text: "", clientMessageId: "m2" });
    expect(request.chat?.attachments).toHaveLength(2);
    expect(request.chat?.attachments[0]).toMatchObject({
      path: "/home/dev/.cache/muxflow/uploads/shot (1).png",
      name: "shot.png",
      mediaType: "",
      sizeBytes: 0n,
    });
    expect(request.chat?.attachments[1]).toMatchObject({ path: "/home/dev/.cache/muxflow/uploads/notes.txt", name: "notes.txt" });
  });
});

describe("history request builders", () => {
  const entry = { adapterId: "codex", nativeSessionId: "s1" };
  const topology = { expectedServerIdentity: "server-a", expectedTopologyGeneration: 42n };

  it("lists with no target, and names the session for every other action", () => {
    const list = historyList();
    expect(list.operation).toBe(Operation.SESSION_HISTORY);
    expect(list.history).toMatchObject({ action: HistoryAction.LIST, adapterId: "", nativeSessionId: "" });

    expect(historyResume(entry, topology).history).toMatchObject({ action: HistoryAction.RESUME, ...entry, ...topology });
    expect(historySnooze(entry, 1_800_000_000_000n, topology).history)
      .toMatchObject({ action: HistoryAction.SNOOZE, ...entry, ...topology, snoozeUntilUnixMillis: 1_800_000_000_000n });
    expect(historyWake(entry).history).toMatchObject({ action: HistoryAction.WAKE, ...entry, snoozeUntilUnixMillis: 0n });
  });
});
