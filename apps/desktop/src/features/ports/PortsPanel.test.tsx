// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PortForward } from "./api";
import { PortsPanel, type PortsHost } from "./PortsPanel";
import { usePortForwards } from "./usePortForwards";

const api = vi.hoisted(() => ({
  forward: vi.fn(),
  stop: vi.fn(),
  stopHost: vi.fn(),
  list: vi.fn(),
  detect: vi.fn(),
  onChanged: vi.fn(),
}));
vi.mock("./api", async (original) => ({ ...await original<typeof import("./api")>(), portsApi: api }));

const devbox: PortsHost = { profileId: "devbox", label: "Devbox", connection: { mode: "ssh", profileId: "devbox", target: "devbox" } };
const gpu: PortsHost = { profileId: "gpu", label: "GPU", connection: { mode: "ssh", profileId: "gpu", target: "gpu" } };

function forwardOf(profileId: string, remotePort: number): PortForward {
  return { profileId, remotePort, localPort: remotePort, state: "active", error: null };
}

async function render(element: React.ReactElement): Promise<ReactTestRenderer> {
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(element); });
  return renderer;
}

async function submit(renderer: ReactTestRenderer, remote: string, local = "") {
  await act(async () => { renderer.root.findByProps({ "aria-label": "Remote port" }).props.onChange({ target: { value: remote } }); });
  await act(async () => {
    renderer.root.findByProps({ "aria-label": "Local port (same as remote when empty)" }).props.onChange({ target: { value: local } });
  });
  await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault: vi.fn() }); });
}

beforeEach(() => {
  vi.clearAllMocks();
  api.forward.mockResolvedValue(undefined);
  api.stopHost.mockResolvedValue(undefined);
  api.detect.mockResolvedValue([]);
  api.list.mockResolvedValue([]);
  api.onChanged.mockResolvedValue(() => undefined);
});

describe("Ports panel", () => {
  it("forwards a typed port to the same local port unless told otherwise", async () => {
    const renderer = await render(<PortsPanel activeProfileId="devbox" forwards={[]} hosts={[devbox]} />);
    expect(renderer.root.findAllByType("select")).toHaveLength(0);

    await submit(renderer, "3000");
    expect(api.forward).toHaveBeenLastCalledWith(devbox.connection, 3000, 3000);
    await submit(renderer, "5173", "8080");
    expect(api.forward).toHaveBeenLastCalledWith(devbox.connection, 5173, 8080);

    await submit(renderer, "70000");
    expect(api.forward).toHaveBeenCalledTimes(2);
    expect(JSON.stringify(renderer.toJSON())).toContain("Ports are numbers from 1 to 65535.");
  });

  it("offers a host picker only with several hosts, starting on the active one", async () => {
    api.detect.mockResolvedValue([{ port: 3000, process: "node" }, { port: 8000, process: null }]);
    const renderer = await render(<PortsPanel activeProfileId="gpu" forwards={[forwardOf("gpu", 3000)]} hosts={[devbox, gpu]} />);
    expect(renderer.root.findByType("select").props.value).toBe("gpu");
    expect(api.detect).toHaveBeenCalledWith(gpu.connection);

    // 3000 is already forwarded on this host, so only 8000 is suggested.
    const suggestions = renderer.root.findAll((node) => node.props.className === "port-suggestion");
    expect(suggestions.map((node) => node.props.title)).toEqual(["Forward 8000"]);
    await act(async () => { suggestions[0].props.onClick(); });
    expect(api.forward).toHaveBeenCalledWith(gpu.connection, 8000, 8000);

    await act(async () => { renderer.root.findByType("select").props.onChange({ target: { value: "devbox" } }); });
    expect(api.detect).toHaveBeenLastCalledWith(devbox.connection);
  });

  it("keeps typing and retrying open when detection fails", async () => {
    api.detect.mockRejectedValueOnce(new Error("control master not ready"));
    const renderer = await render(<PortsPanel activeProfileId="devbox" forwards={[]} hosts={[devbox]} />);
    expect(renderer.root.findAll((node) => node.props.className === "port-suggestion")).toHaveLength(0);
    expect(renderer.root.findAllByType("form")).toHaveLength(1);

    api.detect.mockResolvedValueOnce([{ port: 3000, process: "node" }]);
    await act(async () => { renderer.root.findByProps({ "aria-label": "Detect listening ports" }).props.onClick(); });
    expect(renderer.root.findAll((node) => node.props.className === "port-suggestion")).toHaveLength(1);
  });

  it("says forwarding needs an SSH host when none is shown", async () => {
    const renderer = await render(<PortsPanel activeProfileId="local" forwards={[]} hosts={[]} />);
    expect(JSON.stringify(renderer.toJSON())).toContain("Port forwarding needs an SSH host.");
  });
});

describe("port forward state", () => {
  function Probe(props: { shown: readonly string[]; loaded: boolean }) {
    usePortForwards(props.shown, props.loaded);
    return null;
  }

  it("ends the forwards of a host that is no longer shown, once profiles are loaded", async () => {
    api.list.mockResolvedValue([forwardOf("devbox", 3000), forwardOf("gpu", 8000)]);
    // A reloaded webview sees the native forwards before any host is shown.
    const none: string[] = [];
    const renderer = await render(<Probe loaded={false} shown={none} />);
    expect(api.stopHost).not.toHaveBeenCalled();

    const shown = ["devbox", "gpu"];
    await act(async () => { renderer.update(<Probe loaded shown={shown} />); });
    expect(api.stopHost).not.toHaveBeenCalled();

    const fewer = ["devbox"];
    await act(async () => { renderer.update(<Probe loaded shown={fewer} />); });
    expect(api.stopHost).toHaveBeenCalledTimes(1);
    expect(api.stopHost).toHaveBeenCalledWith("gpu");
  });
});
