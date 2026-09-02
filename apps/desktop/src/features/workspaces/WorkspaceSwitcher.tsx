import { useEffect, useId, useMemo, useRef, useState } from "react";
import { keyboardEventIsComposing } from "../../commands/registry";
import { fuzzyRank } from "../../commands/fuzzy";
import { useModalDialog } from "../../commands/useModalDialog";
import { AgentStateIndicator } from "../../ui/AgentStateIndicator";
import type { MergedWorkspaceRow } from "./mergedWorkspaceRows";
import { workspaceMetaLine } from "./workspaceRows";

interface WorkspaceSwitcherProps {
  rows: readonly MergedWorkspaceRow[];
  /** Draws a shape as well as a color in each state dot. */
  stateGlyphs: boolean;
  onClose(): void;
  onSelect(row: MergedWorkspaceRow): void;
}

/** A DOM id for the row: the key holds a NUL, which no attribute should. */
const optionId = (row: MergedWorkspaceRow) => `workspace-option-${encodeURIComponent(row.key)}`;

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
  // Branch *and* path. The sidebar stopped printing the working directory, but
  // typing a path fragment is one of the two ways anyone finds a workspace
  // here, so it stays a match key — and the row shows what it matched on.
  const rows = useMemo(
    () => fuzzyRank(props.rows, query, (row) => `${row.session.name} ${workspaceMetaLine(row)}`),
    [props.rows, query],
  );
  useEffect(() => setActiveIndex(0), [query]);
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>("[aria-selected=true]")?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, rows]);

  const choose = (row: MergedWorkspaceRow | undefined) => {
    if (!row) return;
    props.onSelect(row);
    props.onClose();
  };

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
    if (event.currentTarget === event.target) props.onClose();
  }}>
    <section aria-labelledby={titleId} aria-modal="true" className="palette" ref={dialog} role="dialog">
      <h2 className="sr-only" id={titleId}>Switch workspace</h2>
      <div className="palette-input">
        <input
          aria-activedescendant={rows[activeIndex] ? optionId(rows[activeIndex]) : undefined}
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
            else if (event.key === "Enter") { event.preventDefault(); choose(rows[activeIndex]); }
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
          id={optionId(row)}
          key={row.key}
          onClick={() => choose(row)}
          onMouseEnter={() => setActiveIndex(index)}
          role="option"
          // Focus stays in the input; see the palette for why.
          tabIndex={-1}
          type="button"
        >
          {/* Not hidden from the accessibility tree, unlike the sidebar's: this
              row has no host label to say "on <host>" with, and the letter is
              the only thing telling two same-named workspaces apart. */}
          {row.letter && <span className="host-letter">{row.letter}</span>}
          <span className="palette-title">{row.session.name}</span>
          {row.attention !== "none" && <AgentStateIndicator glyphs={props.stateGlyphs} label={`Agent ${row.attention}`} state={row.attention} />}
          {workspaceMetaLine(row) && <span className="palette-meta">{workspaceMetaLine(row)}</span>}
        </button>)}
      </div>
    </section>
  </div>;
}
