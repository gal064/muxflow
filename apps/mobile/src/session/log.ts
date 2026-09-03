// `[muxflow]`-prefixed logging with the §12 ring buffer of the last 500 lines.

const RING_SIZE = 500;
const ring: string[] = [];

export function log(line: string): void {
  const stamped = `${new Date().toISOString().slice(11, 23)} ${line.startsWith("[muxflow]") ? line : `[muxflow] ${line}`}`;
  ring.push(stamped);
  if (ring.length > RING_SIZE) ring.splice(0, ring.length - RING_SIZE);
  console.log(stamped);
}

export function logLines(): readonly string[] {
  return ring;
}
