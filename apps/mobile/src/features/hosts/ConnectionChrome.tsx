import { router, useFocusEffect } from "expo-router";
import { useCallback, useEffect, useState } from "react";

import { ConnectionErrorScreen } from "./ConnectionErrorScreen";
import { ConnectionSheet } from "./ConnectionSheet";
import { ConnectionStrip } from "./ConnectionStrip";
import { describeConnectionFailure } from "./errorMatrix";
import { HostKeyDialog } from "./HostKeyDialog";
import { useDiagnostics, useSession } from "./hooks";
import { useOwnsGlobalModals } from "./primaryChrome";

export interface ConnectionChromeProps {
  /**
   * Set on the Hosts screen: a connection that failed for good pops back to it,
   * because that is where the full-screen row and the host list live. Every
   * other screen only renders what it is showing.
   */
  returnOnFailure?: boolean;
}

/**
 * The per-screen half of what design.md §9 calls global chrome: the connection
 * strip and the full-screen rows of §12. Mount it as the first child of a
 * screen's root view — the error row covers that view. The §9.10 dialog and
 * the §9.8 sheet are app-wide and live in the root layout instead
 * (`ConnectionModals`); this renders them only where no root mount exists.
 */
export function ConnectionChrome({ returnOnFailure = false }: ConnectionChromeProps) {
  const connection = useSession((state) => state.connection);
  const lastClose = useDiagnostics((state) => state.lastClose);
  const fatal = connection.state === "failed" || connection.state === "incompatible";
  const failure = fatal
    ? describeConnectionFailure({
        state: connection.state,
        message: connection.message,
        close: lastClose ?? undefined,
        host: connection.host,
      })
    : undefined;

  useEffect(() => {
    if (fatal && returnOnFailure) router.dismissTo("/");
  }, [fatal, returnOnFailure]);

  // Screens below the top of the stack stay mounted, so only the focused one
  // draws the strip and the full-screen row. The modals belong to the root
  // layout (`ConnectionModals`); this renders them only when nothing else
  // does.
  const [focused, setFocused] = useState(false);
  useFocusEffect(
    useCallback(() => {
      setFocused(true);
      return () => setFocused(false);
    }, []),
  );
  const owns = useOwnsGlobalModals("screen");

  return (
    <>
      {focused ? <ConnectionStrip /> : null}
      {focused && failure?.presentation === "fullScreen" ? (
        <ConnectionErrorScreen failure={failure} hostId={connection.host?.id} />
      ) : null}
      {owns ? <HostKeyDialog /> : null}
      {owns ? <ConnectionSheet /> : null}
    </>
  );
}
