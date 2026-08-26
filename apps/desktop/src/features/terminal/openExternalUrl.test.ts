// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));

import { invoke } from "@tauri-apps/api/core";
import { openExternalUrl } from "./openExternalUrl";

describe("openExternalUrl", () => {
  afterEach(() => {
    vi.mocked(invoke).mockClear();
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it("asks the shell to open the link inside Tauri", async () => {
    (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
    const open = vi.spyOn(window, "open").mockImplementation(() => null);
    await openExternalUrl("https://example.com/");
    expect(invoke).toHaveBeenCalledWith("open_external_url", { url: "https://example.com/" });
    expect(open).not.toHaveBeenCalled();
    open.mockRestore();
  });

  it("falls back to a new browser tab outside Tauri", async () => {
    const open = vi.spyOn(window, "open").mockImplementation(() => null);
    await openExternalUrl("https://example.com/");
    expect(invoke).not.toHaveBeenCalled();
    expect(open).toHaveBeenCalledWith("https://example.com/", "_blank", "noopener,noreferrer");
    open.mockRestore();
  });
});
