import { router } from "expo-router";
import { useEffect } from "react";

import { ConnectionErrorScreen } from "./ConnectionErrorScreen";
import { ConnectionSheet } from "./ConnectionSheet";
import { ConnectionStrip } from "./ConnectionStrip";
import { describeConnectionFailure } from "./errorMatrix";
import { HostKeyDialog } from "./HostKeyDialog";
import { useDiagnostics, useSession } from "./hooks";

export interface ConnectionChromeProps {
  /**
   * Set on the Hosts screen: a connection that failed for good pops back to it,
   * because that is where the full-screen row and the host list live. Every
   * other screen only renders what it is showing.
   */
  returnOnFailure?: boolean;
}

/**
 * Everything design.md §9 calls global chrome, in one mount: the connection
 * strip, the full-screen rows of §12, the §9.10 trust dialog and the §9.8
 * sheet. Mount it as the first child of a screen's root view — the error row
 * covers that view.
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

  return (
    <>
      <ConnectionStrip />
      {failure?.presentation === "fullScreen" ? (
        <ConnectionErrorScreen failure={failure} hostId={connection.host?.id} />
      ) : null}
      <HostKeyDialog />
      <ConnectionSheet />
    </>
  );
}
