// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PanelExtensionProps, PanelHost } from "../shell/panelExtensions";
import { publishAutomationChanged } from "./api";
import { AutomationEditor, commandPreview, idFromName } from "./AutomationEditor";
import { AutomationsPanel } from "./AutomationsPanel";
import type { AutomationListing, AutomationSummary } from "./types";

const api = vi.hoisted(() => ({
  list: vi.fn(),
  save: vi.fn(),
  remove: vi.fn(),
  runNow: vi.fn(),
  previewSchedule: vi.fn(),
}));
vi.mock("./api", async (original) => ({ ...await original<typeof import("./api")>(), automationsApi: api }));

const devbox: PanelHost = {
  profileId: "devbox", label: "Devbox", clientId: "client-1", connected: true,
  serverIdentity: "tmux:a", hasPane: (paneId) => paneId === "%87",
};

function summary(overrides: Partial<AutomationSummary["definition"]> = {}): AutomationSummary {
  return {
    definition: {
      id: "fix-ci", name: "Fix CI", enabled: true, schedule: "0 9 * * 1-5", cwd: "~/dev/muxflow",
      agent: "codex", command: "", prompt: "Fix CI or say it is green.", keepTabs: 5, maxRuntimeMinutes: 120,
      ...overrides,
    },
    definitionError: "",
    nextRun: "2099-01-01T09:00",
    recentRuns: [{
      number: 12, scheduledFor: "2026-10-13T09:00", startedAt: 1_000, finishedAt: 61_000, status: "done",
      sessionId: "$4", windowId: "@41", paneId: "%87", serverIdentity: "tmux:a", lastMessage: "CI is green.", error: "",
    }],
    commandLine: "codex --approve-for-me -- \"$(cat '/state/automations/prompts/fix-ci-<n>.txt')\"",
  };
}

function listing(automations: AutomationSummary[], extra: Partial<AutomationListing> = {}): AutomationListing {
  return { automations, fileError: "", generation: "g1", stateDir: "/home/me/.local/state/muxflow", ...extra };
}

function props(overrides: Partial<PanelExtensionProps> = {}): PanelExtensionProps {
  return { activeProfileId: "devbox", hosts: [devbox], onMessage: vi.fn(), onOpenPane: vi.fn(), ...overrides };
}

const mounted: ReactTestRenderer[] = [];

async function render(element: React.ReactElement): Promise<ReactTestRenderer> {
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(element); });
  mounted.push(renderer);
  return renderer;
}

/** Every string rendered, joined, so text split across children still reads whole. */
const text = (renderer: ReactTestRenderer): string => {
  const strings = (node: unknown): string => typeof node === "string" ? node
    : Array.isArray(node) ? node.map(strings).join("")
      : node && typeof node === "object" && "children" in node ? strings((node as { children: unknown }).children) : "";
  return strings(renderer.toJSON());
};
const button = (renderer: ReactTestRenderer, label: string) =>
  renderer.root.findAll((node) => node.type === "button" && node.children.join("") === label)[0];

afterEach(async () => {
  await act(async () => { for (const renderer of mounted.splice(0)) renderer.unmount(); });
});

beforeEach(() => {
  vi.clearAllMocks();
  api.previewSchedule.mockResolvedValue({ nextRuns: ["2099-01-01T09:00"], scheduleError: "" });
});

