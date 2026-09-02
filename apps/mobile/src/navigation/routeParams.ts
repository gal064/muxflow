// Route parameters that survive expo-router exactly.
//
// expo-router (57.0.16) percent-decodes a route parameter twice on its way
// from `router.push` to `useLocalSearchParams`:
//
//   1. `build/link/href.js` `encodeParam` (line 73) / `createQueryParams`
//      (line 79) encode every param once with `encodeURIComponent`.
//   2. `build/fork/getStateFromPath.js` line 300 decodes a path segment with
//      `safelyDecodeURIComponent` (`getStateFromPath-forks.js` line 40), and
//      `parseQueryParams` (`getStateFromPath-forks.js` line 371) decodes a
//      query param through `URLSearchParams`.
//   3. `build/hooks/useLocalSearchParams.js` lines 34-40 decode every value
//      again with `decodeURIComponent`, swallowing the error when it throws.
//
// One encode, two decodes: any value that is itself a valid percent-escape
// after the first decode is decoded once more. tmux pane ids look like `%12`,
// so `%12` -> `%2512` -> `%12` -> U+0012 and the store lookup misses ("This
// terminal no longer exists"). `%1` only survives because the second decode
// throws on a lone `%1`. File paths can carry `%`, `+`, `#`, `?`, spaces and
// unicode, all of which are at the mercy of the same passes and of
// `URLSearchParams` (`+` -> space).
//
// The fix is an encoding whose output is a fixed point of every one of those
// passes: only `[A-Za-z0-9_-]` pass through, everything else becomes `~`
// followed by the four-hex-digit UTF-16 code unit. The output contains no
// `%`, `+`, `/`, `?`, `#`, `.` or whitespace, so `encodeURIComponent` leaves
// it alone (`~`, `-` and `_` are unreserved), `decodeURIComponent` and
// `URLSearchParams` leave it alone, and it can never be read as `.`/`..` or
// split into segments. However many decodes expo-router applies, the reader
// gets back exactly what the writer produced, and `fromRouteParam` is the
// only decode that actually changes anything.

const PASSTHROUGH = /[A-Za-z0-9_-]/;

/** Encodes a value for a `params` entry of a push, replace, `<Redirect>` or `<Link>` href. */
export function toRouteParam(value: string): string {
  let out = "";
  for (let i = 0; i < value.length; i += 1) {
    const ch = value[i] as string;
    out += PASSTHROUGH.test(ch) ? ch : `~${ch.charCodeAt(0).toString(16).toUpperCase().padStart(4, "0")}`;
  }
  return out;
}

/** Decodes a value read from `useLocalSearchParams`. Text the encoder never emits passes through unchanged. */
export function fromRouteParam(value: string): string;
export function fromRouteParam(value: string | undefined): string | undefined;
export function fromRouteParam(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  return value.replace(/~([0-9A-Fa-f]{4})/g, (_match, hex: string) => String.fromCharCode(parseInt(hex, 16)));
}
