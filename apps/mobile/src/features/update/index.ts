// Installs the update check once, from wireApp.

import Constants from "expo-constants";
import { AppState, Linking, Platform } from "react-native";

import { log } from "../../session/log";
import { MANIFEST_URL, startUpdateCheck } from "./updateCheck";

const TIMEOUT_MS = 10_000;

async function fetchManifest(): Promise<unknown> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const response = await fetch(MANIFEST_URL, { signal: controller.signal });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return await response.json();
  } finally {
    clearTimeout(timer);
  }
}

let started = false;

export function startAppUpdateCheck(): void {
  // GitHub publishes Android artifacts; iOS testers update through TestFlight.
  if (started || Platform.OS === "ios") return;
  started = true;
  const runningVersion = Constants.expoConfig?.version;
  if (!runningVersion) return;
  let previous = AppState.currentState;
  startUpdateCheck({
    runningVersion,
    fetchManifest,
    now: () => Date.now(),
    onForeground: (callback) => {
      AppState.addEventListener("change", (next) => {
        if (next === "active" && previous !== "active") callback();
        previous = next;
      });
    },
    log,
  });
}

export { updateStore, type AvailableUpdate } from "./updateCheck";


/** Initial iOS distribution uses TestFlight; change this when App Store ships. */
export async function openAppUpdates(): Promise<void> {
  if (Platform.OS === "ios") {
    try { await Linking.openURL("itms-beta://"); }
    catch { await Linking.openURL("https://apps.apple.com/app/testflight/id899247664"); }
  } else {
    await openDesktopUpdates();
  }
}

/** Desktop installs the matching helper; both update together from GitHub. */
export function openDesktopUpdates(): Promise<void> {
  return Linking.openURL("https://github.com/gal064/muxflow/releases/latest");
}
