import { useCallback, useEffect, useRef, useState } from "react";
import type { PanelExtensionProps, PanelHost } from "../shell/panelExtensions";
import { automationsApi, isStaleGeneration, onAutomationChanged, refusalText } from "./api";
import { AutomationEditor, PRESET_COMMANDS } from "./AutomationEditor";
import { describeDuration, describeMinute, describeSchedule } from "./cronText";
import type { AutomationListing, AutomationRun, AutomationSummary } from "./types";

type Loaded = { profileId: string; listing: AutomationListing };
type Editing = { summary?: AutomationSummary; runAfterSave?: boolean };

const STATUS_TEXT: Record<string, string> = {
  launched: "starting",
  working: "working",
  needs_attention: "needs attention",
  done: "done",
  closed: "tab closed",
  failed: "failed",
  skipped: "skipped",
};

export const statusText = (status: string) => STATUS_TEXT[status] ?? status;

const unfinished = (run: AutomationRun) =>
  run.status === "launched" || run.status === "working" || run.status === "needs_attention";

/** How long an unfinished run has gone, at the half-minute it refreshes. */
const age = (run: AutomationRun) => {
  const elapsed = Date.now() - run.startedAt;
  return elapsed < 60_000 ? "<1m" : describeDuration(elapsed);
};

/** What a run is doing, in one chip: `#12 done · 09:00`. */
function runChip(run: AutomationRun): string {
  const when = unfinished(run)
    ? age(run)
    : run.scheduledFor ? describeMinute(run.scheduledFor) : "manual";
  return `#${run.number} ${statusText(run.status)} · ${when}`;
}

export function agentText(summary: AutomationSummary): string {
  const { agent, command } = summary.definition;
  return agent === "custom" ? command : agent;
}

/** The command a run types, without the prompt the helper appends. */
function commandText(summary: AutomationSummary): string {
  const { agent, command } = summary.definition;
  return agent === "custom" ? command : PRESET_COMMANDS[agent] ?? agent;
}

/** Re-renders every half minute while some run is unfinished, so its age moves. */
function useTickWhile(active: boolean) {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(() => setTick((tick) => tick + 1), 30_000);
    return () => window.clearInterval(timer);
  }, [active]);
}

/**
 * The host's automations (docs/automations.md), beside Files, Git and Ports.
 * Host-scoped like Ports: the list belongs to the host the chooser names.
 */
