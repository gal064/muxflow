import { describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  startNotifications: vi.fn(),
  onToast: vi.fn(),
  setForegroundService: vi.fn(),
  setTransportFactory: vi.fn(),
  muxflowSsh: vi.fn(() => ({ native: true })),
  sshTransportFactory: vi.fn(),
  toastShow: vi.fn(),
  addAppStateListener: vi.fn((_event: string, _listener: (state: string) => void) => ({ remove: vi.fn() })),
  log: vi.fn(),
}));

vi.mock("react-native", () => ({
  AppState: { currentState: "active", addEventListener: mocks.addAppStateListener },
  ToastAndroid: { SHORT: 0, show: mocks.toastShow },
}));
vi.mock("../features/notifications", () => ({ startNotifications: mocks.startNotifications }));
vi.mock("../ssh/MuxflowSsh", () => ({ muxflowSsh: mocks.muxflowSsh }));
vi.mock("../ssh/registerTransport", () => ({ sshTransportFactory: mocks.sshTransportFactory }));
vi.mock("./connectionManager", () => ({
  onToast: mocks.onToast,
  setForegroundService: mocks.setForegroundService,
  setTransportFactory: mocks.setTransportFactory,
}));
vi.mock("./log", () => ({ log: mocks.log }));

const { wireApp } = await import("./appWiring");
const { sessionStore } = await import("../store/sessionStore");

describe("wireApp", () => {
  it("starts agent notifications with the rest of the app wiring, once", () => {
    wireApp();
    wireApp();

    expect(mocks.startNotifications).toHaveBeenCalledTimes(1);
    expect(mocks.setTransportFactory).toHaveBeenCalledWith(mocks.sshTransportFactory);
    expect(mocks.muxflowSsh).toHaveBeenCalledTimes(1);
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
});
