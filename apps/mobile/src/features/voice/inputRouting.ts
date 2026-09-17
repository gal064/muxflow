export interface RecordingInputDescription {
  name: string;
  type: string;
  uid: string;
}

/** Bluetooth voice profiles first, then wired and platform-exposed external microphones. */
export function preferredExternalInput<T extends RecordingInputDescription>(inputs: readonly T[]): T | undefined {
  const ranked = inputs
    .map((input, index) => ({ input, index, rank: externalInputRank(input) }))
    .filter((candidate) => candidate.rank > 0)
    .sort((left, right) => right.rank - left.rank || left.index - right.index);
  return ranked[0]?.input;
}

/** A non-SCO input clears Android's temporary Bluetooth call route after recording. */
export function playbackInputAfterBluetooth<T extends RecordingInputDescription>(inputs: readonly T[]): T | undefined {
  return inputs.find((input) => !isBluetoothInput(input));
}

export function isBluetoothInput(input: RecordingInputDescription): boolean {
  return /bluetooth|sco|hfp/.test(inputDescription(input));
}

function externalInputRank(input: RecordingInputDescription): number {
  const description = inputDescription(input);
  if (isBluetoothInput(input)) return 3;
  if (/headset|headphone|wired|usb|external|linein|line in/.test(description)) return 2;
  return 0;
}

function inputDescription(input: RecordingInputDescription): string {
  return `${input.type} ${input.name}`.toLowerCase();
}
