import { useEffect, useRef } from "react";
import { keyboardEventIsComposing } from "./registry";

const isolatedElements = new WeakMap<HTMLElement, {
  count: number;
  inert: boolean;
  ariaHidden: string | null;
}>();

const FOCUSABLE = "button:not(:disabled), summary, [href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [contenteditable=true], [tabindex]:not([tabindex='-1'])";

function isolateModalSiblings(dialog: HTMLElement): () => void {
  const backdrop = dialog.closest<HTMLElement>(".modal-backdrop");
  const parent = backdrop?.parentElement;
  if (!backdrop || !parent) return () => undefined;
  const siblings = [...parent.children].filter((element): element is HTMLElement => (
    element instanceof HTMLElement && element !== backdrop
  ));
  for (const sibling of siblings) {
    const existing = isolatedElements.get(sibling);
    if (existing) {
      existing.count += 1;
      continue;
    }
    isolatedElements.set(sibling, {
      count: 1,
      inert: Boolean(sibling.inert),
      ariaHidden: sibling.getAttribute("aria-hidden"),
    });
    sibling.inert = true;
    sibling.setAttribute("aria-hidden", "true");
  }
  return () => {
    for (const sibling of siblings) {
      const existing = isolatedElements.get(sibling);
      if (!existing) continue;
      existing.count -= 1;
      if (existing.count > 0) continue;
      sibling.inert = existing.inert;
      if (existing.ariaHidden === null) sibling.removeAttribute("aria-hidden");
      else sibling.setAttribute("aria-hidden", existing.ariaHidden);
      isolatedElements.delete(sibling);
    }
  };
}

/** Shared Escape, focus trap, and focus-return behavior for native app modals. */
export function useModalDialog<T extends HTMLElement>(onCancel: () => void, enabled = true) {
  const dialog = useRef<T>(null);
  const cancel = useRef(onCancel);
  cancel.current = onCancel;
  useEffect(() => {
    if (!enabled) return;
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
    const currentDialog = dialog.current;
    if (!currentDialog) return;
    currentDialog.tabIndex = -1;
    const restoreSiblings = isolateModalSiblings(currentDialog);
    queueMicrotask(() => {
      if (!currentDialog.isConnected || currentDialog.contains(document.activeElement)) return;
      (currentDialog.querySelector<HTMLElement>("[autofocus], " + FOCUSABLE) ?? currentDialog).focus();
    });
    const handleKey = (event: KeyboardEvent) => {
      if (keyboardEventIsComposing(event)) return;
      if (event.key === "Escape") { event.preventDefault(); cancel.current(); return; }
      if (event.key !== "Tab" || !dialog.current) return;
      const focusable = [...dialog.current.querySelectorAll<HTMLElement>(FOCUSABLE)]
        .filter((element) => !element.closest("[inert]") && element.getAttribute("aria-hidden") !== "true");
      if (!focusable.length) { event.preventDefault(); dialog.current.focus(); return; }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && (document.activeElement === first || document.activeElement === dialog.current)) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && (document.activeElement === last || document.activeElement === dialog.current)) { event.preventDefault(); first.focus(); }
    };
    window.addEventListener("keydown", handleKey, true);
    return () => {
      window.removeEventListener("keydown", handleKey, true);
      restoreSiblings();
      if (previous?.isConnected) previous.focus();
    };
  }, [enabled]);
  return dialog;
}
