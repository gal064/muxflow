// Whether a newer Muxflow release has been published. There is no auto-update:
// this reads the small manifest the release workflow attaches to every GitHub
// release so the home app bar can say a newer version exists and link to its
// release page. `releases/latest` never resolves to a draft or a pre-release,
// so a version appears here only once it has been published. The desktop runs
// the same check (apps/desktop/src-tauri/src/update_check.rs).

import { createStore, type StoreApi } from "zustand/vanilla";

export const MANIFEST_URL = "https://github.com/gal064/muxflow/releases/latest/download/latest.json";
/** The only links the pill may open, whatever the manifest says. */
export const RELEASE_PAGE_PREFIX = "https://github.com/gal064/muxflow/releases/";
/** At launch, then on return to the foreground once a day has passed. */
export const UPDATE_CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

export interface AvailableUpdate {
  version: string;
  url: string;
}

type Version = readonly [number, number, number];

/** A strict `X.Y.Z`: the only shape release/set-version.sh writes. */
export function parseVersion(value: unknown): Version | undefined {
  if (typeof value !== "string") return undefined;
  const match = /^(\d+)\.(\d+)\.(\d+)$/u.exec(value);
  return match ? [Number(match[1]), Number(match[2]), Number(match[3])] : undefined;
}

function newer([major, minor, patch]: Version, [runningMajor, runningMinor, runningPatch]: Version): boolean {
  if (major !== runningMajor) return major > runningMajor;
  if (minor !== runningMinor) return minor > runningMinor;
  return patch > runningPatch;
}

/**
 * The update to offer, or null when the running app is the newest. Throws for
 * a manifest it cannot trust, which the caller treats like a failed fetch.
 */
export function evaluateManifest(runningVersion: string, manifest: unknown): AvailableUpdate | null {
  const running = parseVersion(runningVersion);
  if (!running) throw new Error(`unreadable running version: ${runningVersion}`);
  const fields = typeof manifest === "object" && manifest !== null ? (manifest as Record<string, unknown>) : {};
  const published = parseVersion(fields.version);
  if (!published) throw new Error("unreadable update version");
  if (typeof fields.url !== "string" || !fields.url.startsWith(RELEASE_PAGE_PREFIX)) {
    throw new Error("update manifest points outside the Muxflow releases");
  }
  return newer(published, running) ? { version: fields.version as string, url: fields.url } : null;
}

export interface UpdateState {
  update: AvailableUpdate | null;
}

export const updateStore: StoreApi<UpdateState> = createStore<UpdateState>(() => ({ update: null }));

export interface UpdateCheckDeps {
  runningVersion: string;
  fetchManifest(): Promise<unknown>;
  now(): number;
  /** Calls back whenever the app returns to the foreground. */
  onForeground(callback: () => void): void;
  log(message: string): void;
}

/**
 * Checks now, then again on each return to the foreground once a day has
 * passed since the last attempt. A failed check leaves the last answer
 * standing: it is the next successful check that decides, never a network
 * error.
 */
export function startUpdateCheck(deps: UpdateCheckDeps, store: StoreApi<UpdateState> = updateStore): void {
  let lastAttempt = Number.NEGATIVE_INFINITY;
  const run = () => {
    if (deps.now() - lastAttempt < UPDATE_CHECK_INTERVAL_MS) return;
    lastAttempt = deps.now();
    deps
      .fetchManifest()
      .then((manifest) => store.setState({ update: evaluateManifest(deps.runningVersion, manifest) }))
      .catch((error: unknown) => deps.log(`update.check.failed ${error instanceof Error ? error.message : String(error)}`));
  };
  run();
  deps.onForeground(run);
}
