import { useEffect, useId, useMemo, useRef, useState } from "react";
import { keyboardEventIsComposing } from "../../commands/registry";
import { fuzzyRank } from "../../commands/fuzzy";
import { useModalDialog } from "../../commands/useModalDialog";
import type { WorkspaceRowModel } from "./workspaceRows";

interface WorkspaceSwitcherProps {
  rows: readonly WorkspaceRowModel[];
  onClose(): void;
  onSelect(sessionId: string): void;
}

/**
 * ⌘P: type a few letters of a workspace name and land in it.
 *
 * It shares the palette's frame deliberately — same width, same row height,
 * same keyboard contract — because they are the same gesture pointed at
 * different things, and two differently-shaped overlays for "search and pick"
 * is exactly the kind of thing this phase removed.
 */
export function WorkspaceSwitcher(props: WorkspaceSwitcherProps) {
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const titleId = useId();
  const dialog = useModalDialog<HTMLElement>(props.onClose);
  const listRef = useRef<HTMLDivElement>(null);
  const rows = useMemo(
    () => fuzzyRank(props.rows, query, (row) => `${row.session.name} ${row.metadata ?? ""}`),
    [props.rows, query],
  );
  useEffect(() => setActiveIndex(0), [query]);
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>("[aria-selected=true]")?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, rows]);

  const choose = (sessionId: string | undefined) => {
    if (!sessionId) return;
    props.onSelect(sessionId);
    props.onClose();
  };

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
    if (event.currentTarget === event.target) props.onClose();
  }}>
    <section aria-labelledby={titleId} aria-modal="true" className="palette" ref={dialog} role="dialog">
      <h2 className="sr-only" id={titleId}>Switch workspace</h2>
      <div className="palette-input">
        <input
          aria-activedescendant={rows[activeIndex] ? `workspace-option-${rows[activeIndex].session.id}` : undefined}
          aria-controls="workspace-results"
          aria-expanded="true"
          aria-label="Search workspaces"
          role="combobox"
          autoFocus
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (keyboardEventIsComposing(event.nativeEvent)) return;
            if (event.key === "ArrowDown") { event.preventDefault(); setActiveIndex((value) => Math.min(rows.length - 1, value + 1)); }
            else if (event.key === "ArrowUp") { event.preventDefault(); setActiveIndex((value) => Math.max(0, value - 1)); }
            else if (event.key === "Enter") { event.preventDefault(); choose(rows[activeIndex]?.session.id); }
          }}
          placeholder="Go to workspace"
          value={query}
        />
        <span aria-hidden="true" className="palette-hint">↑↓ navigate · ↩ open · esc</span>
      </div>
      <div className="palette-list" id="workspace-results" ref={listRef} role="listbox">
        {rows.length === 0 && <p className="quiet-empty">No matching workspace.</p>}
        {rows.map((row, index) => <button
          aria-selected={index === activeIndex}
          className={index === activeIndex ? "palette-row selected" : "palette-row"}
          id={`workspace-option-${row.session.id}`}
          key={row.session.id}
          onClick={() => choose(row.session.id)}
          onMouseEnter={() => setActiveIndex(index)}
          role="option"
          type="button"
        >
          <span className="palette-title">{row.session.name}</span>
          {row.attention !== "none" && <span aria-label={`Agent ${row.attention}`} className={`state-dot ${row.attention}`} role="img" />}
          {row.metadata && <span className="palette-meta">{row.metadata}</span>}
        </button>)}
      </div>
    </section>
  </div>;
}
