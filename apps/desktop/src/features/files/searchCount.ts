import { getSearchQuery, searchPanelOpen } from "@codemirror/search";
import type { EditorState } from "@codemirror/state";
import { ViewPlugin, type EditorView, type ViewUpdate } from "@codemirror/view";

/**
 * Where counting stops, so a one-letter query in a long file does not collect
 * every match it has. The scan itself still reads the whole document.
 */
export const SEARCH_COUNT_LIMIT = 1000;

type Match = { from: number; to: number };

/** The query's matches, up to one past the limit so going over it shows. Empty without a query to count. */
export function searchMatches(state: EditorState): Match[] {
  const query = getSearchQuery(state);
  if (!query.search || !query.valid) return [];
  const matches: Match[] = [];
  const cursor = query.getCursor(state);
  for (let match = cursor.next(); !match.done && matches.length <= SEARCH_COUNT_LIMIT; match = cursor.next()) {
    matches.push(match.value);
  }
  return matches;
}

/**
 * "2 of 5": the match the selection is on, out of how many. "?" when the
 * selection is on none of them, as after typing a query before stepping to a
 * match. Empty without a query to count.
 */
export function searchCountLabel(state: EditorState, matches = searchMatches(state)): string {
  const query = getSearchQuery(state);
  if (!query.search || !query.valid) return "";
  if (matches.length === 0) return "No results";
  const { from, to } = state.selection.main;
  const current = matches.findIndex((match) => match.from === from && match.to === to) + 1;
  const of = matches.length > SEARCH_COUNT_LIMIT ? `${SEARCH_COUNT_LIMIT}+` : String(matches.length);
  return `${current && current <= SEARCH_COUNT_LIMIT ? current : "?"} of ${of}`;
}

/**
 * The count, shown in CodeMirror's own search panel beside its buttons. The
 * panel is not ours to build, so the label is placed into it once it is drawn.
 *
 * Matches are found again only when the document or query changes; moving the
 * selection, every arrow key, only looks itself up among them.
 */
export const searchCount = ViewPlugin.fromClass(class {
  readonly label = document.createElement("span");
  matches: Match[] | undefined;

  constructor(readonly view: EditorView) {
    this.label.className = "cm-search-count";
    this.label.setAttribute("aria-live", "polite");
    this.place();
  }

  update(update: ViewUpdate) {
    const open = searchPanelOpen(update.state);
    if (!open) {
      // Edits made while closed are not tracked; the panel counts afresh when it opens.
      this.matches = undefined;
      return;
    }
    const queryChanged = !getSearchQuery(update.state).eq(getSearchQuery(update.startState));
    if (update.docChanged || queryChanged) this.matches = undefined;
    if (update.docChanged || queryChanged || update.selectionSet || open !== searchPanelOpen(update.startState)) this.place();
  }

  place() {
    // In the write phase, after the update that opens the panel has drawn it.
    this.view.requestMeasure({
      key: this,
      read: () => undefined,
      write: () => {
        const panel = this.view.dom.querySelector(".cm-search");
        if (!panel) return;
        this.matches ??= searchMatches(this.view.state);
        const text = searchCountLabel(this.view.state, this.matches);
        // Rewritten only on a change, or the live region is announced again.
        if (this.label.textContent !== text) this.label.textContent = text;
        if (this.label.parentNode !== panel) panel.querySelector("button[name=select]")?.after(this.label);
      },
    });
  }
});
