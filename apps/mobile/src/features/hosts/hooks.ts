// React bindings for the vanilla zustand stores this feature reads.

import { useStore } from "zustand";

import { diagnosticsStore, type DiagnosticsActions, type DiagnosticsState } from "./connectionDiagnostics";
import { hostKeyStore, type HostKeyActions, type HostKeyState } from "./hostKeyStore";
import { logStore, type LogActions, type LogState } from "../../session/log";
import { hostsStore, type HostsActions, type HostsState } from "../../store/hostsStore";
import { sessionStore, type SessionActions, type SessionState } from "../../store/sessionStore";

export function useHosts<T>(selector: (state: HostsState & HostsActions) => T): T {
  return useStore(hostsStore, selector);
}

export function useSession<T>(selector: (state: SessionState & SessionActions) => T): T {
  return useStore(sessionStore, selector);
}

export function useDiagnostics<T>(selector: (state: DiagnosticsState & DiagnosticsActions) => T): T {
  return useStore(diagnosticsStore, selector);
}

export function useHostKeyPrompt<T>(selector: (state: HostKeyState & HostKeyActions) => T): T {
  return useStore(hostKeyStore, selector);
}

export function useLog<T>(selector: (state: LogState & LogActions) => T): T {
  return useStore(logStore, selector);
}
