// Small, copy-time context for the memory-only flight recorder. It reads the
// existing in-memory stores synchronously; copying performs no I/O.

import Constants from "expo-constants";
import { AppState, Platform } from "react-native";

import { voiceStore } from "../features/voice/voiceStore";
import { windowGrid } from "../features/terminal/sizing";
import { sessionStore } from "../store/sessionStore";
import { logStore } from "./log";

function device(): string {
  if (Platform.OS === "android") return `${Platform.constants.Manufacturer}/${Platform.constants.Model}`;
  if (Platform.OS === "ios") return `ios/${Platform.constants.interfaceIdiom}`;
  return Platform.OS;
}

function build(): string {
  const androidVersionCode =
    Constants.platform?.android?.versionCode ?? Constants.expoConfig?.android?.versionCode;
  if (Platform.OS === "android" && androidVersionCode !== undefined) {
    return String(androidVersionCode);
  }
  return Constants.expoRuntimeVersion ?? "development";
}

export function diagnosticHeader(now = Date.now()): string[] {
  const state = sessionStore.getState();
  const voice = voiceStore.getState();
  const recorder = logStore.getState();
  const focusedPane = state.focusedPaneId ? state.panes[state.focusedPaneId] : undefined;
  const hostGrid = focusedPane ? windowGrid(Object.values(state.panes), focusedPane.windowId) : undefined;
  const phases = Object.values(voice.sessions).reduce<Record<string, number>>((counts, session) => {
    counts[session.phase] = (counts[session.phase] ?? 0) + 1;
    return counts;
  }, {});
  const phaseSummary = Object.entries(phases).map(([phase, count]) => `${phase}:${count}`).join(",") || "none";
  const working = Object.values(state.agents).filter((agent) => agent.present && agent.lifecycle === "working").length;
  const appName = Constants.expoConfig?.name ?? "Muxflow";
  const version = Constants.expoConfig?.version ?? "unknown";
  return [
    `diagnostics copied=${new Date(now).toISOString()} storage=memory-only events=${recorder.lines.length} bytes=${recorder.bytes}`,
    `app name=${appName} version=${version} build=${build()} platform=${Platform.OS} os=${String(Platform.Version)} device=${device()}`,
    `state lifecycle=${AppState.currentState} connection=${state.connection.state} attempt=${state.connection.attempt} topology=${state.topologyGeneration} focusedPane=${state.focusedPaneId ?? "none"} hostGrid=${hostGrid ? `${hostGrid.cols}x${hostGrid.rows}` : "unknown"} agents=${Object.keys(state.agents).length} working=${working} voice=${phaseSummary}`,
  ];
}
