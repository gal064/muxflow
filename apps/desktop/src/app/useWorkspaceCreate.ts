import { useCallback, type MutableRefObject } from "react";
import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";
import { workspaceDefaultsFor } from "../features/shell/model";
import type { PersistedAppState } from "../features/shell/types";
import type { CreateSessionOptions } from "./useShellNavigation";

interface WorkspaceCreateOptions {
  appStateRef: MutableRefObject<PersistedAppState>;
  clientIdRef: MutableRefObject<string | undefined>;
  createSession(name: string, options?: CreateSessionOptions): void;
  currentHostProfileId: string;
  hostScopeRef: MutableRefObject<HostScopeToken>;
  sendInput(clientId: string, paneId: string, data: string): Promise<unknown>;
  setStatus(status: string): void;
}

/**
 * Creating a workspace with this host's configured defaults applied.
 *
 * The defaults are read when the create is submitted, not when this hook is
 * built: the command that starts a create opens a prompt, and the answer
 * arrives whenever the user gets around to it. Reading through the ref is what
 * makes "the directory configured now" the one that is used.
 *
 * The startup command is delivered from `onCreated`, and only from there. That
 * is the single point where the host has named the pane, the dispatcher has
 * already attached a control client to it, and the connection can still be
 * proved to be the one the create was issued on. Nothing records that it was
 * sent, because nothing may send it again: a reconnect, a restart, a replay or
 * a workspace reselection has no path back to this callback, and a new tab in
 * an existing workspace is not a create at all.
 *
 * Failures are kept apart. A create that the host refuses — a start directory
 * that is not there — fails as a create, through the action's own error
 * surfacing, and no workspace exists. A send that is refused on the way out of
 * the app is reported as what it is instead: the workspace exists, and only the
 * command did not go. Input is fire-and-forget past that point, as every
 * keystroke in this app is, so a host-side refusal surfaces the way one always
 * does — as a pane resnapshot, not as a rejection here.
 */
export function useWorkspaceCreate(options: WorkspaceCreateOptions) {
  const {
    appStateRef, clientIdRef, createSession, currentHostProfileId, hostScopeRef, sendInput, setStatus,
  } = options;
  return useCallback((name: string) => {
    const defaults = workspaceDefaultsFor(appStateRef.current, currentHostProfileId);
    const startupCommand = defaults.startupCommand;
    createSession(name, {
      directory: defaults.directory,
      // Read at submit time, like the defaults: a workspace created while the
      // list shows pinned only is born pinned, or it would drop out of the
      // sidebar on the first switch away.
      pinned: appStateRef.current.shell.pinnedOnly || undefined,
      // Built only when there is something to send: an unset command must not
      // leave a callback behind that reaches a pane with an empty line.
      onCreated: startupCommand
        ? (created, scope) => {
          const liveClientId = clientIdRef.current;
          if (!liveClientId || !sameHostConnection(scope, hostScopeRef.current)) return;
          // Shell text, sent verbatim: escaping it would turn the command the
          // user configured into a different one.
          void sendInput(liveClientId, created.paneId, `${startupCommand}\n`).catch((error) => {
            setStatus(`Workspace created, but the startup command could not be sent: ${String(error)}`);
          });
        }
        : undefined,
    });
  }, [appStateRef, clientIdRef, createSession, currentHostProfileId, hostScopeRef, sendInput, setStatus]);
}