export function AutomationsPanel(props: PanelExtensionProps) {
  const [chosenProfileId, setChosenProfileId] = useState<string>();
  const host = props.hosts.find((candidate) => candidate.profileId === chosenProfileId)
    ?? props.hosts.find((candidate) => candidate.profileId === props.activeProfileId)
    ?? props.hosts[0];
  const [loaded, setLoaded] = useState<Loaded>();
  const [error, setError] = useState<string>();
  const [selectedId, setSelectedId] = useState<string>();
  const [editing, setEditing] = useState<Editing>();
  const panel = useRef<HTMLElement>(null);
  const [starting, setStarting] = useState<ReadonlySet<string>>(new Set());
  const clientId = host?.connected ? host.clientId : undefined;
  const profileId = host?.profileId;
  // Only the newest list may land: an older one, from a host switched away
  // from, would overwrite it.
  const latestList = useRef(0);

  const reload = useCallback(() => {
    if (!clientId || !profileId) return;
    const request = ++latestList.current;
    automationsApi.list(clientId)
      .then((listing) => {
        if (request !== latestList.current) return;
        setLoaded({ profileId, listing });
        setError(undefined);
      })
      .catch((reason) => { if (request === latestList.current) setError(refusalText(reason)); });
  }, [clientId, profileId]);

  // An editor belongs to the host it was opened on.
  useEffect(() => { setSelectedId(undefined); setEditing(undefined); }, [profileId]);
  useEffect(reload, [reload]);
  useEffect(() => onAutomationChanged((changed) => { if (changed === profileId) reload(); }), [profileId, reload]);

  if (!host) return <section aria-label="Automations" className="automations-panel">
    <p className="quiet-empty">No host is shown.</p>
  </section>;

  const listing = loaded?.profileId === host.profileId ? loaded.listing : undefined;
  const selected = listing?.automations.find((summary) => summary.definition.id === selectedId);
  const readOnly = Boolean(listing?.fileError);

  // Every answer is a listing; like a list, only the newest may land.
  const act = (work: Promise<AutomationListing>) => {
    setError(undefined);
    const request = ++latestList.current;
    const from = host.profileId;
    return work
      .then((next) => { if (request === latestList.current) setLoaded({ profileId: from, listing: next }); })
      .catch((reason) => {
        if (request !== latestList.current) return;
        if (isStaleGeneration(reason)) {
          props.onMessage("automations.json changed on the host; the list is reloaded.");
          reload();
        } else setError(refusalText(reason));
      });
  };
  const toggle = (summary: AutomationSummary) => {
    if (!clientId || !listing || readOnly) return;
    act(automationsApi.save(clientId, { ...summary.definition, enabled: !summary.definition.enabled }, listing.generation));
  };
  // A run started now skips the overlap check, so a second click must wait.
  const runNow = (summary: AutomationSummary) => {
    const id = summary.definition.id;
    if (!clientId || starting.has(id)) return;
    setStarting((current) => new Set(current).add(id));
    void act(automationsApi.runNow(clientId, id)).finally(() => setStarting((current) => {
      const next = new Set(current);
      next.delete(id);
      return next;
    }));
  };

  const back = () => {
    const id = selectedId;
    setSelectedId(undefined);
    // Back to the row the detail was opened from.
    window.requestAnimationFrame(() => panel.current
      ?.querySelector<HTMLElement>(`[data-automation-id="${CSS.escape(id ?? "")}"] .automation-row-main`)?.focus());
  };
  const empty = listing && listing.automations.length === 0 && !listing.fileError;

  return <section aria-label="Automations" className="automations-panel" ref={panel}>
    {!selected && <header className="automations-head">
      {props.hosts.length > 1
        ? <label className="automations-host">
          <span className="sr-only">Host</span>
          <select onChange={(event) => setChosenProfileId(event.target.value)} value={host.profileId}>
            {props.hosts.map((candidate) => <option key={candidate.profileId} value={candidate.profileId}>{candidate.label}</option>)}
          </select>
        </label>
        : <span className="automations-host-name">{host.label}</span>}
      {!empty && <button
        className="primary automations-new"
        disabled={!listing || readOnly}
        onClick={() => setEditing({})}
        type="button"
      >New</button>}
    </header>}

    {!clientId ? <Disconnected host={host} />
      : <>
        {error && <div className="surface-error" role="alert">{error}</div>}
        {listing?.fileError && <div className="automations-banner" role="alert">
          <strong>The file can't be read</strong>
          <span>{listing.fileError}</span>
          <span>The last version that loaded is still running. Fix the file on the host to edit here.</span>
        </div>}
        {!listing ? (!error && <p className="quiet-empty loading-delayed">Loading…</p>)
          : selected ? <AutomationDetail
            onBack={back}
            onEdit={() => setEditing({ summary: selected })}
            readOnly={readOnly}
            // A finished run's pane id may have been reused by a later tmux
            // server; an unfinished run's server is the one showing.
            openable={(run) => Boolean(run.paneId) && host.hasPane(run.paneId)
              && (unfinished(run) || run.serverIdentity === host.serverIdentity)}
            onOpenRun={(run) => props.onOpenPane({
              profileId: host.profileId,
              sessionId: run.sessionId,
              paneId: run.paneId,
              source: `${selected.definition.name} #${run.number}`,
            })}
            onRunNow={() => runNow(selected)}
            starting={starting.has(selected.definition.id)}
            summary={selected}
          />
            : empty ? <div className="automations-empty">
              <strong>No automations on {host.label}</strong>
              <p>Run an agent on a cron schedule. Each run opens a new tab in the automation workspace.</p>
              <button className="primary" onClick={() => setEditing({})} type="button">New automation</button>
              <code>{listing.stateDir}/automations.json</code>
            </div>
              : <ul aria-label="Automations" className="automations-list">
                {listing.automations.map((summary, index) => <AutomationRow
                  key={`${index}:${summary.definition.id}`}
                  onOpen={() => setSelectedId(summary.definition.id)}
                  onRunNow={() => runNow(summary)}
                  onToggle={() => toggle(summary)}
                  readOnly={readOnly}
                  starting={starting.has(summary.definition.id)}
                  summary={summary}
                />)}
              </ul>}
      </>}

    {/* Kept through a reconnect, so a draft survives it; saving waits for the host. */}
    {editing && listing && <AutomationEditor
      clientId={clientId}
      existingIds={listing.automations.map((summary) => summary.definition.id)}
      generation={listing.generation}
      hostLabel={host.label}
      onClose={() => setEditing(undefined)}
      onSaved={(next, deletedId) => {
        setLoaded({ profileId: host.profileId, listing: next });
        if (deletedId && deletedId === selectedId) setSelectedId(undefined);
        setEditing(undefined);
      }}
      onStale={reload}
      stateDir={listing.stateDir}
      summary={editing.summary}
    />}
  </section>;
}

function Disconnected({ host }: { host: PanelHost }) {
  return <div className="automations-empty">
    <strong>{host.label} is disconnected</strong>
    <p>The helper keeps running automations while the desktop is closed. They stop if the host reboots, until the next time you connect.</p>
  </div>;
}

