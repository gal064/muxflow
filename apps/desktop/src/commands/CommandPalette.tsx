import { useEffect, useId, useMemo, useState } from "react";
import {
  commandAvailable,
  commandRegistry,
  shortcutFor,
  type CommandContext,
  type CommandId,
  type Platform,
  type ShortcutOverrides,
  keyboardEventIsComposing,
} from "./registry";
import { useModalDialog } from "./useModalDialog";

interface Props {
  context: CommandContext;
  onClose(): void;
  onInvoke(commandId: CommandId): void;
  platform: Platform;
  shortcuts: ShortcutOverrides;
}

export function CommandPalette({ context, onClose, onInvoke, platform, shortcuts }: Props) {
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const titleId = useId();
  const dialog = useModalDialog<HTMLElement>(onClose);
  const commands = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    return commandRegistry.filter((command) => !needle || `${command.group} ${command.title}`.toLocaleLowerCase().includes(needle));
  }, [query]);
  useEffect(() => setActiveIndex(0), [query]);

  const invokeActive = () => {
    const command = commands[activeIndex];
    if (command && commandAvailable(command, context)) { onInvoke(command.id); onClose(); }
  };

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
    if (event.currentTarget === event.target) onClose();
  }}>
    <section aria-labelledby={titleId} aria-modal="true" className="command-palette" ref={dialog} role="dialog">
      <h2 className="sr-only" id={titleId}>Command palette</h2>
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
      <div className="command-list" id="command-results" role="listbox">
        {commands.map((command, index) => {
          const enabled = commandAvailable(command, context);
          return <button
            aria-selected={index === activeIndex}
            className={index === activeIndex ? "selected" : ""}
            disabled={!enabled}
            id={`command-${command.id}`}
            key={command.id}
            onMouseEnter={() => setActiveIndex(index)}
            onClick={() => { onInvoke(command.id); onClose(); }}
            role="option"
            type="button"
          >
            <span><small>{command.group}</small>{command.title}</span>
            <kbd>{shortcutFor(command, platform, shortcuts) ?? ""}</kbd>
          </button>;
        })}
      </div>
    </section>
  </div>;
}
