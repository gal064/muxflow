// Rendering screens in vitest with react-test-renderer (React Native is
// stubbed, see vitest.config.ts). Test-only.

import type { ReactElement } from "react";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// React 19 warns once per render that the renderer is deprecated; it is the
// one that runs without a DOM, and the warning says nothing about the test.
const consoleError = console.error;
console.error = (...args: unknown[]) => {
  if (typeof args[0] === "string" && args[0].startsWith("react-test-renderer is deprecated")) return;
  consoleError(...args);
};

/** A stubbed React Native element of this name (see src/testing/stubs). */
export function isHost(node: ReactTestInstance, name: string): boolean {
  return (node.type as unknown) === name;
}

export function render(element: ReactElement): ReactTestRenderer {
  let renderer: ReactTestRenderer | undefined;
  act(() => {
    renderer = create(element);
  });
  return renderer!;
}

export function rerender(renderer: ReactTestRenderer, element: ReactElement): void {
  act(() => renderer.update(element));
}

/** Every host element whose accessibility label is `label`. */
export function allByLabel(renderer: ReactTestRenderer, label: string | RegExp): ReactTestInstance[] {
  return renderer.root.findAll((node) => typeof node.type === "string"
    && typeof node.props.accessibilityLabel === "string"
    && (typeof label === "string" ? node.props.accessibilityLabel === label : label.test(node.props.accessibilityLabel)));
}

export function byLabel(renderer: ReactTestRenderer, label: string | RegExp): ReactTestInstance {
  const found = allByLabel(renderer, label);
  if (found.length !== 1) throw new Error(`expected one element labelled ${String(label)}, found ${found.length}`);
  return found[0]!;
}

/** The pressable whose own text is `text` (a button without a label). */
export function byText(renderer: ReactTestRenderer, text: string): ReactTestInstance {
  const found = renderer.root.findAll((node) => isHost(node, "Pressable") && node.findAll((child) => isHost(child, "Text") && child.children.join("") === text).length > 0);
  if (found.length === 0) throw new Error(`no pressable reads "${text}"`);
  return found.at(-1)!;
}

export function press(node: ReactTestInstance): void {
  act(() => {
    (node.props.onPress as (() => void) | undefined)?.();
  });
}

export function longPress(node: ReactTestInstance): void {
  act(() => {
    (node.props.onLongPress as (() => void) | undefined)?.();
  });
}

export function type(node: ReactTestInstance, text: string): void {
  act(() => {
    (node.props.onChangeText as ((value: string) => void) | undefined)?.(text);
  });
}
