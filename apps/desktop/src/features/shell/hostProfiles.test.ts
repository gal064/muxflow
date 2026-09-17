import { describe, expect, it } from "vitest";
import { hostLetter, profileIdForSshConnection, savedSshProfileId } from "./hostProfiles";

describe("host letter", () => {
  it("prefers the saved letter, then the label, then the id, always upper-cased", () => {
    expect(hostLetter({ id: "ssh-work", label: "workbox", letter: "w" })).toBe("W");
    expect(hostLetter({ id: "local", label: "Local" })).toBe("L");
    expect(hostLetter({ id: "ssh-work", label: "  prod.example " })).toBe("P");
    expect(hostLetter({ id: "ssh-work", label: "   " })).toBe("S");
    expect(hostLetter({ id: "", label: "" })).toBe("");
  });

  it("takes one whole character, not one UTF-16 code unit", () => {
    expect(hostLetter({ id: "x", label: "🦀 crab" })).toBe("🦀");
    expect(hostLetter({ id: "x", label: "élan" })).toBe("É");
  });
});

describe("saved SSH profile identity", () => {
  it("does not collide for targets that share the same display slug", () => {
    expect(savedSshProfileId("prod.example", "")).not.toBe(savedSshProfileId("prod-example", ""));
    expect(savedSshProfileId("user@host", "/a")).not.toBe(savedSshProfileId("user@host", "/b"));
    expect(savedSshProfileId("prod.example", "")).toMatch(/^ssh-[a-zA-Z0-9_-]+-[0-9a-f]{16}$/);
  });

  it("reuses a legacy persisted ID for the exact same SSH connection", () => {
    const profiles = [{ id: "legacy", label: "Prod", connection: { mode: "ssh" as const, profileId: "legacy", target: "prod", configPath: "/cfg" } }];
    expect(profileIdForSshConnection(profiles, "prod", "/cfg")).toBe("legacy");
  });
});
