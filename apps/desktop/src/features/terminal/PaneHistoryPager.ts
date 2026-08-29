import { requestTerminalHistory, type TerminalEvent } from "./api";
import type { HistoryPageAnchor, TerminalRenderer } from "./TerminalRenderer";
import type { CachedHistoryState } from "./TerminalStateCache";

/**
 * How much scrollback the first request above a screen fetches.
 *
 * A page, not the whole history. Asking for tmux's default `history-limit` of
 * 2,000 lines took ~195 KB per pane and put it on the ordered stream *ahead of
 * the user's next keystroke* — twelve such loads in one drill moved 2.3 MB, and
 * scrolling felt blocked for a moment after every tab switch on Wi-Fi as well
 * as on a slow link. 300 lines is ~30 KB, several screens' worth, and lands
 * inside one frame even at the shaped link's ~70 KB/s.
 *
 * Paging is the whole mechanism: the desktop says how many rows it already
 * holds (`skip`), so repeating this request as the user reaches the top again
 * walks up the history a page at a time and no row is fetched twice. It is what
 * tmux's own copy-mode does — it does not photograph the whole buffer to let
 * you scroll up one screen.
 */
export const HISTORY_PAGE_LINES = 300;

/**
 * The largest page a later request may grow to.
 *
 * Every page is applied by rewriting the whole buffer — xterm has no prepend,
 * so `prependHistory` serializes what is on the terminal and replaces it with
 * the history plus that serialization. The rewrite costs what the buffer
 * weighs, not what the page weighs, so a fixed page size makes N pages O(N^2)
 * bytes through xterm, and past ~256 KB the reset and the content land in
 * different frames, which the user reads as a flicker.
 *
 * So the page doubles as the buffer grows: 300, 600, 1200, 2400, and then this
 * ceiling. A full 10,000-row scrollback is six pages rather than thirty-four,
 * and the first one — the prefetch that rides behind the switch's own paint —
 * stays the small one the link can carry inside a frame. The ceiling keeps the
 * largest single answer near a tenth of the whole-history capture this replaced
 * (~470 KB at the measured ~98 bytes a row), and the host clamps at
 * `MAX_HISTORY_LINES` regardless.
 */
export const HISTORY_MAX_PAGE_LINES = 4_800;

/**
 * The furthest above its screen a pane may ask the host to start.
 *
 * The host's own `MAX_HISTORY_SKIP_LINES`, mirrored here because both ends
 * clamping and neither *ending* is a loop: the host answers a skip past this
 * with the rows at the clamp, so a pane whose tmux `history-limit` is larger
 * than this asks for the same rows on every wheel-up and splices them in again
 * each time. Reaching it is the end of what this protocol can fetch, and the
 * pager latches exhausted on it rather than asking a question whose answer it
 * has already had.
 *
 * Kept as a number rather than plumbed from the host: it is a property of the
 * request this side makes, an old host clamps to exactly this, and a newer one
 * can only clamp lower — either way the pane stops.
 */
export const HISTORY_MAX_SKIP_LINES = 10_000;

/**
 * How many unanswered history requests one pane will remember.
 *
 * The answers carry no request identity, so the pager matches them to its own
 * requests by order — see `#awaiting`. A request the host never answers (a link
 * that dropped under it) would otherwise sit at the head of that queue forever
 * and misattribute every answer after it. The bound is what makes that failure
 * finite: the oldest expectation is dropped, and the worst that costs is one
 * page of scrollback attributed to the request before it — a page whose skip
 * belongs to a different question, so it may repeat rows this buffer already
 * holds. One page, once, on a link that lost eight answers in a row.
 */
export const HISTORY_MAX_AWAITING = 8;

/** Why a page was asked for. Journalled, never branched on. */
export type HistoryPageTrigger = "prefetch" | "scrolledToTop";

/**
 * What the pager needs of the terminal it pages: five members, all of them on
 * the `TerminalRenderer` interface. No xterm and no DOM — the pager is
 * arithmetic over the rows a terminal reports holding.
 */
export type PagerRenderer = Pick<
  TerminalRenderer,
  "scrollbackRows" | "scrollbackLimit" | "grid" | "isAlternateScreenActive" | "prependHistory"
