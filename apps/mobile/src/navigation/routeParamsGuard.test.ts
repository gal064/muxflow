// Keeps every navigation site honest: a `paneId`, `sessionId`, `path` or
// `name` route param must be written with `toRouteParam` and read with
// `fromRouteParam` (see `routeParams.ts` for why). Regex per statement, on
// purpose: it only has to catch the obvious slip of passing a raw value.

import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

const ROOT = join(__dirname, "..", "..");
const KEYS = ["paneId", "sessionId", "path", "name"] as const;
const KEY_PATTERN = KEYS.join("|");

function sourceFiles(): string[] {
  const files: string[] = [];
  for (const dir of ["app", "src"]) {
    for (const entry of readdirSync(join(ROOT, dir), { recursive: true, withFileTypes: true })) {
      if (!entry.isFile()) continue;
      const file = join(entry.parentPath, entry.name);
      if (!/\.tsx?$/.test(file) || /\.test\.tsx?$/.test(file)) continue;
      if (dir === "src" || file.endsWith(".tsx")) files.push(file);
    }
  }
  return files.sort();
}

/** Every `params: { ... }` object literal (type literals, `key: string`, are skipped). */
function paramsLiterals(source: string): string[] {
  return [...source.matchAll(/\bparams:\s*\{([^{}]*)\}/g)]
    .map((match) => match[1] as string)
    .filter((body) => !/:\s*string\b/.test(body));
}

function unencodedWrites(source: string): string[] {
  const offenders: string[] = [];
  for (const body of paramsLiterals(source)) {
    for (const key of KEYS) {
      // The property `key` as shorthand (`{ paneId }`) or with its value (`paneId: <value>`).
      const property = body.match(new RegExp(`(?:^|,)\\s*${key}\\s*(?::\\s*([^,]*))?(?=,|$)`));
      if (property && !property[1]?.trimStart().startsWith("toRouteParam(")) {
        offenders.push(`params: {${body}}`);
        break;
      }
    }
  }
  return offenders;
}

function undecodedReads(source: string): string[] {
  const offenders: string[] = [];
  for (const match of source.matchAll(/const\s+(\{[^}]*\}|\w+)\s*=\s*useLocalSearchParams\b/g)) {
    const target = match[1] as string;
    if (target.startsWith("{")) {
      if (new RegExp(`\\b(${KEY_PATTERN})\\b`).test(target)) offenders.push(match[0]);
      continue;
    }
    for (const read of source.matchAll(new RegExp(`(fromRouteParam\\()?\\b${target}\\.(${KEY_PATTERN})\\b`, "g"))) {
      if (!read[1]) offenders.push(read[0]);
    }
  }
  return offenders;
}

describe("route param guard", () => {
  const files = sourceFiles();

  it("scans the app", () => {
    expect(files.some((file) => file.endsWith("app/terminal/[paneId].tsx"))).toBe(true);
    expect(files.some((file) => file.endsWith("src/features/notifications/taps.ts"))).toBe(true);
  });

  it("writes paneId, sessionId, path and name params through toRouteParam", () => {
    const findings = files.flatMap((file) => unencodedWrites(readFileSync(file, "utf8")).map((o) => `${relative(ROOT, file)}: ${o}`));
    expect(findings).toEqual([]);
  });

  it("reads paneId, sessionId, path and name params through fromRouteParam", () => {
    const findings = files.flatMap((file) => undecodedReads(readFileSync(file, "utf8")).map((o) => `${relative(ROOT, file)}: ${o}`));
    expect(findings).toEqual([]);
  });

  it("catches the slips it exists for", () => {
    expect(unencodedWrites(`router.push({ pathname: "/terminal/[paneId]", params: { paneId } });`)).toHaveLength(1);
    expect(unencodedWrites(`params: { paneId: toRouteParam(id), sessionId: sessionId ?? "" }`)).toHaveLength(1);
    expect(unencodedWrites(`params: { paneId: toRouteParam(id), name: toRouteParam(n) }`)).toHaveLength(0);
    expect(unencodedWrites(`interface R { params: { paneId: string } }`)).toHaveLength(0);
    expect(undecodedReads(`const { paneId } = useLocalSearchParams<{ paneId: string }>();`)).toHaveLength(1);
    expect(undecodedReads(`const p = useLocalSearchParams(); const a = p.paneId; const b = fromRouteParam(p.path);`)).toEqual(["p.paneId"]);
  });
});
