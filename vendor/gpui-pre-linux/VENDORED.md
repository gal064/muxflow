# gpui-pre-linux 0.3.11

Vendored from the crates.io release, under Apache-2.0 (see `LICENSE-APACHE`).
The workspace patches this exact version; the gpui-kit pin is unchanged.

The only code changes remove the KDE blur fallback in
`src/linux/wayland/client.rs` and `window.rs`, and remove the
`wayland-protocols-plasma` dependency from `Cargo.toml`. Its generated blur
bindings originate in a non-permissively licensed protocol definition, which
is not included here. Muxflow uses opaque windows and does not request blur.
Normal Wayland and X11 support and the standard background-effects protocol
remain available.

Remove this patch when the pinned upstream backend no longer requires those
bindings, or allows disabling them independently of Wayland support.
