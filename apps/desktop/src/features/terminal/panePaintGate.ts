/**
 * When a pane's screen reached the DOM, for the surfaces that must not queue
 * their own requests ahead of it.
 *
 * A window switch is one link, and everything the desktop asks for during it
 * shares the bandwidth of the answer the user is actually waiting for. The
 * Explorer's root probe was issued from the *optimistic* switch — before the
 * action was even acked — and it cascades into a directory listing and a Git
 * lease, so a slow link spent its first hundreds of milliseconds carrying a
 * file tree while the terminal the user asked for stayed blank.
 *
 * So: a pane's reveal arms the gate, its first paint disarms it, and anything
 * ordered behind `awaitPanePaint` follows the pane's own screen.
 *
 * A module singleton in the same idiom as `terminalStateCache` and
 * `pendingPaneHandoffs` — the pane that paints and the surface that waits are
 * in different subtrees, and threading a callback between them through the app
 * shell would be a prop for a fact.
 *
 * **This is a timeout, never a barrier.** A pane that never paints — a wedged
 * renderer, a reveal the host never answers, a pane that unmounts on the way —
 * must not also cost the user their file tree, so every wait expires. A pane
 * nobody armed resolves immediately: there is no paint pending, and the caller
 * is behind whatever is already on screen.
 */

/**
 * How long a waiter follows a pane before giving up on it.
 *
 * Long enough to cover a reveal answered over a slow link (the verified
 * baseline is ~360 ms each way for an echo), short enough that a pane which
 * never paints costs a noticeable pause and not a broken Explorer.
 */
export const PANE_PAINT_TIMEOUT_MS = 600;

/**
 * Panes remembered at once.
 *
 * The map is bounded because pane ids are not: a long session opens and closes
 * many, and a paint gate that grows forever is a leak in the one process that
 * has to stay resident. Eviction only forgets that a pane painted, which makes
 * the next wait on it resolve immediately — the safe direction.
 */
const MAX_TRACKED_PANES = 64;

interface PanedPaint {
  /** False between an arm and the paint that answers it. */
  painted: boolean;
  waiters: Set<() => void>;
}

const panePaints = new Map<string, PanedPaint>();

function stateFor(paneId: string): PanedPaint {
  const existing = panePaints.get(paneId);
  if (existing) return existing;
  const created: PanedPaint = { painted: false, waiters: new Set() };
  panePaints.set(paneId, created);
  while (panePaints.size > MAX_TRACKED_PANES) {
    const oldest = panePaints.keys().next().value as string | undefined;
    if (oldest === undefined || oldest === paneId) break;
    // Waiters on an evicted pane are released rather than stranded: the
    // eviction says nothing about that pane's screen, and a promise nobody
    // will ever settle is the one failure this module must not have.
    const evicted = panePaints.get(oldest);
    panePaints.delete(oldest);
    evicted?.waiters.forEach((resolve) => resolve());
  }
  return created;
}

/**
 * A reveal for this pane has been issued; its screen is on its way.
 *
 * Called where the reveal goes out rather than at mount, because that is the
 * request whose answer the paint is: a pane whose reveal is superseded or
 * never issued arms nothing and gates nobody.
 */
export function armPanePaint(paneId: string): void {
  const state = stateFor(paneId);
  state.painted = false;
}

/** This pane's content reached the DOM. Releases whatever waited for it. */
export function notePanePainted(paneId: string): void {
  const state = stateFor(paneId);
  state.painted = true;
  const waiters = [...state.waiters];
  state.waiters.clear();
  waiters.forEach((resolve) => resolve());
}

/**
 * Resolves on the first paint after the current arm, or on the timeout.
 *
 * Never rejects and never waits forever: the caller is being ordered behind
 * the pane, not made conditional on it.
 */
export function awaitPanePaint(paneId: string, timeoutMs = PANE_PAINT_TIMEOUT_MS): Promise<void> {
  const state = panePaints.get(paneId);
  // Unknown or already painted: nothing is pending, so nothing is waited for.
  // The synchronous return matters — an armed pane that painted before this
  // call must not cost the caller a turn of the event loop, let alone a
  // timeout.
  if (!state || state.painted) return Promise.resolve();
  return new Promise((resolve) => {
    let settled = false;
    const settle = () => {
      if (settled) return;
      settled = true;
      state.waiters.delete(settle);
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(settle, timeoutMs);
    state.waiters.add(settle);
  });
}

/** Test seam: forgets every pane, including anything still waiting. */
export function resetPanePaintGate(): void {
  panePaints.forEach((state) => state.waiters.forEach((resolve) => resolve()));
  panePaints.clear();
}
