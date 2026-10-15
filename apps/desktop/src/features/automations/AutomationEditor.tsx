import { useEffect, useId, useRef, useState } from "react";
import { useModalDialog } from "../../commands/useModalDialog";
import { automationsApi, isStaleGeneration, refusalText } from "./api";
import { describeMinute, describeSchedule } from "./cronText";
import type { AutomationDefinition, AutomationListing, AutomationSummary, SchedulePreview } from "./types";

/** The helper's unattended defaults, in each agent's auto-approval mode; docs/automations.md. */
export const PRESET_COMMANDS: Record<string, string> = {
  claude: "claude --permission-mode auto",
  codex: "codex --approve-for-me",
};

const AGENTS = [
  { id: "claude", label: "Claude Code" },
  { id: "codex", label: "Codex" },
  { id: "custom", label: "Custom command" },
];

/** A new automation's id: its name, as the file's id rules allow, made unique. */
export function idFromName(name: string, taken: readonly string[]): string {
  const base = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 56) || "automation";
  let id = base;
  for (let index = 2; taken.includes(id); index += 1) id = `${base}-${index}`;
  return id;
}

/** The line the helper types into a run's tab. */
export function commandPreview(definition: AutomationDefinition, stateDir: string): string {
  const program = definition.agent === "custom"
    ? definition.command.trim() || "<command>"
    : `${PRESET_COMMANDS[definition.agent] ?? definition.agent} --`;
  return `${program} "$(cat '${stateDir}/automations/prompts/${definition.id || "<id>"}-<n>.txt')"`;
}

const blank: AutomationDefinition = {
  id: "",
  name: "",
  enabled: true,
  schedule: "0 9 * * 1-5",
  cwd: "~/",
  agent: "claude",
  command: "",
  prompt: "",
  keepTabs: 5,
  maxRuntimeMinutes: 120,
};

interface AutomationEditorProps {
  /** Absent while the host is reconnecting; saving waits for it. */
  clientId?: string;
  hostLabel: string;
  stateDir: string;
  generation: string;
  existingIds: readonly string[];
  /** Absent for a new automation. */
  summary?: AutomationSummary;
  onClose(): void;
  onSaved(listing: AutomationListing, deletedId?: string): void;
  /** The file changed on the host since it was opened: list it again. */
  onStale(): void;
}

