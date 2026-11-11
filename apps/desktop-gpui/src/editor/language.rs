//! Which grammar highlights a file, and the highlight additions Muxflow makes
//! to the bundled ones. Port of `features/files/editorLanguage.ts` and of the
//! Python and TypeScript additions in `features/files/monarch.ts`.
//!
//! The table is the exact set of languages the app claims to highlight. A
//! grammar is compiled in only for these, so `cargo deny` sees each one.

use gpui_kit::component::highlighter::{GrammarConfig, LanguageRegistry};

/// The grammar for each extension the app highlights, by the Kit's name.
pub const LANGUAGE_BY_EXTENSION: [(&str, &str); 15] = [
    ("ts", "typescript"),
    ("tsx", "tsx"),
    ("js", "javascript"),
    ("jsx", "javascript"),
    ("rs", "rust"),
    ("py", "python"),
    ("md", "markdown"),
    ("json", "json"),
    ("css", "css"),
    ("html", "html"),
    ("sh", "bash"),
    ("yml", "yaml"),
    ("yaml", "yaml"),
    ("toml", "toml"),
    // A Markdown file can also be spelled out.
    ("markdown", "markdown"),
];

/// The grammar for `path`, or plain text rather than a guess.
pub fn language_for_path(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, extension)) = name.rsplit_once('.') else {
        return "text";
    };
    let extension = extension.to_ascii_lowercase();
    LANGUAGE_BY_EXTENSION
        .iter()
        .find(|(known, _)| *known == extension)
        .map_or("text", |(_, language)| language)
}

/// Python 3's built-in functions and types. The bundled grammar colours few
/// of them, and a call to `open()` read like any other name.
const PYTHON_BUILTINS: &str = "abs aiter all anext any ascii bin bool breakpoint bytearray bytes \
callable chr classmethod compile complex delattr dict dir divmod enumerate eval exec filter float \
format frozenset getattr globals hasattr hash help hex id input int isinstance issubclass iter len \
list locals map max memoryview min next object oct open ord pow print property range repr reversed \
round set setattr slice sorted staticmethod str sum super tuple type vars zip";

/// Placed before the bundled patterns, because the first capture of a node
/// wins: which names are functions, which are built-ins, `self`/`cls`, and a
/// decorator read whole, name and all.
fn python_additions() -> String {
    let builtins = PYTHON_BUILTINS
        .split_whitespace()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"((identifier) @variable.special (#any-of? @variable.special "self" "cls"))
(keyword_argument name: (identifier) @variable)
(call function: (identifier) @type.builtin (#any-of? @type.builtin {builtins}))
(call function: (attribute attribute: (identifier) @function))
(call function: (identifier) @function)
(function_definition name: (identifier) @function)
(class_definition name: (identifier) @type)
(decorator) @attribute
(decorator (identifier) @attribute)
(decorator (call function: (identifier) @attribute))
"#
    )
}

/// `this` and `super` read as the language's own names, not as variables.
const SCRIPT_ADDITIONS: &str = "[(this) (super)] @variable.special\n";

/// Registers the additions over the bundled grammars, once per process: the
/// registry is process-wide, and a second pass would prepend them twice.
pub fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(register_additions);
}

fn register_additions() {
    let registry = LanguageRegistry::singleton();
    let prepend = |name: &str, additions: &str, base: Option<&str>| {
        let Some(mut config) = registry.language(name) else {
            return;
        };
        let base = base
            .and_then(|base| registry.language(base))
            .map(|base| base.highlights.to_string())
            .unwrap_or_default();
        config.highlights = format!("{additions}{base}{}", config.highlights).into();
        registry.register(name, &config);
    };
    prepend("python", &python_additions(), None);
    prepend("typescript", SCRIPT_ADDITIONS, None);
    prepend("javascript", SCRIPT_ADDITIONS, None);
    // The bundled TSX query is TypeScript's extension of the JavaScript one,
    // which on its own leaves JSX and every plain name uncoloured.
    prepend("tsx", SCRIPT_ADDITIONS, Some("javascript"));
}

