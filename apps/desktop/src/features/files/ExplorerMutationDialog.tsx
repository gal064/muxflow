import { useId, useRef, useState } from "react";
import { useModalDialog } from "../../commands/useModalDialog";
import { SurfaceError } from "../../ui/SurfaceError";
import type { ActiveRoot, FileEntry, FileMutation } from "./types";

export type MutationAction = "newFile" | "newDirectory" | "rename" | "move" | "duplicate" | "delete";

/**
 * One file action the user has started, bound to the root and connection it was
 * started under.
 *
 * Both are carried because the dialog outlives a keystroke: a root replaced
 * while somebody was typing a destination would otherwise apply the mutation
 * against a different tree entirely.
 */
export interface PendingMutation {
  action: MutationAction;
  rootToken: string;
  scopeIdentity: string;
  entry?: FileEntry;
}

interface Props {
  pending: PendingMutation;
  root?: ActiveRoot;
  scopeIdentity: string;
  disabled: boolean;
  onMutate(mutation: FileMutation): Promise<void>;
  onClose(): void;
}

/**
 * The create/rename/move/duplicate/delete dialog.
 *
 * Its own component because every keystroke in it is a state change, and while
 * it lived in the tree that meant re-rendering every mounted row to type one
 * character into a text field none of them can see.
 */
export function ExplorerMutationDialog(props: Props) {
  const { pending } = props;
  const [value, setValue] = useState(
    pending.action === "rename" || pending.action === "duplicate" ? pending.entry?.path ?? "" : "",
  );
  const [overwrite, setOverwrite] = useState(false);
  const [nonEmptyOverwrite, setNonEmptyOverwrite] = useState(false);
  const [error, setError] = useState<string>();
  // IME composition is not a commit. Without this, Enter accepting a Japanese
  // or Chinese candidate also submitted the form behind it.
  const composing = useRef(false);
  const titleId = useId();
  const dialogRef = useModalDialog<HTMLFormElement>(props.onClose, true);
  const destructive = pending.action === "delete";
  const replaceable = pending.action === "rename" || pending.action === "move" || pending.action === "duplicate";

  const submit = async () => {
    const root = props.root;
    if (!root) return;
    if (pending.rootToken !== root.token || pending.scopeIdentity !== props.scopeIdentity || props.disabled) {
      setError("This file action was cancelled because its host or active root changed.");
      return;
    }
    const named = value.trim();
    const entry = pending.entry;
    const mutation = ((): FileMutation | undefined => {
      if (pending.action === "newFile" || pending.action === "newDirectory") {
        if (!named) return undefined;
        return {
          kind: pending.action === "newFile" ? "createFile" : "createDirectory",
          parent: entry?.kind === "directory" ? entry.path : root.path,
          name: named,
        };
      }
      if (!entry) return undefined;
      if (pending.action === "delete") {
        return { kind: "delete", path: entry.path, confirmedNonEmpty: entry.kind === "directory" };
      }
      if (!named) return undefined;
      return {
        kind: pending.action, path: entry.path, destination: named,
        overwrite, confirmedNonEmpty: nonEmptyOverwrite,
      };
    })();
    try {
      if (mutation) await props.onMutate(mutation);
      props.onClose();
    } catch (failure) {
      setError(String(failure));
    }
  };

  return <div className="modal-backdrop" role="presentation">
    <form
      aria-labelledby={titleId}
      aria-modal="true"
      className="file-dialog confirmation"
      onSubmit={(event) => { event.preventDefault(); if (!composing.current) void submit(); }}
      ref={dialogRef}
      role="dialog"
    >
      <h2 id={titleId}>{labelForAction(pending.action)}</h2>
      {destructive
        ? <p>
          Delete <code>{pending.entry?.path}</code>?
          {pending.entry?.kind === "directory" && " Non-empty directories require this confirmation."}
        </p>
        : <label>
          {pending.action.startsWith("new") ? "Name" : "Destination path"}
          <input
            autoFocus
            onChange={(event) => setValue(event.target.value)}
            onCompositionEnd={() => { composing.current = false; }}
            onCompositionStart={() => { composing.current = true; }}
            value={value}
          />
        </label>}
      {replaceable && <label className="overwrite">
        <input checked={overwrite} onChange={(event) => setOverwrite(event.target.checked)} type="checkbox" />
        {" "}Allow overwrite after confirmation
      </label>}
      {replaceable && overwrite && <label className="overwrite">
        <input checked={nonEmptyOverwrite} onChange={(event) => setNonEmptyOverwrite(event.target.checked)} type="checkbox" />
        {" "}Also replace a non-empty destination directory
      </label>}
      {error && <SurfaceError detail={error} />}
      <div className="dialog-actions">
        <button onClick={props.onClose} type="button">Cancel</button>
        <button className={destructive ? "danger" : "primary"} type="submit">
          {destructive ? "Delete" : "Apply"}
        </button>
      </div>
    </form>
  </div>;
}

export function labelForAction(action: MutationAction): string {
  return ({
    newFile: "Create file", newDirectory: "Create folder", rename: "Rename",
    move: "Move", duplicate: "Duplicate", delete: "Delete",
  } as const)[action];
}
