import { create } from "@bufbuild/protobuf";
import { describe, expect, it, vi } from "vitest";
import {
  ActiveRootSchema,
  FileMetadataSchema,
  FileServiceResponseSchema,
  ResponseSchema,
} from "../../protocol/gen/envelope_pb";
import { initialSessionState } from "../../store/sessionStore";
import { createFilesStore, terminalFileKey } from "./filesStore";
import { resolveTerminalFile, terminalFileCaptureIsCurrent, type TerminalFileCapture } from "./terminalFile";

const capture: TerminalFileCapture = {
  pane: { id: "%7", sessionId: "$2", windowId: "@5", currentPath: "/work/src" },
  serverIdentity: "server-a",
  topologyGeneration: 40n,
};

describe("terminal file resolution", () => {
  it("maps the host-returned canonical path and narrow root capability", async () => {
    const request = vi.fn().mockResolvedValue(create(ResponseSchema, {
      file: create(FileServiceResponseSchema, {
        activeRoot: create(ActiveRootSchema, {
          paneId: "%7",
          root: "/outside",
          rootToken: "file-v2:narrow",
          serverIdentity: "server-a",
          topologyGeneration: 41n,
          rootGeneration: 9n,
        }),
        metadata: create(FileMetadataSchema, { path: "/outside/notes.txt", name: "notes.txt" }),
      }),
    }));

    await expect(resolveTerminalFile(request, "../notes.txt", capture)).resolves.toEqual({
      path: "/outside/notes.txt",
      name: "notes.txt",
      topologyGeneration: 41n,
      root: {
        paneId: "%7",
        root: "/outside",
        rootToken: "file-v2:narrow",
        gitWorktree: false,
        rootGeneration: 9n,
      },
    });
    expect(request.mock.calls[0]?.[0].file).toMatchObject({
      paneId: "%7",
      path: "../notes.txt",
      expectedSessionId: "$2",
      expectedWindowId: "@5",
      expectedCwd: "/work/src",
      expectedTopologyGeneration: 40n,
    });
  });

  it("rejects a mismatched host or stale topology", async () => {
    const response = (serverIdentity: string, topologyGeneration: bigint) => create(ResponseSchema, {
      file: create(FileServiceResponseSchema, {
        activeRoot: create(ActiveRootSchema, {
          paneId: "%7", root: "/work", rootToken: "token", serverIdentity, topologyGeneration,
        }),
        metadata: create(FileMetadataSchema, { path: "/work/a.txt", name: "a.txt" }),
      }),
    });
    await expect(resolveTerminalFile(vi.fn().mockResolvedValue(response("server-b", 41n)), "./a.txt", capture))
      .rejects.toThrow("another terminal scope");
    await expect(resolveTerminalFile(vi.fn().mockResolvedValue(response("server-a", 39n)), "./a.txt", capture))
      .rejects.toThrow("stale topology");
  });

  it("accepts only the same live pane route at or beyond the resolved generation", () => {
    const state = initialSessionState();
    state.serverIdentity = "server-a";
    state.topologyGeneration = 41n;
    state.panes["%7"] = {
      id: "%7", sessionId: "$2", windowId: "@5", currentPath: "/work/src",
      index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentCommand: "bash",
    };
    expect(terminalFileCaptureIsCurrent(capture, state, 41n)).toBe(true);
    state.panes["%7"]!.currentPath = "/moved";
    expect(terminalFileCaptureIsCurrent(capture, state, 41n)).toBe(false);
  });

  it("keeps a terminal file capability separate from the pane's browsable root", () => {
    const store = createFilesStore();
    const browsable = { paneId: "%7", root: "/work", rootToken: "file-v2:root", gitWorktree: true, rootGeneration: 3n };
    const narrow = { paneId: "%7", root: "/outside", rootToken: "file-v2:narrow", gitWorktree: false, rootGeneration: 4n };
    store.getState().setRoot(browsable);
    store.getState().setTerminalFileRoot("/outside/notes.txt", narrow);

    expect(store.getState().roots["%7"]).toEqual(browsable);
    expect(store.getState().terminalFileRoots[terminalFileKey("%7", "/outside/notes.txt")]).toEqual(narrow);
    store.getState().clearAll();
    expect(store.getState().terminalFileRoots).toEqual({});
  });
});
