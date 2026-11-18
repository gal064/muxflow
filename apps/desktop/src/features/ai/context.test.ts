import { describe, expect, it } from "vitest";
import type { AgentGeneration } from "../agents/generation";
import {
  ALL_AI_SOURCES, buildContext, buildSystemPrompt, contextSection,
  type AiContextInput,
} from "./context";
import type { GitStatusSnapshot } from "../git/types";

const input = (overrides: Partial<AiContextInput> = {}): AiContextInput => ({
  hostLabel: "devbox",
  transport: "ssh",
  connected: true,
  snapshot: {
    sessions: [{ id: "$1", name: "work", windowCount: 2, attachedClients: 1 }],
    windows: [{ id: "@1", sessionId: "$1", index: 0, name: "editor", active: true, layout: "even-horizontal" }],
    panes: [{
      id: "1", sessionId: "$1", windowId: "@1", index: 0, active: true,
      width: 80, height: 24, left: 0, top: 0, currentPath: "/home/dev/repo", currentCommand: "node",
    }],
  },
  activeSessionId: "$1",
  activeWindowId: "@1",
  activePaneId: "1",
  agents: [],
  openTabs: [],
  portForwards: [],
  ...overrides,
});

const gitStatus = (entries: number, overrides: Partial<GitStatusSnapshot> = {}): GitStatusSnapshot => ({
  repository: { id: "r", worktreeRoot: "/home/dev/repo", initial: false, detachedHead: false, headName: "main" },
  generation: "7",
  sourceGeneration: "7",
  authoritative: true,
  entries: Array.from({ length: entries }, (_, index) => ({
    path: `cGF0aA${index}`, displayPath: `src/file${index}.ts`,
    indexKind: "modified", worktreeKind: "none", indexStatus: "M", worktreeStatus: " ",
    conflicted: false, untracked: false, ignored: false, submodule: false, symlink: false, binary: false,
  })),
  ...overrides,
});

describe("the assistant's context snapshot", () => {
  it("names every section after the tab it maps to", () => {
    const context = buildContext(ALL_AI_SOURCES, input({
      activeRootPath: "/home/dev/repo",
      gitStatus: gitStatus(1),
      automations: { automations: [], fileError: "", generation: "1", stateDir: "/s" },
    }));
    for (const header of ["## Session tab", "## Files tab", "## Git tab", "## Ports tab", "## Automations tab"]) {
      expect(context).toContain(header);
    }
  });

  it("leaves out the sections the user turned off", () => {
    const context = buildContext(["git"], input({ gitStatus: gitStatus(0) }));
    expect(context).toContain("## Git tab");
    expect(context).not.toContain("## Files tab");
    expect(context).not.toContain("## Session tab");
    expect(buildContext([], input())).toBe("");
  });

  it("describes the live session: the workspace, the window, the panes and the agents", () => {
    const section = contextSection("session", input({
      agents: [{
        id: "a1", adapterId: "claude", nativeSessionId: "n", displayName: "claude", paneTitle: "claude ✦ fix",
        lifecycle: "blocked", lifecycleGeneration: "1" as AgentGeneration, attentionGeneration: "1" as AgentGeneration,
        attentionKind: "blocked", seenGeneration: "0" as AgentGeneration,
        updatedAt: 0, lifecycleChangedAt: 0, attentionSeenAt: 0,
        detectedManually: false, present: true,
        hostProfileId: "dev", serverIdentity: "tmux:a", sessionId: "$1", sessionName: "work",
        windowId: "@1", windowName: "editor", paneId: "1",
      }],
    }));
    expect(section).toContain("devbox (ssh, connected)");
    expect(section).toContain('window "editor" of workspace "work"');
    expect(section).toContain("#0 (active) in /home/dev/repo running node");
    expect(section).toContain("claude in work/editor — blocked, waiting for input");
  });

  it("says plainly what an empty tab holds instead of saying nothing", () => {
    expect(contextSection("git", input())).toContain("no repository");
    expect(contextSection("files", input())).toContain("no root");
    expect(contextSection("ports", input())).toContain("No port forwards are active");
    expect(contextSection("automations", input())).toContain("not reachable");
  });

  it("lists git changes and bounds a huge status with a count of the rest", () => {
    const few = contextSection("git", input({ gitStatus: gitStatus(2) }));
    expect(few).toContain("branch main");
    expect(few).toContain("M src/file0.ts");
    const many = contextSection("git", input({ gitStatus: gitStatus(70) }));
    expect(many).toContain("Changes (70)");
    expect(many).toContain("…and 10 more.");
    expect(many).not.toContain("src/file60.ts");
  });

  it("carries the open documents and the ports the host is listening on", () => {
    expect(contextSection("files", input({
      activeRootPath: "/home/dev/repo",
      openTabs: [{ kind: "file", title: "main.rs", resource: "/home/dev/repo/src/main.rs" }],
    }))).toContain("main.rs [file]");
    const ports = contextSection("ports", input({
      portForwards: [{ profileId: "dev", remotePort: 3000, localPort: 3000, state: "active" }],
      detectedPorts: [{ port: 8080, process: "python" }],
    }));
    expect(ports).toContain("3000→localhost:3000 (active)");
    expect(ports).toContain("8080 (python)");
  });

  it("describes automations in the words the Automations tab uses", () => {
    const section = contextSection("automations", input({
      automations: {
        automations: [{
          definition: {
            id: "nightly", name: "Nightly triage", enabled: true, schedule: "0 7 * * *",
            cwd: "/w", agent: "claude", command: "", prompt: "triage", keepTabs: 3, maxRuntimeMinutes: 60,
          },
          definitionError: "", nextRun: "", recentRuns: [{
            number: 12, scheduledFor: "2026-11-14T07:00", startedAt: 0, finishedAt: 1,
            status: "done", sessionId: "$1", windowId: "@1", paneId: "%1", serverIdentity: "s",
            lastMessage: "", error: "",
          }],
          commandLine: "claude",
        }],
        fileError: "", generation: "1", stateDir: "/s",
      },
    }));
    expect(section).toContain("Nightly triage");
    expect(section).toContain("on");
    expect(section).toContain("last run #12 done");
  });
});

describe("the assistant's system prompt", () => {
  it("names the app, the tabs and the rule against inventing what a tab holds", () => {
    const prompt = buildSystemPrompt(buildContext(["git"], input({ gitStatus: gitStatus(0) })));
    expect(prompt).toContain("Muxflow");
    expect(prompt).toContain("Files, Git, Ports, Automations");
    expect(prompt).toContain("## Git tab");
    expect(prompt).toContain("never invent");
  });

  it("still answers as a snapshot when nothing was included", () => {
    expect(buildSystemPrompt("")).toContain("No context was included");
  });
});
