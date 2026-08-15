import { invoke } from "@tauri-apps/api/core";
import { revealDownloadLabel } from "./downloadFlow";

/**
 * What to do with a finished download, offered on both surfaces that mention
 * one: the completion toast and the Explorer's downloads list. They differ
 * only in where a refusal is reported, so that is the only parameter. A
 * later success clears the earlier refusal, so a row cannot keep showing an
 * error it has since recovered from.
 *
 * Both commands refuse any path the app did not itself just write; see
 * `src-tauri/src/connection/files/download_opener.rs`.
 */
export function DownloadActions({ destination, onResult }: {
  destination: string;
  /** Called with the refusal, or with `undefined` when the open succeeds. */
  onResult(error?: string): void;
}) {
  const act = (command: "open_download" | "reveal_download") =>
    void invoke(command, { path: destination })
      .then(() => onResult(undefined))
      .catch((error) => onResult(String(error)));
  const reveal = revealDownloadLabel();
  return <>
    <button aria-label={`Open ${destination}`} onClick={() => act("open_download")} type="button">Open</button>
    <button aria-label={`${reveal}: ${destination}`} onClick={() => act("reveal_download")} type="button">{reveal}</button>
  </>;
}
