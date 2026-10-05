import { useEffect, useState } from "react";
import type { ConnectionSpec } from "../../app/types";
import { openExternalUrl } from "../terminal/openExternalUrl";
import { Icon } from "../../ui/Icon";
import { parsePort, portsApi, type DetectedPort, type PortForward } from "./api";

export interface PortsHost {
  profileId: string;
  label: string;
  connection: Extract<ConnectionSpec, { mode: "ssh" }>;
}

interface PortsPanelProps {
  /** Shown SSH hosts; Local has nothing to forward. */
  hosts: readonly PortsHost[];
  activeProfileId: string;
  forwards: readonly PortForward[];
}

type Detection = { profileId: string; ports: readonly DetectedPort[] } | undefined;

/**
 * Type a port, and the host's port of that number answers on this machine's.
 * The local port is the remote one unless the person says otherwise, which is
 * the second, optional field. Forwards are not remembered across launches;
 * detection makes re-forwarding one click.
 */
export function PortsPanel(props: PortsPanelProps) {
  const [chosenProfileId, setChosenProfileId] = useState<string>();
  const host = props.hosts.find((candidate) => candidate.profileId === chosenProfileId)
    ?? props.hosts.find((candidate) => candidate.profileId === props.activeProfileId)
    ?? props.hosts[0];
  const [remoteText, setRemoteText] = useState("");
  const [localText, setLocalText] = useState("");
  const [error, setError] = useState<string>();
  const [detection, setDetection] = useState<Detection>();
  const [detectRequest, setDetectRequest] = useState(0);

  useEffect(() => {
    if (!host) return;
    let live = true;
    portsApi.detect(host.connection)
      .then((ports) => { if (live) setDetection({ profileId: host.profileId, ports }); })
      // A host without `ss`, or a failed attempt, gets no suggestions; ⟳
      // retries, and typing a port always works.
      .catch(() => { if (live) setDetection({ profileId: host.profileId, ports: [] }); });
    return () => { live = false; };
    // The connection object is rebuilt on every render; the profile names it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host?.profileId, detectRequest]);

  if (!host) return <section aria-label="Ports" className="ports-panel">
    <p className="quiet-empty">Port forwarding needs an SSH host.</p>
  </section>;

  const forwards = props.forwards.filter((forward) => forward.profileId === host.profileId);
  const forwarded = new Set(forwards.map((forward) => forward.remotePort));
  const suggestions = detection?.profileId === host.profileId
    ? detection.ports.filter((detected) => !forwarded.has(detected.port))
    : [];

  const forward = async (remotePort: number, localPort: number) => {
    setError(undefined);
    try {
      await portsApi.forward(host.connection, remotePort, localPort);
      return true;
    } catch (reason) {
      setError(String(reason));
      return false;
    }
  };

  const submit = async () => {
    const remotePort = parsePort(remoteText);
    const localPort = localText.trim() === "" ? remotePort : parsePort(localText);
    if (remotePort === undefined || localPort === undefined) {
      setError("Ports are numbers from 1 to 65535.");
      return;
    }
    if (await forward(remotePort, localPort)) {
      setRemoteText("");
      setLocalText("");
    }
  };

  return <section aria-label="Ports" className="ports-panel">
    {props.hosts.length > 1 && <label className="ports-host">
      <span className="section-label">Host</span>
      <select onChange={(event) => setChosenProfileId(event.target.value)} value={host.profileId}>
        {props.hosts.map((candidate) => <option key={candidate.profileId} value={candidate.profileId}>{candidate.label}</option>)}
      </select>
    </label>}

    <form className="ports-form" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
      <input
        aria-label="Remote port"
        inputMode="numeric"
        onChange={(event) => setRemoteText(event.target.value)}
        placeholder="Port"
        value={remoteText}
      />
      <span aria-hidden="true">→</span>
      <input
        aria-label="Local port (same as remote when empty)"
        inputMode="numeric"
        onChange={(event) => setLocalText(event.target.value)}
        placeholder={remoteText.trim() || "same"}
        value={localText}
      />
      <button className="primary" disabled={remoteText.trim() === ""} type="submit">Forward</button>
    </form>
    {error && <div className="surface-error" role="alert">{error}</div>}

    <ul aria-label="Forwarded ports" className="ports-list">
      {forwards.map((item) => <li className={`port-forward ${item.state}`} key={item.remotePort}>
        <span className="port-forward-dot" title={item.state} />
        <button
          className="port-forward-link"
          disabled={item.state !== "active"}
          onClick={() => void openExternalUrl(`http://127.0.0.1:${item.localPort}`).catch((reason) => setError(String(reason)))}
          title={`Open http://127.0.0.1:${item.localPort}`}
          type="button"
        >{item.remotePort} → 127.0.0.1:{item.localPort}</button>
        {item.state === "failed" && <button
          className="inline-action"
          onClick={() => void forward(item.remotePort, item.localPort)}
          type="button"
        >Retry</button>}
        <button
          aria-label={`Stop forwarding ${item.remotePort}`}
          className="bar-button"
          onClick={() => void portsApi.stop(host.profileId, item.remotePort).catch((reason) => setError(String(reason)))}
          title="Stop forwarding"
          type="button"
        ><Icon name="close" /></button>
        {item.error && <small className="port-forward-error">{item.error}</small>}
      </li>)}
    </ul>

    <div className="ports-detected">
      <div className="ports-detected-header">
        <span className="section-label">Detected on host</span>
        <button
          aria-label="Detect listening ports"
          className="bar-button"
          onClick={() => setDetectRequest((count) => count + 1)}
          title="Detect listening ports"
          type="button"
        ><Icon name="refresh" /></button>
      </div>
      {suggestions.length > 0 && <div className="ports-suggestions">
        {suggestions.map((detected) => <button
          className="port-suggestion"
          key={detected.port}
          onClick={() => void forward(detected.port, detected.port)}
          title={`Forward ${detected.port}`}
          type="button"
        >{detected.port}{detected.process && <small>{detected.process}</small>}</button>)}
      </div>}
    </div>
  </section>;
}