/// The bundled grammar a name resolves to, for tests and the surface.
pub fn grammar(name: &str) -> Option<GrammarConfig> {
    LanguageRegistry::singleton().language(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_extensions_the_app_claims_to_highlight() {
        assert_eq!(language_for_path("/repo/src/main.ts"), "typescript");
        assert_eq!(language_for_path("/repo/src/main.tsx"), "tsx");
        assert_eq!(language_for_path("/repo/Cargo.toml"), "toml");
        assert_eq!(language_for_path("/repo/.github/workflows/ci.yml"), "yaml");
        assert_eq!(language_for_path("/repo/README.MD"), "markdown");
        assert_eq!(language_for_path("/repo/run.sh"), "bash");
    }

    #[test]
    fn falls_back_to_plain_text_rather_than_guessing() {
        assert_eq!(language_for_path("/repo/LICENSE"), "text");
        assert_eq!(language_for_path("/repo/notes.unknown"), "text");
        assert_eq!(language_for_path("/repo.d/Makefile"), "text");
    }

    /// The capture each named token gets, recovered from a resolver that
    /// gives every capture name its own colour.
    fn captures(language: &str, code: &str) -> Vec<(String, String)> {
        use std::sync::Mutex;

        use gpui_kit::{
            HighlightStyle, Hsla,
            component::{Rope, highlighter::SyntaxHighlighter, input::HighlightStyleResolver},
        };

        #[derive(Default)]
        struct Names(Mutex<Vec<String>>);
        impl HighlightStyleResolver for Names {
            fn style(&self, name: &str) -> Option<HighlightStyle> {
                let mut names = self.0.lock().unwrap();
                let index = match names.iter().position(|known| known == name) {
                    Some(index) => index,
                    None => {
                        names.push(name.to_owned());
                        names.len() - 1
                    }
                };
                Some(HighlightStyle {
                    color: Some(Hsla {
                        h: index as f32 / 1000.,
                        s: 1.,
                        l: 0.5,
                        a: 1.,
                    }),
                    ..HighlightStyle::default()
                })
            }
        }

        register();
        let rope = Rope::from(code);
        let mut highlighter = SyntaxHighlighter::new(language);
        highlighter.update(None, &rope, None);
        let names = Names::default();
        let styles = highlighter.styles(&(0..code.len()), &names);
        let names = names.0.lock().unwrap();
        styles
            .into_iter()
            .filter_map(|(range, style)| {
                let index = (style.color?.h * 1000.).round() as usize;
                Some((code[range].trim().to_owned(), names.get(index)?.clone()))
            })
            .filter(|(text, _)| !text.is_empty())
            .collect()
    }

    fn capture_of(language: &str, code: &str, token: &str) -> String {
        let all = captures(language, code);
        all.iter()
            .find(|(text, _)| text == token)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| panic!("{token:?} is not highlighted in {language}: {all:?}"))
    }

    /// Every grammar the table names colours something in a line of its own
    /// language.
    #[test]
    fn every_grammar_highlights() {
        let samples = [
            ("bash", "echo hi # note", "# note", "comment"),
            ("css", "a { color: red; }", "color", "property"),
            ("html", "<div class=\"x\"></div>", "div", "tag"),
            (
                "javascript",
                "function f() { return 1; }",
                "return",
                "keyword",
            ),
            ("json", "{\"a\": 1}", "1", "number"),
            ("markdown", "# Title", "#", "punctuation"),
            ("python", "def main():\n    pass", "def", "keyword"),
            ("rust", "fn main() {}", "fn", "keyword"),
            ("toml", "name = \"x\"", "\"x\"", "string"),
            ("tsx", "const a = <div />;", "const", "keyword"),
            ("typescript", "let x: number = 1;", "number", "type"),
            ("yaml", "key: value # c", "# c", "comment"),
        ];
        for (language, code, token, expected) in samples {
            let name = capture_of(language, code, token);
            assert!(
                name.starts_with(expected),
                "{language}: {token:?} is {name}, expected {expected}"
            );
        }
    }

    #[test]
    fn python_names_functions_where_defined_and_called() {
        assert!(
            capture_of("python", "def main() -> None:\n    pass", "main").starts_with("function")
        );
        assert!(capture_of("python", "ap.add_argument(x)", "add_argument").starts_with("function"));
        assert!(capture_of("python", "class Row:\n    pass", "Row").starts_with("type"));
        let decorated = "@dataclass\nclass A:\n    pass";
        assert_eq!(capture_of("python", decorated, "@dataclass"), "attribute");
    }

    #[test]
    fn python_separates_built_ins_and_self_from_keywords() {
        let line = "for l in open(self.path): print(int(l))";
        assert!(capture_of("python", line, "for").starts_with("keyword"));
        assert_eq!(capture_of("python", line, "open"), "type.builtin");
        assert_eq!(capture_of("python", line, "print"), "type.builtin");
        assert_eq!(capture_of("python", line, "self"), "variable.special");
    }

    #[test]
    fn python_keeps_a_keyword_argument_a_plain_name_even_when_it_spells_a_built_in() {
        let line = "f(type=float, default=3)";
        assert_eq!(capture_of("python", line, "type"), "variable");
        assert!(capture_of("python", line, "3").starts_with("number"));
    }

    #[test]
    fn script_this_and_super_read_as_the_languages_own_names() {
        for language in ["typescript", "javascript", "tsx"] {
            assert_eq!(
                capture_of(language, "this.x = null;", "this"),
                "variable.special"
            );
        }
        assert_eq!(
            capture_of("typescript", "super(props);", "super"),
            "variable.special"
        );
        assert!(capture_of("tsx", "const box = host.getBox();", "getBox").starts_with("function"));
    }

    /// The measured capability surface: growing it is one line here, and
    /// shrinking it has to be argued for.
    #[test]
    fn asks_for_twelve_grammars() {
        let mut names: Vec<_> = LANGUAGE_BY_EXTENSION.iter().map(|(_, l)| *l).collect();
        names.sort();
        names.dedup();
        assert_eq!(
            names,
            [
                "bash",
                "css",
                "html",
                "javascript",
                "json",
                "markdown",
                "python",
                "rust",
                "toml",
                "tsx",
                "typescript",
                "yaml"
            ]
        );
        for name in names {
            assert!(
                grammar(name).is_some_and(|config| config.has_grammar()),
                "{name} has no grammar compiled in"
            );
        }
    }
}
