import { execFileSync } from "node:child_process";
import type { TerminalEvent } from "../api";
import { ownTerminalBytes } from "../TerminalBytes";
import type { TerminalEventHub } from "../TerminalEventHub";

// Resolve Cargo's actual artifact, including when CARGO_TARGET_DIR is set.
// Compile before any fake timers are installed.
const build = execFileSync("cargo", ["build", "-q", "-p", "tmux-control", "--example", "terminal_contract", "--message-format=json"], { encoding: "utf8" });
const executable = build.split("\n").filter(Boolean).map((line) => JSON.parse(line))
  .find((artifact) => artifact.target?.name === "terminal_contract" && artifact.executable)?.executable as string;
if (!executable) throw new Error("cargo did not produce terminal_contract");

type Checkpoint = { terminalEpoch: number; outputGeneration: number };
type Action = { kind: string; paneId?: string; text?: string; holdsSnapshot?: boolean; state?: string } & Partial<Checkpoint>;
type WireEvent = { kind: string; data?: number[]; rawTail?: number[] };

/** A transport/capture harness around Rust's actual PaneResourceStore.
 * Actions replay in a fresh process so tests need no long-lived child or IPC.
 * Host service routing and tmux capture are outside this test's scope.
 */
export class RustPaneHost {
  #actions: Action[] = [];
  #eventCount = 0;
  #pending: TerminalEvent[] = [];
  #publishing = false;
  generation = 0;
  revealTailBound = 0;
  readonly emitted: string[] = [];

  constructor(readonly hub: TerminalEventHub) { this.#run(); }

  #run(action?: Action): void {
    if (action) this.#actions.push(action);
    const result = JSON.parse(execFileSync(executable, { input: JSON.stringify(this.#actions), encoding: "utf8", maxBuffer: 16 * 1024 * 1024 })) as {
      events: WireEvent[]; generation: number; revealTailBound: number; error: string | null;
    };
    this.generation = result.generation;
    this.revealTailBound = result.revealTailBound;
    const events = result.events.slice(this.#eventCount);
    this.#eventCount = result.events.length;
    for (const raw of events) {
      const event = { ...raw,
        ...(raw.data ? { data: ownTerminalBytes(Uint8Array.from(raw.data)) } : {}),
        ...(raw.rawTail ? { rawTail: ownTerminalBytes(Uint8Array.from(raw.rawTail)) } : {}),
      } as TerminalEvent;
      if (event.kind === "seed" || event.kind === "output") this.emitted.push(`${event.kind}@${event.generation}`);
      if (event.kind === "paneResource" && action?.kind === "reveal") {
        this.emitted.push(`reveal(resume=${event.resumeFromRenderer},bytes=${event.rawTail.byteLength})`);
      }
      this.#pending.push(event);
    }
    // A hub callback can synchronously request another seed. Keep its events
    // behind the already-produced events, just as an ordered transport does.
    if (!this.#publishing) {
      this.#publishing = true;
      try {
        while (this.#pending.length) this.hub.publish(this.#pending.shift()!);
      } finally {
        this.#publishing = false;
      }
    }
    if (result.error) throw new Error(result.error);
  }

  announceEpoch(): void { this.#run({ kind: "epoch", terminalEpoch: 7 }); }
  output(paneId: string, text: string): number { this.#run({ kind: "output", paneId, text }); return this.generation; }
  capture(paneId: string): void { this.#run({ kind: "capture", paneId }); }
  seedOnRequest(paneId: string): void { this.#run({ kind: "seed", paneId }); }
  hide(paneId: string, checkpoint: Checkpoint): void { this.#run({ kind: "hide", paneId, ...checkpoint }); }
  reveal(paneId: string, holdsSnapshot: boolean, checkpoint: Checkpoint): void { this.#run({ kind: "reveal", paneId, holdsSnapshot, ...checkpoint }); }
  publishUnusableResource(paneId: string, state: string): void { this.#run({ kind: "unusable", paneId, state }); }
}
