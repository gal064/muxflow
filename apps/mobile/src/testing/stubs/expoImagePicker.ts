// `expo-image-picker` for vitest (aliased in vitest.config.ts): no picker
// opens. A test says on `imagePicker` what the user chose, and reads back how
// the picker was asked. Import `imagePicker` from this file by its path.

export interface StubImage {
  uri: string;
  fileName?: string | null;
  fileSize?: number;
  mimeType?: string;
  width?: number;
  height?: number;
}

type Result = { canceled: true; assets: null } | { canceled: false; assets: StubImage[] };

export const imagePicker = {
  /** What the library answers; undefined is the user backing out. */
  library: undefined as StubImage[] | undefined,
  /** What the camera answers; undefined is the user backing out. */
  camera: undefined as StubImage[] | undefined,
  cameraGranted: true,
  /** Every launch, with the options it was given. */
  launches: [] as { from: "library" | "camera"; options: Record<string, unknown> }[],
  reset(): void {
    imagePicker.library = undefined;
    imagePicker.camera = undefined;
    imagePicker.cameraGranted = true;
    imagePicker.launches = [];
  },
};

function answer(assets: StubImage[] | undefined): Result {
  return assets === undefined ? { canceled: true, assets: null } : { canceled: false, assets: assets.map((asset) => ({ width: 0, height: 0, ...asset })) };
}

export async function launchImageLibraryAsync(options: Record<string, unknown> = {}): Promise<Result> {
  imagePicker.launches.push({ from: "library", options });
  return answer(imagePicker.library);
}

export async function launchCameraAsync(options: Record<string, unknown> = {}): Promise<Result> {
  imagePicker.launches.push({ from: "camera", options });
  return answer(imagePicker.camera);
}

export async function requestCameraPermissionsAsync(): Promise<{ granted: boolean; status: string; canAskAgain: boolean; expires: string }> {
  const granted = imagePicker.cameraGranted;
  return { granted, status: granted ? "granted" : "denied", canAskAgain: !granted, expires: "never" };
}
