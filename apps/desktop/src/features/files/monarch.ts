import type { languages } from "monaco-editor";

/**
 * Small additions to Monaco's bundled Python and TypeScript grammars.
 *
 * The bundled Monarch grammars sort almost everything into `identifier`, so a
 * theme can only colour keywords, strings, numbers and comments, and the bulk of
 * a file reads as one grey. These keep the bundled grammar and add the few
 * distinctions a reader actually scans for: which names are functions, which
 * are built-ins or constants, and `self`/`this`. A full TextMate grammar would
 * draw more, at the cost of a new dependency and a much larger bundle.
 *
 * Both functions take the bundled definition rather than copying it, so a
 * Monaco upgrade still brings its grammar fixes along.
 */

type Monarch = languages.IMonarchLanguage & Record<string, unknown>;
type Rule = languages.IMonarchLanguageRule;

/**
 * Python 3's hard and soft keywords. The bundled list mixes these with
 * built-ins (`int`, `open`, `print`), so a call to `open()` coloured like `if`.
 */
const PYTHON_KEYWORDS = [
  "False", "None", "True", "and", "as", "assert", "async", "await", "break", "case", "class", "continue",
  "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
  "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
];

/**
 * Python 3's built-in functions and types. Written out rather than taken from
 * the bundled list, which is Python 2's: it coloured ordinary names like `file`
 * and `long` as built-ins and missed `bytes`.
 */
const PYTHON_BUILTINS = [
  "abs", "aiter", "all", "anext", "any", "ascii", "bin", "bool", "breakpoint", "bytearray", "bytes", "callable",
  "chr", "classmethod", "compile", "complex", "delattr", "dict", "dir", "divmod", "enumerate", "eval", "exec",
  "filter", "float", "format", "frozenset", "getattr", "globals", "hasattr", "hash", "help", "hex", "id", "input",
  "int", "isinstance", "issubclass", "iter", "len", "list", "locals", "map", "max", "memoryview", "min", "next",
  "object", "oct", "open", "ord", "pow", "print", "property", "range", "repr", "reversed", "round", "set",
  "setattr", "slice", "sorted", "staticmethod", "str", "sum", "super", "tuple", "type", "vars", "zip",
];

export function extendPython(base: languages.IMonarchLanguage): languages.IMonarchLanguage {
  const language = base as Monarch;
  const word = {
    cases: {
      "True|False|None": "constant",
      "self|cls": "variable.language",
      "@keywords": "keyword",
      "@builtins": "predefined",
      "@default": "identifier",
    },
  };
  const call = { cases: { "@keywords": "keyword", "@builtins": "predefined", "@default": "function" } };
  const root: Rule[] = [
    { include: "@whitespace" },
    { include: "@numbers" },
    // `r"…"`, `b"…"`, `rb"…"`: the prefix is part of the literal, not a name.
    // A lone `f` is left to the bundled f-string rule.
    [/(?:[rR][bBfF]?|[bB][rR]?|[fF][rR]|[uU])(?=["'])/, "keyword"],
    { include: "@strings" },
    [/(def)(\s+)([a-zA-Z_]\w*)/, ["keyword", "white", "function"]],
    [/(class)(\s+)([a-zA-Z_]\w*)/, ["keyword", "white", "type"]],
    [/@[a-zA-Z_][\w.]*/, "annotation"],
    [/->|[=<>!+\-*/%&|^~]+/, "operator"],
    // An attribute (`re.compile`, `args.file`) is a name, even when it spells a
    // built-in or a keyword.
    [/(\.)([a-zA-Z_]\w*)(?=\s*\()/, ["delimiter", "function"]],
    [/(\.)([a-zA-Z_]\w*)/, ["delimiter", "identifier"]],
    [/[,:;.]/, "delimiter"],
    [/[{}[\]()]/, "@brackets"],
    // A keyword argument (`type=float`) is a name, even when it spells a built-in.
    [/[a-zA-Z_]\w*(?==[^=])/, "identifier"],
    [/[a-zA-Z_]\w*(?=\s*\()/, call],
    [/[a-zA-Z_]\w*/, word],
  ];
  return {
    ...language,
    keywords: PYTHON_KEYWORDS,
    builtins: PYTHON_BUILTINS,
    tokenizer: { ...language.tokenizer, root },
  } as languages.IMonarchLanguage;
}

/**
 * The same additions for TypeScript and JavaScript, whose bundled grammar the
 * JavaScript one reuses. The bundled `common` state opens with the lower-case
 * word rule; these two rules take its place.
 */
export function extendTypeScript(base: languages.IMonarchLanguage): languages.IMonarchLanguage {
  const language = base as Monarch;
  const [, ...common] = language.tokenizer.common;
  const call: Rule = [/#?[a-z_$][\w$]*(?=\s*\()/, {
    cases: { super: "variable.language", "@keywords": "keyword", "@default": "function" },
  }];
  const word: Rule = [/#?[a-z_$][\w$]*/, {
    cases: {
      "true|false|null|undefined": "constant",
      "this|super": "variable.language",
      "@keywords": "keyword",
      "@default": "identifier",
    },
  }];
  return {
    ...language,
    tokenizer: { ...language.tokenizer, common: [call, word, ...common] },
  } as languages.IMonarchLanguage;
}
