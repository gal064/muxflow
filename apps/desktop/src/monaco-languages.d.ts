// Monaco ships its Monarch grammars without type declarations. `monarch.ts`
// extends two of them, so their shape is declared here.
declare module "monaco-editor/languages/definitions/*" {
  import type { languages } from "monaco-editor";
  export const conf: languages.LanguageConfiguration;
  export const language: languages.IMonarchLanguage;
}