>;

export interface PaneHistoryPagerOptions {
  paneId: string;
  renderer: PagerRenderer;
  /** Read at the moment of the request, because a pane outlives a connection. */
  clientId(): string | undefined;
  journal(kind: string, detail: Record<string, unknown>): void;
  /** Seam for the tests; production passes nothing and gets the real request. */
  request?: typeof requestTerminalHistory;
}

/** One expectation: the request that is outstanding, and the buffer it describes. */
interface HistoryRequest {
  serial: number;
  /**
   * The buffer this page was asked for: how much scrollback the pane held, and
   * the grid a row was measured at.
   *
   * Read at the request and quoted back at the splice, because the answer is
   * read against it twice. The rows requested are the skip plus the page, and
   * tmux's own history size says whether that reached the top; and the skip is
   * what tells the splice how much of the page the pane printed over while it
   * was on the wire, because tmux measures its capture from the display at the
   * moment it runs rather than at the moment it was asked.
   */
  anchor: HistoryPageAnchor;
  lines: number;
  /** Cleared when the screen this page was asked against is replaced. */
  current: boolean;
  /** What replaced it, once it has been. Journalled when the answer arrives. */
  orphanedBy?: OrphanReason;
}

/** Why a page in flight stopped describing the buffer it was asked against. */
type OrphanReason =
  /** A seed replaced the screen. */
  | "reseed"
  /** The screen went away without one: a blank pane, or a host-owned restore. */
  | "screenGone"
  /** The terminal reflowed, moving rows across tmux's history boundary. */
  | "gridChanged";

/**
 * One pane's walk up its own scrollback.
 *
 * A screen-only seed is the visible grid and nothing above it, so the history
 * a pane used to be given unasked is now fetched a page at a time: once behind
 * the seed's first paint, and again each time the user reaches the top. This
 * holds the whole of that protocol — what is on the wire, what has been
 * spliced, how large the next page is, and when to stop — so that the pane
 * component holds a pager rather than seven interdependent `let`s.
 *
 * Per pane and per mount. What survives a mount is exactly
 * [`CachedHistoryState`], which is a fact about the *bytes* a hidden pane kept
 * rather than about the terminal that produced them: a screen restored onto a
 * fresh xterm is showing scrollback it has already fetched, and must continue
 * above it rather than fetch it twice.
 */
export class PaneHistoryPager {
  readonly #paneId: string;
  readonly #renderer: PagerRenderer;
  readonly #clientId: () => string | undefined;
  readonly #journal: (kind: string, detail: Record<string, unknown>) => void;
  readonly #request: typeof requestTerminalHistory;

  /**
   * Whether this pane is showing a screen with unfetched history above it.
   *
   * A seed is a photograph of the grid, so its scrollback is something to ask
   * for; a screen restored from this side's own cache, or resumed onto the one
   * the renderer kept, is showing a buffer that already carries whatever
   * history it had, and a host-owned serialization carries its own scrollback.
   */
  #screenSeeded = false;
  /**
   * Whether there is anything left above this screen to ask for.
   *
   * Set by tmux's own `history_size` saying the last page reached the top, or
   * by either ceiling this side cannot page past — `HISTORY_MAX_SKIP_LINES`
   * and the renderer's own `scrollbackLimit`. Never inferred from how many rows
   * an answer happened to carry.
   */
  #exhausted = false;
  /** How many lines the next page asks for. Grows with the buffer it rewrites. */
  #nextPageLines = HISTORY_PAGE_LINES;
  /** Per-pane, per-mount request numbering. Journalled, never matched on. */
  #serial = 0;
  /**
   * The requests whose answers have not arrived, oldest first.
   *
   * There is only ever one page *outstanding* — `#busy` is that rule — but a
   * page can outlive the screen it was asked against: a watchdog reseed, or the
   * host's own `emit_resnapshot` after it rejected a block, replaces the screen
   * while the answer is still on the wire. That answer still arrives (the hub
   * delivers history outside its seed-debt ladder), and before this queue
   * existed it was spliced above the *new* screen using a skip the post-reseed
   * prefetch had already overwritten, and then cleared the newer request's
   * latch — so a third request fetched rows the buffer already held and showed
   * them twice.
   *
   * Answers arrive in the order the requests went out, so the head of this queue
   * is whose answer this is. A request the screen outlived is marked
   * `current: false` and its answer is journalled and dropped, touching nothing
   * the live request owns.
   */
  #awaiting: HistoryRequest[] = [];
  /**
   * Whether a page is on the wire or being spliced. One at a time: a wheel-up
   * during the prefetch, or three of them in a row, is the same question.
   */
  #busy = false;
  /**
   * Whether a splice is between its request and its answer.
   *
   * The renderer's rewrite waits on a barrier, so `prependHistory` outlives the
   * call: until it settles this buffer is about to be replaced, and `#busy` must
   * not be cleared under it. The two paths that *do* clear it say so explicitly,
   * because they are the ones that take the barrier away with the queue it was
   * sitting in — and a splice whose barrier was dropped never answers.
   */
  #splicing = false;
  #disposed = false;

