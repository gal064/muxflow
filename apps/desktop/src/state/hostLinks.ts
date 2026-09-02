import type { SetStateAction } from "react";
import type { ConnectionSpec, HostProfile } from "../app/types";
import { resolveSelectedSession } from "../features/shell/model";
import { hostProfileId } from "../features/shell/types";
import { connectionReducer, initialHostState, type HostAction, type NormalizedHostState } from "./connectionReducer";

/** One shown host: the bridge the renderer keeps to it, and what it has said so far. */
export interface HostLink {
  profileId: string;
  connection: ConnectionSpec;
  /** Renderer incarnation; a bump restarts this host's bridge. Never repeats for a profile in one process. */
  connectionEpoch: number;
  clientId?: string;
  /** Native GenerationEpoch of the live bridge; 0 before the first one. */
  terminalEpoch: number;
  hostState: NormalizedHostState;
  /** Remembered per host so switching back lands where the user left. */
  activeSessionId?: string;
  activeWindowId?: string;
  detail: string;
}

export interface HostLinksState {
  order: string[];
  byProfileId: Record<string, HostLink>;
  /**
   * The next connection epoch to hand out, to any link. One counter for every
   * host is what keeps an epoch from repeating for a profile that is shown,
   * hidden and shown again: the re-added link cannot start over from zero and
   * collide with a scope token the previous incarnation minted.
   */
  nextEpoch: number;
}

/** A host to keep a link to: the profile id and the connection to open it with. */
export interface ShownHostProfile {
  profileId: string;
  connection: ConnectionSpec;
}

export type HostLinksAction =
  /** Makes the link set exactly `hosts`, in that order; untouched links keep their state. */
  | { type: "sync"; hosts: readonly ShownHostProfile[] }
  | { type: "add"; profileId: string; connection: ConnectionSpec }
  | { type: "remove"; profileId: string }
  /** A new renderer incarnation for one host: its bridge restarts. */
  | { type: "reconnect"; profileId: string }
  | { type: "reconnectAll" }
  | { type: "client"; profileId: string; clientId: string | undefined }
  | { type: "terminalEpoch"; profileId: string; terminalEpoch: number }
  | { type: "detail"; profileId: string; detail: string }
  /**
   * A host-state reduction for one link. A snapshot that describes sessions
   * also re-resolves the link's remembered session against them, so the
   * memory survives a rename and falls back to the first session when the
   * remembered one is gone — for every host, active or not, because that is
   * the session activating the host will land on.
   */
  | { type: "host"; profileId: string; action: HostAction }
  | { type: "activeSession"; profileId: string; sessionId: SetStateAction<string | undefined> }
  | { type: "activeWindow"; profileId: string; windowId: SetStateAction<string | undefined> };

export const initialHostLinksState: HostLinksState = { order: [], byProfileId: {}, nextEpoch: 0 };

export function emptyHostLink(profileId: string, connection: ConnectionSpec, connectionEpoch: number): HostLink {
  return { profileId, connection, connectionEpoch, terminalEpoch: 0, hostState: initialHostState, detail: "" };
}

function sameConnection(left: ConnectionSpec, right: ConnectionSpec): boolean {
  return left === right || JSON.stringify(left) === JSON.stringify(right);
}

function resolve<T>(update: SetStateAction<T>, current: T): T {
  return typeof update === "function" ? (update as (current: T) => T)(current) : update;
}

function replaceLink(state: HostLinksState, link: HostLink): HostLinksState {
  return { ...state, byProfileId: { ...state.byProfileId, [link.profileId]: link } };
}

/** Applies `change` to one link; the state is untouched when the link is unknown or nothing changed. */
function updateLink(
  state: HostLinksState,
  profileId: string,
  change: (link: HostLink) => Partial<HostLink>,
): HostLinksState {
  const link = state.byProfileId[profileId];
  if (!link) return state;
  const patch = change(link);
  const changed = (Object.keys(patch) as (keyof HostLink)[]).some((key) => patch[key] !== link[key]);
  return changed ? replaceLink(state, { ...link, ...patch }) : state;
}

