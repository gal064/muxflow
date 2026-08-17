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
      // Monaco's own automatic layout is deliberately left on: the custom
      // observer exists because it does not survive the hidden container
      // `@monaco-editor/react` mounts into, not because it is wrong.
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
