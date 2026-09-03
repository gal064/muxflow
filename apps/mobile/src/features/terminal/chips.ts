// The key chips (design doc §9.5 step 3), in order, with the exact bytes.

export interface KeyChip {
  label: string;
  /** Sent on a plain tap. Absent only on the Shift modifier chip. */
  bytes?: Uint8Array;
  /** Sent instead of `bytes` when Shift is armed. */
  shifted?: Uint8Array;
}

/** A one-shot modifier: tapping it arms Shift for the next chip and sends nothing. */
export const SHIFT_CHIP: KeyChip = { label: "⇧" };

/** CSI-u Shift+Enter, as the desktop sends it: the agent composers insert a newline. */
const SHIFT_ENTER = Uint8Array.of(0x1b, 0x5b, 0x31, 0x33, 0x3b, 0x32, 0x75);
/** Back-tab: cycles Codex's mode. */
const SHIFT_TAB = Uint8Array.of(0x1b, 0x5b, 0x5a);

export const KEY_CHIPS: readonly KeyChip[] = [
  { label: "Esc", bytes: Uint8Array.of(0x1b) },
  SHIFT_CHIP,
  { label: "Tab", bytes: Uint8Array.of(0x09), shifted: SHIFT_TAB },
  { label: "⇧Tab", bytes: SHIFT_TAB },
  { label: "Enter", bytes: Uint8Array.of(0x0d), shifted: SHIFT_ENTER },
  { label: "⇧Enter", bytes: SHIFT_ENTER },
  { label: "↑", bytes: Uint8Array.of(0x1b, 0x5b, 0x41), shifted: Uint8Array.of(0x1b, 0x5b, 0x31, 0x3b, 0x32, 0x41) },
  { label: "↓", bytes: Uint8Array.of(0x1b, 0x5b, 0x42), shifted: Uint8Array.of(0x1b, 0x5b, 0x31, 0x3b, 0x32, 0x42) },
  { label: "Ctrl-C", bytes: Uint8Array.of(0x03) },
  { label: "Ctrl-D", bytes: Uint8Array.of(0x04) },
  { label: "y", bytes: Uint8Array.of(0x79) },
  { label: "n", bytes: Uint8Array.of(0x6e) },
];

export const CR = Uint8Array.of(0x0d);

export interface ChipPress {
  /** Bytes to send, or nothing for the modifier itself. */
  send: Uint8Array | undefined;
  /** Whether Shift is armed after this press. */
  shiftArmed: boolean;
}

/**
 * One chip tap. The Shift chip toggles the armed state and sends nothing; any
 * other chip sends its `shifted` bytes when armed and it has them, else its
 * `bytes`, and disarms either way.
 */
export function pressChip(chip: KeyChip, shiftArmed: boolean): ChipPress {
  if (chip.bytes === undefined) return { send: undefined, shiftArmed: !shiftArmed };
  return { send: shiftArmed && chip.shifted ? chip.shifted : chip.bytes, shiftArmed: false };
}
