import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { renderSafeMarkdown } from "../files/markdown";
import { automationsApi } from "../automations/api";
import { portsApi } from "../ports/api";
import { sendInput as sendTerminalInput } from "../terminal/api";
import { terminalCacheKey } from "../terminal/TerminalStateCache";
import { readPaneScreen } from "../terminal/screenReaderRegistry";
import type { PanelExtensionProps, PanelSessionContext } from "../shell/panelExtensions";
import { aiChat } from "./api";
import { aiConversations, conversationKey, type AiHostConversation } from "./conversationStore";
import {
  AI_SOURCES, ALL_AI_SOURCES, buildContext, buildSystemPrompt,
  type AiContextInput, type AiSourceId,
} from "./context";
import {
  AI_TOOLS, APPROVAL_TOOLS, approvalTitle, executeTool, summarizeCall,
  type ToolContext,
} from "./tools";
import type { AutomationListing } from "../automations/types";
import type { DetectedPort } from "../ports/api";
import type { AiChatMessage, AiEvent, AiToolCall } from "./types";

/** How much of the conversation the model is shown. */
const HISTORY_TURNS = 12;

/** The clock stamp under each bubble, in the user's locale and clock. */
const TURN_TIME = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });
/** Tool rounds one question buys before the panel asks the user whether the
 * run is worth continuing; Continue spends another budget. A loop that
 * cannot answer is stopped by the user, not by a number. */
const MAX_TOOL_ROUNDS = 6;

const NO_CONVERSATION: AiHostConversation = { turns: [], streaming: false, pending: [], generation: 0 };

/**
 * The source checkboxes outlive the panel within the run of the app: clicking
 * another tab of the right panel unmounts this component, and a choice about
 * what the model may see is not something to re-make for it.
 */
let rememberedSources: readonly AiSourceId[] = ALL_AI_SOURCES;

/**
 * The session's assistant, beside Files, Git, Ports and Automations.
 *
 * One thread per tab of the strip: switching tabs switches conversations,
 * and closing a tab discards its thread. At the moment a question is sent
 * the panel snapshots what the other tabs already hold — the tmux view, the
 * Explorer's root and open documents, the Git status, the port list, the
 * automations — and puts it in front of an OpenAI-compatible model the user
 * configured.
 *
 * The model can act, but only through the four tools in `tools.ts`, and the
 * two that touch anything outside the screen — typing into a pane, starting
 * an automation — park on an approval card first. Nothing is typed anywhere
 * without the user pressing Allow.
 */
