import { useEffect, useId, useMemo, useRef, useState } from "react";
import {
  commandAvailable,
  commandsForSurface,
  shortcutFor,
  type CommandContext,
  type CommandDefinition,
  type CommandId,
  type Platform,
  type ShortcutOverrides,
  keyboardEventIsComposing,
} from "./registry";
import { fuzzyRank } from "./fuzzy";
import { shortcutGlyphs, shortcutSpoken } from "./shortcutGlyphs";
import { useModalDialog } from "./useModalDialog";
import { Icon } from "../ui/Icon";

interface Props {
  context: CommandContext;
  onClose(): void;
  onInvoke(commandId: CommandId): void;
  platform: Platform;
  shortcuts: ShortcutOverrides;
}

/**
 * 560px wide, 28px rows, group names as inline headings.
 *
 * Three things changed from the palette this replaces: the group is a heading
 * above its rows instead of an 80px column repeated on every one of them; an
 * unbound command renders no shortcut lozenge at all rather than an empty box;
 * and matching is a subsequence match, so typing the letters you remember in
 * the order you remember them finds the command.
 */
export function CommandPalette({ context, onClose, onInvoke, platform, shortcuts }: Props) {
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const titleId = useId();
  const dialog = useModalDialog<HTMLElement>(onClose);
  const listRef = useRef<HTMLDivElement>(null);
  const commands = useMemo(
    () => fuzzyRank(commandsForSurface("palette"), query, (command) => `${command.title} ${command.group}`),
    [query],
  );
  useEffect(() => setActiveIndex(0), [query]);
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>("[aria-selected=true]")?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, commands]);

  const invokeActive = () => {
    const command = commands[activeIndex];
    if (command && commandAvailable(command, context)) { onInvoke(command.id); onClose(); }
  };

  // Group headings only make sense while the list is still in registry order;
  // once a query has reordered rows by score, a heading would lie about what
  // follows it.
  const grouped = query.trim() === "";
  let previousGroup: CommandDefinition["group"] | undefined;

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
    if (event.currentTarget === event.target) onClose();
  }}>
    <section aria-labelledby={titleId} aria-modal="true" className="palette" ref={dialog} role="dialog">
      <h2 className="sr-only" id={titleId}>Command palette</h2>
      <div className="palette-input">
        <Icon className="palette-search" name="search" size={13} />
        <input
          aria-activedescendant={commands[activeIndex] ? `command-${commands[activeIndex].id}` : undefined}
          aria-controls="command-results"
          aria-expanded="true"
          aria-label="Search commands"
          role="combobox"
          autoFocus
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (keyboardEventIsComposing(event.nativeEvent)) return;
            if (event.key === "ArrowDown") { event.preventDefault(); setActiveIndex((value) => Math.min(commands.length - 1, value + 1)); }
            else if (event.key === "ArrowUp") { event.preventDefault(); setActiveIndex((value) => Math.max(0, value - 1)); }
            else if (event.key === "Enter") { event.preventDefault(); invokeActive(); }
          }}
          placeholder="Type a command"
          value={query}
        />
        <span aria-hidden="true" className="palette-hint">↑↓ navigate · ↩ run · esc</span>
      </div>
      <div className="palette-list" id="command-results" ref={listRef} role="listbox">
        {commands.length === 0 && <p className="quiet-empty">No matching command.</p>}
        {commands.map((command, index) => {
          const enabled = commandAvailable(command, context);
          const shortcut = shortcutFor(command, platform, shortcuts);
          const glyphs = shortcutGlyphs(shortcut, platform);
          const heading = grouped && command.group !== previousGroup ? command.group : undefined;
          previousGroup = command.group;
          // `role="presentation"` on the wrapper: a generic element between a
          // listbox and its options breaks ownership.
          return <div key={command.id} role="presentation">
            {heading && <div className="palette-group" role="presentation">{heading}</div>}
            <button
              aria-disabled={!enabled}
              aria-selected={index === activeIndex}
              className={index === activeIndex ? "palette-row selected" : "palette-row"}
              // `aria-disabled` rather than `disabled`: `aria-activedescendant`
              // may point here, and several screen readers drop a disabled
              // element from the tree entirely, leaving the combobox pointing
              // at nothing.
              data-unavailable={enabled ? undefined : "true"}
              id={`command-${command.id}`}
              onMouseEnter={() => setActiveIndex(index)}
              onClick={() => { if (enabled) { onInvoke(command.id); onClose(); } }}
              role="option"
              // The input owns focus and points here with `aria-activedescendant`;
              // leaving these as tab stops made the modal focus trap cycle through
              // every row before it came back to the field.
              tabIndex={-1}
              type="button"
            >
              <span className="palette-title">{command.title}</span>
              {glyphs && <kbd aria-label={shortcutSpoken(shortcut, platform)}>{glyphs}</kbd>}
            </button>
          </div>;
        })}
      </div>
    </section>
  </div>;
}
