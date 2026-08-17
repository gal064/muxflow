import { DiffEditor } from "@monaco-editor/react";
import { languageForPath } from "../files/editorLanguage";
import { useEditorLayout } from "../files/editorLayout";
import { ADE_MONACO_THEME } from "../files/monaco";

export interface GitDiffEditorProps {
  /** The displayed path, which decides the language. */
  path: string;
  original: string;
  modified: string;
  originalModelPath: string;
  modifiedModelPath: string;
  /** The editor exists and is about to paint. */
  onReady(): void;
}

/**
 * The Monaco diff editor, and the only Git-side module that imports it.
 *
 * Split out for the same reason as the file editor: a binary diff, an oversized
 * diff and a diff that never arrives all render without it, and the diff
 * request itself must not queue behind the editor bundle being evaluated.
 */
export function GitDiffEditor(props: GitDiffEditorProps) {
  const attachLayout = useEditorLayout();
  return <DiffEditor
    keepCurrentModifiedModel
    keepCurrentOriginalModel
    language={languageForPath(props.path)}
    modified={props.modified}
    modifiedModelPath={props.modifiedModelPath}
    onMount={(editor) => {
      props.onReady();
      attachLayout(editor);
    }}
    options={{
      // See `FileEditor`: the custom observer supplements Monaco's automatic
      // layout rather than replacing it.
      automaticLayout: true,
      enableSplitViewResizing: true,
      minimap: { enabled: false },
      originalEditable: false,
      readOnly: true,
      renderSideBySide: true,
      scrollBeyondLastLine: false,
    }}
    original={props.original}
    originalModelPath={props.originalModelPath}
    theme={ADE_MONACO_THEME}
  />;
}
