// Details (docs/design/mobile-chat-mock.html, S8): the host, the folder as
// the desktop names it, and the Git rows in each state the host can leave
// them in.

import type { ReactTestInstance, ReactTestRenderer } from "react-test-renderer";
import { describe, expect, it } from "vitest";

import { allByLabel, byLabel, isHost, press, render } from "../../../testing/render";
import { textOf, writeRender } from "../../../testing/renderTree";
import { colors } from "../../../ui/tokens";
import { NO_GIT_DETAILS, NOT_A_REPOSITORY, type GitDetails, type GitRead, type GitSummary } from "../gitDetails";
import { chatItems, paneFrom, snapshotEvent } from "../testing";
import { DetailsSheet } from "./DetailsSheet";

const FOLDER = "/home/operator/dev/muxflow/apps/mobile";
const ready = <T,>(value: T): GitRead<T> => ({ status: "ready", value });
const summary = (baseRef: string, files: number, added: number, removed: number): GitRead<GitSummary> => ready({ baseRef, files: BigInt(files), added: BigInt(added), removed: BigInt(removed) });
const READING = { status: "reading" } as const;

/** The mock's repository: on gpui/chat, 14 files against main, one uncommitted. */
function repository(fields: Partial<GitDetails> = {}): GitDetails {
  return {
    repository: "git",
    root: "/home/operator/dev/muxflow",
    branch: ready("gpui/chat"),
    changes: summary("origin/main", 14, 412, 96),
    uncommitted: summary("HEAD", 1, 3, 1),
    ...fields,
  };
}

function details(git: GitDetails | undefined, fields: { folder?: string; hostLabel?: string; options?: unknown[] } = {}): ReactTestRenderer {
  return render(
    <DetailsSheet
      busy={false}
      folder={fields.folder ?? FOLDER}
      git={git}
      hostLabel={fields.hostLabel ?? "devbox"}
      onDismiss={() => undefined}
      onSetOptions={(options) => fields.options?.push(options)}
      pane={paneFrom([snapshotEvent(5n, chatItems())])}
      visible
    />,
  );
}

function flat(style: unknown): Record<string, unknown> {
  if (!style) return {};
  if (Array.isArray(style)) return Object.assign({}, ...style.map(flat));
  return style as Record<string, unknown>;
}

/** The text run reading `text` inside `row`. */
function run(row: ReactTestInstance, text: string): ReactTestInstance {
  return row.find((node) => isHost(node, "Text") && node.children.join("") === text);
}

/** What the top group reads, before the context meter. */
function place(view: ReactTestRenderer): string {
  return textOf(view.toJSON()).split(" Context ")[0]!;
}

