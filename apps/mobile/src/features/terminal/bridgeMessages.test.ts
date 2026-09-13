import { describe, expect, it } from "vitest";
import { parseFromPageMessage } from "./bridgeMessages";

describe("terminal WebView interaction messages", () => {
  it.each([
    [{ t: "copy", text: "curl --fail https://example.test" }, { t: "copy", text: "curl --fail https://example.test" }],
    [{ t: "openLink", href: "https://example.test/a" }, { t: "openLink", href: "https://example.test/a" }],
    [{ t: "openFile", path: "../README.md" }, { t: "openFile", path: "../README.md" }],
  ])("accepts an explicit interaction", (message, expected) => {
    expect(parseFromPageMessage(JSON.stringify(message))).toEqual(expected);
  });

  it.each([
    { t: "copy", text: "" },
    { t: "copy", text: 7 },
    { t: "openLink", href: null },
    { t: "openFile", path: "" },
  ])("rejects a malformed interaction %#", (message) => {
    expect(parseFromPageMessage(JSON.stringify(message))).toBeUndefined();
  });
});