export function AiPanel(props: PanelExtensionProps) {
  const session = props.session;
  const host = props.hosts.find((candidate) => candidate.profileId === props.activeProfileId)
    ?? props.hosts[0];
  // The thread belongs to the tab (tmux window) on screen, not to the host.
  const windowId = session?.activeWindowId;
  const key = host && windowId ? conversationKey(host.profileId, windowId) : undefined;
  const [sources, setSources] = useState<readonly AiSourceId[]>(rememberedSources);
  const [draft, setDraft] = useState("");
  const [gathering, setGathering] = useState(false);
  const conversation = useSyncExternalStore(
    aiConversations.subscribe,
    () => (key ? aiConversations.read(key) : NO_CONVERSATION),
  );
  const list = useRef<HTMLOListElement>(null);
  // The tools run when the model asks, which is often after a re-render:
  // these carry the current host and shell state into a loop started earlier.
  const sessionRef = useRef<PanelSessionContext | undefined>(session);
  sessionRef.current = session;
  const hostRef = useRef(host);
  hostRef.current = host;
  const assistant = session?.assistant;
  const configured = Boolean(assistant && assistant.baseUrl.trim() && assistant.model.trim());

  // The newest turn is the reason to have scrolled; keep it in view.
  // Walking away does not stop a generation: the store keeps filling the
  // answer, and a remounted panel reads it back mid-stream.
  useEffect(() => {
    const element = list.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [conversation.turns]);

  // A closed tab's thread is a thread about something that no longer exists.
  // Pruning waits for a snapshot that proves the host has answered: the empty
  // snapshot of a reconnect would otherwise clear every thread it survived.
  useEffect(() => {
    if (!host || !session || !session.connected) return;
    if (session.snapshot.sessions.length === 0 && session.snapshot.windows.length === 0) return;
    aiConversations.pruneHost(host.profileId, new Set(session.snapshot.windows.map((window) => window.id)));
  }, [host?.profileId, session?.connected, session?.snapshot]);

  const toggleSource = (id: AiSourceId) => {
    setSources((current) => {
      const chosen = new Set(current);
      if (chosen.has(id)) chosen.delete(id);
      else chosen.add(id);
      const next = ALL_AI_SOURCES.filter((source) => chosen.has(source));
      rememberedSources = next;
      return next;
    });
  };

  const send = async () => {
    const question = draft.trim();
    if (!question || !session || !assistant || !configured || !key || conversation.streaming || gathering) return;
    setGathering(true);
    // The two sources the panel does not already hold: ask the host for them
    // now, so the snapshot is as current as the other three.
    let automations: AutomationListing | undefined;
    let automationsError: string | undefined;
    let detectedPorts: DetectedPort[] | undefined;
    const clientId = host?.connected ? host.clientId : undefined;
    try {
      if (sources.includes("automations") && clientId) {
        automations = await automationsApi.list(clientId).catch((reason: unknown) => {
          automationsError = String(reason).replace(/^[a-z_]+: /, "");
          return undefined;
        });
      }
      if (sources.includes("ports") && session.sshConnection) {
        detectedPorts = await portsApi.detect(session.sshConnection).catch(() => undefined);
      }
    } finally {
      setGathering(false);
    }
    if (!host) return;
    const input: AiContextInput = {
      hostLabel: host?.label ?? session.hostLabel,
      transport: session.transport,
      connected: session.connected,
      snapshot: session.snapshot,
      activeSessionId: session.activeSessionId,
      activeWindowId: session.activeWindowId,
      activePaneId: session.activePaneId,
      agents: session.agents,
      openTabs: session.openTabs,
      activeRootPath: session.activeRootPath,
      gitStatus: session.gitStatus,
      gitError: session.gitError,
      portForwards: session.portForwards,
      detectedPorts,
      automations,
      automationsError,
    };
    const { history, generation } = aiConversations.begin(key, question);
    setDraft("");
    // False the moment the conversation is cleared, pruned by a window
    // closing, or reopened by a newer question; every await checks it.
    const alive = () => {
      const current = aiConversations.read(key);
      return current.generation === generation && current.streaming;
    };
    const liveClientId = () => {
      const current = hostRef.current;
      return current?.connected ? current.clientId : undefined;
    };
    const messages: AiChatMessage[] = [
      { role: "system", content: buildSystemPrompt(buildContext(sources, input)) },
      ...history.slice(-HISTORY_TURNS)
        .filter((turn) => turn.text !== "" || turn.actions?.length)
        .map((turn) => ({
          role: turn.role,
          content: turn.actions?.length
            ? `${turn.text}\n\n(You did: ${turn.actions.map((action) => `${action.name} — ${action.summary} (${action.outcome})`).join("; ")})`
            : turn.text,
        })),
      { role: "user", content: question },
    ];
    const tools: ToolContext = {
      session: () => sessionRef.current,
      get clientId() {
        return liveClientId();
      },
      profileId: host.profileId,
      openPane: props.onOpenPane,
      sendInput: async (paneId, text, pressEnter) => {
        const live = liveClientId();
        if (!live) throw new Error("the host is not connected");
        await sendTerminalInput(live, paneId, pressEnter ? `${text}\r` : text);
      },
      readScreen: (paneId, rows) => readPaneScreen(terminalCacheKey(host.profileId, paneId), rows),
      runAutomation: async (id) => {
        const live = liveClientId();
        if (!live) return "The host is not connected, so the automation cannot run.";
        const listing = await automationsApi.list(live);
        const summary = listing.automations.find((candidate) => candidate.definition.id === id);
        if (!summary) return `There is no automation with the id "${id}".`;
        const after = await automationsApi.runNow(live, id);
        const run = after.automations.find((candidate) => candidate.definition.id === id)?.recentRuns[0];
        return run
          ? `Started run #${run.number} of "${summary.definition.name || id}"; it opens a new tab.`
          : `Asked to run "${summary.definition.name || id}"; the host did not confirm a run.`;
      },
    };
    let bubbleHasText = false;
    let roundsLeft = MAX_TOOL_ROUNDS;
    // Each round that ends in tool calls spends one. A spent budget is not an
    // ending — it is a question to the user, parked on the conversation
    // exactly like an approval, so a tab switch cannot lose it either.
    for (;;) {
      while (roundsLeft > 0) {
        roundsLeft--;
        if (bubbleHasText) aiConversations.delta(key, "\n\n");
        let text = "";
        const calls: AiToolCall[] = [];
        let failure: string | undefined;
        const call = aiChat(assistant, messages, AI_TOOLS, (event: AiEvent) => {
          if (event.type === "delta") {
            text += event.text;
            aiConversations.delta(key, event.text);
          } else if (event.type === "toolCall") {
            calls.push({ id: event.id, name: event.name, arguments: event.arguments });
          } else if (event.type === "error") {
            // A refusal is terminal: show it now, not when the call settles.
            failure = event.message;
            aiConversations.end(key, event.message);
          }
        });
        aiConversations.attach(key, () => call.cancel());
        await call.done;
        if (!alive()) {
          call.cancel();
          return;
        }
        if (failure) {
          aiConversations.end(key, failure);
          return;
        }
        if (calls.length === 0) {
          if (!text && !bubbleHasText) aiConversations.delta(key, "(no answer)");
          aiConversations.end(key);
          return;
        }
        if (text) bubbleHasText = true;
        // The model asked for actions: answer every one — asking the user first
        // where the tool says to — and let it see the results.
        messages.push({ role: "assistant", content: text || null, toolCalls: calls });
        for (const toolCall of calls) {
          const summary = summarizeCall(toolCall);
          aiConversations.setActivity(key, `${toolCall.name}: ${summary}`);
          let outcome: "done" | "denied" | "failed" = "done";
          let result: string;
          if (APPROVAL_TOOLS.has(toolCall.name)) {
            const approved = await new Promise<boolean>((resolve) => {
              aiConversations.addApproval(key, { id: toolCall.id, name: toolCall.name, summary, decide: resolve });
            });
            if (!approved) {
              result = "The user denied this.";
              outcome = "denied";
            } else if (!alive()) {
              return;
            } else {
              try {
                result = await executeTool(toolCall, tools);
              } catch (reason) {
                result = `Failed: ${String(reason)}`;
                outcome = "failed";
              }
            }
          } else {
            try {
              result = await executeTool(toolCall, tools);
            } catch (reason) {
              result = `Failed: ${String(reason)}`;
              outcome = "failed";
            }
          }
          if (!alive()) return;
          aiConversations.addAction(key, { name: toolCall.name, summary, outcome });
          messages.push({ role: "tool", content: result, toolCallId: toolCall.id });
        }
        aiConversations.setActivity(key, undefined);
      }
      // The budget is spent without an answer. Stop asking the model and ask
      // the user instead; Continue spends another budget on the same run.
      const keepGoing = await new Promise<boolean>((resolve) => {
        aiConversations.addContinuation(key, { roundsUsed: MAX_TOOL_ROUNDS, resume: resolve });
      });
      if (!keepGoing || !alive()) return;
      aiConversations.clearContinuation(key);
      roundsLeft = MAX_TOOL_ROUNDS;
    }
  };

  const stop = () => {
    conversation.cancel?.();
    if (key) aiConversations.end(key);
  };

  if (!host) return <section aria-label="AI assistant" className="ai-panel">
    <p className="quiet-empty">No host is shown.</p>
  </section>;

  const streaming = conversation.streaming;
  return <section aria-label="AI assistant" className="ai-panel">
    <header className="ai-head">
      <span className="ai-head-label">Ask about</span>
      <span className="ai-sources" role="group" aria-label="What the assistant is shown">
        {AI_SOURCES.map((source) => <label className="ai-source" key={source.id}>
          <input
            checked={sources.includes(source.id)}
            onChange={() => toggleSource(source.id)}
            type="checkbox"
          />{source.label}
        </label>)}
      </span>
      {conversation.turns.length > 0 && !streaming && key && <button
        className="ai-clear"
        onClick={() => aiConversations.clear(key)}
        type="button"
      >Clear</button>}
    </header>

    {!configured && <div className="ai-setup">
      <strong>No model is configured</strong>
      <p>Set an <strong>Endpoint</strong> and a <strong>Model</strong> under Settings → Assistant.
        Any OpenAI-compatible server answers — Ollama, MLX, vLLM, or a hosted API.</p>
    </div>}

    {conversation.error && <div className="surface-error" role="alert">{conversation.error}</div>}

    {conversation.turns.length === 0
      ? <p className="quiet-empty ai-empty">{key
        ? "Nothing asked yet. The assistant is shown what the Session, Files, Git, Ports and Automations tabs hold, and this tab keeps its own thread."
        : "Open a tab — each tab of the strip keeps its own conversation."}</p>
      : <ol aria-label="Conversation" className="ai-turns" ref={list}>
        {conversation.turns.map((turn, index) => <li className={`ai-turn ai-turn-${turn.role}`} key={index}>
          {turn.role === "user"
            ? <span className="ai-question">{turn.text}</span>
            : <>
              {turn.actions?.map((action, actionIndex) => <span
                className={`ai-action ai-action-${action.outcome}`}
                key={actionIndex}
              >{action.summary} — {action.outcome}</span>)}
              {turn.text
                ? <div className="ai-answer" dangerouslySetInnerHTML={{ __html: renderSafeMarkdown(turn.text) }} />
                : streaming && index === conversation.turns.length - 1 && !conversation.pending.length
                  ? <span className="ai-thinking" role="status">Thinking…</span>
                  : !turn.actions?.length && <span className="ai-answer quiet-empty">(no answer)</span>}
            </>}
          {turn.createdAt > 0 && <time className="ai-turn-time" dateTime={new Date(turn.createdAt).toISOString()}>
            {TURN_TIME.format(turn.createdAt)}
          </time>}
        </li>)}
      </ol>}

    {conversation.pending.map((approval) => <div className="ai-approval" key={approval.id}>
      <strong>{approvalTitle(approval.name)}</strong>
      <p className="ai-approval-detail">{approval.summary}</p>
      <span className="ai-approval-actions">
        <button
          autoFocus
          className="primary"
          onClick={() => key && aiConversations.resolveApproval(key, approval.id, true)}
          type="button"
        >Allow</button>
        <button onClick={() => key && aiConversations.resolveApproval(key, approval.id, false)} type="button">Deny</button>
      </span>
    </div>)}

    {conversation.continuation && key && <div className="ai-approval ai-continuation">
      <strong>This is taking a while</strong>
      <p className="ai-approval-detail">The assistant has used {conversation.continuation.roundsUsed} tool rounds
        without finishing its answer. Continue gives it {conversation.continuation.roundsUsed} more on the same run.</p>
      <span className="ai-approval-actions">
        <button
          autoFocus
          className="primary"
          onClick={() => {
            const waiting = aiConversations.read(key).continuation;
            // The store clears the card first: the loop must not find it still
            // parked when it resumes a microtask later.
            aiConversations.clearContinuation(key);
            waiting?.resume(true);
          }}
          type="button"
        >Continue</button>
        <button onClick={() => aiConversations.end(key)} type="button">Stop</button>
      </span>
    </div>}

    {conversation.activity && <div className="ai-activity" role="status">→ {conversation.activity}</div>}

    <footer className="ai-compose">
      <textarea
        aria-label="Ask about this session"
        disabled={!configured || gathering || !key}
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && !event.shiftKey) {
            event.preventDefault();
            void send();
          }
        }}
        placeholder={key
          ? (configured ? "What is happening in this tab?" : "Set an endpoint in Settings → Assistant first")
          : "Open a tab to talk to the assistant"}
        rows={2}
        value={draft}
      />
      {streaming
        ? <button className="primary" onClick={stop} type="button">Stop</button>
        : <button
          className="primary"
          disabled={!configured || gathering || !draft.trim() || !key}
          onClick={() => void send()}
          type="button"
        >{gathering ? "Reading…" : "Ask"}</button>}
    </footer>
  </section>;
}
