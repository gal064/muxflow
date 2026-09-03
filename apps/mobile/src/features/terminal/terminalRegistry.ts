// Routes the connection's terminal events (§7.4) to whichever
// TerminalController is mounted for a pane, and fans reconnects out to them
// (§7.6 step 5). One instance per app; the connection manager hands it to
// HostConnection as its `terminals` sink.

import type { TerminalSink } from "../../protocol/HostConnection";

export interface RegisteredTerminal {
  paneId: string;
  sessionId: string;
  seed(bytes: Uint8Array, generation: bigint): void;
  output(bytes: Uint8Array, generation: bigint): void;
  history(bytes: Uint8Array, historySize: number, sizeKnown: boolean): void;
  exit(detail: string): void;
  onConnected(): void;
}

export class TerminalRegistry implements TerminalSink {
  private readonly terminals = new Map<string, RegisteredTerminal>();

  register(terminal: RegisteredTerminal): () => void {
    this.terminals.set(terminal.paneId, terminal);
    return () => {
      if (this.terminals.get(terminal.paneId) === terminal) this.terminals.delete(terminal.paneId);
    };
  }

  get(paneId: string): RegisteredTerminal | undefined {
    return this.terminals.get(paneId);
  }

  seed(paneId: string, bytes: Uint8Array, generation: bigint): void {
    this.terminals.get(paneId)?.seed(bytes, generation);
  }

  output(paneId: string, bytes: Uint8Array, generation: bigint): void {
    this.terminals.get(paneId)?.output(bytes, generation);
  }

  history(paneId: string, bytes: Uint8Array, historySize: number, sizeKnown: boolean): void {
    this.terminals.get(paneId)?.history(bytes, historySize, sizeKnown);
  }

  /** TERMINAL_EXIT is scoped by session id ("$N"): every pane of that session. */
  exit(sessionId: string, detail: string): void {
    for (const terminal of this.terminals.values()) {
      if (terminal.sessionId === sessionId) terminal.exit(detail);
    }
  }

  onConnected(): void {
    for (const terminal of this.terminals.values()) terminal.onConnected();
  }
}

export const terminalRegistry = new TerminalRegistry();
