// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TextInputDialog } from "./TextInputDialog";
import { useModalDialog } from "./useModalDialog";

const mounted: Array<{ root: ReturnType<typeof createRoot>; container: HTMLElement }> = [];

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
});

afterEach(async () => {
  while (mounted.length) {
    const current = mounted.pop()!;
    await act(async () => current.root.unmount());
    current.container.remove();
  }
});

function Modal({ onCancel }: { onCancel(): void }) {
  const dialog = useModalDialog<HTMLElement>(onCancel);
  return <main><button id="behind">Behind</button><div className="modal-backdrop"><section aria-modal="true" ref={dialog} role="dialog"><button autoFocus>First</button><button>Last</button></section></div></main>;
}

async function render(element: ReactNode): Promise<HTMLElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => root.render(element));
  return container;
}

describe("modal accessibility", () => {
  it("isolates background content, traps focus, ignores IME Escape, and restores state", async () => {
    const cancel = vi.fn();
    const container = await render(<Modal onCancel={cancel} />);
    const behind = container.querySelector<HTMLElement>("#behind")!;
    expect(behind.inert).toBe(true);
    expect(behind.getAttribute("aria-hidden")).toBe("true");
    const composingEscape = new KeyboardEvent("keydown", { bubbles: true, key: "Escape" });
    Object.defineProperty(composingEscape, "isComposing", { value: true });
    window.dispatchEvent(composingEscape);
    expect(cancel).not.toHaveBeenCalled();
    window.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key: "Escape" }));
    expect(cancel).toHaveBeenCalledTimes(1);
    await act(async () => mounted.at(-1)!.root.unmount());
    expect(behind.inert).toBe(false);
    expect(behind.hasAttribute("aria-hidden")).toBe(false);
    mounted.pop();
    container.remove();
  });

  it("does not submit a text dialog while its value is being composed", async () => {
    const submit = vi.fn();
    const container = await render(<TextInputDialog initialValue="日本語" label="Name" onCancel={vi.fn()} submit={submit} title="Rename" />);
    const input = container.querySelector("input")!;
    input.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    expect(submit).not.toHaveBeenCalled();
    input.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true }));
    container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    expect(submit).toHaveBeenCalledWith("日本語");
  });
});
