// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import type { HostScopeToken } from "../features/shell/hostScope";
import { defaultAppState, type PersistedAppState } from "../features/shell/types";
import type { Session } from "./types";
import { useAppRecoveryController } from "./useAppRecoveryController";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

interface HarnessProps {
  serverIdentity?: string;
  session: Session;
  connectionEpoch: number;
  hostProfileId?: string;
}

let currentOffer: ReturnType<typeof useAppRecoveryController>["offer"];

function Harness({ serverIdentity, session, connectionEpoch, hostProfileId = "remote" }: HarnessProps) {
  const [appState, setAppState] = useState<PersistedAppState>(() => ({
    ...defaultAppState,
    appTabs: [{
      id: "tab-a",
      hostProfileId: "remote",
      serverIdentity: "server-a",
      sessionId: "$1",
      sessionName: "workspace",
      kind: "file" as const,
      resource: "/repo/a.ts",
      title: "a.ts",
      order: 0,
    }],
  }));
  const scope: HostScopeToken = {
    hostProfileId,
    connectionKey: `ssh:${hostProfileId}`,
    connectionEpoch,
    serverIdentity,
    generation: 1,
  };
  currentOffer = useAppRecoveryController({
    appState,
    currentHostProfileId: hostProfileId,
    currentScope: scope,
    serverIdentity,
    sessions: [session],
    windows: [],
    setAppState,
  }).offer;
  return null;
}

describe("useAppRecoveryController", () => {
  it("preserves the last live identity across the reconnect reset gap", async () => {
    const oldSession: Session = {
      id: "$1", name: "workspace", windowCount: 1, attachedClients: 0, order: 0,
    };
    const replacementSession: Session = { ...oldSession, id: "$99" };
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(
        <Harness serverIdentity="server-a" session={oldSession} connectionEpoch={1} />,
      );
    });
    await act(async () => {
      renderer.update(
        <Harness serverIdentity={undefined} session={oldSession} connectionEpoch={2} />,
      );
    });
    await act(async () => {
      renderer.update(
        <Harness serverIdentity="server-b" session={replacementSession} connectionEpoch={2} />,
      );
    });

    expect(currentOffer).toMatchObject({
      count: 1,
      previousServerIdentity: "server-a",
      scope: { serverIdentity: "server-b" },
    });
  });

  it("retains replacement history independently for each host profile", async () => {
    const oldSession: Session = {
      id: "$1", name: "workspace", windowCount: 1, attachedClients: 0, order: 0,
    };
    const replacementSession: Session = { ...oldSession, id: "$99" };
    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(
        <Harness hostProfileId="remote" serverIdentity="server-a" session={oldSession} connectionEpoch={1} />,
      );
    });
    await act(async () => {
      renderer.update(
        <Harness hostProfileId="other" serverIdentity="server-other" session={oldSession} connectionEpoch={2} />,
      );
    });
    await act(async () => {
      renderer.update(
        <Harness hostProfileId="remote" serverIdentity="server-b" session={replacementSession} connectionEpoch={3} />,
      );
    });

    expect(currentOffer).toMatchObject({
      count: 1,
      previousServerIdentity: "server-a",
      scope: { hostProfileId: "remote", serverIdentity: "server-b" },
    });
  });
});
