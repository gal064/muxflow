import { describe, expect, it } from "vitest";
import { liveFileScope } from "./useWorkspaceDomainController";

const selection = {
  hostProfileId: "local",
  serverIdentity: "server",
  sessionId: "$1",
  paneId: "%1",
};

describe("workspace file readiness", () => {
  it("does not create a request scope from the snapshot-before-connected gap", () => {
    expect(liveFileScope(false, "client", 41, 7, selection)).toBeUndefined();
    expect(liveFileScope(true, "client", 41, 7, selection)).toEqual({
      ...selection,
      clientId: "client",
      generation: 7,
      terminalEpoch: 41,
    });
  });

  it("requires every native transport identity field", () => {
    expect(liveFileScope(true, undefined, 41, 7, selection)).toBeUndefined();
    expect(liveFileScope(true, "client", 0, 7, selection)).toBeUndefined();
    expect(liveFileScope(true, "client", 41, 7, undefined)).toBeUndefined();
  });
});
