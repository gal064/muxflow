import { useRef } from "react";

/** One host's contribution: what it depends on, and how to build it when that changes. */
export interface PerHostEntry<Value> {
  key: string;
  deps: readonly unknown[];
  build(): Value;
}

/**
 * `useMemo`, once per host, for a list of hosts that changes length.
 *
 * Hooks cannot be called in a loop, and one memo over every host's inputs
 * would rebuild every host's rows when any host's snapshot arrived — which
 * with three hosts relaying topology is most renders. Each key keeps its own
 * dependency list instead; only the entries whose deps changed are rebuilt,
 * and the map itself is the same object until some entry, or the key order,
 * changes, so a memo downstream can key on it.
 *
 * The cache is a ref mutated during render, on purpose: an entry is a pure
 * function of its deps, so a render React discards can only leave behind a
 * value the next render would have built anyway.
 */
export function usePerHostMemo<Value>(entries: readonly PerHostEntry<Value>[]): ReadonlyMap<string, Value> {
  const cache = useRef(new Map<string, { deps: readonly unknown[]; value: Value }>());
  const result = useRef<ReadonlyMap<string, Value>>(new Map());
  const next = new Map<string, Value>();
  let changed = false;
  for (const entry of entries) {
    const cached = cache.current.get(entry.key);
    if (cached && sameDeps(cached.deps, entry.deps)) {
      next.set(entry.key, cached.value);
      continue;
    }
    const value = entry.build();
    cache.current.set(entry.key, { deps: entry.deps, value });
    next.set(entry.key, value);
    changed = true;
  }
  for (const key of [...cache.current.keys()]) {
    if (next.has(key)) continue;
    cache.current.delete(key);
    changed = true;
  }
  if (!changed) {
    const previous = [...result.current.keys()];
    const current = [...next.keys()];
    changed = previous.length !== current.length || current.some((key, index) => key !== previous[index]);
  }
  if (changed) result.current = next;
  return result.current;
}

function sameDeps(left: readonly unknown[], right: readonly unknown[]): boolean {
  return left.length === right.length && left.every((value, index) => Object.is(value, right[index]));
}
