# gpui-pre-linux 0.3.12

Vendored from the crates.io release, under Apache-2.0 (see `LICENSE-APACHE`).
The workspace patches this exact version, the one `gpui-kit` 0.10.0 pins.

There are two code changes.

The first removes the KDE blur fallback in `src/linux/wayland/client.rs` and
`window.rs`, and removes the `wayland-protocols-plasma` dependency from
`Cargo.toml`. Its generated blur bindings originate in a non-permissively
licensed protocol definition, which is not included here. Muxflow uses opaque
windows and does not request blur. Normal Wayland and X11 support and the
standard background-effects protocol remain available. Drop it when the pinned
upstream backend no longer requires those bindings, or allows disabling them
independently of Wayland support.

The second lets queued foreground work run before an X11 window's next frame
(`FramePacing` in `src/linux/x11/client.rs`, counted by `QueuedRunnables` in
`src/linux/dispatcher.rs`). Upstream draws as soon as the refresh timer or an
Expose event fires, ahead of runnables that arrived during the previous frame.
With software rendering a frame takes hundreds of milliseconds, so each step of
the app's startup waited a whole frame. A frame is held only while runnables
are queued, and for at most half the last frame's cost (4 to 250 ms). Drop it
when upstream runs queued work before drawing.
