// Installed once by app/_layout.tsx: the transport factory for this build, the
// foreground service's notification and the toast sink. The SSH module (§6)
// is the transport for every host.

import { AppState, ToastAndroid } from "react-native";
import { startNotifications } from "../features/notifications";
import { muxflowSsh } from "../ssh/MuxflowSsh";
import { sshTransportFactory } from "../ssh/registerTransport";
import { onToast, setForegroundService, setTransportFactory } from "./connectionManager";
import { log } from "./log";
import { sessionStore } from "../store/sessionStore";

let wired = false;

export function wireApp(): void {
  if (wired) return;
  wired = true;
  startFlightRecorder();
  onToast((message) => ToastAndroid.show(message, ToastAndroid.SHORT));
  startNotifications();
  setTransportFactory(sshTransportFactory);
  setForegroundService(muxflowSsh());
}

/** Always-on, transition-only instrumentation. No polling or background work. */
function startFlightRecorder(): void {
  let lifecycle = AppState.currentState;
  log(`app.lifecycle state=${lifecycle}`);
  AppState.addEventListener("change", (next) => {
    if (next === lifecycle) return;
    log(`app.lifecycle ${lifecycle}->${next}`);
    lifecycle = next;
  });

  sessionStore.subscribe((state, previous) => {
    if (state.connection.state !== previous.connection.state || state.connection.attempt !== previous.connection.attempt) {
      log(`connection ${previous.connection.state}->${state.connection.state} attempt=${state.connection.attempt} topology=${state.topologyGeneration}`);
    }
    if (state.topologyGeneration !== previous.topologyGeneration) {
      log(`topology generation=${previous.topologyGeneration}->${state.topologyGeneration} sessions=${Object.keys(state.sessions).length} windows=${Object.keys(state.windows).length} panes=${Object.keys(state.panes).length}`);
    }
  });
}
