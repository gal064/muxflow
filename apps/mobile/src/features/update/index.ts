// Installs the update check once, from wireApp.

import Constants from "expo-constants";
import { File, Paths } from "expo-file-system";
import { startActivityAsync } from "expo-intent-launcher";
import { AppState, Linking, Platform } from "react-native";

import { toast } from "../../session/connectionManager";
import { log } from "../../session/log";
import { apkUrl, LATEST_RELEASE_URL, MANIFEST_URL, startUpdateCheck, updateStore } from "./updateCheck";

const TIMEOUT_MS = 10_000;
const FLAG_GRANT_READ_URI_PERMISSION = 1;

const downloadedApk = () => new File(Paths.cache, "muxflow-update.apk");

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
  // The installer ends this process, so a finished update is cleaned up here.
  if (downloadedApk().exists) downloadedApk().delete();
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
    await installAppUpdate();
  }
}

/**
 * Downloads the offered APK and hands it to Android's installer, which asks the
 * user to confirm and verifies the APK's signature. With nothing offered, or on
 * any failure, the release page is the way to update.
 */
export async function installAppUpdate(): Promise<void> {
  const { update, downloading } = updateStore.getState();
  if (downloading) return;
  if (!update) return openDesktopUpdates();
  updateStore.setState({ downloading: true });
  // Android cuts the download about five seconds after the app leaves the foreground.
  toast("Keep Muxflow open while the update downloads.");
  try {
    const apk = await File.downloadFileAsync(apkUrl(update.version), downloadedApk(), { idempotent: true });
    await startActivityAsync("android.intent.action.VIEW", {
      data: apk.contentUri,
      flags: FLAG_GRANT_READ_URI_PERMISSION,
      type: "application/vnd.android.package-archive",
    });
  } catch (error) {
    log(`update.install.failed ${error instanceof Error ? error.message : String(error)}`);
    await openDesktopUpdates();
  } finally {
    updateStore.setState({ downloading: false });
  }
}

/** Desktop installs the matching helper; both update together from GitHub. */
export function openDesktopUpdates(): Promise<void> {
  return Linking.openURL(LATEST_RELEASE_URL);
}
