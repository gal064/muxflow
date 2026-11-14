// `react-native-svg` for vitest (see reactNative.tsx): every shape is a named element.

import { createElement, type ReactNode } from "react";

type Props = Record<string, unknown> & { children?: ReactNode };

function shape(name: string) {
  return function Shape(props: Props) {
    return createElement(name, props);
  };
}

export const Svg = shape("Svg");
export const Path = shape("Path");
export const Rect = shape("Rect");
export const Circle = shape("Circle");
export const G = shape("G");
export const Line = shape("Line");
export default Svg;
