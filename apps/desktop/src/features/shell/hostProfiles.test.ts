import { describe, expect, it } from "vitest";
import { profileIdForSshConnection, savedSshProfileId } from "./hostProfiles";

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
