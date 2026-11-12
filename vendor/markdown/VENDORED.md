# markdown 1.0.0

Vendored from crates.io, unmodified except for one change in
`src/construct/list_item.rs` and this file. Only what the crate needs to build
is kept: `src/`, `Cargo.toml`, `README.md` and `license`. Patched in through
`[patch.crates-io]` in the root `Cargo.toml`, so both our `live-markdown` and
gpui-base's Markdown preview use it.

**Why:** 1.0.0 panics in `to_mdast` ("mismatched (non-jsx): ListUnordered /
ListOrdered") on a list item that ends in unclosed fenced code followed by
another item, for example `"  - a10.\n  - ```\n   2. \n   2. "`. Typing reaches
such text in Live, and opening it in Preview or Split crashed the app, again on
every relaunch while the tab was restored. No released version fixes it (1.0.0
is the latest as of this change).

**Change:** the list item resolver found an item's exit with
`skip::opt(.., [ListItem])`, which also skips a next item that starts right at
that exit (unclosed code keeps the line ending inside it). It now walks to the
item's own exit with a balance loop. This is upstream's open pull request
https://github.com/wooorm/markdown-rs/pull/267, applied as is to `src/`; the
change is marked by a comment at the site.

**Remove it** (this directory, the `[patch.crates-io]` entry and the `exclude`
entry in the root `Cargo.toml`, and its line in `THIRD_PARTY_PATHS` in
`tests/release/check-repo-privacy.py`, which lets upstream's addresses
stand) when a released `markdown` contains that fix.

Upstream code is MIT, see `license`.
