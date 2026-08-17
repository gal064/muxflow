import Editor from "@monaco-editor/react";
import { languageForPath } from "./editorLanguage";
import { useEditorLayout } from "./editorLayout";
import { ADE_MONACO_THEME } from "./monaco";

export interface FileEditorProps {
  /** The file being edited, which decides the language. */
  path: string;
  /** Stable identity of the buffer, so Monaco keeps its view state per tab. */
  modelPath: string;
  value: string;
  readOnly: boolean;
  wordWrap: boolean;
  onChange(content: string): void;
  /** The editor exists and is about to paint. */
  onReady(): void;
}

/**
 * The Monaco text editor, and the only file-side module that imports it.
 *
 * It is separate from the tab surface so that the editor bundle is fetched when
 * a text file is actually going to be shown in one, rather than before the
 * surface's first line of code runs. Everything the surface needs to decide
 * whether to render this — the open file, its size, the view mode — is known
 * without it.
 */
export function FileEditor(props: FileEditorProps) {
  const attachLayout = useEditorLayout();
  return <Editor
    language={languageForPath(props.path)}
    onChange={(content) => { if (typeof content === "string") props.onChange(content); }}
    onMount={(editor) => {
      props.onReady();
      attachLayout(editor);
    }}
    options={{
      // Left on, and it is a deferral rather than a preference. Turning it off
      // is gated on coverage of every resize and restore path, and the paths
      // the custom observer in `editorLayout.ts` exists for — WKWebView not
      // delivering the first transition, window minimise/restore, display-scale
      // change — need the packaged macOS lane. Removing the second observer on
      // jsdom evidence alone risks the blank 30x157 editor it was written for.
      automaticLayout: true,
      minimap: { enabled: false },
      readOnly: props.readOnly,
      scrollBeyondLastLine: false,
      wordWrap: props.wordWrap ? "on" : "off",
    }}
    path={props.modelPath}
    saveViewState
    theme={ADE_MONACO_THEME}
    value={props.value}
  />;
}
