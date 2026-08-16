import { memo, useRef, useState } from "react";
import { keyboardEventIsComposing } from "../../commands/registry";
import { SurfaceError } from "../../ui/SurfaceError";
import type { GitCommandResult } from "./types";

interface Props {
  stagedCount: number;
  disabled: boolean;
  commit(message: string): Promise<GitCommandResult | undefined>;
  onMessage(message: string): void;
}

/**
 * The commit form owns its own draft.
 *
 * It used to live in `GitSidebar`, so every keystroke re-rendered the whole
 * panel and rebuilt every status row — up to a thousand of them for one
 * character. Nothing above this component needs to know what is being typed.
 */
export const GitCommitForm = memo(function GitCommitForm(props: Props) {
  const [message, setMessage] = useState("");
  const [output, setOutput] = useState<GitCommandResult>();
  const [error, setError] = useState<string>();
  const composing = useRef(false);

  const submit = async () => {
    setError(undefined);
    setOutput(undefined);
    if (!message.trim()) {
      setError("Enter a commit message.");
      return;
    }
    try {
      const result = await props.commit(message);
      if (!result) return;
      setOutput(result);
      if (result.outcome === "applied") setMessage("");
      else setError(result.outcome === "partialOrUnknown" ? "Commit outcome is uncertain; inspect HEAD before retrying." : "Git did not create a commit.");
    } catch (cause) { setError(String(cause)); }
  };

  return <form className="git-commit" onSubmit={(event) => { event.preventDefault(); if (!composing.current) void submit(); }}>
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
          void submit();
        }
      }}
      placeholder="Commit message…"
      value={message}
    />
    <button className="primary" disabled={props.disabled} type="submit">Commit {props.stagedCount} staged</button>
    {error && <SurfaceError detail={error} />}
    {output && <pre aria-label="Git commit output" className={output.outcome === "applied" && !output.refreshFailed ? "git-output" : "git-output error"}>{commandDetails(output)}</pre>}
  </form>;
});

function commandDetails(result: GitCommandResult): string {
  return [result.stdout, result.stdoutTruncated ? "[stdout truncated]" : "", result.stderr, result.stderrTruncated ? "[stderr truncated]" : "", result.error,
    result.outcome === "partialOrUnknown" ? `Outcome is partial or unknown. HEAD ${result.preHeadOid || "?"} → ${result.postHeadOid || "?"}. Inspect the repository before retrying.` : "",
    result.refreshFailed ? `Commit completed, but ${result.statusOmitted ? "post-command status was omitted to keep the connection responsive" : "status refresh failed"}: ${result.refreshError}` : ""].filter(Boolean).join("\n")
    || (result.outcome === "applied" ? "Commit created." : `Git exited with code ${result.exitCode}.`);
}