function addLink(state: HostLinksState, profileId: string, connection: ConnectionSpec): HostLinksState {
  if (state.byProfileId[profileId]) return state;
  return {
    order: [...state.order, profileId],
    byProfileId: { ...state.byProfileId, [profileId]: emptyHostLink(profileId, connection, state.nextEpoch) },
    nextEpoch: state.nextEpoch + 1,
  };
}

function removeLink(state: HostLinksState, profileId: string): HostLinksState {
  if (!state.byProfileId[profileId]) return state;
  const { [profileId]: _removed, ...byProfileId } = state.byProfileId;
  return { ...state, order: state.order.filter((id) => id !== profileId), byProfileId };
}

function bumpEpoch(state: HostLinksState, profileId: string): HostLinksState {
  const link = state.byProfileId[profileId];
  if (!link) return state;
  return {
    ...replaceLink(state, { ...link, connectionEpoch: state.nextEpoch }),
    nextEpoch: state.nextEpoch + 1,
  };
}

/**
 * The link set made exactly `hosts`, in that order. Exported so a caller can
 * tell, before dispatching, whether the sync would change anything: the state
 * comes back untouched — the same object — when it would not.
 */
export function syncHostLinks(state: HostLinksState, hosts: readonly ShownHostProfile[]): HostLinksState {
  let next = state;
  const wanted = new Set(hosts.map((host) => host.profileId));
  for (const profileId of state.order) {
    if (!wanted.has(profileId)) next = removeLink(next, profileId);
  }
  for (const host of hosts) {
    next = addLink(next, host.profileId, host.connection);
    next = updateLink(next, host.profileId, (link) =>
      sameConnection(link.connection, host.connection) ? {} : { connection: host.connection });
  }
  const order = hosts.map((host) => host.profileId);
  const sameOrder = order.length === next.order.length && order.every((id, index) => id === next.order[index]);
  return sameOrder ? next : { ...next, order };
}

export function hostLinksReducer(state: HostLinksState, action: HostLinksAction): HostLinksState {
  switch (action.type) {
    case "sync":
      return syncHostLinks(state, action.hosts);
    case "add":
      return addLink(state, action.profileId, action.connection);
    case "remove":
      return removeLink(state, action.profileId);
    case "reconnect":
      return bumpEpoch(state, action.profileId);
    case "reconnectAll":
      return state.order.reduce(bumpEpoch, state);
    case "client":
      return updateLink(state, action.profileId, () => ({ clientId: action.clientId }));
    case "terminalEpoch":
      return updateLink(state, action.profileId, () => ({ terminalEpoch: action.terminalEpoch }));
    case "detail":
      return updateLink(state, action.profileId, () => ({ detail: action.detail }));
    case "host":
      return updateLink(state, action.profileId, (link) => {
        const hostState = connectionReducer(link.hostState, action.action);
        const described = action.action.type === "snapshot" ? action.action.snapshot : undefined;
        const activeSessionId = described
          ? resolveSelectedSession(
            described.sessions,
            link.activeSessionId,
            link.activeSessionId === undefined ? undefined : link.hostState.sessions[link.activeSessionId]?.name,
          )?.id
          : link.activeSessionId;
        return { hostState, activeSessionId };
      });
    case "activeSession":
      return updateLink(state, action.profileId, (link) => ({
        activeSessionId: resolve(action.sessionId, link.activeSessionId),
      }));
    case "activeWindow":
      return updateLink(state, action.profileId, (link) => ({
        activeWindowId: resolve(action.windowId, link.activeWindowId),
      }));
  }
}

/**
 * The hosts to keep links to: every saved profile the user checked, in
 * profile order, with the active host always among them — under the live
 * connection spec, which is the profile's own except while Settings is
 * connecting to an address it has just corrected.
 */
export function shownHostProfiles(profiles: readonly HostProfile[], active: ConnectionSpec): ShownHostProfile[] {
  const activeId = hostProfileId(active);
  const shown = profiles.flatMap((profile): ShownHostProfile[] => {
    if (profile.id === activeId) return [{ profileId: activeId, connection: active }];
    return profile.shown ? [{ profileId: profile.id, connection: profile.connection }] : [];
  });
  return shown.some((host) => host.profileId === activeId)
    ? shown
    : [{ profileId: activeId, connection: active }, ...shown];
}