describe("Details: where the chat runs", () => {
  it("the host, the folder inside its repository, the branch and both change rows", () => {
    const view = details(repository());
    expect(place(view)).toBe("Details devbox muxflow/apps/mobile gpui/chat Changes vs main +412 −96 14 files Uncommitted +3 −1 1 file");
    byLabel(view, "Host: devbox");
    // A screen reader gets the whole path the line shortens.
    const folder = byLabel(view, `Folder: ${FOLDER}`);
    const name = run(folder, "muxflow/apps/mobile");
    expect(name.props.numberOfLines).toBe(1);
    expect(name.props.ellipsizeMode).toBe("head");
    expect(flat(name.props.style).fontFamily).toBe("JetBrains Mono");
    expect(flat(run(byLabel(view, "Branch: gpui/chat"), "gpui/chat").props.style).fontFamily).toBe("JetBrains Mono");

    const changes = byLabel(view, "Changes vs main: +412 −96, 14 files");
    expect(flat(run(changes, "+412").props.style).color).toBe(colors.okInk);
    expect(flat(run(changes, "−96").props.style).color).toBe(colors.dangerInk);
    expect(flat(run(changes, "14 files").props.style).color).toBe(colors.chromeDim);
    const uncommitted = byLabel(view, "Uncommitted: +3 −1, 1 file");
    expect(flat(run(uncommitted, "+3").props.style).color).toBe(colors.okInk);
    expect(flat(run(uncommitted, "−1").props.style).color).toBe(colors.dangerInk);
    run(uncommitted, "1 file");
    // Host, folder and branch each carry a drawn mark; the change rows sit under the branch's.
    for (const label of ["Host: devbox", `Folder: ${FOLDER}`, "Branch: gpui/chat"]) {
      expect(byLabel(view, label).findAll((node) => isHost(node, "Svg"))).toHaveLength(1);
    }
    expect(changes.findAll((node) => isHost(node, "Svg"))).toHaveLength(0);
    writeRender("details-git", view.toJSON());
  });

  it("no Git repository: the folder relative to home, and no Git row", () => {
    for (const git of [NOT_A_REPOSITORY, NO_GIT_DETAILS, undefined]) {
      const view = details(git, { folder: "/home/operator/notes/drafts" });
      expect(place(view)).toBe("Details devbox ~/notes/drafts");
      expect(allByLabel(view, /^Branch:|^Changes|^Uncommitted/)).toHaveLength(0);
    }
    expect(place(details(NOT_A_REPOSITORY, { folder: "/srv/app" }))).toBe("Details devbox /srv/app");
    writeRender("details-no-git", details(NOT_A_REPOSITORY, { folder: "/home/operator/notes/drafts" }).toJSON());
  });

  it("a read in flight: … where the branch and the counts will be", () => {
    const view = details(repository({ branch: READING, changes: READING, uncommitted: READING }));
    expect(place(view)).toBe("Details devbox muxflow/apps/mobile … Changes … Uncommitted …");
    expect(flat(run(byLabel(view, "Changes: …"), "…").props.style).color).toBe(colors.chromeDim);
    byLabel(view, "Uncommitted: …");
    writeRender("details-reading", view.toJSON());
  });

  it("detached HEAD, a clean tree, and a branch with nothing on it", () => {
    const view = details(repository({ branch: ready(""), changes: summary("origin/main", 0, 0, 0), uncommitted: summary("HEAD", 0, 0, 0) }));
    expect(place(view)).toBe("Details devbox muxflow/apps/mobile Detached HEAD Uncommitted none");
    byLabel(view, "Branch: Detached HEAD");
    const uncommitted = byLabel(view, "Uncommitted: none");
    // A row with nothing to show is a step quieter.
    expect(flat(run(uncommitted, "Uncommitted").props.style).color).toBe(colors.chromeDim);
    expect(flat(run(uncommitted, "none").props.style).color).toBe(colors.chromeDim);
    expect(allByLabel(view, /^Changes/)).toHaveLength(0);
  });

  it("a read that failed leaves its row out", () => {
    const failed = { status: "failed" } as const;
    const view = details(repository({ branch: failed, uncommitted: failed }));
    expect(place(view)).toBe("Details devbox muxflow/apps/mobile Changes vs main +412 −96 14 files");
  });

  it("names a folder outside the repository it was given from home, and leaves the host out when it has no name", () => {
    const view = details(repository(), { folder: "/home/operator/dev/other", hostLabel: "" });
    expect(place(view)).toContain("Details ~/dev/other gpui/chat");
    expect(allByLabel(view, /^Host:/)).toHaveLength(0);
  });

  it("keeps the context meter and the controls under it", () => {
    const options: unknown[] = [];
    const view = details(repository(), { options });
    const shown = textOf(view.toJSON());
    for (const words of ["Context", "209k / 1M", "Model", "Opus 5.5", "Effort", "Permissions", "Speed", "Changes apply from the next message."]) expect(shown).toContain(words);
    expect(byLabel(view, "Effort: High").props.accessibilityState).toMatchObject({ checked: true });
    press(byLabel(view, "Effort: Max"));
    expect(options).toEqual([{ effort: "max" }]);
  });
});
