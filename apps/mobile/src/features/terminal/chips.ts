// The key chips (design doc §9.5 step 3), in order, with the exact bytes.

export interface KeyChip { label: string; bytes: Uint8Array }

export const KEY_CHIPS: readonly KeyChip[] = [
  { label: "Esc", bytes: Uint8Array.of(0x1b) },
  { label: "Tab", bytes: Uint8Array.of(0x09) },
  { label: "↑", bytes: Uint8Array.of(0x1b, 0x5b, 0x41) },
  { label: "↓", bytes: Uint8Array.of(0x1b, 0x5b, 0x42) },
  { label: "Enter", bytes: Uint8Array.of(0x0d) },
  { label: "Ctrl-C", bytes: Uint8Array.of(0x03) },
  { label: "Ctrl-D", bytes: Uint8Array.of(0x04) },
  { label: "y", bytes: Uint8Array.of(0x79) },
  { label: "n", bytes: Uint8Array.of(0x6e) },
];

export const CR = Uint8Array.of(0x0d);
