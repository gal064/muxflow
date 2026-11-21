// The Git read behind Details against the real `muxflow-host bridge --stdio`
// (docs/design/mobile-chat-plan.md §3 "Git rows"): the root the file screens
// resolve is one the Git service takes, and the answers make the rows the
// plan describes. Skipped when cargo, tmux or git is missing.

import { spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { liveHostAvailability, startHostHarness, type HostHarness } from "../../../scripts/host-harness";
import { HostConnection } from "../../protocol/HostConnection";
import { createSessionStore } from "../../store/sessionStore";
import { folderLabel, gitRows, NO_GIT_DETAILS, type GitDetails } from "./gitDetails";
import { readGitDetails, type GitChange } from "./useGitDetails";

const availability = liveHostAvailability();
const hasGit = spawnSync("git", ["--version"]).status === 0;

async function waitFor<T>(label: string, probe: () => T | undefined, timeoutMs = 15_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = probe();
    if (value !== undefined) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

describe.skipIf(!availability.available || !hasGit)(`live Git details (${availability.reason ?? "cargo + tmux present"})`, () => {
  let harness: HostHarness;
  const store = createSessionStore();
  let control: HostConnection;
  let epoch = 0;
  const say = (line: string) => console.log(`live: ${line}`);

  /** Git in the scratch folder, with none of this machine's configuration. */
  const git = (...args: string[]) => {
    const result = spawnSync("git", ["-c", "user.name=Muxflow", "-c", "user.email=muxflow@example.invalid", "-c", "commit.gpgsign=false", ...args], {
      cwd: harness.workDir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
    if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${result.stderr.trim()}`);
  };

  const read = async (paneId: string, from: GitDetails = NO_GIT_DETAILS): Promise<GitDetails> => {
    let details = from;
    const apply = (change: GitChange) => {
      details = change(details);
      return true;
    };
    await readGitDetails(control, paneId, undefined, apply);
    return details;
  };

  beforeAll(async () => {
    harness = await startHostHarness();
    control = new HostConnection({
      dial: async () => harness.transport,
      nextConnectionEpoch: () => (epoch += 1),
      store,
    });
    // `startHostHarness` may have to `cargo build -p muxflow-host` first.
  }, 600_000);

  afterAll(async () => {
    control?.disconnect();
    await harness?.stop();
  }, 60_000);

  it("reads the branch, the changes against the default branch, and the uncommitted work", async () => {
    // One commit on main, which the remote has; a branch with one more; one
    // line not committed yet.
    const file = (name: string, text: string) => writeFileSync(path.join(harness.workDir, name), text);
    git("init", "--quiet", "--initial-branch=main");
    file("README.md", "# Scratch\n");
    git("add", "README.md");
    git("commit", "--quiet", "-m", "Start");
    git("update-ref", "refs/remotes/origin/main", "HEAD");
    git("symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main");
    git("switch", "--quiet", "-c", "feature/details");
    file("notes.txt", "one\ntwo\n");
    git("add", "notes.txt");
    git("commit", "--quiet", "-m", "Add notes");
    file("README.md", "# Scratch\nA line not committed.\n");

    control.connect();
    await waitFor("connected", () => (store.getState().connection.state === "connected" ? true : undefined));
    const pane = Object.values(store.getState().panes).find((candidate) => candidate.sessionId === harness.primarySessionId)!;

    const details = await read(pane.id);
    say(`Git details → ${JSON.stringify(details, (_key, value: unknown) => (typeof value === "bigint" ? Number(value) : value))}`);
    expect(details.repository).toBe("git");
    expect(details.root).toBe(harness.workDir);
    expect(folderLabel(pane.currentPath, details.root)).toBe(path.basename(harness.workDir));
    const rows = gitRows(details);
    expect(rows.branch).toBe("feature/details");
    // Against origin/main: the committed file and the line not committed.
    expect(rows.changes).toMatchObject({ title: "Changes vs main", count: { kind: "lines", added: "+3", removed: "−0", files: "2 files" } });
    // Against HEAD: only the line not committed, and the answer says it is against HEAD.
    expect(details.uncommitted).toMatchObject({ status: "ready", value: { baseRef: "HEAD" } });
    expect(rows.uncommitted).toMatchObject({ title: "Uncommitted", count: { kind: "lines", added: "+1", removed: "−0", files: "1 file" } });

    // A clean tree on a detached HEAD.
    git("commit", "--quiet", "-am", "Say more");
    git("checkout", "--quiet", "--detach");
    const detached = gitRows(await read(pane.id, details));
    say(`detached and clean → ${JSON.stringify(detached)}`);
    expect(detached.branch).toBe("Detached HEAD");
    expect(detached.uncommitted?.count).toEqual({ kind: "none" });
    expect(detached.changes).toMatchObject({ title: "Changes vs main", count: { kind: "lines", added: "+3", removed: "−0", files: "2 files" } });
  }, 120_000);
});
