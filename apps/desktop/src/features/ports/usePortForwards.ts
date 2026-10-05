import { useEffect, useState } from "react";
import { portsApi, type PortForward } from "./api";

/**
 * Every forward the native side owns, kept current by its change event.
 *
 * Mounted with the app rather than with the Ports tab, because it also ends
 * the forwards of a host that is no longer shown — whichever tab is open.
 * Not before the profiles are loaded, though: until then no host is shown,
 * and a webview reload would otherwise end every forward the native side kept.
 */
export function usePortForwards(shownProfileIds: readonly string[], profilesLoaded: boolean): PortForward[] {
  const [forwards, setForwards] = useState<PortForward[]>([]);

  useEffect(() => {
    let live = true;
    const refresh = () => {
      portsApi.list().then((next) => { if (live) setForwards(next ?? []); }).catch(() => undefined);
    };
    refresh();
    const unlisten = portsApi.onChanged(refresh);
    return () => {
      live = false;
      void unlisten.then((stop) => stop()).catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    if (!profilesLoaded) return;
    const orphaned = new Set(forwards
      .map((forward) => forward.profileId)
      .filter((profileId) => !shownProfileIds.includes(profileId)));
    for (const profileId of orphaned) void portsApi.stopHost(profileId).catch(() => undefined);
  }, [forwards, profilesLoaded, shownProfileIds]);

  return forwards;
}
