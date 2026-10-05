import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ConnectionSpec } from "../../app/types";

export type ForwardState = "starting" | "active" | "failed";

/** One local forward, owned by the native side for this app session only. */
export interface PortForward {
  profileId: string;
  remotePort: number;
  localPort: number;
  state: ForwardState;
  error?: string | null;
}

export interface DetectedPort {
  port: number;
  /** Known only for the user's own processes on the host. */
  process?: string | null;
}

export const portsApi = {
  forward: (connection: ConnectionSpec, remotePort: number, localPort: number) =>
    invoke<void>("ports_forward", { connection, remotePort, localPort }),
  stop: (profileId: string, remotePort: number) => invoke<void>("ports_stop", { profileId, remotePort }),
  stopHost: (profileId: string) => invoke<void>("ports_stop_host", { profileId }),
  list: () => invoke<PortForward[]>("ports_list"),
  detect: (connection: ConnectionSpec) => invoke<DetectedPort[]>("ports_detect", { connection }),
  onChanged: (handler: () => void): Promise<UnlistenFn> => listen("port-forwards-changed", handler),
};

/** A TCP port typed by a person: digits only, 1–65535. */
export function parsePort(text: string): number | undefined {
  const trimmed = text.trim();
  if (!/^\d{1,5}$/u.test(trimmed)) return undefined;
  const port = Number(trimmed);
  return port >= 1 && port <= 65535 ? port : undefined;
}
