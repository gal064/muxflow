import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  platform: "android",
  setConnectionAppActive: vi.fn(),
  alert: vi.fn(),
  startNotifications: vi.fn(),
  startAppUpdateCheck: vi.fn(),
  onToast: vi.fn(),
  setForegroundService: vi.fn(),
  setTransportFactory: vi.fn(),
  androidConnectionServices: vi.fn(() => ({ native: true })),
  sshTransportFactory: vi.fn(),
  toastShow: vi.fn(),
  addAppStateListener: vi.fn((_event: string, _listener: (state: string) => void) => ({ remove: vi.fn() })),
  log: vi.fn(),
}));

vi.mock("react-native", () => ({
  Platform: { get OS() { return mocks.platform; } },
  Alert: { alert: mocks.alert },
  AppState: { currentState: "active", addEventListener: mocks.addAppStateListener },
  ToastAndroid: { SHORT: 0, show: mocks.toastShow },
}));
vi.mock("../features/notifications", () => ({ startNotifications: mocks.startNotifications }));
vi.mock("../features/update", () => ({ startAppUpdateCheck: mocks.startAppUpdateCheck }));
vi.mock("./AndroidConnectionServices", () => ({ androidConnectionServices: mocks.androidConnectionServices }));
vi.mock("../ssh/registerTransport", () => ({ sshTransportFactory: mocks.sshTransportFactory }));
vi.mock("./connectionManager", () => ({
  onToast: mocks.onToast,
  setConnectionAppActive: mocks.setConnectionAppActive,
  setForegroundService: mocks.setForegroundService,
  setTransportFactory: mocks.setTransportFactory,
}));
vi.mock("./backgroundTimer", () => ({ setBackgroundTimer: vi.fn(), createNativeBackgroundTimer: vi.fn(), jsBackgroundTimer: {} }));
vi.mock("./log", () => ({ log: mocks.log }));


describe("wireApp", () => {
  beforeEach(() => { vi.resetModules(); vi.clearAllMocks(); mocks.platform = "android"; });
  it("starts agent notifications with the rest of the app wiring, once", async () => {
    const { wireApp } = await import("./appWiring");
    const { sessionStore } = await import("../store/sessionStore");
    wireApp();
    wireApp();

    expect(mocks.startNotifications).toHaveBeenCalledTimes(1);
    expect(mocks.startAppUpdateCheck).toHaveBeenCalledTimes(1);
    expect(mocks.setTransportFactory).toHaveBeenCalledWith(mocks.sshTransportFactory);
    expect(mocks.androidConnectionServices).toHaveBeenCalledTimes(1);
    expect(mocks.setForegroundService).toHaveBeenCalledWith({ native: true });
    expect(mocks.log).toHaveBeenCalledWith("app.lifecycle state=active");
    expect(mocks.addAppStateListener).toHaveBeenCalledTimes(1);

    const appStateListener = mocks.addAppStateListener.mock.calls[0]![1] as (state: string) => void;
    appStateListener("background");
    sessionStore.getState().setConnection({ state: "sshConnecting", attempt: 1 });
    sessionStore.getState().applyTopologyAck(7n);
    expect(mocks.log).toHaveBeenCalledWith("app.lifecycle active->background");
    expect(mocks.log).toHaveBeenCalledWith("connection idle->sshConnecting attempt=1 topology=0");
    expect(mocks.log).toHaveBeenCalledWith("topology generation=0->7 sessions=0 windows=0 panes=0");
  });
  it("uses iOS lifecycle and JS timers without loading Android services", async () => {
    mocks.platform = "ios";
    const { wireApp } = await import("./appWiring");
    const timers = await import("./backgroundTimer");
    wireApp();
    expect(mocks.androidConnectionServices).not.toHaveBeenCalled();
    expect(mocks.setForegroundService).toHaveBeenCalledWith(undefined);
    expect(timers.setBackgroundTimer).toHaveBeenCalledWith(timers.jsBackgroundTimer);
    const listener = mocks.addAppStateListener.mock.calls.at(-1)![1];
    listener("inactive");
    expect(mocks.setConnectionAppActive.mock.calls).toEqual([[true]]);
    listener("background"); listener("active");
    expect(mocks.setConnectionAppActive.mock.calls).toEqual([[true], [false], [true]]);
    const toast = mocks.onToast.mock.calls[0]![0] as (message: string) => void;
    toast("Hello");
    expect(mocks.alert).toHaveBeenCalledWith("Muxflow", "Hello");
  });
});
