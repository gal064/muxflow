import { describe, expect, it } from "vitest";

import { emptyHostForm, hostFormValues, validateHostForm } from "./validation";

const valid = { host: "devbox", port: "22", user: "dev", label: "" };

describe("validateHostForm (§9.2)", () => {
  it("accepts a filled form and trims what it saves", () => {
    const result = validateHostForm({ host: " devbox ", port: "2222", user: " dev ", label: " Work " });
    expect(result.valid).toBe(true);
    expect(result.draft).toEqual({ host: "devbox", port: 2222, user: "dev", label: "Work" });
  });

  it("requires a host with no spaces", () => {
    expect(validateHostForm({ ...valid, host: "" }).errors.host).toBeDefined();
    expect(validateHostForm({ ...valid, host: "dev box" }).errors.host).toBeDefined();
    expect(validateHostForm({ ...valid, host: "10.0.2.2" }).valid).toBe(true);
  });

  it("requires a user with no spaces", () => {
    expect(validateHostForm({ ...valid, user: "" }).errors.user).toBeDefined();
    expect(validateHostForm({ ...valid, user: "two words" }).errors.user).toBeDefined();
  });

  it("requires an integer port between 1 and 65535", () => {
    for (const port of ["", "0", "65536", "22.5", "-1", "abc", "2 2"]) {
      const result = validateHostForm({ ...valid, port });
      expect(result.valid, `port ${JSON.stringify(port)}`).toBe(false);
      expect(result.errors.port).toBeDefined();
    }
    for (const port of ["1", "22", "22222", "65535"]) {
      expect(validateHostForm({ ...valid, port }).valid, `port ${port}`).toBe(true);
    }
  });

  it("keeps the label optional", () => {
    const result = validateHostForm(valid);
    expect(result.valid).toBe(true);
    expect(result.draft?.label).toBe("");
  });

  it("starts an empty form on port 22", () => {
    expect(emptyHostForm()).toEqual({ host: "", port: "22", user: "", label: "" });
    expect(validateHostForm(emptyHostForm()).valid).toBe(false);
  });

  it("shows a defaulted label as empty when editing", () => {
    expect(hostFormValues({ host: "devbox", port: 22, user: "dev", label: "devbox" }).label).toBe("");
    expect(hostFormValues({ host: "devbox", port: 22, user: "dev", label: "Work" }).label).toBe("Work");
  });
});
