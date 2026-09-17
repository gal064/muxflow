import { recordIncident } from "../../diagnostics/incidents";

/**
 * Watches the one thing that turns a shared-atlas page merge into pixels the
 * user can photograph: a renderer whose baked texture coordinates are older
 * than the atlas they point into.
 *
 * The atlas is shared by every terminal with the same font config, and merging
 * its pages rewrites the texture coordinates of every glyph already rasterised
 * into it (`TextureAtlas._mergePages`). Our patch gives the atlas a monotonic
 * invalidation count and each `GlyphRenderer` the count it last rebuilt for, so
 * "this pane is drawing from coordinates that no longer exist" is a plain
 * numeric comparison — and a pane that stays behind across two samples is a
 * pane where no frame ran at all, which is exactly the state that leaves
 * scattered glyphs on screen with no new output to wash them away.
 *
 * This is instrumentation, not a guard. It reads private fields of a
 * dependency through optional chaining and reports nothing at all when the
 * shapes it expects are gone, so an addon upgrade that renames them costs the
 * journal a signal and costs the app nothing.
 */

/** How often every registered pane is compared against its atlas. */
const SAMPLE_INTERVAL_MS = 5_000;
/**
 * Two consecutive stale samples, not one: a single sample can catch a pane in
 * the moment between the merge and the repaint that answers it, which is the
 * healthy path. Staying behind for a whole interval means no frame is coming.
 */
const STALE_SAMPLES_REQUIRED = 2;
/** Ceiling on the plain invalidation-rate line, so this costs one line a minute. */
const INVALIDATION_REPORT_INTERVAL_MS = 60_000;

/** The private shape this probe reads. Every step of it is optional. */
interface AtlasProbeSubject {
  _renderer?: {
    _charAtlas?: { _requestClearModel?: unknown };
    _glyphRenderer?: { value?: { _atlasClearModelVersion?: unknown } };
  };
}

interface Registration {
  paneId: string | undefined;
  subject: AtlasProbeSubject;
  /** Consecutive samples this pane has been behind its atlas. */
  staleSamples: number;
  /** The atlas count of the episode already reported, so one episode is one line. */
  reportedFor?: number;
}

interface Reading {
  atlasCount: number;
  rendererVersion: number;
}

export interface AtlasProbeTimers {
  setInterval: (handler: () => void, ms: number) => unknown;
  clearInterval: (handle: unknown) => void;
  now: () => number;
}

const registrations = new Set<Registration>();
let timers: AtlasProbeTimers = {
  setInterval: (handler, ms) => globalThis.setInterval(handler, ms),
  clearInterval: (handle) => globalThis.clearInterval(handle as ReturnType<typeof setInterval>),
  now: () => Date.now(),
};
let handle: unknown;
let lastReportedCount: number | undefined;
let lastReportAt = 0;

function read(subject: AtlasProbeSubject): Reading | undefined {
  const atlasCount = subject?._renderer?._charAtlas?._requestClearModel;
  const rendererVersion = subject?._renderer?._glyphRenderer?.value?._atlasClearModelVersion;
  if (typeof atlasCount !== "number" || typeof rendererVersion !== "number") return undefined;
  return { atlasCount, rendererVersion };
}

function sample(): void {
  let highestCount: number | undefined;
  for (const registration of registrations) {
    const reading = read(registration.subject);
    if (!reading) continue;
    const { atlasCount, rendererVersion } = reading;
    if (highestCount === undefined || atlasCount > highestCount) highestCount = atlasCount;

    // `-1` is the value a renderer carries before its first frame, and before
    // its first frame it has nothing baked to be wrong — `beginFrame` rebuilds
    // it wholesale whenever it does draw. A hidden pane sitting at `-1` is
    // healthy, and reporting it would bury the panes that are not.
    if (rendererVersion < 0 || rendererVersion >= atlasCount) {
      registration.staleSamples = 0;
      registration.reportedFor = undefined;
      continue;
    }
    registration.staleSamples++;
    if (registration.staleSamples < STALE_SAMPLES_REQUIRED) continue;
    if (registration.reportedFor === atlasCount) continue;
    registration.reportedFor = atlasCount;
    recordIncident("render.atlasStale", {
      paneId: registration.paneId,
      atlasCount,
      rendererVersion,
      staleForMs: registration.staleSamples * SAMPLE_INTERVAL_MS,
    });
  }

  if (highestCount === undefined) return;
  if (lastReportedCount === undefined) {
    lastReportedCount = highestCount;
    return;
  }
  const delta = highestCount - lastReportedCount;
  if (delta <= 0) return;
  const at = timers.now();
  if (at - lastReportAt < INVALIDATION_REPORT_INTERVAL_MS) return;
  lastReportAt = at;
  lastReportedCount = highestCount;
  recordIncident("render.atlasInvalidations", { count: highestCount, delta });
}

/**
 * Starts watching one pane's WebGL addon, and returns the call that stops it.
 * The single interval starts with the first pane and stops with the last, so a
 * build with no GPU renderer anywhere never arms a timer.
 */
export function watchAtlasStaleness(paneId: string | undefined, addon: unknown): () => void {
  const registration: Registration = { paneId, subject: addon as AtlasProbeSubject, staleSamples: 0 };
  registrations.add(registration);
  if (handle === undefined) handle = timers.setInterval(sample, SAMPLE_INTERVAL_MS);
  return () => {
    registrations.delete(registration);
    if (registrations.size > 0 || handle === undefined) return;
    timers.clearInterval(handle);
    handle = undefined;
  };
}

/**
 * The shared atlas's invalidation count as one pane's addon currently reads it,
 * or `undefined` when that pane has no registered addon or the addon no longer
 * has the shape this reads.
 *
 * Sampled either side of a write by `paintTailProbe`: a count that moved across
 * a slow paint is the atlas rebuilding mid-frame, which is one of the three
 * things a slow paint can be. Same contract as the rest of this module — a
 * private field read through optional chaining, silent when it is gone.
 */
export function readAtlasInvalidationCount(paneId: string): number | undefined {
  for (const registration of registrations) {
    if (registration.paneId !== paneId) continue;
    const count = registration.subject?._renderer?._charAtlas?._requestClearModel;
    if (typeof count === "number") return count;
  }
  return undefined;
}

/** Test seam. Resets the module to the state a fresh app launch starts in. */
export function __resetAtlasProbeForTests(override?: Partial<AtlasProbeTimers>): void {
  if (handle !== undefined) timers.clearInterval(handle);
  handle = undefined;
  registrations.clear();
  lastReportedCount = undefined;
  lastReportAt = 0;
  timers = {
    setInterval: (h, ms) => globalThis.setInterval(h, ms),
    clearInterval: (h) => globalThis.clearInterval(h as ReturnType<typeof setInterval>),
    now: () => Date.now(),
    ...override,
  };
}

/** Test seam. Runs one sampling pass without waiting for the interval. */
export function __sampleAtlasProbeForTests(): void {
  sample();
}