export function AutomationEditor(props: AutomationEditorProps) {
  const titleId = useId();
  const scheduleHintId = useId();
  const errorId = useId();
  const dialog = useModalDialog<HTMLElement>(props.onClose);
  const [draft, setDraft] = useState<AutomationDefinition>(() => ({ ...blank, ...props.summary?.definition }));
  const [preview, setPreview] = useState<SchedulePreview>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  // The version of the file this draft was made against. It moves only when
  // the user has seen a refusal and saves again over the newer file, so an
  // edit made on the host meanwhile is never overwritten silently.
  const [baseGeneration, setBaseGeneration] = useState(props.generation);
  // Set to the refused version; the next different listing becomes the base.
  const [refused, setRefused] = useState<string>();
  useEffect(() => {
    if (refused === undefined || props.generation === refused) return;
    setBaseGeneration(props.generation);
    setRefused(undefined);
  }, [refused, props.generation]);
  // Fixed by the first save, so retrying "Save and run now" never makes a twin.
  const [savedId, setSavedId] = useState<string>();
  const isNew = !props.summary;
  const id = savedId ?? (isNew ? idFromName(draft.name, props.existingIds) : draft.id);
  const clientId = props.clientId;
  const update = (patch: Partial<AutomationDefinition>) => {
    setDraft((current) => ({ ...current, ...patch }));
    // An answer about the draft before this edit no longer applies.
    setError(undefined);
  };

  // The host reads the schedule, so what is shown is what will run.
  useEffect(() => {
    if (!clientId) return;
    let live = true;
    const timer = window.setTimeout(() => {
      automationsApi.previewSchedule(clientId, draft.schedule)
        .then((next) => { if (live) setPreview(next); })
        .catch(() => { if (live) setPreview(undefined); });
    }, 300);
    return () => { live = false; window.clearTimeout(timer); };
  }, [draft.schedule, clientId]);

  const missing = !draft.name.trim() ? "name"
    : !draft.prompt.trim() ? "prompt"
      : !draft.cwd.trim() ? "cwd"
        : draft.agent === "custom" && !draft.command.trim() ? "command"
          : undefined;
  const missingText = { name: "a name", prompt: "a prompt", cwd: "a working directory", command: "a command" };
  const [flagged, setFlagged] = useState<keyof typeof missingText>();
  const form = useRef<HTMLFormElement>(null);
  const invalid = (field: keyof typeof missingText) => flagged === field && missing === field;
  /** Marks the field the footer's message is about, and points it there. */
  const flag = (field: keyof typeof missingText) => invalid(field)
    ? { "aria-invalid": true, "aria-describedby": errorId }
    : { "aria-invalid": false };
  const ask = (field: keyof typeof missingText) => {
    setFlagged(field);
    setError(`Add ${missingText[field]}.`);
    const input = form.current?.querySelector<HTMLElement>(`[name="${field}"]`);
    input?.focus();
    input?.closest("label")?.scrollIntoView?.({ block: "nearest" });
  };

  const failed = (reason: unknown) => {
    setBusy(false);
    if (isStaleGeneration(reason)) {
      setError("automations.json changed on the host since this was opened. Save again to apply your edit over the newer file.");
      setRefused(baseGeneration);
      props.onStale();
    } else setError(refusalText(reason));
  };

  const save = async (runAfter: boolean) => {
    if (missing) return ask(missing);
    if (!clientId) return setError(`${props.hostLabel} is not connected.`);
    setBusy(true);
    setError(undefined);
    const definition = { ...draft, id, name: draft.name.trim() };
    try {
      let listing = await automationsApi.save(clientId, definition, baseGeneration);
      setSavedId(id);
      setBaseGeneration(listing.generation);
      if (runAfter) listing = await automationsApi.runNow(clientId, id);
      props.onSaved(listing);
    } catch (reason) {
      failed(reason);
    }
  };

  const remove = async () => {
    if (!confirmingDelete) return setConfirmingDelete(true);
    if (!clientId) return setError(`${props.hostLabel} is not connected.`);
    setBusy(true);
    try {
      props.onSaved(await automationsApi.remove(clientId, draft.id, baseGeneration), draft.id);
    } catch (reason) {
      failed(reason);
    }
  };

  const reading = preview?.scheduleError ? preview.scheduleError : describeSchedule(draft.schedule);
  const next = preview?.nextRuns.map((key) => describeMinute(key)).join(" · ");

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
    if (event.currentTarget === event.target) props.onClose();
  }}>
    <section aria-labelledby={titleId} aria-modal="true" className="automation-editor" ref={dialog} role="dialog">
      <header>
        <h2 id={titleId}>{isNew ? "New automation" : "Edit automation"}</h2>
        <span className="automation-editor-host">{props.hostLabel}</span>
      </header>
      <form className="automation-editor-form" onSubmit={(event) => { event.preventDefault(); void save(false); }} ref={form}>
        <div className="automation-editor-body">
        <label>Name
          <input {...flag("name")} autoFocus name="name" onChange={(event) => update({ name: event.target.value })} value={draft.name} />
        </label>
        <fieldset className="automation-editor-agent">
          <legend>Runs</legend>
          <div aria-label="Runs" className="segmented" role="radiogroup">
            {AGENTS.map((agent) => <button
              aria-checked={draft.agent === agent.id}
              className={draft.agent === agent.id ? "segment active" : "segment"}
              key={agent.id}
              onClick={() => update({ agent: agent.id })}
              role="radio"
              type="button"
            >{agent.label}</button>)}
          </div>
        </fieldset>
        <label>Command
          <input
            {...flag("command")}
            className="mono"
            name="command"
            onChange={(event) => update({ command: event.target.value })}
            placeholder="e.g. cx --print"
            readOnly={draft.agent !== "custom"}
            value={draft.agent === "custom" ? draft.command : PRESET_COMMANDS[draft.agent] ?? ""}
          />
        </label>
        <p className="settings-hint">{draft.agent === "custom"
          ? "Typed exactly as written into an interactive shell, so aliases and functions work. The prompt is added at the end; making the command work is up to you."
          : draft.agent === "codex"
            ? "Default Codex command: approval requests go to Codex's automatic reviewer, inside its workspace sandbox. The prompt is added at the end."
            : "Default Claude Code command in auto mode: actions are approved automatically after a safety check. The prompt is added at the end."}</p>
        <label>Prompt
          <textarea {...flag("prompt")} name="prompt" onChange={(event) => update({ prompt: event.target.value })} rows={5} value={draft.prompt} />
        </label>
        <label>Working directory
          <input {...flag("cwd")} className="mono" name="cwd" onChange={(event) => update({ cwd: event.target.value })} value={draft.cwd} />
        </label>
        <p className="settings-hint">On {props.hostLabel}. Checked when you save.</p>
        <label>Schedule (cron)
          <input aria-describedby={scheduleHintId} aria-invalid={Boolean(preview?.scheduleError)} className="mono" onChange={(event) => update({ schedule: event.target.value })} value={draft.schedule} />
        </label>
        <p className={preview?.scheduleError ? "settings-hint dialog-error" : "settings-hint"} id={scheduleHintId}>
          <strong>{reading}</strong>{preview?.scheduleError ? "" : ", host time"}
          {next && !preview?.scheduleError ? <><br />Next: {next}</> : null}
        </p>
        <div className="automation-editor-pair">
          <label>Keep tabs
            <input max={50} min={1} onChange={(event) => update({ keepTabs: Number(event.target.value) })} type="number" value={draft.keepTabs} />
          </label>
          <label>Overlap limit (minutes)
            <input max={1440} min={5} onChange={(event) => update({ maxRuntimeMinutes: Number(event.target.value) })} type="number" value={draft.maxRuntimeMinutes} />
          </label>
        </div>
        <p className="settings-hint">The newest runs stay open; older tabs close once their run is done. A run is skipped while the last one is still working, for up to the overlap limit.</p>
        <div className="automation-editor-preview">
          <span className="section-label">What runs</span>
          <code>{commandPreview({ ...draft, id }, props.stateDir)}</code>
        </div>
        </div>
        <footer>
        {error && <div className="surface-error" id={errorId} role="alert">{error}</div>}
        <div className="dialog-actions">
          {!isNew && <button className="danger-quiet automation-editor-delete" disabled={busy} onClick={() => void remove()} type="button">
            {confirmingDelete ? "Delete for good" : "Delete"}
          </button>}
          <button disabled={busy} onClick={props.onClose} type="button">Cancel</button>
          <button disabled={busy} onClick={() => void save(true)} type="button">Save and run now</button>
          <button className="primary" disabled={busy} type="submit">Save</button>
        </div>
        </footer>
      </form>
    </section>
  </div>;
}
