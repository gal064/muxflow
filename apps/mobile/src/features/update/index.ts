// Installs the update check once, from wireApp.

import Constants from "expo-constants";
import { AppState } from "react-native";

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
  if (started) return;
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
