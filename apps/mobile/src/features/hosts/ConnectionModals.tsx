import { ConnectionSheet } from "./ConnectionSheet";
import { HostKeyDialog } from "./HostKeyDialog";
import { useOwnsGlobalModals } from "./primaryChrome";

/**
 * The two app-wide surfaces of design.md §9: the host key trust dialog (§9.10)
 * and the connection sheet (§9.8). Mounted once, from the root layout, because
 * both can be raised while any screen is on top — a connection started from
 * Hosts asks for host key trust after Home has been pushed.
 */
export function ConnectionModals() {
  const owns = useOwnsGlobalModals("root");
  if (!owns) return null;
  return (
    <>
      <HostKeyDialog />
      <ConnectionSheet />
    </>
  );
}
