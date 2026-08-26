import { invoke } from "@tauri-apps/api/core";

/**
 * Hand an `http(s)` URL to the machine's default browser.
 *
 * Inside Tauri, `window.open` does not leave the webview, so the shell owns
 * the hop: `open_external_url` re-validates the scheme before any platform
 * call. In the browser dev harness there is no shell, and a new tab is the
 * honest equivalent.
 */
export async function openExternalUrl(url: string): Promise<void> {
  if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
    await invoke("open_external_url", { url });
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}
