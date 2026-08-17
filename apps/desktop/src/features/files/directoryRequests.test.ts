import { describe, expect, it } from "vitest";
import { DirectoryRequests } from "./directoryRequests";

describe("DirectoryRequests", () => {
  it("supersedes a read of the same kind and stops the one it replaced", () => {
    const requests = new DirectoryRequests();
    const first = requests.open("/repo", "list");
    const second = requests.open("/repo", "list");
    expect(first.signal.aborted).toBe(true);
    expect(first.current()).toBe(false);
    expect(second.current()).toBe(true);
  });

  /**
   * A page read extends a listing rather than replacing one, so nothing
   * supersedes it and it supersedes nothing. Keying only on the path made every
   * "Load more" click silently abandon a recovery in flight for that directory.
   */
  it("leaves a page read alone in both directions", () => {
    const requests = new DirectoryRequests();
    const page = requests.open("/repo", "page");
    const list = requests.open("/repo", "list");
    expect(page.signal.aborted).toBe(false);
    expect(page.current()).toBe(true);
    expect(list.current()).toBe(true);
  });

  /**
   * The rule the module's doc now states, pinned so the doc and the code cannot
   * drift apart again: `"list"` and `"restore"` both answer "what does this
   * directory contain now", so the later question wins.
   */
  it("lets a list and a restore supersede each other, later wins", () => {
    const requests = new DirectoryRequests();
    const restore = requests.open("/repo", "restore");
    const refresh = requests.open("/repo", "list");
    expect(restore.current()).toBe(false);
    expect(refresh.current()).toBe(true);

    const later = requests.open("/repo", "restore");
    expect(refresh.current()).toBe(false);
    expect(later.current()).toBe(true);
  });

  /**
   * The exact failure this answers: a directory whose watch was refused gets a
   * fallback list, and `sync` re-enters `onDeferred` on every expand and every
   * collapse anywhere in the tree. Guarding on "has the tree got a listing yet"
   * cannot see a read that is still fetching — it has installed nothing — so
   * each interaction re-issued the read and aborted the one already in flight.
   * On the remote link that starves the directory indefinitely while paying for
   * a cancelled round trip per keystroke.
   */
  it("reports a read of one kind in flight without conflating it with another", () => {
    const requests = new DirectoryRequests();
    expect(requests.reading("/repo", "list")).toBe(false);

    const slot = requests.open("/repo", "list");
    expect(requests.reading("/repo", "list")).toBe(true);
    expect(requests.reading("/repo", "restore")).toBe(false);
    expect(requests.reading("/other", "list")).toBe(false);

    slot.close();
    expect(requests.reading("/repo", "list")).toBe(false);
  });

  it("stops reporting a read the tree abandoned wholesale", () => {
    const requests = new DirectoryRequests();
    requests.open("/repo", "list");
    requests.abort(() => true);
    expect(requests.reading("/repo", "list")).toBe(false);
  });

  it("keeps a superseded read's replacement in flight rather than both", () => {
    const requests = new DirectoryRequests();
    const first = requests.open("/repo", "list");
    requests.open("/repo", "list");
    // The replaced slot must not unregister the one that replaced it.
    first.close();
    expect(requests.reading("/repo", "list")).toBe(true);
  });
});
