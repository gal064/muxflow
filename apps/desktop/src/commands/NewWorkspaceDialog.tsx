import { useId, useRef, useState } from "react";
import type { ConnectionPhase } from "../state/connectionReducer";
import { anchorForElement, ContextMenu, type ContextMenuAnchor } from "../ui/ContextMenu";
import { useModalDialog } from "./useModalDialog";

export interface NewWorkspaceHostOption {
  profileId: string;
  label: string;
  letter: string;
  phase: ConnectionPhase;
  canMutate: boolean;
}

interface NewWorkspaceDialogProps {
  hosts: readonly NewWorkspaceHostOption[];
  selectedHostProfileId: string;
  onCancel(): void;
  onHost(profileId: string): void;
  onSubmit(name: string, hostProfileId: string): void;
}

/**
 * The host remembered by the create dialog, repaired when it no longer names
 * one of the hosts the multi-host shell is showing.
 */
export function defaultNewWorkspaceHostId(
  hosts: readonly NewWorkspaceHostOption[],
  rememberedProfileId: string | undefined,
  activeProfileId: string,
): string | undefined {
  if (hosts.some((host) => host.profileId === rememberedProfileId)) return rememberedProfileId;
  return hosts.find((host) => host.profileId === activeProfileId && host.canMutate)?.profileId
    ?? hosts.find((host) => host.canMutate)?.profileId
    ?? hosts.find((host) => host.profileId === activeProfileId)?.profileId
    ?? hosts[0]?.profileId;
}

/** Workspace name plus the host on which that workspace will be created. */
export function NewWorkspaceDialog(props: NewWorkspaceDialogProps) {
  const [name, setName] = useState("");
  const [menu, setMenu] = useState<ContextMenuAnchor>();
  const titleId = useId();
  const composing = useRef(false);
  // The menu owns Escape and focus while it is open. Leaving the parent modal's
  // capture listener active would close the entire dialog before the menu's
  // own Escape handler had a chance to close only the dropdown.
  const dialog = useModalDialog<HTMLElement>(props.onCancel, true, menu === undefined);
  const selected = props.hosts.find((host) => host.profileId === props.selectedHostProfileId);
  const selectedPhase = selected && selected.phase !== "connected" ? `, ${phaseWord(selected.phase)}` : "";

  return <div className="modal-backdrop" role="presentation">
    <section aria-labelledby={titleId} aria-modal="true" className="confirmation new-workspace-dialog" ref={dialog} role="dialog">
      <h2 id={titleId}>New workspace</h2>
      <form onSubmit={(event) => {
        event.preventDefault();
        if (composing.current) return;
        const trimmed = name.trim();
        if (trimmed && selected?.canMutate) props.onSubmit(trimmed, selected.profileId);
      }}>
        <label>Workspace name
          <input
            autoFocus
            onChange={(event) => setName(event.target.value)}
            onCompositionEnd={() => { composing.current = false; }}
            onCompositionStart={() => { composing.current = true; }}
            value={name}
          />
        </label>
        <label>Host
          <button
            aria-expanded={menu !== undefined}
            aria-haspopup="menu"
            aria-label={selected ? `Host: ${selected.label}${selectedPhase}` : "Host: unavailable"}
            className="new-workspace-host-picker"
            disabled={props.hosts.length === 0}
            onClick={(event) => setMenu(anchorForElement(event.currentTarget))}
            type="button"
          >
            {selected ? <>
              <span aria-hidden="true" className={`link-dot ${selected.phase}`} />
              <span aria-hidden="true" className="host-letter">{selected.letter}</span>
              <span className="new-workspace-host-label">{selected.label}</span>
              {selected.phase !== "connected" && <span className="new-workspace-host-phase">{phaseWord(selected.phase)}</span>}
              <span aria-hidden="true" className="new-workspace-host-caret" />
            </> : <span className="new-workspace-host-label">No host available</span>}
          </button>
        </label>
        <div className="dialog-actions">
          <button onClick={props.onCancel} type="button">Cancel</button>
          <button className="primary" disabled={!name.trim() || !selected?.canMutate} type="submit">Save</button>
        </div>
      </form>
      {menu && <ContextMenu
        anchor={menu}
        items={props.hosts.map((host) => ({
          id: `new-workspace-host-${host.profileId}`,
          label: `${host.letter} ${host.label}${host.phase === "connected" ? "" : ` · ${phaseWord(host.phase)}`}`,
          checked: host.profileId === selected?.profileId,
          disabled: !host.canMutate,
          dot: host.phase,
          run: () => props.onHost(host.profileId),
        }))}
        label="Host"
        matchAnchorWidth
        onClose={() => setMenu(undefined)}
      />}
    </section>
  </div>;
}

/** A phase as a word in a label; only read-only is not already one. */
function phaseWord(phase: ConnectionPhase): string {
  return phase === "readOnly" ? "read-only" : phase;
}
