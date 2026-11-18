// `expo-document-picker` for vitest (aliased in vitest.config.ts): no picker
// opens. A test says on `documentPicker` what the user chose, and reads back
// how the picker was asked. Import `documentPicker` from this file by its path.

export interface StubDocument {
  uri: string;
  name: string;
  size?: number;
  mimeType?: string;
}

type Result = { canceled: true; assets: null } | { canceled: false; assets: StubDocument[] };

export const documentPicker = {
  /** What the picker answers; undefined is the user backing out. */
  files: undefined as StubDocument[] | undefined,
  /** The options of every time it was opened. */
  opened: [] as Record<string, unknown>[],
  reset(): void {
    documentPicker.files = undefined;
    documentPicker.opened = [];
  },
};

export async function getDocumentAsync(options: Record<string, unknown> = {}): Promise<Result> {
  documentPicker.opened.push(options);
  return documentPicker.files === undefined ? { canceled: true, assets: null } : { canceled: false, assets: documentPicker.files };
}
