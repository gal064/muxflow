// Drives the real expo-router code that `router.push` runs, from href to the
// params `useLocalSearchParams` hands a screen, to prove the encoding
// survives it. The chain in expo-router 57.0.16 is
// `build/global-state/router.js` line 69 (`push` -> `resolveHref`) ->
// `build/global-state/getNavigationAction.js` line 26
// (`store.linking.getStateFromPath`, the fork re-exported by
// `build/link/linking.js` line 46) -> `LocalRouteParamsContext` ->
// `build/hooks/useLocalSearchParams.js`.

import { createRequire, Module } from "node:module";

import { getReactNavigationConfig } from "expo-router/build/getReactNavigationConfig";
import { findFocusedRoute } from "expo-router/build/fork/findFocusedRoute";
import { resolveHref } from "expo-router/build/link/href";
import { describe, expect, it } from "vitest";

import { fromRouteParam, toRouteParam } from "./routeParams";

// `getStateFromPath` requires `../react-navigation/native` only for
// `validatePathConfig` (`build/fork/getStateFromPath.js` line 170), and that
// module pulls in react-native, whose flow-typed entry point cannot be parsed
// under node. vitest loads `node_modules` through Node's own `require`, out of
// reach of `vi.mock`, so the stand-in goes into `require.cache` instead.
const nodeRequire = createRequire(import.meta.url);
const nativeId = nodeRequire.resolve("expo-router/build/react-navigation/native");
const nativeStandIn = new Module(nativeId);
nativeStandIn.exports = { validatePathConfig: () => undefined };
nativeStandIn.loaded = true;
nodeRequire.cache[nativeId] = nativeStandIn;
const { getStateFromPath } = await import("expo-router/build/fork/getStateFromPath");

// The app's route tree, in the shape `getReactNavigationConfig` reads, so the
// `[paneId]` -> `:paneId` conversion is the real one too.
const routeTree = {
  route: "",
  children: [
    "terminal/[paneId]",
    "files/[paneId]/index",
    "files/[paneId]/dir",
    "file/[paneId]",
    "workspace/[sessionId]",
  ].map((route) => ({ route, children: [] })),
};
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const linkingConfig = getReactNavigationConfig(routeTree as any, true);

/** What `useLocalSearchParams` does to each value (`build/hooks/useLocalSearchParams.js` lines 34-40). */
function decodeLikeUseLocalSearchParams(params: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(params).map(([key, value]) => {
      if (typeof value !== "string") return [key, value];
      try {
        return [key, decodeURIComponent(value)];
      } catch {
        return [key, value];
      }
    }),
  );
}

/** Pushes an href through the router and returns the params the screen would read. */
function paramsSeenByScreen(href: { pathname: string; params: Record<string, string> }): Record<string, unknown> {
  const state = getStateFromPath(resolveHref(href), linkingConfig);
  expect(state).toBeDefined();
  const focused = findFocusedRoute(state!);
  expect(focused?.name).toBe(href.pathname.slice(1));
  return decodeLikeUseLocalSearchParams((focused?.params ?? {}) as Record<string, unknown>);
}

const paneIds = ["%1", "%12", "%100", "%2512", "%0", "%99999"];
const sessionIds = ["$0", "$3", "$12"];
const paths = [
  "/home/x/100% done/file #1.txt",
  "/tmp/a+b c?d=e&f/naïve — 日本語 🚀.md",
  "/srv/~0025/%2512/..",
];

describe("expo-router pipeline with the encoding", () => {
  it("delivers tmux pane and session ids to the terminal route exactly", () => {
    for (const paneId of paneIds) {
      for (const sessionId of sessionIds) {
        const seen = paramsSeenByScreen({
          pathname: "/terminal/[paneId]",
          params: { paneId: toRouteParam(paneId), sessionId: toRouteParam(sessionId) },
        });
        expect(fromRouteParam(seen.paneId as string)).toBe(paneId);
        expect(fromRouteParam(seen.sessionId as string)).toBe(sessionId);
      }
    }
  });

  it("delivers exact file paths and names to the files and file routes", () => {
    for (const pathname of ["/files/[paneId]/dir", "/file/[paneId]"]) {
      for (const path of paths) {
        const name = path.slice(path.lastIndexOf("/") + 1);
        const seen = paramsSeenByScreen({
          pathname,
          params: { paneId: toRouteParam("%12"), path: toRouteParam(path), name: toRouteParam(name) },
        });
        expect(fromRouteParam(seen.paneId as string)).toBe("%12");
        expect(fromRouteParam(seen.path as string)).toBe(path);
        expect(fromRouteParam(seen.name as string)).toBe(name);
      }
    }
  });

  it("delivers a session id to the workspace route exactly", () => {
    const seen = paramsSeenByScreen({ pathname: "/workspace/[sessionId]", params: { sessionId: toRouteParam("$12") } });
    expect(fromRouteParam(seen.sessionId as string)).toBe("$12");
  });
});

describe("expo-router pipeline without the encoding (why the helper exists)", () => {
  it("mangles every pane id from %10 upward, and only %1 survives by accident", () => {
    const seen = paramsSeenByScreen({ pathname: "/terminal/[paneId]", params: { paneId: "%12", sessionId: "$3" } });
    // `%12` -> `%2512` on the wire -> `%12` after the parse -> U+0012 after the hook.
    expect(seen.paneId).toBe(String.fromCharCode(0x12));

    const escaped = paramsSeenByScreen({ pathname: "/terminal/[paneId]", params: { paneId: "%2512" } });
    expect(escaped.paneId).toBe("%12");

    const single = paramsSeenByScreen({ pathname: "/terminal/[paneId]", params: { paneId: "%1" } });
    expect(single.paneId).toBe("%1");
  });

  it("mangles a query param whose text happens to look like a percent-escape", () => {
    const seen = paramsSeenByScreen({ pathname: "/file/[paneId]", params: { paneId: "%1", path: "/home/x/100%20done.txt" } });
    expect(seen.path).toBe("/home/x/100 done.txt");
    // ...while `% d` survives only because the second decode throws on it.
    const lucky = paramsSeenByScreen({ pathname: "/file/[paneId]", params: { paneId: "%1", path: "/home/x/100% done.txt" } });
    expect(lucky.path).toBe("/home/x/100% done.txt");
  });
});
