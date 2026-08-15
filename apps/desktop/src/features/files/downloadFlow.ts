import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { currentPlatform } from "../../commands/registry";

/** What the user pointed at on the host. Where it lands is the flow's job. */
export interface DownloadIntent {
  path: string;
  kind: "file" | "folder";
}

export interface ChosenDestination {
  destination: string;
  /**
   * The save panel displayed this exact name, so its own "…already exists.
   * Replace?" prompt is consent to overwrite it. False when the backend will
   * write somewhere the panel never showed — see `chooseDownloadDestination`.
   */
  panelConfirmed: boolean;
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
 * act, and macOS's own "…already exists. Replace?" is the confirmation for it.
 *
 * The one case where that consent does not cover the file actually written:
 * the backend appends `.tar` to a folder archive whose name lacks it, *after*
 * the panel has closed. Type `archive` over the suggested `archive.tar` and
 * the panel asks nothing while the backend replaces an existing `archive.tar`.
 * `panelConfirmed` reports that, and the caller downgrades to a policy that
 * refuses rather than overwrites.
 *
 * Note the deliberate consequence of computing the unique name up front: the
 * panel opens on an absolute path under the OS Downloads directory, so it no
 * longer returns to wherever the user last saved. Uniqueness has to be checked
 * against a directory this side can name, and Downloads is the only one.
 *
 * Returns `undefined` when the user cancelled — cancelling a save panel is a
 * complete answer and must not raise anything.
 */
export async function chooseDownloadDestination(intent: DownloadIntent): Promise<ChosenDestination | undefined> {
  const name = suggestedDownloadName(intent);
  // A failure here costs a nicer default, not the download: fall back to the
  // bare basename and let the panel resolve it against its own last directory.
  const defaultPath = await invoke<string>("suggest_download_destination", { fileName: name })
    .catch(() => name);
  const selected = await save({
    title: intent.kind === "folder" ? "Save folder archive" : "Save file",
    defaultPath,
  });
  if (!selected) return undefined;
  return { destination: selected, panelConfirmed: intent.kind !== "folder" || selected.endsWith(".tar") };
}

export function suggestedDownloadName(intent: DownloadIntent): string {
  const trimmed = intent.path.replace(/\/+$/u, "");
  const name = trimmed.slice(trimmed.lastIndexOf("/") + 1) || "download";
  return intent.kind === "folder" && !name.endsWith(".tar") ? `${name}.tar` : name;
}

/** Only macOS has a Finder; saying so anywhere else is wrong. */
export function revealDownloadLabel(platform = currentPlatform()): string {
  return platform === "mac" ? "Show in Finder" : "Show in folder";
}
