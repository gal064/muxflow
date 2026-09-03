// The Files list model (design doc §9.6): which entries are shown, in what
// order, and what a tap on one does.
//
// Everything here is pure. `FileMetadata` comes straight off the wire; the
// screen never reads the proto message directly, so the hiding, sorting and
// navigability rules have exactly one implementation and one test.

import { FileKind, type FileMetadata } from "../../protocol/gen/envelope_pb";

/**
 * §9.6 step 4: names no listing shows. Dotfiles other than these are listed.
 *
 * The host already refuses to report `.git` and `.DS_Store` from any listing
 * (`ALWAYS_HIDDEN` in apps/host/src/service/filesystem.rs) and deliberately
 * *shows* `node_modules`. The phone hides a superset, so the filter is applied
 * client-side and stays correct whichever end drops a name first.
 */
export const HIDDEN_NAMES: ReadonlySet<string> = new Set([
  ".git",
  "node_modules",
  "target",
  ".venv",
  "__pycache__",
  ".DS_Store",
]);

/** §9.6 step 3: Markdown files sort ahead of other files and use the strong ink. */
const MARKDOWN_EXTENSIONS = [".md", ".markdown"] as const;

export interface DirectoryEntry {
  /** Exactly the string the host returned in `FileMetadata.path`; never rebuilt by joining. */
  path: string;
  name: string;
  kind: FileKind;
  size: bigint;
  symlink: boolean;
  markdown: boolean;
  /** What a tap does, decided once from the metadata the listing carried. */
  action: EntryAction;
}

/** What a tap on a row does (§9.6 steps 4 and 5). */
export type EntryAction = "openDirectory" | "openFile" | "none";

export function isMarkdownName(name: string): boolean {
  const lower = name.toLowerCase();
  return MARKDOWN_EXTENSIONS.some((extension) => lower.endsWith(extension));
}

export function isHiddenName(name: string): boolean {
  return HIDDEN_NAMES.has(name);
}

export function toDirectoryEntry(metadata: FileMetadata): DirectoryEntry {
  return {
    path: metadata.path,
    name: metadata.name,
    kind: metadata.kind,
    size: metadata.size,
    symlink: metadata.symlink || metadata.kind === FileKind.SYMLINK,
    markdown: metadata.kind !== FileKind.DIRECTORY && isMarkdownName(metadata.name),
    action: entryAction(metadata),
  };
}

/**
 * §9.6 step 4: a symlink is shown but is not navigable unless the host resolved
 * its target to a file.
 *
 * Directory enumeration never follows a link — `metadata_for_directory_entry`
 * in apps/host/src/service/filesystem.rs sets `symlink_target_kind` to
 * `UNSPECIFIED` for every entry it reports, on purpose ("the target is resolved
 * only when a later authorized operation opens it descriptor-relative"). So in
 * practice every symlink in a listing is inert, and the `FILE` branch exists
 * for the day the host starts answering the question.
 */
export function entryAction(metadata: Pick<FileMetadata, "kind" | "symlink" | "symlinkTargetKind">): EntryAction {
  if (metadata.symlink || metadata.kind === FileKind.SYMLINK) {
    return metadata.symlinkTargetKind === FileKind.FILE ? "openFile" : "none";
  }
  if (metadata.kind === FileKind.DIRECTORY) return "openDirectory";
  if (metadata.kind === FileKind.FILE) return "openFile";
  // FIFOs, sockets and devices: shown, not opened. `regular_file_target`
  // refuses them anyway, and the refusal is not worth a screen.
  return "none";
}

/**
 * §9.6 step 3: directories first, then files; each group case-insensitively by
 * name, with Markdown files ahead of the rest within the files group.
 */
export function compareEntries(left: DirectoryEntry, right: DirectoryEntry): number {
  const leftDirectory = left.kind === FileKind.DIRECTORY ? 0 : 1;
  const rightDirectory = right.kind === FileKind.DIRECTORY ? 0 : 1;
  if (leftDirectory !== rightDirectory) return leftDirectory - rightDirectory;
  if (leftDirectory === 1) {
    const leftMarkdown = left.markdown ? 0 : 1;
    const rightMarkdown = right.markdown ? 0 : 1;
    if (leftMarkdown !== rightMarkdown) return leftMarkdown - rightMarkdown;
  }
  const folded = left.name.toLocaleLowerCase().localeCompare(right.name.toLocaleLowerCase());
  // A stable tiebreak so two names that fold together keep a fixed order.
  return folded !== 0 ? folded : left.name.localeCompare(right.name);
}

/** Hides, maps and sorts one assembled listing. */
export function visibleEntries(entries: readonly FileMetadata[]): DirectoryEntry[] {
  return entries
    .filter((entry) => !isHiddenName(entry.name))
    .map(toDirectoryEntry)
    .sort(compareEntries);
}

/**
 * §9.6 step 3 sizes: `12 KB`, `1.4 MB`.
 *
 * One decimal below ten, none above, which is what both of the document's
 * examples show. Powers of 1024, as the desktop's `formatBytes` uses; the
 * labels are the ones §9.6 spells.
 */
export function formatSize(bytes: bigint): string {
  const units = [
    [1024n ** 4n, "TB"],
    [1024n ** 3n, "GB"],
    [1024n ** 2n, "MB"],
    [1024n, "KB"],
  ] as const;
  for (const [unit, label] of units) {
    if (bytes >= unit) {
      const tenths = (bytes * 10n) / unit;
      const whole = tenths / 10n;
      return whole >= 10n ? `${(tenths + 5n) / 10n} ${label}` : `${whole}.${tenths % 10n} ${label}`;
    }
  }
  return `${bytes} B`;
}
