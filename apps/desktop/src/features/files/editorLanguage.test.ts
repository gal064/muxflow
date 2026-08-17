import { describe, expect, it } from "vitest";
import { languageForPath, requestedLanguageIds } from "./editorLanguage";

describe("languageForPath", () => {
  it("maps the extensions the app claims to highlight", () => {
    expect(languageForPath("/repo/src/main.tsx")).toBe("typescript");
    expect(languageForPath("/repo/Cargo.toml")).toBe("ini");
    expect(languageForPath("/repo/.github/workflows/ci.yml")).toBe("yaml");
    expect(languageForPath("/repo/README.MD")).toBe("markdown");
  });

  it("falls back to plain text rather than guessing", () => {
    expect(languageForPath("/repo/LICENSE")).toBe("plaintext");
    expect(languageForPath("/repo/notes.unknown")).toBe("plaintext");
  });

  /**
   * The measured capability surface.
   *
   * The bundled editor ships every language it knows; this is the set the app
   * ever asks for, and it is pinned so that a narrower bundle is a decision
   * with a diff rather than a silent loss of highlighting. Growing the set is
   * one line here; shrinking it has to be argued for.
   */
  it("asks for eleven languages out of everything the editor bundles", () => {
    expect(requestedLanguageIds()).toEqual([
      "css", "html", "ini", "javascript", "json", "markdown",
      "python", "rust", "shell", "typescript", "yaml",
    ]);
  });
});
