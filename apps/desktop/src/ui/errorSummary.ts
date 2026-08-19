/**
 * Turning a host rejection into a sentence, and keeping the diagnostic.
 *
 * The host fails closed and says so precisely: `file_mutation_rejected: File
 * name too long (os error 63)` is exactly right for whoever has to debug it and
 * tells a person nothing about what to do next. That is the first half of
 * M10-E059, and Phase 11.4.4 is where it is answered: a human-readable summary
 * in front, the diagnostic itself behind a disclosure, never deleted.
 *
 * Deliberately a *fallback chain*, not a lookup table with an "unknown" branch:
 * an unmapped rejection still produces a readable first line rather than
 * disappearing. The mapping only ever improves the wording of something that
 * would be shown anyway.
 */

export interface SurfaceErrorText {
  /** One sentence a person can act on. Always present, never empty. */
  summary: string;
  /** The original text, present only when it says more than the summary. */
  detail?: string;
}

/**
 * `<domain>_<verb>` codes the host prefixes its structured refusals with. Only
 * the families that reach a user-facing surface are listed; anything else falls
 * through to the generic phrasing below.
 */
const CODE_SUMMARY: readonly (readonly [RegExp, string])[] = [
  [/^file_mutation_(rejected|task_failed)$/, "The host refused that file change."],
  [/^file_read_rejected$/, "The host refused to read that file."],
  [/^file_write_.*_rejected$/, "The host refused to save that file."],
  [/^(directory_rejected|unwatch_rejected)$/, "The host refused that folder request."],
  [/^download_.*_rejected$/, "The host refused that download."],
  [/^upload_.*_rejected$/, "The host refused that upload."],
  [/^upload_outcome_unavailable$/, "The host could not confirm how that upload ended."],
  [/^git_rejected$/, "Git refused that change."],
  [/^tmux_action_rejected$/, "tmux refused that action."],
  [/^tmux_unavailable$/, "The tmux server is not reachable right now."],
  [/^terminal_.*_rejected$/, "The terminal connection refused that request."],
  [/^mutation_rejected$/, "The connection to the host is not accepting changes right now."],
  [/^connection_unavailable$/, "The connection to the host is still reconnecting."],
  [/^connection_read_only$/, "The host helper connection is read-only."],
  [/^confirmation_required$/, "That change needs an explicit confirmation first."],
  [/^cancelled$/, "That request was cancelled."],
];

/**
 * What the reason text means in practice. Checked before the code, because
 * "you cannot put a slash in the name" helps and "the host refused that file
 * change" does not. Every entry here was written against a real message the
 * host or the OS produces — the errno arms are the ones the Phase 10 pass hit.
 */
const REASON_SUMMARY: readonly (readonly [RegExp, string])[] = [
  [/file name too long|os error 63/i, "That name is longer than this filesystem allows. Use a shorter one."],
  [
    /mutation parent changed|no-follow directory|unsafe component|outside the active root|escapes the active root|leaf changed or is unsafe/i,
    "The name has to be a plain file name inside the folder you chose — no “/”, no “..”, and no path through a symlink.",
  ],
  [/permission denied|os error 13|read-only file system|os error 30/i, "The host account is not allowed to write there."],
  [/no space left|os error 28/i, "The host has run out of disk space."],
  [/file exists|already exists|os error 17/i, "Something is already at that path. Allow overwrite, or choose another name."],
  [/no such file or directory|os error 2|does not exist|is no longer available/i, "That path is no longer on the host. Refresh and try again."],
  [/not a directory|os error 20/i, "Part of that path is a file, not a folder."],
  [/directory not empty|os error 66/i, "That folder still has files in it; confirm the non-empty replacement to continue."],
  [/nul byte/i, "That name contains a character the filesystem cannot store."],
  [/timed out|timeout/i, "The host did not answer in time. Check the connection and try again."],
  [/disconnected or reconciling/i, "The connection to the host is still reconnecting."],
  [/server is unavailable|connection (closed|lost)|not connected/i, "The connection to the host is down. Reconnect and try again."],
  [/not writable|read-only/i, "The host helper connection is read-only."],
];

/** How much of an unmapped message reads as a summary before it is a wall. */
const SUMMARY_LIMIT = 140;

export function summarizeSurfaceError(raw: string): SurfaceErrorText {
  const text = raw.trim();
  if (text === "") return { summary: "Something went wrong." };
  const stripped = stripErrorPrefix(text);
  const split = stripped.match(/^([a-z][a-z0-9_]*):\s*([\s\S]+)$/);
  const code = split?.[1];
  const reason = split?.[2]?.trim() ?? stripped;

  // Rewriting only ever applies to a *structured host diagnostic*. The same
  // status channel also carries sentences the app wrote itself — "Could not
  // mark %117 hidden: timed out" — and answering one of those with generic
  // advice would throw away the half that says which pane. An app-authored
  // line is shown as written; it only gains a disclosure if it is long or
  // multi-line.
  const structured = code !== undefined && (/_(rejected|unavailable|failed)$/.test(code) || matched(CODE_SUMMARY, code) !== undefined);
  const summary = structured
    ? matched(REASON_SUMMARY, reason) ?? matched(CODE_SUMMARY, code!) ?? firstSentence(reason)
    : firstSentence(stripped);
  return summary === text ? { summary } : { summary, detail: text };
}

/**
 * `String(error)` on a JS error, sometimes twice over when a wrapper rethrows,
 * puts one or more `Error:` prefixes in front of text that is not about
 * JavaScript at all.
 */
function stripErrorPrefix(text: string): string {
  let current = text;
  for (;;) {
    const next = current.replace(/^(?:Error|TypeError|AggregateError)\s*:\s*/, "");
    if (next === current) return current;
    current = next;
  }
}

function matched(table: readonly (readonly [RegExp, string])[], value: string): string | undefined {
  return table.find(([pattern]) => pattern.test(value))?.[1];
}

/** The first line, ended at a sentence boundary if there is one in range. */
function firstSentence(text: string): string {
  const line = text.split("\n")[0].trim();
  const stop = line.search(/[.!?](\s|$)/);
  const candidate = stop >= 0 && stop < SUMMARY_LIMIT ? line.slice(0, stop + 1) : line;
  if (candidate.length <= SUMMARY_LIMIT) return candidate;
  const cut = candidate.lastIndexOf(" ", SUMMARY_LIMIT);
  return `${candidate.slice(0, cut > 40 ? cut : SUMMARY_LIMIT).trimEnd()}…`;
}
