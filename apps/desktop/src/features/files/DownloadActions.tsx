import { invoke } from "@tauri-apps/api/core";
import { revealDownloadLabel } from "./downloadFlow";

/**
 * What to do with a finished download, offered on both surfaces that mention
 * one: the completion toast and the Explorer's downloads list. They differ
 * only in where a refusal is reported, so that is the only parameter.
 *
 * Both commands refuse any path the app did not itself just write; see
 * `src-tauri/src/connection/files/download_opener.rs`.
 */
export function DownloadActions({ destination, onError }: { destination: string; onError(message: string): void }) {
  const act = (command: "open_download" | "reveal_download") =>
    void invoke(command, { path: destination }).catch((error) => onError(String(error)));
  return <>
    <button aria-label={`Open ${destination}`} onClick={() => act("open_download")} type="button">Open</button>
    <button aria-label={`${revealDownloadLabel()}: ${destination}`} onClick={() => act("reveal_download")} type="button">{revealDownloadLabel()}</button>
  </>;
}
