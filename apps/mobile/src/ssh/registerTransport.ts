// Wires the SSH module into the connection manager at app start (imported for
// its side effect from `app/_layout.tsx`).
//
// One `connectionId` per lane per attempt: the native module refuses an id that
// is still open, and a reconnect may overlap the close of the channel it
// replaces.

import { diagnosticsStore } from "../features/hosts/connectionDiagnostics";
import { hostKeyStore } from "../features/hosts/hostKeyStore";
import { log } from "../features/hosts/logBuffer";
import { setTransportFactory, type TransportLane } from "../session/connectionManager";
import { hostLabel, hostsStore, type SavedHost } from "../store/hostsStore";
import { sessionStore } from "../store/sessionStore";
import { openSshTransport } from "./sshTransport";

let attempts = 0;

export function sshTransportFactory(host: SavedHost, lane: TransportLane) {
  attempts += 1;
  // The pin may have been added (or forgotten) since the caller read the host.
  const saved = hostsStore.getState().hosts.find((candidate) => candidate.id === host.id) ?? host;
  diagnosticsStore.getState().setLastClose(null);
  return openSshTransport({
    connectionId: `${host.id}.${lane}.${attempts}`,
    target: { host: saved.host, port: saved.port, user: saved.user },
    trustedHostKeyFingerprint: saved.trustedHostKeyFingerprint,
    hostAddress: { host: saved.host, port: saved.port },
    onHostKey: async (prompt) => {
      // Only the control lane may ask. The bulk lane joins the same
      // authenticated SSH transport, so a prompt there would mean it is
      // dialling something else; refusing is the safe answer (§14).
      if (lane !== "control") {
        log(`hostKey.refused lane=${lane}`);
        return false;
      }
      // §7.2 names this state; the dial is parked until the dialog answers.
      sessionStore.getState().setConnection({ state: "awaitingHostKeyTrust" });
      const trusted = await hostKeyStore.getState().request({
        prompt,
        hostId: saved.id,
        hostLabel: hostLabel(saved),
      });
      if (sessionStore.getState().connection.state === "awaitingHostKeyTrust") {
        sessionStore.getState().setConnection({ state: "sshConnecting" });
      }
      if (trusted) {
        hostsStore.getState().setTrustedHostKeyFingerprint(saved.id, prompt.fingerprintSha256);
      }
      return trusted;
    },
    onHostKeyCancelled: () => hostKeyStore.getState().cancel(),
    onClose: (close) => diagnosticsStore.getState().setLastClose(close),
    log,
  });
}

setTransportFactory(sshTransportFactory);
