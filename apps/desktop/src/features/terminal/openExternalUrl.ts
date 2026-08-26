import { invoke } from "@tauri-apps/api/core";

/**
 * Hand an `http(s)` URL to the machine's default browser.
 *
 * Inside Tauri, `window.open` does not leave the webview, so the shell owns
 * the hop through the same `open_external_link` boundary the Markdown preview
 * uses; it re-validates the URL before any platform call. `confirmed` is
 * already true here because the caller's gesture — the modifier-click a
 * terminal link requires — is the deliberate act. In the browser dev harness
 * there is no shell, and a new tab is the honest equivalent.
 */
export async function openExternalUrl(url: string): Promise<void> {
  if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
    await invoke("open_external_link", { url, confirmed: true });
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}
