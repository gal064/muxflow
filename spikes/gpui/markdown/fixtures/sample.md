# Desktop migration to **GPUI**

Replace the [Tauri](https://tauri.app) app with a *native* GPUI app, at feature
parity, on Linux and macOS. The host helper and the `wire protocol` stay as they
are, and a hard-wrapped line like this one renders as one paragraph.

## Terminal data path

Parsing runs **off the main thread** and bytes never cross a serialization
boundary. GPUI repaints at most *once per frame*; 日本語 and café keep their `bytes`.

1. Parse in the `terminal-engine` thread
2. Coalesce damage and notify
   - hidden panes keep their grids
   - a [ledger](docs/gpui-migration-ledger.md) tracks every module
3. Paint with **TerminalElement**

- [x] Pin `gpui-kit` exactly
- [ ] Run the *license* gate

> Flow control stays. The engine becomes the
> acknowledger, after admission.

| Area              | Size   | Fate                 |
|-------------------|:------:|----------------------|
| `apps/desktop`    | 45k    | Rewritten in Rust    |
| **styles.css**    | 2.2k   | Becomes a Rust theme |
| crates \| hosts   |        | Unchanged            |

```rust
fn repaint(damage: Damage) -> Frame {
    damage.coalesce()
}
```

---

Autolink <https://muxflow.dev> and a bare https://example.com link sit beside
`inline code` and ~~old~~ text.