  constructor(options: PaneHistoryPagerOptions) {
    this.#paneId = options.paneId;
    this.#renderer = options.renderer;
    this.#clientId = options.clientId;
    this.#journal = options.journal;
    this.#request = options.request ?? requestTerminalHistory;
  }

  /**
   * Adopts what a restored screen says about its own history.
   *
   * The pages already spliced are *in* those bytes, so the next request
   * continues above them — which is what the row count the renderer reports as
   * `skip` does — and a pane that already reached the top of tmux's history
   * must not go asking for it a second time.
   */
  restore(state: CachedHistoryState): void {
    this.#screenSeeded = state.screenSeeded;
    this.#exhausted = state.historyExhausted;
    // Zero is an entry that says nothing about the ladder — one written before
    // this was carried, or a screen nobody paged — and the next page above such
    // a screen is the first size.
    this.#nextPageLines = state.historyNextPageLines || HISTORY_PAGE_LINES;
  }

  /** What this pane's current screen should carry into the cache on a hide. */
  snapshot(): CachedHistoryState {
    return {
      screenSeeded: this.#screenSeeded,
      historyExhausted: this.#exhausted,
      historyNextPageLines: this.#nextPageLines,
    };
  }

  /**
   * A seed replaced the screen: its scrollback is once again something to fetch
   * rather than something the pane holds.
   *
   * The ladder starts over with the buffer it is sizing — this screen holds
   * nothing above it, so its first page is the small one again — and any page
   * in flight is orphaned, which both lets this screen ask its own first
   * question straight away and keeps the answer, when it arrives, from being
   * spliced above a screen it never sat above.
   */
  noteScreenSeeded(): void {
    this.#screenSeeded = true;
    this.#exhausted = false;
    this.#nextPageLines = HISTORY_PAGE_LINES;
    // The seed dropped everything queued for this terminal, the splice barrier
    // included, so a splice in flight will never answer and must not hold the
    // latch shut waiting for it.
    this.#splicing = false;
    this.#orphan("reseed");
  }

  /**
   * The screen this pager was paging is gone, and what replaces it is not a
   * screen-only seed: a blank awaiting one, or a host-owned serialization that
   * carries its own scrollback.
   *
   * Either way there is nothing here to splice above until a seed says
   * otherwise, and any page on the wire belongs to the screen that went away.
   */
  noteScreenGone(): void {
    this.#screenSeeded = false;
    // Same as a reseed: whatever replaced this screen dropped the queue and the
    // barrier with it.
    this.#splicing = false;
    this.#orphan("screenGone");
  }

  /**
   * The terminal reflowed.
   *
   * A resize moves rows across the boundary between what tmux keeps in its
   * history and what it shows on its display, so the `skip` a page in flight
   * was asked with no longer names where this buffer begins — and unlike
   * ordinary output, the difference is not rows this side has gained, so the
   * overlap the splice trims cannot repair it. The page is orphaned, the latch
   * reopens, and the next reach-the-top asks with numbers that describe the
   * buffer the user is now looking at. The screen itself is untouched: it is
   * the same screen, rewrapped, and everything already spliced into it is still
   * above it.
   */
  noteGridChanged(): void {
    this.#orphan("gridChanged");
  }

