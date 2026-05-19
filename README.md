# tobyscope-x11

`tobyscope-x11` is an X11 overview/expose MVP for i3wm. It opens a fullscreen
override-redirect overlay, shows i3 windows across all normal workspaces in a
WM-style grid, draws real thumbnails through XComposite/GetImage with a PNG
cache, and focuses the selected window through i3 IPC.

This is X11-only. It is not a Wayland compositor tool.

## Build

Native build requirements on Debian/Ubuntu:

```bash
sudo apt install build-essential pkg-config libx11-dev libxcomposite-dev libxrender-dev libxdamage-dev libxfixes-dev
cargo build --release
```

Docker build/check environment:

```bash
docker build -t tobyscope-x11-dev .
docker run --rm -v "$PWD:/work" -w /work tobyscope-x11-dev cargo check
```

Running the overview should normally happen natively on the host X11 session:

```bash
./target/release/tobyscope-x11
```

Diagnostics:

```bash
cargo run -- --list-windows --debug
cargo run -- --debug
```

The first run may take longer because it visits workspaces behind the overlay to
capture real thumbnails. Later runs paint cached thumbnails from
`$XDG_CACHE_HOME/tobyscope-x11/` immediately. To avoid visible flashing, cached
thumbnails are refreshed only when missing or older than roughly 10 minutes.

## i3 binding

After building a release binary, bind it in your i3 config:

```i3
bindsym $mod+space exec --no-startup-id /home/zjairo/Projects/OpenSource/tobyscope-x11/target/release/tobyscope-x11
```

Then reload i3:

```bash
i3-msg reload
```

## Controls

- Arrow keys or `h/j/k/l`: move selection.
- Enter: focus the selected window.
- Mouse hover: select a window.
- Left click: focus the clicked window.
- Escape: close without changing focus.

## Troubleshooting

- `DISPLAY is not set`: run inside an X11 session. Wayland-only sessions are not supported.
- Missing `Composite` or `Render`: the current X server does not expose the required X11 extensions.
- `--list-windows --debug` should list windows from all i3 workspaces. If i3 IPC is unavailable, the program falls back to currently visible X11 clients and prints a warning in debug mode.
- Per-window `preview error`: the server rejected the XComposite/GetImage path for that window. Run with `--debug` to see the exact X11 error.
- For picom rules, the overlay sets `WM_CLASS` to `tobyscope-x11`; use `class_g = 'tobyscope-x11'` to exclude shadows, rounded corners, blur, or fading.
- Docker is intended for compilation checks. Full runtime testing from Docker requires access to the host X socket and matching `DISPLAY`, for example with `/tmp/.X11-unix` mounted and appropriate `xhost` permissions.
