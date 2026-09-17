/** Only the terminal input that owns the IME may shrink its screen. */
export function terminalKeyboardInset(safeBottom: number, keyboardHeight: number, inputFocused: boolean): number {
  "worklet";
  return inputFocused ? Math.max(safeBottom, keyboardHeight) : safeBottom;
}
