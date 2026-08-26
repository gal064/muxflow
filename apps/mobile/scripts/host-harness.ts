// Spawns the real muxflow-host (`bridge --stdio`) against a scratch runtime
// directory and a private tmux server, and wraps its stdio as a Transport.
// Mirrors tests/integration/protocol/protocol-driver's local transport so the
// user's real tmux server and muxflow daemon are never touched.

import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import path from "node:path";
import type { Transport, TransportClose } from "../src/protocol/Transport";

export const REPO_ROOT = path.resolve(import.meta.dirname, "../../..");

export interface LiveHostAvailability {
  available: boolean;
  reason?: string;
}

/** cargo and tmux must both be on PATH; otherwise the live test is skipped. */
export function liveHostAvailability(): LiveHostAvailability {
  for (const [program, args] of [["cargo", ["--version"]], ["tmux", ["-V"]]] as const) {
    const probe = spawnSync(program, args, { encoding: "utf8" });
    if (probe.error || probe.status !== 0) return { available: false, reason: `${program} is not available` };
  }
  return { available: true };
}

export function hostBinaryPath(): string {
  if (process.env.MUXFLOW_HOST_BINARY) return process.env.MUXFLOW_HOST_BINARY;
  const target = process.env.CARGO_TARGET_DIR ?? path.join(REPO_ROOT, "target");
  return path.join(target, "debug", "muxflow-host");
}

/** `cargo build -p muxflow-host` (apps/host/Cargo.toml names the crate `muxflow-host`). */
export function buildHost(): string {
  const binary = hostBinaryPath();
  if (process.env.MUXFLOW_HOST_BINARY && existsSync(binary)) return binary;
  const build = spawnSync("cargo", ["build", "-p", "muxflow-host"], { cwd: REPO_ROOT, encoding: "utf8" });
  if (build.status !== 0) throw new Error(`cargo build -p muxflow-host failed:\n${build.stderr}`);
  if (!existsSync(binary)) throw new Error(`host binary missing after build: ${binary}`);
  return binary;
}

export interface HostHarness {
  /** The first bridge process, for the control connection. */
  transport: Transport;
  /** Spawns another `bridge --stdio` against the same daemon (a bulk lane). */
  dial(): Transport;
  /** The tmux session id ("$N") of the pre-created `primary` session. */
  primarySessionId: string;
  /** Working directory of the primary pane; a scratch directory the test may populate. */
  workDir: string;
  tmux(args: string[]): string;
  /** Lines the helper wrote to stderr. */
  stderr: string[];
  stop(): Promise<void>;
}

class ChildTransport implements Transport {
  private dataListener: ((chunk: Uint8Array) => void) | undefined;
  private closedListener: ((close: TransportClose) => void) | undefined;
  private closed = false;

  constructor(private readonly child: ChildProcess) {
    child.stdout!.on("data", (chunk: Buffer) => this.dataListener?.(new Uint8Array(chunk)));
    child.on("exit", (code, signal) => {
      if (this.closed) return;
      this.closed = true;
      this.closedListener?.({
        reason: "exited",
        ...(code === null ? {} : { exitCode: code }),
        ...(signal ? { message: `killed by ${signal}` } : {}),
      });
    });
  }

  write(bytes: Uint8Array): void {
    if (this.closed) throw new Error("bridge transport is closed");
    this.child.stdin!.write(bytes);
  }

  onData(listener: (chunk: Uint8Array) => void): void {
    this.dataListener = listener;
  }

  onClosed(listener: (close: TransportClose) => void): void {
    this.closedListener = listener;
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    // Stdin closing is how the bridge learns the client is gone (apps/host/src/bridge.rs).
    this.child.stdin!.end();
  }
}

export async function startHostHarness(): Promise<HostHarness> {
  const binary = buildHost();
  // Under /tmp on purpose: the daemon's Unix socket lives here and socket
  // paths are limited to ~104 bytes, which a deep scratch path exceeds.
  const runtime = mkdtempSync("/tmp/mxm-");
  const workDir = mkdtempSync("/tmp/mxm-work-");
  const socket = `mxm-${process.pid}-${path.basename(runtime).slice(4)}`;
  const env = { ...process.env, ADE_HOST_RUNTIME_DIR: runtime, ADE_TMUX_SOCKET_NAME: socket };
  const tmux = (args: string[]): string => {
    const result = spawnSync("tmux", ["-L", socket, ...args], { encoding: "utf8" });
    if (result.status !== 0) throw new Error(`tmux ${args.join(" ")} failed: ${result.stderr.trim()}`);
    return result.stdout.trim();
  };
  // A private server with no user configuration and a quiet shell.
  tmux(["-f", "/dev/null", "new-session", "-d", "-s", "primary", "-x", "80", "-y", "24", "-c", workDir, "exec bash --norc --noprofile"]);
  const primarySessionId = tmux(["display-message", "-p", "-t", "primary", "#{session_id}"]);

  const stderr: string[] = [];
  const children: Array<{ child: ChildProcess; transport: ChildTransport }> = [];
  const dial = (): Transport => {
    const child = spawn(binary, ["bridge", "--stdio"], { env, stdio: ["pipe", "pipe", "pipe"] });
    child.stderr!.on("data", (chunk: Buffer) => {
      for (const line of chunk.toString("utf8").split("\n")) if (line.trim()) stderr.push(line);
    });
    const transport = new ChildTransport(child);
    children.push({ child, transport });
    return transport;
  };
  const transport = dial();

  let stopped = false;
  const stop = async (): Promise<void> => {
    if (stopped) return;
    stopped = true;
    for (const { child, transport: bridge } of children) {
      bridge.close();
      await new Promise<void>((resolve) => {
        if (child.exitCode !== null) return resolve();
        const timer = setTimeout(() => {
          child.kill("SIGKILL");
          resolve();
        }, 5000);
        child.once("exit", () => {
          clearTimeout(timer);
          resolve();
        });
      });
    }
    spawnSync(binary, ["daemon-stop"], { env, encoding: "utf8" });
    spawnSync("tmux", ["-L", socket, "kill-server"], { encoding: "utf8" });
    rmSync(runtime, { recursive: true, force: true });
    rmSync(workDir, { recursive: true, force: true });
  };

  return { transport, dial, primarySessionId, workDir, tmux, stderr, stop };
}