describe("Automations panel", () => {
  it("lists a host's automations with their last run and next run", async () => {
    api.list.mockResolvedValue(listing([summary()]));
    const renderer = await render(<AutomationsPanel {...props()} />);
    expect(api.list).toHaveBeenCalledWith("client-1");
    const shown = text(renderer);
    expect(shown).toContain("Fix CI");
    expect(shown).toContain("#12 done");
    expect(shown).toContain("Weekdays 09:00");
    expect(shown).toContain("codex");
  });

  it("pauses through a save over the listed version, and runs now", async () => {
    api.list.mockResolvedValue(listing([summary()]));
    api.save.mockResolvedValue(listing([summary({ enabled: false })], { generation: "g2" }));
    api.runNow.mockResolvedValue(listing([summary()]));
    const renderer = await render(<AutomationsPanel {...props()} />);
    await act(async () => { renderer.root.findByProps({ role: "switch" }).props.onClick(); });
    expect(api.save).toHaveBeenCalledWith("client-1", expect.objectContaining({ id: "fix-ci", enabled: false }), "g1");
    expect(text(renderer)).toContain("Paused");
    await act(async () => { button(renderer, "Run now").props.onClick(); });
    expect(api.runNow).toHaveBeenCalledWith("client-1", "fix-ci");
  });

  it("reloads when the host's file changed under a save", async () => {
    api.list.mockResolvedValue(listing([summary()]));
    api.save.mockRejectedValue("stale_generation: automations.json changed since it was listed");
    const onMessage = vi.fn();
    const renderer = await render(<AutomationsPanel {...props({ onMessage })} />);
    await act(async () => { renderer.root.findByProps({ role: "switch" }).props.onClick(); });
    expect(api.list).toHaveBeenCalledTimes(2);
    expect(onMessage).toHaveBeenCalledWith(expect.stringContaining("reloaded"));
  });

  it("lists again when its host says automations changed, and only its host", async () => {
    api.list.mockResolvedValue(listing([]));
    await render(<AutomationsPanel {...props()} />);
    await act(async () => { publishAutomationChanged("other"); });
    expect(api.list).toHaveBeenCalledTimes(1);
    await act(async () => { publishAutomationChanged("devbox"); });
    expect(api.list).toHaveBeenCalledTimes(2);
  });

  it("shows where the file lives when there is none, and the error when it can't be read", async () => {
    api.list.mockResolvedValue(listing([]));
    const empty = await render(<AutomationsPanel {...props()} />);
    expect(text(empty)).toContain("No automations on Devbox");
    expect(text(empty)).toContain("/home/me/.local/state/muxflow/automations.json");

    api.list.mockResolvedValue(listing([summary()], { fileError: "automations.json is not valid JSON: line 14" }));
    const broken = await render(<AutomationsPanel {...props()} />);
    expect(text(broken)).toContain("can't be read");
    expect(text(broken)).toContain("line 14");
    expect(text(broken)).toContain("Fix CI");
    expect(button(broken, "New").props.disabled).toBe(true);
    expect(broken.root.findByProps({ role: "switch" }).props.disabled).toBe(true);
    expect(button(broken, "Run now").props.disabled).toBe(false);
  });

  it("starts one run per click, not one per impatient click", async () => {
    api.list.mockResolvedValue(listing([summary()]));
    let answer!: (value: AutomationListing) => void;
    api.runNow.mockReturnValueOnce(new Promise((resolve) => { answer = resolve; }));
    const renderer = await render(<AutomationsPanel {...props()} />);
    await act(async () => { button(renderer, "Run now").props.onClick(); });
    expect(button(renderer, "Starting…").props.disabled).toBe(true);
    await act(async () => { button(renderer, "Starting…").props.onClick(); });
    expect(api.runNow).toHaveBeenCalledTimes(1);
    await act(async () => { answer(listing([summary()])); });
    expect(button(renderer, "Run now").props.disabled).toBe(false);
  });

  it("shows a run still working over a newer skip", async () => {
    const working = summary();
    working.recentRuns = [
      { ...working.recentRuns[0], number: 13, status: "skipped", lastMessage: "#12 was still working" },
      { ...working.recentRuns[0], status: "working", finishedAt: 0, startedAt: Date.now() - 20 * 60_000 },
    ];
    api.list.mockResolvedValue(listing([working]));
    const renderer = await render(<AutomationsPanel {...props()} />);
    expect(text(renderer)).toContain("#12 working · 20m");
  });

  it("explains a disconnected host instead of asking it anything", async () => {
    const renderer = await render(<AutomationsPanel {...props({ hosts: [{ ...devbox, connected: false }] })} />);
    expect(api.list).not.toHaveBeenCalled();
    expect(text(renderer)).toContain("Devbox is disconnected");
  });

  it("opens a run's tab from the detail view", async () => {
    api.list.mockResolvedValue(listing([summary()]));
    const onOpenPane = vi.fn();
    const renderer = await render(<AutomationsPanel {...props({ onOpenPane })} />);
    await act(async () => { renderer.root.findByProps({ className: "automation-row-main" }).props.onClick(); });
    expect(text(renderer)).toContain("CI is green.");
    await act(async () => { button(renderer, "Open tab").props.onClick(); });
    expect(onOpenPane).toHaveBeenCalledWith({ profileId: "devbox", sessionId: "$4", paneId: "%87", source: "Fix CI #12" });
  });

  it("offers no Open tab for a tab that has closed, or one on a tmux server since replaced", async () => {
    api.list.mockResolvedValue(listing([summary()]));
    for (const host of [{ ...devbox, hasPane: () => false }, { ...devbox, serverIdentity: "tmux:b" }]) {
      const renderer = await render(<AutomationsPanel {...props({ hosts: [host] })} />);
      await act(async () => { renderer.root.findByProps({ className: "automation-row-main" }).props.onClick(); });
      expect(button(renderer, "Open tab")).toBeUndefined();
      expect(text(renderer)).toContain("tab closed");
    }
  });

  it("keeps only the newest list when an older one answers late", async () => {
    let answerSlow!: (value: AutomationListing) => void;
    api.list.mockReturnValueOnce(new Promise((resolve) => { answerSlow = resolve; }));
    api.list.mockResolvedValue(listing([summary({ name: "Newest" })]));
    const renderer = await render(<AutomationsPanel {...props()} />);
    await act(async () => { publishAutomationChanged("devbox"); });
    await act(async () => { answerSlow(listing([summary({ name: "Stale" })])); });
    expect(text(renderer)).toContain("Newest");
    expect(text(renderer)).not.toContain("Stale");
  });
});

