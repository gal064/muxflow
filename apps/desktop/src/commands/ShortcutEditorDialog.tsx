import { useId, useState } from "react";
import { commandRegistry, shortcutCollisions, shortcutFor, unsafeShortcutBindings, type Platform, type ShortcutOverrides } from "./registry";
import { useModalDialog } from "./useModalDialog";

interface Props {
  overrides: ShortcutOverrides;
  platform: Platform;
  onChange(overrides: ShortcutOverrides): void;
  onClose(): void;
}

export function ShortcutEditorDialog({ overrides, platform, onChange, onClose }: Props) {
  const [draft, setDraft] = useState(overrides);
  const unsafe = unsafeShortcutBindings(draft);
  const conflicts = shortcutCollisions(platform, draft);
  const titleId = useId();
  const dialog = useModalDialog<HTMLElement>(onClose);

  return <div className="modal-backdrop" role="presentation">
    <section aria-labelledby={titleId} aria-modal="true" className="shortcut-dialog" ref={dialog} role="dialog">
      <header><div><h2 id={titleId}>Keyboard shortcuts</h2><p>Application shortcuts control the outer tmux workspace without a prefix.</p></div><button aria-label="Close keyboard shortcuts" onClick={onClose} type="button">×</button></header>
      {conflicts.length > 0 && <div className="shortcut-warning" role="alert">Conflicting shortcuts are disabled: {conflicts.map((collision) => collision.shortcut).join(", ")}</div>}
      {unsafe.length > 0 && <div className="shortcut-warning" role="alert">Shortcuts must include Ctrl, Alt, or Meta. Unsafe bindings are not saved.</div>}
      <div className="shortcut-list">
        {commandRegistry.filter((command) => command.defaults).map((command, index) => <label key={command.id}>
          <span><strong>{command.title}</strong><small>{command.group}</small></span>
          <input autoFocus={index === 0} aria-label={`Shortcut for ${command.title}`} value={draft[command.id] ?? shortcutFor(command, platform, {}) ?? ""} onChange={(event) => setDraft({ ...draft, [command.id]: event.target.value || null })} />
        </label>)}
      </div>
      <footer><button onClick={() => setDraft({})} type="button">Restore defaults</button><button className="primary" disabled={unsafe.length > 0} onClick={() => { onChange(draft); onClose(); }} type="button">Done</button></footer>
    </section>
  </div>;
}
