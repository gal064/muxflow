import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { currentPlatform } from "../../commands/registry";

/** What the user pointed at on the host. Where it lands is the flow's job. */
export interface DownloadIntent {
  path: string;
  kind: "file" | "folder";
}

/**
 * Downloading is one gesture: the OS save panel, then the transfer.
 *
 * It used to be three — an in-app modal, a "Choose…" button that opened the
 * real panel, then "Start download" — plus a collision dropdown asking the user
 * to pre-decide what should happen to a file they had not yet named. The panel
 * is the only thing in that sequence a person recognises, so it goes first and
 * alone.
 *
 * The default name is made unique *before* the panel opens, which is what turns
 * "download the same file three times" into `x.pdf`, `x (1).pdf`, `x (2).pdf`
 * with nothing to answer. Naming an existing file anyway is then a deliberate
 * act, and macOS's own "…already exists. Replace?" is the confirmation for it —
 * which is why the transfer is started with `overwrite` rather than `rename`:
 * quietly renaming a file after the user clicked Replace would ignore them.
 *
 * Returns the chosen absolute path, or `undefined` when the user cancelled —
 * cancelling a save panel is a complete answer and must not raise anything.
 */
export async function chooseDownloadDestination(intent: DownloadIntent): Promise<string | undefined> {
  const name = suggestedDownloadName(intent);
  // A failure here costs a nicer default, not the download: fall back to the
  // bare basename and let the panel resolve it against its own last directory.
  const defaultPath = await invoke<string>("suggest_download_destination", { fileName: name })
    .catch(() => name);
  const selected = await save({
    title: intent.kind === "folder" ? "Save folder archive" : "Save file",
    defaultPath,
  });
  return selected ?? undefined;
}

export function suggestedDownloadName(intent: DownloadIntent): string {
  const trimmed = intent.path.replace(/\/+$/u, "");
  const name = trimmed.slice(trimmed.lastIndexOf("/") + 1) || "download";
  return intent.kind === "folder" && !name.endsWith(".tar") ? `${name}.tar` : name;
}

/**
 * Both openers refuse any path the app did not itself just write; see
 * `src-tauri/src/connection/files/download_opener.rs`.
 */
export function openDownload(destination: string): Promise<void> {
  return invoke("open_download", { path: destination });
}

export function revealDownload(destination: string): Promise<void> {
  return invoke("reveal_download", { path: destination });
}

/** Only macOS has a Finder; saying so anywhere else is wrong. */
export function revealDownloadLabel(platform = currentPlatform()): string {
  return platform === "mac" ? "Show in Finder" : "Show in folder";
}
