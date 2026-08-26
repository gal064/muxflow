import { memo, useRef, useState } from "react";
import { keyboardEventIsComposing } from "../../commands/registry";
import { anchorForElement, ContextMenu, type ContextMenuAnchor } from "../../ui/ContextMenu";
import { SurfaceError } from "../../ui/SurfaceError";
import type { GitCommandResult } from "./types";

interface Props {
  stagedCount: number;
  disabled: boolean;
  /** Whether this repository can publish at all — a repository with no commit cannot. */
  canPush: boolean;
  commit(message: string): Promise<GitCommandResult | undefined>;
  onPush(): Promise<GitCommandResult>;
}

/**
 * The commit form owns its own draft.
 *
 * It used to live in `GitSidebar`, so every keystroke re-rendered the whole
 * panel and rebuilt every status row — up to a thousand of them for one
 * character. Nothing above this component needs to know what is being typed.
 *
 * Push lives here too, as a sibling button and as the split button's one menu
 * item, because "commit and publish it" is a single intention and splitting it
 * across two surfaces made the second half easy to forget. Each half still
 * reports its own outcome: a commit that worked followed by a push the remote
 * refused is two facts, not one failure.
 */
export const GitCommitForm = memo(function GitCommitForm(props: Props) {
  const [message, setMessage] = useState("");
  const [output, setOutput] = useState<{ result: GitCommandResult; verb: "Commit" | "Push" }>();
  const [error, setError] = useState<string>();
  /** What has already settled — "Committed." survives a push that then failed. */
  const [note, setNote] = useState<string>();
  /** What is happening right now. Always cleared when the work ends. */
  const [progress, setProgress] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [menu, setMenu] = useState<ContextMenuAnchor>();
  const composing = useRef(false);

  const runCommit = async (): Promise<GitCommandResult | undefined> => {
    if (!message.trim()) {
      setError("Enter a commit message.");
      return undefined;
    }
    const result = await props.commit(message);
    if (!result) return undefined;
    setOutput({ result, verb: "Commit" });
    if (result.outcome === "applied") setMessage("");
    else setError(result.outcome === "partialOrUnknown" ? "Commit outcome is uncertain; inspect HEAD before retrying." : "Git did not create a commit.");
    return result;
  };

  // A push is never reported as having worked on the strength of the request
  // returning. The host says whether the remote accepted it, and "unknown" is
  // a third answer that has to survive all the way to this line.
  const runPush = async (already: string): Promise<void> => {
    const result = await props.onPush();
    setOutput({ result, verb: "Push" });
    if (result.outcome === "applied") setNote(`${already}Pushed to ${result.pushTarget || "the upstream"}.`);
    else if (result.outcome === "partialOrUnknown") setError("Push outcome is unknown; check the remote before retrying.");
    else setError("The remote did not accept the push.");
  };

  const run = (work: () => Promise<void>) => {
    if (busy) return;
    setError(undefined);
    setOutput(undefined);
    setNote(undefined);
    setProgress(undefined);
    setBusy(true);
    void work()
      .catch((cause) => setError(String(cause)))
      // Whatever happened, nothing is in flight any more — a progress line left
      // reading "Pushing…" beside an error is the app lying about its own state.
      .finally(() => { setBusy(false); setProgress(undefined); });
  };

  const submit = () => run(async () => { await runCommit(); });
  const commitAndPush = () => run(async () => {
    const committed = await runCommit();
    // Only a commit that actually happened is worth publishing. An uncertain
    // one is a reason to look at HEAD, not to talk to a remote.
    if (committed?.outcome !== "applied") return;
    setNote("Committed.");
    setProgress("Pushing…");
    await runPush("Committed. ");
  });
  const push = () => run(async () => {
    setProgress("Pushing…");
    await runPush("");
  });

  const nothingStaged = props.stagedCount === 0;
  return <form className="git-commit" onSubmit={(event) => { event.preventDefault(); if (!composing.current) submit(); }}>
    <textarea
      aria-label="Commit message"
      disabled={props.disabled}
      id="git-commit-message"
      onChange={(event) => setMessage(event.target.value)}
      onCompositionEnd={() => { composing.current = false; }}
      onCompositionStart={() => { composing.current = true; }}
      onKeyDown={(event) => {
        if (!keyboardEventIsComposing(event.nativeEvent) && event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
          event.preventDefault();
          submit();
        }
      }}
      placeholder="Commit message…"
      value={message}
    />
    <div className="git-commit-actions">
      <span className="split-button">
        <button className="primary" disabled={props.disabled || busy || nothingStaged} type="submit">Commit {props.stagedCount} staged</button>
        <button
          aria-haspopup="menu"
          aria-label="More commit actions"
          className="primary split-button-more"
          disabled={props.disabled || busy}
          onClick={(event) => setMenu(anchorForElement(event.currentTarget))}
          type="button"
        >▾</button>
      </span>
      <button
        aria-label="Push to upstream"
        disabled={props.disabled || busy || !props.canPush}
        onClick={push}
        type="button"
      >Push</button>
    </div>
    {(note || progress) && <p className="surface-note" role="status">{[note, progress].filter(Boolean).join(" ")}</p>}
    {error && <SurfaceError detail={error} />}
    {output && <pre aria-label={`Git ${output.verb.toLowerCase()} output`} className={output.result.outcome === "applied" && !output.result.refreshFailed ? "git-output" : "git-output error"}>{commandDetails(output.result, output.verb)}</pre>}
    {menu && <ContextMenu
      anchor={menu}
      items={[{ id: "commitAndPush", label: "Commit & Push", disabled: nothingStaged || !props.canPush, run: commitAndPush }]}
      label="More commit actions"
      onClose={() => setMenu(undefined)}
    />}
  </form>;
});

function commandDetails(result: GitCommandResult, verb: "Commit" | "Push"): string {
  return [result.stdout, result.stdoutTruncated ? "[stdout truncated]" : "", result.stderr, result.stderrTruncated ? "[stderr truncated]" : "", result.error,
    result.outcome === "partialOrUnknown" ? `Outcome is partial or unknown. HEAD ${result.preHeadOid || "?"} → ${result.postHeadOid || "?"}. Inspect the repository before retrying.` : "",
    result.refreshFailed ? `${verb} completed, but ${result.statusOmitted ? "post-command status was omitted to keep the connection responsive" : "status refresh failed"}: ${result.refreshError}` : ""].filter(Boolean).join("\n")
    || (result.outcome === "applied" ? (verb === "Push" ? `Pushed to ${result.pushTarget || "the upstream"}.` : "Commit created.") : `Git exited with code ${result.exitCode}.`);
}
