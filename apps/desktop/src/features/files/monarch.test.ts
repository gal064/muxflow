// @vitest-environment jsdom
import { beforeAll, describe, expect, it } from "vitest";
import { extendPython, extendTypeScript } from "./monarch";

type Tokenize = (text: string, languageId: string) => { type: string; text: string }[];
let tokenize: Tokenize;

beforeAll(async () => {
  // Monaco's standalone services ask for `matchMedia` while starting; jsdom has none.
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: () => ({ matches: false, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }),
  });
  const monaco = await import("monaco-editor/editor/editor.api");
  const python = await import("monaco-editor/languages/definitions/python/python");
  const typescript = await import("monaco-editor/languages/definitions/typescript/typescript");
  for (const [id, language] of [["test-python", extendPython(python.language)], ["test-typescript", extendTypeScript(typescript.language)]] as const) {
    monaco.languages.register({ id });
    monaco.languages.setMonarchTokensProvider(id, language);
  }
  tokenize = (text, languageId) => {
    const [line] = monaco.editor.tokenize(text, languageId);
    return line
      .map((token, i) => ({ type: token.type.replace(/\.(python|ts)$/u, ""), text: text.slice(token.offset, line[i + 1]?.offset) }))
      .filter((token) => token.type !== "white" && token.text.trim() !== "");
  };
});

function kinds(text: string, languageId: string): Record<string, string> {
  return Object.fromEntries(tokenize(text, languageId).map((token) => [token.text, token.type]));
}

describe("extended Python grammar", () => {
  it("names a function where it is defined and where it is called", () => {
    expect(kinds("def main() -> None:", "test-python")).toMatchObject({
      def: "keyword", main: "function", "->": "operator", None: "constant", ":": "delimiter",
    });
    expect(kinds("ap.add_argument(x)", "test-python")).toMatchObject({ ap: "identifier", ".": "delimiter", add_argument: "function" });
  });

  it("separates built-ins and self from keywords", () => {
    expect(kinds("for l in open(self.path): print(int(l))", "test-python")).toMatchObject({
      for: "keyword", in: "keyword", open: "predefined", self: "variable.language", print: "predefined", int: "predefined",
    });
  });

  it("keeps a keyword argument a plain name even when it spells a built-in", () => {
    expect(kinds("f(type=float, default=3)", "test-python")).toMatchObject({
      type: "identifier", float: "predefined", default: "identifier", "=": "operator", 3: "number",
    });
  });

  it("never reads an attribute as a built-in or keyword", () => {
    expect(kinds("re.compile(p).match(s)", "test-python")).toMatchObject({ re: "identifier", compile: "function", match: "function" });
    expect(kinds("for file in args.long:", "test-python")).toMatchObject({ file: "identifier", long: "identifier" });
  });

  it("treats a string prefix and a decorator as what they are", () => {
    expect(kinds('x = r"a.*"', "test-python")).toMatchObject({ r: "keyword", "a.*": "string" });
    expect(kinds("x = b'ab'", "test-python")).toMatchObject({ b: "keyword", ab: "string" });
    // A lone `f` must still open the bundled f-string state, which colours `{a}` as a name.
    expect(kinds('x = f"{a}"', "test-python")).toMatchObject({ 'f"': "string.escape", "{a}": "identifier" });
    expect(kinds("@dataclass", "test-python")).toMatchObject({ "@dataclass": "annotation" });
    expect(kinds("class Row:", "test-python")).toMatchObject({ class: "keyword", Row: "type" });
  });
});

describe("extended TypeScript grammar", () => {
  it("names calls and leaves other words to the bundled rules", () => {
    expect(kinds("const box = host.getBoundingClientRect();", "test-typescript")).toMatchObject({
      const: "keyword", box: "identifier", host: "identifier", getBoundingClientRect: "function",
    });
    expect(kinds("if (ready) return Promise;", "test-typescript")).toMatchObject({
      if: "keyword", ready: "identifier", return: "keyword", Promise: "type.identifier",
    });
  });

  it("colours literals and this apart from keywords", () => {
    expect(kinds("this.x = null ?? undefined ?? true", "test-typescript")).toMatchObject({
      this: "variable.language", null: "constant", undefined: "constant", true: "constant",
    });
    expect(kinds("super(props)", "test-typescript")).toMatchObject({ super: "variable.language" });
  });

  it("replaces the bundled word rule, not some other rule", async () => {
    // `extendTypeScript` drops the first rule of the bundled `common` state by
    // position. A Monaco upgrade that reorders it must fail here, not silently.
    const typescript = await import("monaco-editor/languages/definitions/typescript/typescript");
    const [first] = (typescript.language.tokenizer as unknown as { common: [RegExp, unknown][] }).common;
    expect(first[0].source).toBe("#?[a-z_$][\\w$]*");
  });
});
