import { describe, expect, it } from "vitest";

import { createNotificationAttention } from "./attention";

describe("notification attention", () => {
  it("tracks a focused agent and clears it on the focus cleanup", () => {
    const attention = createNotificationAttention();
    const blurOrUnmount = attention.focusAgent("a1");
    expect(attention.viewedAgentId()).toBe("a1");

    blurOrUnmount();
    expect(attention.viewedAgentId()).toBeUndefined();
  });

  it("is inactive in the background and restores the still-focused screen on return", () => {
    const attention = createNotificationAttention();
    attention.focusAgent("a1");

    attention.setAppActive(false);
    expect(attention.viewedAgentId()).toBeUndefined();
    attention.setAppActive(true);
    expect(attention.viewedAgentId()).toBe("a1");
  });

  it("does not let an old screen cleanup clear the screen now in focus", () => {
    const attention = createNotificationAttention();
    const clearFirst = attention.focusAgent("a1");
    const clearSecond = attention.focusAgent("a2");

    clearFirst();
    expect(attention.viewedAgentId()).toBe("a2");
    clearSecond();
    expect(attention.viewedAgentId()).toBeUndefined();
  });
});
