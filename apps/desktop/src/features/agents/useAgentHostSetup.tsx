import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { AgentHostSetupDialog } from "./AgentHostSetupDialog";
import { hostHookWiring, hookWiringNotice, shouldPromptForSetup } from "./hookWiring";
import type { HostSetupDecision } from "../shell/types";
import type { AgentAdapterDescriptor, AgentHookReview, AgentHostNamingOutcome } from "./types";

export interface AgentHostSetupOptions {
  adapters: readonly AgentAdapterDescriptor[];
  /** Live, mutable, authoritative — nothing is offered without all three. */
  connected: boolean;
  /** False while another host-level consent flow owns the modal/setup lane. */
  setupAllowed?: boolean;
  hostProfileId: string;
  /**
   * The connected host this render's adapters describe, or `undefined` when
   * there is none. Consent is answered about one host and stays bound to it
   * for the whole install; see `agentHostIdentity`.
   */
  hostIdentity?: string;
  /** False while the app state cannot be written back, so no answer would keep. */
  decisionsArePersistable: boolean;
  hostLabel: string;
  /** What the user last answered for this host, if anything. */
  decision?: HostSetupDecision;
  recordDecision(hostProfileId: string, decision: HostSetupDecision): void;
  reviewHooks(adapter: string, action: "install" | "uninstall", expectedHost: string): Promise<AgentHookReview>;
  applyHooks(review: AgentHookReview, expectedHost: string): Promise<void>;
  /**
   * Applies the recommended tmux window naming. Separate from the hooks
   * because it lives in the tmux server's memory rather than a config file,
   * so it has to be re-asserted rather than installed once.
   */
  applyHostNaming(expectedHost: string): Promise<AgentHostNamingOutcome>;
  /** Re-asks the host what its wiring is now, after a change to it. */
  refreshWiring(): void;
  /**
   * A consent this host has already been given, elsewhere, for exactly this.
   *
   * Installing the remote helper asks a question that names agent status as
   * part of what it sets up, so arriving here and asking again is asking twice
   * about one decision. Carried as the host it was answered for rather than a
   * bare flag, because the answer belongs to that machine and the connection it
   * applies to is the one that comes back *after* the install — by which time
   * the app may be somewhere else entirely. `consume` spends it: it is good for
   * one host, once, and never survives a relaunch.
   */
  autoSetup?: { hostProfileId: string; consume(): void };
  onStatus(message: string): void;
  /** Opens the existing exact-diff dialog after this flow has loaded it. */
  openReview(review: AgentHookReview, host: { profileId: string; identity: string }): void;
}

/**
 * One host, named two ways: the durable key a decision is remembered under,
 * and the connection identity every write in this flow is checked against.
 */
interface ConsentedHost {
  profileId: string;
  identity: string;
}

function consentedHost(options: AgentHostSetupOptions): ConsentedHost | undefined {
  // `connected` and a host identity are the same fact from two sources; both
  // are required, because the write happens down the connection and the
  // decision is remembered under the profile.
  //
  // `decisionsArePersistable` is the third. The app state is write-frozen after
  // a file it could not parse, and in that mode `recordDecision` reaches memory
  // and nothing else: the install would happen, the answer would be lost on
  // quit, and the next launch would find configured hooks and no record of
  // agreeing to them. Consent that cannot be recorded is not consent.
  return options.connected && options.setupAllowed !== false
    && options.hostIdentity && options.hostProfileId
    && options.decisionsArePersistable
    ? { profileId: options.hostProfileId, identity: options.hostIdentity }
    : undefined;
}

export interface AgentHostSetup {
  /** Rendered by the shell; null when nothing is being asked. */
  dialog: ReactElement | null;
  /** The shell reads this rather than being told; one source of truth. */
  open: boolean;
  /** The agents section's honest line, or `undefined` when status works. */
  notice?: string;
  /** Whether anything on this host can report what an agent is doing. */
  reports: boolean;
  /** True when Settings should offer to set this host up. */
  offerable: boolean;
  /** Re-opens the prompt from Settings or the sidebar's notice. */
  offer(): void;
}

/**
 * "Set up this host", asked once per host and remembered.
 *
 * The prompt is driven by what the daemon observed, not by whether the app has
 * seen agents: a host with no agent running right now still cannot report one
 * later, and finding that out at the moment an agent finally blocks is exactly
 * the failure this replaces.
 *
 * Two rules keep it from becoming noise. It is asked at most once per host —
 * a recorded "not now" is as durable as a yes, and Settings is where either is
 * revisited. And it is never asked about a configuration the host could not
 * read: that is reported, not overwritten.
 */
