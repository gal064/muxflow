// Installed once by app/_layout.tsx: the transport factory for this build and
// the toast sink.
//
// The SSH module (§6) is the transport for every saved host. Development
// builds add two hosts that are not saved anywhere: the terminal dev bridge
// (`scripts/dev-tcp-bridge-terminal.mjs`) and the files dev bridge
// (`scripts/dev-tcp-bridge.mjs`), both plain TCP to a real helper on the
// machine running the emulator. They are routed by host id, so a real host
// never dials TCP and load order never decides which factory wins.

import { ToastAndroid } from "react-native";
import { startNotifications } from "../features/notifications";
import { sshTransportFactory } from "../ssh/registerTransport";
import { onToast, setTransportFactory, type TransportFactory } from "./connectionManager";

let wired = false;

export function wireApp(): void {
  if (wired) return;
  wired = true;
  onToast((message) => ToastAndroid.show(message, ToastAndroid.SHORT));
  startNotifications();
  setTransportFactory(__DEV__ ? devAwareFactory : sshTransportFactory);
}

const devAwareFactory: TransportFactory = async (host, lane) => {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const dev = require("./devTransport") as typeof import("./devTransport");
  if (host.id === dev.DEV_HOST.id) return dev.devTransportFactory(host, lane);
  if (host.id === dev.DEV_FILES_HOST.id) {
    const { connectDevTcpTransport } = await import("../dev/tcpTransport");
    return connectDevTcpTransport();
  }
  return sshTransportFactory(host, lane);
};
