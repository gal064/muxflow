// A stand-in for `react-native` in vitest (aliased in vitest.config.ts), so
// screens can be rendered with react-test-renderer in Node. Host components
// render as elements named after the component, with their props, so a test
// can find them and call their handlers; `renderHtml` draws the same tree.

import { createElement, forwardRef, Fragment, isValidElement, useImperativeHandle, type ComponentType, type ReactNode } from "react";

type Props = Record<string, unknown> & { children?: ReactNode };

function host(name: string) {
  const component = forwardRef<unknown, Props>(function Host(props, ref) {
    useImperativeHandle(ref, () => ({ scrollToEnd() {}, scrollTo() {}, focus() {}, blur() {}, measure() {} }));
    return createElement(name, props);
  });
  component.displayName = name;
  return component;
}

export const View = host("View");
export const Text = host("Text");
export const ScrollView = host("ScrollView");
export const TextInput = host("TextInput");
export const ActivityIndicator = host("ActivityIndicator");
export const Switch = host("Switch");
export const Image = host("Image");
export const KeyboardAvoidingView = host("View");

/** Every `scrollToOffset` a list was sent, oldest first. A test empties it and reads what its own steps sent. */
export const flatListScrolls: { offset: number; animated: boolean }[] = [];
/** Every `scrollToIndex` a list was sent, oldest first, as `flatListScrolls`. */
export const flatListIndexScrolls: { index: number; viewPosition: number | undefined }[] = [];

/**
 * A FlatList draws every row, in the order the screen shows them: an inverted
 * list's first row is the last one on screen, its header under that and its
 * footer on top. A test fires `onScroll` and `onEndReached` from the element's
 * props. `scrollToOffset` reports the offset it went to through `onScroll`, as
 * the real list does once it has scrolled.
 */
export const FlatList = forwardRef<unknown, Props>(function FlatList(props, ref) {
  const { data, renderItem, keyExtractor, ListHeaderComponent, ListFooterComponent, ...rest } = props as Props & {
    data?: readonly unknown[] | null;
    renderItem: (info: { item: unknown; index: number }) => ReactNode;
    keyExtractor?: (item: unknown, index: number) => string;
    ListHeaderComponent?: ComponentType | ReactNode;
    ListFooterComponent?: ComponentType | ReactNode;
  };
  useImperativeHandle(ref, () => ({
    scrollToEnd() {},
    scrollToIndex({ index, viewPosition }: { index: number; viewPosition?: number }) {
      flatListIndexScrolls.push({ index, viewPosition });
    },
    scrollToOffset({ offset, animated }: { offset: number; animated?: boolean }) {
      flatListScrolls.push({ offset, animated: animated ?? true });
      (rest.onScroll as ((event: unknown) => void) | undefined)?.({
        nativeEvent: { contentOffset: { x: 0, y: offset }, contentSize: { height: 0, width: 0 }, layoutMeasurement: { height: 0, width: 0 } },
      });
    },
  }));
  const part = (value: ComponentType | ReactNode, key: string) =>
    value === null || value === undefined || isValidElement(value) ? createElement(Fragment, { key }, value as ReactNode) : createElement(value as ComponentType, { key });
  const cells = (data ?? []).map((item, index) => createElement(Fragment, { key: keyExtractor ? keyExtractor(item, index) : String(index) }, renderItem({ item, index })));
  const header = part(ListHeaderComponent, "$header");
  const footer = part(ListFooterComponent, "$footer");
  return createElement("FlatList", rest, rest.inverted ? [footer, ...cells.reverse(), header] : [header, ...cells, footer]);
});

/** Resolves `style` and `children` given as functions of the press state, as React Native does. */
export const Pressable = forwardRef<unknown, Props>(function Pressable(props, _ref) {
  const { style, children, ...rest } = props;
  const state = { pressed: false, hovered: false };
  return createElement(
    "Pressable",
    { ...rest, style: typeof style === "function" ? (style as (s: typeof state) => unknown)(state) : style },
    (typeof children === "function" ? (children as (s: typeof state) => ReactNode)(state) : children) as ReactNode,
  );
});
export const TouchableOpacity = Pressable;

/** A Modal shows its children only while visible. */
export function Modal(props: Props & { visible?: boolean }) {
  const { visible, children, ...rest } = props;
  return visible === false ? null : createElement("Modal", rest, children);
}

export const StyleSheet = {
  create<T>(styles: T): T {
    return styles;
  },
  flatten(style: unknown): Record<string, unknown> {
    if (!style) return {};
    if (Array.isArray(style)) return Object.assign({}, ...style.map((entry) => StyleSheet.flatten(entry)));
    return style as Record<string, unknown>;
  },
  hairlineWidth: 1,
  absoluteFill: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0 },
  absoluteFillObject: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0 },
};

export const Platform = {
  OS: "android",
  Version: 34,
  select<T>(choices: { android?: T; ios?: T; default?: T }): T | undefined {
    return choices.android ?? choices.default;
  },
};

const subscription = { remove() {} };

export const AppState = { currentState: "active", addEventListener: () => subscription };
export const AccessibilityInfo = {
  isReduceMotionEnabled: async () => false,
  addEventListener: () => subscription,
  announceForAccessibility() {},
};
export const Keyboard = { addListener: () => subscription, dismiss() {}, isVisible: () => false, metrics: () => undefined };
export const Linking = { openURL: async () => undefined };

const backListeners: (() => boolean | null | undefined)[] = [];
/** The system Back button: `pressHardwareBack` is a press of it. */
export const BackHandler = {
  addEventListener(_event: "hardwareBackPress", listener: () => boolean | null | undefined) {
    backListeners.push(listener);
    return {
      remove() {
        const at = backListeners.indexOf(listener);
        if (at >= 0) backListeners.splice(at, 1);
      },
    };
  },
};
/** Asks the listeners newest first, as a device does, until one says it took the press. False: nothing did, and the screen would be left. */
export function pressHardwareBack(): boolean {
  for (const listener of [...backListeners].reverse()) if (listener() === true) return true;
  return false;
}
export const Alert = { alert() {} };
export const ToastAndroid = { show() {}, SHORT: 0, LONG: 1 };
export const Dimensions = { get: () => ({ width: 360, height: 780, scale: 3, fontScale: 1 }), addEventListener: () => subscription };
export const PixelRatio = { get: () => 3, getFontScale: () => 1, roundToNearestPixel: (size: number) => size };
export function useWindowDimensions() {
  return { width: 360, height: 780, scale: 3, fontScale: 1 };
}

class AnimatedValue {
  constructor(private value: number) {}
  setValue(value: number) {
    this.value = value;
  }
  stopAnimation() {}
  interpolate() {
    return this;
  }
  valueOf() {
    return this.value;
  }
}

const animation = { start() {}, stop() {}, reset() {} };

export const Animated = {
  Value: AnimatedValue,
  View: host("View"),
  Text: host("Text"),
  timing: () => animation,
  loop: () => animation,
  sequence: () => animation,
  parallel: () => animation,
};

export const Easing = {
  out: (fn: unknown) => fn,
  in: (fn: unknown) => fn,
  inOut: (fn: unknown) => fn,
  quad: () => 0,
  linear: () => 0,
  bezier: () => () => 0,
};

export default {
  View, Text, ScrollView, TextInput, ActivityIndicator, Switch, Image, Pressable, Modal, StyleSheet, Platform, AppState,
  AccessibilityInfo, Keyboard, Linking, Alert, ToastAndroid, Dimensions, Animated, Easing, useWindowDimensions,
};