  /**
   * Every page still in flight belonged to a screen that is gone.
   *
   * The orphaned requests stay in `#awaiting` because the host will still answer
   * them and the queue is how the answers are told apart.
   */
  #orphan(reason: OrphanReason): void {
    for (const request of this.#awaiting) {
      request.current = false;
      request.orphanedBy = reason;
    }
    // Never under a splice. The rewrite is still going to land, and a page asked
    // for against the buffer as it stands now would quote a skip the rewrite is
    // about to invalidate. The splice's own answer releases it.
    if (!this.#splicing) this.#busy = false;
  }

  /**
   * Asks for the page of scrollback immediately above what this pane holds.
   *
   * One request at a time, and one page at a time. The latches are the whole
   * protocol: `#screenSeeded` says there is anything above this screen to ask
   * for, `#exhausted` says there is nothing further above it, and `#busy` says
   * a page is on the wire — so a wheel-up during the prefetch, or three of them
   * in a row, is the same question and costs nothing.
   *
   * Refused outright on the alternate screen: a TUI's frame has no scrollback
   * to prepend to, `prependHistory` would refuse the splice, and the answer — a
   * whole page of it — would have crossed the link to be thrown away.
   */
  requestPage(trigger: HistoryPageTrigger): void {
    if (this.#disposed || !this.#screenSeeded || this.#exhausted || this.#busy) return;
    if (this.#renderer.isAlternateScreenActive()) return;
    const clientId = this.#clientId();
    if (!clientId) return;
    // Everything above the screen that this terminal already holds — the pages
    // already spliced in included, because they are part of this buffer now.
    // This is what anchors the answer: tmux measures its capture from the
    // pane's current display, so the rows it returns end exactly where this
    // buffer begins, and output printed while the page is on the wire lands
    // below them rather than between them.
    const skip = this.#renderer.scrollbackRows;
    // The end of what this protocol can reach, which is not the same as the top
    // of tmux's history and is the only thing that ends paging when the two
    // disagree. A pane whose `history-limit` is larger than either ceiling never
    // satisfies `skip + page >= history_size`: the host clamps the skip it is
    // given, xterm drops rows off the top of the buffer as new ones are spliced
    // in, and every wheel-up re-fetches the same clamped rows and shows them
    // again. Latched here so the gesture stops asking.
    const rendererLimit = this.#renderer.scrollbackLimit;
    const heldAllItCan = Number.isFinite(rendererLimit) && skip >= rendererLimit;
    if (skip >= HISTORY_MAX_SKIP_LINES || heldAllItCan) {
      this.#exhausted = true;
      this.#journal("pane.historyCapped", { paneId: this.#paneId, trigger, skip, rendererLimit });
      return;
    }
    const lines = this.#nextPageLines;
    const grid = this.#renderer.grid;
    const request: HistoryRequest = {
      serial: (this.#serial += 1),
      anchor: { skip, columns: grid.columns, rows: grid.rows },
      lines,
      current: true,
    };
    this.#busy = true;
    this.#awaiting.push(request);
    // Dropped from the front, because the head is the expectation a lost answer
    // stranded and everything behind it is legitimate.
    while (this.#awaiting.length > HISTORY_MAX_AWAITING) {
      const dropped = this.#awaiting.shift();
      this.#journal("pane.historyExpectationDropped", { paneId: this.#paneId, serial: dropped?.serial ?? 0 });
    }
    void this.#request(clientId, this.#paneId, lines, skip).catch((error) => {
      // The request never went out, so no answer will ever come for it: it
      // leaves the queue rather than shifting every later answer by one. The
      // latch reopens only if this is still the page the pane is waiting for —
      // a seed since then has already reopened it for its own screen.
      this.#awaiting = this.#awaiting.filter((awaited) => awaited !== request);
      if (request.current) this.#busy = false;
      // Journalled rather than spoken: nothing on screen is wrong, and the pane
      // is showing everything it has.
      this.#journal("pane.historyRequestFailed", {
        paneId: this.#paneId,
        trigger,
        error: String(error).slice(0, 200),
      });
    });
  }

  /** One page, answering the oldest request this pager has outstanding. */
  receive(event: Extract<TerminalEvent, { kind: "terminalHistory" }>): void {
    // Whose answer this is. The host echoes nothing that identifies the request,
    // and it does not have to: one page is outstanding at a time and the answers
    // come back in the order the requests went out, so the head of the queue is
    // this one's.
    const request = this.#awaiting.shift();
    if (!request) {
      // A page nobody outstanding asked for — a duplicate answer, or one for a
      // request this mount never made. Nothing here can place it.
      this.#journal("pane.historyUnrequested", { paneId: this.#paneId });
      return;
    }
    // The screen this scrollback belongs above is gone — a reseed replaced it
    // while the page was on the wire, the pane is waiting for a seed, or the
    // terminal reflowed under it.
    // Splicing it onto whatever is there now would put the user's earlier output
    // above a screen it never sat above, using a skip that describes a buffer
    // nothing is holding any more. Deliberately touching no latch: whatever
    // asked for the *current* screen's page is still waiting for its own
    // answer, and clearing its latch here is what let a third request duplicate
    // rows.
    if (!request.current || !this.#screenSeeded) {
      this.#journal("pane.historySupersededByReseed", {
        paneId: this.#paneId,
        serial: request.serial,
        reason: request.orphanedBy ?? "screenGone",
      });
      return;
    }
    // Whether this page reached the top of tmux's history. The rows asked for
    // were the skip this pane already held plus the page it asked for, so
    // anything at or past what tmux is holding is the end of it. Both numbers
    // come from the request rather than from the pager's current state, because
    // the page size grows and the skip has moved on.
    //
    // Never inferred from the answer's own rows. The capture runs without `-J`,
    // so those rows do count in the same unit as everything else here — but an
    // emptier answer still says nothing, because tmux clamps a range that runs
    // past the top of its history and answers one entirely above it with a
    // single row rather than with nothing.
    //
    // An absent size is the host's probe going unanswered, which leaves the
    // question open rather than closing it: this page is spliced like any other
    // and the next reach-the-top asks again.
    const lastPage = event.historySize !== undefined
      && request.anchor.skip + request.lines >= event.historySize;
    // Nothing above this screen after all. Nothing to splice — a rewrite of the
    // whole buffer to add no rows is a frame the user pays for and does not see
    // — but the size still decides whether to ask again.
    if (event.data.byteLength === 0) {
      this.#busy = false;
      this.#exhausted = lastPage;
      return;
    }
    // Latched on what happened, never on the attempt. The splice waits for xterm
    // to finish with what it is already holding and can still be refused there;
    // the ask stays outstanding until it answers, so reaching the top meanwhile
    // does not queue a second one. A refusal leaves the latch open on purpose —
    // the buffer could not take these rows, and the next time the user reaches
    // the top the question is asked against the screen they are looking at.
    // Held across the splice as well as across the wire. A rewrite of this
    // buffer is under way from here until the promise settles, and a page asked
    // for meanwhile would quote a skip that is about to be wrong.
    this.#splicing = true;
    void this.#renderer.prependHistory(event.data, request.anchor).then((outcome) => {
      this.#splicing = false;
      // A reseed or a reflow during the splice orphans this request as surely
      // as one during the wire time: it is the new screen that owns the paging
      // state now. The latch it was not allowed to clear mid-splice is released
      // here instead, now that nothing is touching the buffer.
      if (!request.current) {
        this.#busy = false;
        return;
      }
      if (outcome === "applied") {
        this.#exhausted = lastPage;
        // The next page pays for rewriting a buffer this one just grew, so it
        // fetches proportionally more of what it is paying for.
        this.#nextPageLines = Math.min(request.lines * 2, HISTORY_MAX_PAGE_LINES);
      }
      this.#busy = false;
    });
  }

  /**
   * The pane this pager belongs to is unmounting. Asks for nothing more.
   *
   * Answers still on the wire are simply never delivered — the pane's
   * subscription is gone with it — so there is nothing to unwind here.
   */
  dispose(): void {
    this.#disposed = true;
  }
}