function AutomationRow(props: {
  summary: AutomationSummary;
  readOnly: boolean;
  starting: boolean;
  onOpen(): void;
  onRunNow(): void;
  onToggle(): void;
}) {
  const { definition, definitionError, nextRun, recentRuns } = props.summary;
  // A run still going says so first, over a newer skip and over "Paused".
  const running = recentRuns.find(unfinished);
  useTickWhile(Boolean(running));
  const last = running ?? recentRuns[0];
  const runnable = !definitionError;
  return <li
    className={definition.enabled && runnable ? "automation-row" : "automation-row paused"}
    data-automation-id={definition.id}
  >
    <button className="automation-row-main" onClick={props.onOpen} type="button">
      <span className="automation-name" title={definition.name}>{definition.name || definition.id || "Unnamed"}</span>
      <span className="automation-row-meta">
        {definitionError ? <span className="automation-chip failed">invalid</span>
          : running ? <span className={`automation-chip ${running.status}`}>{runChip(running)}</span>
            : !definition.enabled ? <span className="automation-chip">Paused</span>
              : last && <span className={`automation-chip ${last.status}`}>{runChip(last)}</span>}
        {definitionError
          ? <span className="automation-row-error">{definitionError}</span>
          : <span>{describeSchedule(definition.schedule)}{nextRun ? ` · next ${describeMinute(nextRun)}` : ""}</span>}
        <span className="automation-row-agent">{agentText(props.summary)}</span>
      </span>
    </button>
    <span className="automation-row-actions">
      <button className="automation-button" disabled={!runnable || props.starting} onClick={props.onRunNow} type="button">
        {props.starting ? "Starting…" : "Run now"}
      </button>
      <button
        aria-checked={definition.enabled}
        aria-label={definition.enabled ? `Pause ${definition.name}` : `Resume ${definition.name}`}
        className="automation-switch"
        disabled={!runnable || props.readOnly}
        onClick={props.onToggle}
        role="switch"
        type="button"
      />
    </span>
  </li>;
}

function AutomationDetail(props: {
  summary: AutomationSummary;
  readOnly: boolean;
  starting: boolean;
  onBack(): void;
  onEdit(): void;
  onRunNow(): void;
  onOpenRun(run: AutomationRun): void;
  /** Whether the run's tab is still open on the host. */
  openable(run: AutomationRun): boolean;
}) {
  const { definition, definitionError, nextRun, recentRuns } = props.summary;
  useTickWhile(recentRuns.some(unfinished));
  const hours = definition.maxRuntimeMinutes % 60 === 0
    ? `${definition.maxRuntimeMinutes / 60}h`
    : describeDuration(definition.maxRuntimeMinutes * 60_000);
  return <div className="automation-detail">
    <div className="automation-detail-head">
      <button autoFocus className="inline-action" onClick={props.onBack} type="button">‹ All</button>
      <span className="automation-name" title={definition.name}>{definition.name || definition.id}</span>
      <button className="automation-button" disabled={props.readOnly} onClick={props.onEdit} type="button">Edit</button>
      <button className="automation-button" disabled={Boolean(definitionError) || props.starting} onClick={props.onRunNow} type="button">
        {props.starting ? "Starting…" : "Run now"}
      </button>
    </div>
    {definitionError && <div className="surface-error" role="alert">{definitionError}</div>}
    <dl className="automation-facts">
      <dt>Schedule</dt>
      <dd><code>{definition.schedule}</code> · {describeSchedule(definition.schedule)}{nextRun ? ` · next ${describeMinute(nextRun)}` : ""}</dd>
      <dt>Runs</dt>
      <dd><code>{commandText(props.summary)}</code></dd>
      <dt>Directory</dt>
      <dd><code>{definition.cwd}</code></dd>
      <dt>Tabs</dt>
      <dd>Keep {definition.keepTabs} · overlap limit {hours}</dd>
    </dl>
    <p className="automation-prompt">{definition.prompt}</p>
    <h3 className="section-label">Runs</h3>
    {recentRuns.length === 0 ? <p className="quiet-empty">No runs yet.</p>
      : <ul aria-label="Runs" className="automation-runs">
        {recentRuns.map((run) => <li className="automation-run" key={run.number}>
          <span className="automation-run-number">#{run.number}</span>
          <span className="automation-run-when">
            {run.scheduledFor ? describeMinute(run.scheduledFor) : "manual"}
            {unfinished(run) ? ` · ${age(run)}`
              : run.finishedAt > run.startedAt && run.status !== "skipped" && run.status !== "failed"
                ? ` · ${describeDuration(run.finishedAt - run.startedAt)}` : ""}
          </span>
          <span className={`automation-chip ${run.status}`}>{statusText(run.status)}</span>
          {props.openable(run)
            ? <button className="inline-action automation-run-open" onClick={() => props.onOpenRun(run)} type="button">Open tab</button>
            : <span className="automation-run-gone">{run.paneId ? "tab closed" : "no tab"}</span>}
          {(run.lastMessage || run.error) && <span className="automation-run-message" title={run.lastMessage || run.error}>{run.lastMessage || run.error}</span>}
        </li>)}
      </ul>}
  </div>;
}
