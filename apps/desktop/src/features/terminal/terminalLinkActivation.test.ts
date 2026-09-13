import { describe, expect, it } from "vitest";
import { isTerminalLinkActivation } from "./terminalLinkActivation";

describe("terminal link activation", () => {
  it("uses the platform's own modifier", () => {
    expect(isTerminalLinkActivation({ metaKey: true, ctrlKey: false }, "mac")).toBe(true);
    expect(isTerminalLinkActivation({ metaKey: false, ctrlKey: true }, "mac")).toBe(false);
    expect(isTerminalLinkActivation({ metaKey: false, ctrlKey: true }, "linux")).toBe(true);
    expect(isTerminalLinkActivation({ metaKey: true, ctrlKey: false }, "linux")).toBe(false);
    expect(isTerminalLinkActivation({ metaKey: false, ctrlKey: false }, "mac")).toBe(false);
    expect(isTerminalLinkActivation({ metaKey: false, ctrlKey: false }, "linux")).toBe(false);
  });
});
