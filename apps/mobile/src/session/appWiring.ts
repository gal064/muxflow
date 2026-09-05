// Installed once by app/_layout.tsx: the transport factory for this build, the
// foreground service's notification and the toast sink. The SSH module (§6)
// is the transport for every host.

import { ToastAndroid } from "react-native";
import { startNotifications } from "../features/notifications";
import { muxflowSsh } from "../ssh/MuxflowSsh";
import { sshTransportFactory } from "../ssh/registerTransport";
import { onToast, setForegroundService, setTransportFactory } from "./connectionManager";

let wired = false;

export function wireApp(): void {
  if (wired) return;
  wired = true;
  onToast((message) => ToastAndroid.show(message, ToastAndroid.SHORT));
  startNotifications();
  setTransportFactory(sshTransportFactory);
  setForegroundService(muxflowSsh());
}