export function useAgentHostSetup(options: AgentHostSetupOptions): AgentHostSetup {
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const questionEpoch = useRef(0);

  // Keyed on `options.adapters` alone, which the store replaces only when a
  // snapshot arrives. It briefly also depended on which agents were running,
  // which changes on every hook event — and the effect below, which installs,
  // depended on this.
  const wiring = useMemo(() => hostHookWiring(options.adapters), [options.adapters]);
  // Two different questions. `offerable` is "is there anything left to set up",
  // which is what Settings and the sidebar's line ask about. Raising the modal
  // unasked needs the stronger one: this host reports nothing at all.
  const offerable = options.connected && options.setupAllowed !== false && wiring.setupTargets.length > 0;
  const promptable = options.connected && options.setupAllowed !== false && shouldPromptForSetup(wiring);

  /**
   * The question, as posed — one value, so no part of it can drift.
   *
   * The host, the label the dialog shows and the configuration paths it lists
   * are captured together at the moment the prompt opens, and the dialog is
   * rendered from them. An answer is an answer to what was on the screen: a
   * snapshot or a host switch arriving while the modal is open must not leave
   * the user reading one machine's paths and answering for another's.
   */
  const [asked, setAsked] = useState<{
    id: number;
    host?: ConsentedHost;
    label: string;
    targets: readonly AgentAdapterDescriptor[];
    activity?: "install" | "review";
    error?: string;
  }>();
  const updateQuestion = useCallback((id: number, update: Partial<Pick<NonNullable<typeof asked>, "activity" | "error">>) => {
    setAsked((current) => current?.id === id ? { ...current, ...update } : current);
  }, []);
  const offer = useCallback(() => {
    const current = optionsRef.current;
    const id = ++questionEpoch.current;
    setAsked({
      id,
      host: consentedHost(current),
      label: current.hostLabel,
      targets: hostHookWiring(current.adapters).setupTargets,
    });
  }, []);

  // A host that goes away takes its question with it, rather than leaving a
  // modal over a disconnected app that would act on the next host to connect.
  useEffect(() => {
    if (!options.connected || options.setupAllowed === false) {
      questionEpoch.current += 1;
      setAsked(undefined);
    }
  }, [options.connected, options.setupAllowed]);

  // Once per connection. The host's answer is idempotent, but reaching it is
  // two `tmux show-options` subprocesses over the link, and the effect below
  // re-runs on every snapshot — which is every topology change. Asserting it
  // per snapshot put subprocess spawns on the path this phase budgets at under
  // a second end to end.
  const asserted = useRef(false);
  const assertNaming = useCallback((host: ConsentedHost) => {
    if (asserted.current) return;
    asserted.current = true;
    const current = optionsRef.current;
    void current.applyHostNaming(host.identity).then((outcome) => {
      // A change to the user's running tmux server is worth one line; finding
      // that their own config already does it is not.
      if (outcome === "applied") current.onStatus("Recommended tmux window naming applied to this host's tmux server.");
    }).catch((cause) => {
      // Non-fatal by design — agent status works without it, and the phase that
      // introduced it declared it non-gating — but never silent.
      current.onStatus(`Recommended tmux window naming was not applied: ${String(cause)}`);
    });
  }, []);

  /**
   * Installs exactly `targets` on exactly the host the caller answered about.
   *
   * Both halves of that sentence were the defect (M13-E004). The host was
   * re-resolved at every `await`, so a switch mid-install redirected the write
   * and then recorded "accepted" against whichever host the app had reached —
   * a host the dialog never named and the user never saw a prompt for. The
   * adapter list was re-read from a ref at click time rather than taken from
   * what the dialog listed, so a snapshot arriving while the dialog was open
   * could widen the consent the user actually gave. Both are now arguments,
   * bound once, and checked again before every write.
   */
  const install = useCallback((targets: readonly AgentAdapterDescriptor[], host: ConsentedHost, questionId?: number) => {
    const current = optionsRef.current;
    if (questionId !== undefined) updateQuestion(questionId, { activity: "install", error: undefined });
    // The record follows the write, always in that order and never without it.
    // Recording only after *every* adapter succeeded left a part-way failure
    // holding the worst of both: a configuration file changed on the host, and
    // no record anywhere that the user ever agreed to it — which is half the
    // shape the field machine was found in. Recording before the loop instead
    // would answer for a host that may end up with nothing installed, and the
    // migration path cannot repair that, because it only touches adapters this
    // app already owns entries in.
    let answered = false;
    const answer = () => {
      // Against the host that was written, not the one that happens to be
      // connected now, and once however many adapters it took.
      if (answered) return;
      answered = true;
      current.recordDecision(host.profileId, "accepted");
    };
    return (async () => {
      for (const adapter of targets) {
        const review = await current.reviewHooks(adapter.id, "install", host.identity);
        // The host's own idempotence answer, so a re-run writes nothing.
        if (review.alreadyInstalled) continue;
        await current.applyHooks(review, host.identity);
        answer();
      }
    })().then(() => {
      // Also for the nothing-to-do case: a host already current has been agreed
      // to, and asking again every launch is not "once".
      answer();
      if (questionId !== undefined && questionEpoch.current === questionId) {
        questionEpoch.current += 1;
        setAsked(undefined);
      }
      // Part of the same "set up this host" answer, and deliberately after it:
      // a tmux server that refuses the naming must not lose the hooks. Always
      // sent, even if this connection already asserted it: an uninstall in
      // between took the naming and the Codex pane environment back off the
      // host, and an install is rare enough that the subprocesses cost nothing.
      asserted.current = false;
      assertNaming(host);
      return true;
    }).catch((cause) => {
      // `false` even when an adapter was written: the answer is recorded, but
      // the thing the user asked for did not finish, and the dialog says so.
      if (questionId !== undefined) updateQuestion(questionId, { error: String(cause) });
      return false;
    }).finally(() => {
      // Unconditionally, including after a failure part-way through: an adapter
      // that was installed before the one that threw *is* wired now, and
      // leaving the dialog claiming otherwise is the lie this phase is about.
      if (questionId !== undefined) updateQuestion(questionId, { activity: undefined });
      current.refreshWiring();
    });
  }, [assertNaming, updateQuestion]);

  /**
   * Asked once, when the host has actually answered. `decision` being undefined
   * is the whole condition: a recorded answer of either kind ends this forever.
   *
   * Or not asked at all, when the helper install already asked. That dialog
   * names agent status as part of what it sets up, so a second modal a few
   * seconds later — after the first had visibly succeeded — was two questions
   * about one decision. `autoSetup` is the answer to the first one arriving
   * here, and it buys exactly what `accept` buys: the same targets, through the
   * same host-bound `install`, recording the same "accepted".
   *
   * Placed after `install` rather than beside the other effects because it
   * calls it; its dependencies are otherwise the ones it always had.
   */
  const autoSetupHost = useRef<string>(undefined);
  useEffect(() => {
    if (!promptable || options.decision !== undefined) return;
    const current = optionsRef.current;
    // The "accepted" that `install` records is a state update away, and the
    // wiring refresh it ends with re-runs this effect before that update
    // arrives. Without this the second pass would find the consent already
    // spent and open the very prompt the consent existed to prevent.
    if (autoSetupHost.current === current.hostProfileId) return;
    // Read from the ref, never depended on: spending the consent must not be
    // what re-runs this effect.
    const auto = current.autoSetup?.hostProfileId === current.hostProfileId ? current.autoSetup : undefined;
    const host = auto ? consentedHost(current) : undefined;
    if (!auto || !host) {
      // Either nothing was agreed elsewhere, or it cannot be acted on — a host
      // that is no longer connected, or an app state that could not record the
      // answer. Both fall back to the question that explains itself and can be
      // declined; silently doing nothing would lose the setup entirely.
      offer();
      return;
    }
    autoSetupHost.current = host.profileId;
    auto.consume();
    // `hostHookWiring` re-read here for the same reason `offer` re-reads it:
    // the targets are taken at the moment the decision is acted on, from the
    // adapters this host last reported.
    const targets = hostHookWiring(current.adapters).setupTargets;
    const label = current.hostLabel;
    void install(targets, host).then((ok) => {
      // No dialog to carry the outcome, so the status line does — the same two
      // sentences `accept` and the migration path use.
      optionsRef.current.onStatus(ok
        ? `Agent status hooks installed on ${label}.`
        : `Could not install the agent status hooks on ${label}.`);
    });
    // Exactly the dependencies this effect always had. The host is read through
    // the ref instead: re-running on a host change would rewrite a question
    // already on screen, and answering *that* is what M13-E004 was.
  }, [install, offer, promptable, options.decision]);

  // Consent is to keeping this host set up, not to one particular set of hook
  // events. The managed event set moves when a vendor adds an event worth
  // taking, and a host set up before that reads as partially wired — so without
  // this, the sidebar on every already-consented host would say "agent status
  // unavailable" forever and the one-time prompt, already answered, could never
  // come back to fix it. Merge-only, backed up and idempotent, so re-running it
  // on a host that is already current writes nothing at all.
  const reassert = useRef<{ running: boolean; attempted: boolean }>({ running: false, attempted: false });
  useEffect(() => {
    if (!options.connected) {
      // A new connection is a new chance — and a restarted tmux server has
      // dropped the in-memory naming, so both are reset together.
      reassert.current = { running: false, attempted: false };
      asserted.current = false;
      return;
    }
    // Real, recorded consent for *this* host, and a host identity to bind the
    // writes to. A migration is still a write to the user's configuration
    // files, so it gets no weaker a gate than the first install did.
    const host = consentedHost(optionsRef.current);
    if (!host || options.decision !== "accepted") return;
    // Only adapters this app already owns entries in. `partial` means the
    // managed event set grew under a host the user already approved, which is
    // what this exists for. A `notWired` adapter that appeared *later* is one
    // the consent dialog never named, and writing its configuration without
    // ever showing the user its path is not what "one-time consent" bought.
    const outdated = wiring.setupTargets.filter((adapter) => adapter.hookWiring === "partial");
    if (outdated.length === 0 || reassert.current.running || reassert.current.attempted) {
      // `install` asserts the naming itself when it succeeds; this is the
      // nothing-to-install path, which still has a tmux server to talk to.
      if (!reassert.current.running) assertNaming(host);
      return;
    }
    reassert.current.running = true;
    const named = outdated.map((adapter) => adapter.displayName).join(" and ");
    void install(outdated, host).then((ok) => {
      // Once per connection, whatever happened. `install` finishes by
      // refreshing the wiring, which produces a new snapshot and re-runs this
      // effect; without this an install that cannot reach `wired` — a racing
      // editor, a stale lock, an unwritable file, or simply a host that
      // disagrees — would rewrite the user's configuration in a silent loop.
      reassert.current = { running: false, attempted: true };
      optionsRef.current.onStatus(ok
        ? `Updated the agent status hooks for ${named} on this host.`
        : `Could not update the agent status hooks for ${named} on this host.`);
    });
  }, [assertNaming, install, options.connected, options.decision, options.decisionsArePersistable, options.hostIdentity, options.hostProfileId, options.setupAllowed, wiring.setupTargets]);

  /**
   * `targets` is what the dialog listed, passed down from the render that
   * showed it rather than re-derived here: those config paths are the whole
   * content of the question the user answered.
   */
  const accept = useCallback((question: NonNullable<typeof asked>) => {
    const current = optionsRef.current;
    if (!question.host) {
      // Two different refusals, and the difference matters to the person
      // reading it: one is a host that went away, the other is settings this
      // app could not read and therefore could not record an answer in.
      updateQuestion(question.id, { error: current.decisionsArePersistable
        ? "This host is no longer connected; nothing was changed."
        : "Your saved settings could not be read, so this answer could not be recorded; nothing was changed." });
      return;
    }
    void install(question.targets, question.host, question.id).then((ok) => {
      if (ok) current.onStatus(`Agent status hooks installed on ${question.label}.`);
    });
  }, [install, updateQuestion]);

  const decline = useCallback((question: NonNullable<typeof asked>) => {
    // Unconditionally, including over an earlier "accepted": this prompt is
    // reachable from Settings, and someone who opens it there to say no is
    // changing their mind, not restating it. Against the host that was asked
    // about, so a "no" never lands on a host whose hooks are installed.
    if (question.host) optionsRef.current.recordDecision(question.host.profileId, "declined");
    if (questionEpoch.current === question.id) questionEpoch.current += 1;
    setAsked(undefined);
  }, []);

  const openReview = useCallback((question: NonNullable<typeof asked>) => {
    const first = question.targets[0];
    if (!first) return;
    if (!question.host) {
      updateQuestion(question.id, { error: optionsRef.current.decisionsArePersistable
        ? "This host is no longer connected; the review could not be loaded."
        : "Your saved settings could not be read, so this review could not be bound to a host." });
      return;
    }
    // Keep the question mounted until the asynchronous review exists. The
    // captured target and host are the consent surface; replacing either with
    // the latest render while this request is in flight would make the diff an
    // answer about a machine the user was never shown.
    updateQuestion(question.id, { activity: "review", error: undefined });
    void optionsRef.current.reviewHooks(first.id, "install", question.host.identity).then((review) => {
      if (questionEpoch.current !== question.id) return;
      optionsRef.current.openReview(review, question.host!);
      questionEpoch.current += 1;
      setAsked(undefined);
    }).catch((cause) => {
      // The prompt remains open and the same button becomes a retry. A status
      // toast alone is not enough: it disappears and used to leave no path
      // back to the review the user explicitly requested.
      updateQuestion(question.id, { error: String(cause) });
    }).finally(() => updateQuestion(question.id, { activity: undefined }));
  }, [updateQuestion]);

  const dialog = asked
    ? <AgentHostSetupDialog
      adapters={asked.targets}
      activity={asked.activity}
      error={asked.error}
      hostLabel={asked.label}
      onAccept={() => accept(asked)}
      onDecline={() => decline(asked)}
      onReview={() => openReview(asked)}
    />
    : null;

  return {
    dialog,
    open: asked !== undefined,
    notice: hookWiringNotice(wiring),
    reports: wiring.reports,
    offerable,
    offer,
  };
}