describe("Automation editor", () => {
  const editor = (overrides: Partial<React.ComponentProps<typeof AutomationEditor>> = {}) => <AutomationEditor
    clientId="client-1" existingIds={["fix-ci"]} generation="g1" hostLabel="Devbox"
    onClose={vi.fn()} onSaved={vi.fn()} onStale={vi.fn()} stateDir="/state" {...overrides}
  />;

  it("names a new automation's id after it, uniquely", () => {
    expect(idFromName("Fix CI!", [])).toBe("fix-ci");
    expect(idFromName("Fix CI", ["fix-ci", "fix-ci-2"])).toBe("fix-ci-3");
    expect(idFromName("???", [])).toBe("automation");
  });

  it("previews exactly what is typed for presets and custom commands", () => {
    const base = summary().definition;
    expect(commandPreview(base, "/state")).toBe(
      "codex --approve-for-me -- \"$(cat '/state/automations/prompts/fix-ci-<n>.txt')\"",
    );
    expect(commandPreview({ ...base, agent: "custom", command: "cx --print" }, "/state"))
      .toBe("cx --print \"$(cat '/state/automations/prompts/fix-ci-<n>.txt')\"");
  });

  it("saves a new automation, then runs it when asked", async () => {
    vi.useFakeTimers();
    api.save.mockResolvedValue(listing([summary()]));
    api.runNow.mockResolvedValue(listing([summary()]));
    const onSaved = vi.fn();
    const renderer = await render(editor({ onSaved }));
    const field = (label: string) => renderer.root.findAll((node) => node.type === "label" && node.children.includes(label))[0]
      .findAll((node) => node.type === "input" || node.type === "textarea")[0];
    await act(async () => { field("Name").props.onChange({ target: { value: "Fix CI" } }); });
    await act(async () => { field("Prompt").props.onChange({ target: { value: "Go." } }); });
    await act(async () => { vi.advanceTimersByTime(400); });
    expect(api.previewSchedule).toHaveBeenCalledWith("client-1", "0 9 * * 1-5");
    await act(async () => { button(renderer, "Save and run now").props.onClick(); });
    expect(api.save).toHaveBeenCalledWith("client-1", expect.objectContaining({ id: "fix-ci-2", name: "Fix CI", agent: "claude" }), "g1");
    expect(api.runNow).toHaveBeenCalledWith("client-1", "fix-ci-2");
    expect(onSaved).toHaveBeenCalled();
    vi.useRealTimers();
  });

  it("asks for what is missing instead of saving", async () => {
    const renderer = await render(editor());
    await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault: vi.fn() }); });
    expect(api.save).not.toHaveBeenCalled();
    expect(text(renderer)).toContain("Add a name.");
    expect(renderer.root.findByProps({ name: "name" }).props["aria-invalid"]).toBe(true);
  });

  it("saves against the version it opened on, and keeps its id once saved", async () => {
    api.save.mockResolvedValueOnce(listing([summary()], { generation: "g2" }));
    api.runNow.mockRejectedValueOnce("run_failed: the shell never started");
    api.save.mockResolvedValue(listing([summary()], { generation: "g3" }));
    api.runNow.mockResolvedValue(listing([summary()], { generation: "g3" }));
    const renderer = await render(editor());
    const field = (label: string) => renderer.root.findAll((node) => node.type === "label" && node.children.includes(label))[0]
      .findAll((node) => node.type === "input" || node.type === "textarea")[0];
    await act(async () => { field("Name").props.onChange({ target: { value: "Nightly" } }); });
    await act(async () => { field("Prompt").props.onChange({ target: { value: "Go." } }); });
    await act(async () => { button(renderer, "Save and run now").props.onClick(); });
    expect(text(renderer)).toContain("the shell never started");
    // The panel re-lists meanwhile and passes newer props; the retry must not
    // become a second automation, and saves over what it saved.
    await act(async () => { renderer.update(editor({ existingIds: ["fix-ci", "nightly"], generation: "g9" })); });
    await act(async () => { button(renderer, "Save and run now").props.onClick(); });
    expect(api.save).toHaveBeenNthCalledWith(1, "client-1", expect.objectContaining({ id: "nightly" }), "g1");
    expect(api.save).toHaveBeenNthCalledWith(2, "client-1", expect.objectContaining({ id: "nightly" }), "g2");
  });

  it("saves over a newer file only after it was refused once and the newer file listed", async () => {
    api.save.mockRejectedValueOnce("stale_generation: automations.json changed since it was listed");
    api.save.mockResolvedValue(listing([summary()], { generation: "g3" }));
    const onStale = vi.fn();
    const renderer = await render(editor({ onStale, summary: summary() }));
    await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault: vi.fn() }); });
    expect(onStale).toHaveBeenCalled();
    await act(async () => { renderer.update(editor({ onStale, summary: summary(), generation: "g2" })); });
    await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault: vi.fn() }); });
    expect(api.save).toHaveBeenNthCalledWith(1, "client-1", expect.anything(), "g1");
    expect(api.save).toHaveBeenNthCalledWith(2, "client-1", expect.anything(), "g2");
  });

  it("does not move to a newer listing on its own", async () => {
    api.save.mockResolvedValue(listing([summary()], { generation: "g3" }));
    const renderer = await render(editor({ summary: summary() }));
    await act(async () => { renderer.update(editor({ summary: summary(), generation: "g2" })); });
    await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault: vi.fn() }); });
    expect(api.save).toHaveBeenCalledWith("client-1", expect.anything(), "g1");
  });

  it("deletes only on a second click", async () => {
    api.remove.mockResolvedValue(listing([]));
    const onSaved = vi.fn();
    const renderer = await render(editor({ onSaved, summary: summary() }));
    await act(async () => { button(renderer, "Delete").props.onClick(); });
    expect(api.remove).not.toHaveBeenCalled();
    await act(async () => { button(renderer, "Delete for good").props.onClick(); });
    expect(api.remove).toHaveBeenCalledWith("client-1", "fix-ci", "g1");
    expect(onSaved).toHaveBeenCalledWith(listing([]), "fix-ci");
  });

  it("starts a new automation in the host's start directory, and an existing one where it was", async () => {
    const cwd = (renderer: ReactTestRenderer) => renderer.root.findByProps({ name: "cwd" }).props.value;
    expect(cwd(await render(editor({ startDirectory: "~/dev/muxflow" })))).toBe("~/dev/muxflow");
    expect(cwd(await render(editor()))).toBe("~/");
    expect(cwd(await render(editor({ startDirectory: "~/dev", summary: summary({ cwd: "/srv/app" }) })))).toBe("/srv/app");
  });

  it("warns that a Claude Code automation in the home folder asks for trust every run", async () => {
    const warning = "Claude Code never remembers trust for your home folder";
    const renderer = await render(editor());
    expect(text(renderer)).toContain("ask before working in a folder they don't trust yet");
    expect(text(renderer)).toContain(warning);
    await act(async () => { renderer.root.findByProps({ name: "cwd" }).props.onChange({ target: { value: "~/dev" } }); });
    expect(text(renderer)).not.toContain(warning);
    const codex = await render(editor({ summary: summary({ cwd: "~" }) }));
    expect(text(codex)).not.toContain(warning);
    const custom = await render(editor({ summary: summary({ agent: "custom", command: "cx", cwd: "~" }) }));
    expect(text(custom)).not.toContain("trust");
  });
});
