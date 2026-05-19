# tobyscope-x11

`tobyscope-x11` is an X11 overview/expose tool for i3wm. It opens a fullscreen
override-redirect overlay, shows i3 windows across normal workspaces in a
WM-style grid, draws real thumbnails through XComposite/GetImage with a PNG
cache, and focuses the selected window through i3 IPC.

This is X11-only. It is not a Wayland compositor tool.

## Install On Debian

The first stable release is distributed as a public GitHub Release tarball.
The repository and release must be public for these URLs to work without GitHub
permissions.

```bash
version=v0.1.1
curl -LO "https://github.com/zJairO/tobyscope-x11/releases/download/${version}/tobyscope-x11-linux-x86_64.tar.gz"
curl -LO "https://github.com/zJairO/tobyscope-x11/releases/download/${version}/SHA256SUMS"
sha256sum -c SHA256SUMS
tar -xzf tobyscope-x11-linux-x86_64.tar.gz
install -Dm755 tobyscope-x11-linux-x86_64/tobyscope-x11 ~/.local/bin/tobyscope-x11
```

Make sure `~/.local/bin` is in your `PATH`.

Optional user config:

```bash
install -Dm644 tobyscope-x11-linux-x86_64/config.example.toml ~/.config/tobyscope-x11/config.toml
```

System-wide install alternative:

```bash
sudo install -Dm755 tobyscope-x11-linux-x86_64/tobyscope-x11 /usr/local/bin/tobyscope-x11
```

## Build From Source

Debian's packaged Rust can be too old for Rust edition 2024. Use `rustup` unless
your system Rust is current.

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libx11-dev libxcomposite-dev libxrender-dev libxdamage-dev libxfixes-dev libcairo2-dev libpango1.0-dev curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
cargo build --release --locked
install -Dm755 target/release/tobyscope-x11 ~/.local/bin/tobyscope-x11
```

Docker build/check environment:

```bash
docker build -t tobyscope-x11-dev .
docker run --rm -v "$PWD:/work" -w /work tobyscope-x11-dev cargo check
```

Docker is intended for compilation checks. Runtime testing should normally
happen natively on the host X11/i3 session.

## Usage

```bash
tobyscope-x11
tobyscope-x11 --debug
tobyscope-x11 --list-windows --debug
tobyscope-x11 --config ~/.config/tobyscope-x11/config.toml
```

The first run may take longer because it visits workspaces behind the overlay to
capture real thumbnails. Later runs paint cached thumbnails from
`$XDG_CACHE_HOME/tobyscope-x11/` immediately. By default, cached thumbnails are
refreshed when missing or older than 10 minutes.

Controls:

- Arrow keys or `h/j/k/l`: move selection.
- Enter: focus the selected window.
- Mouse hover: select a window.
- Left click: focus the clicked window.
- Escape: close without changing focus.

## i3 Binding

Add this to your i3 config:

```i3
bindsym $mod+space exec --no-startup-id tobyscope-x11
```

Then reload i3:

```bash
i3-msg reload
```

## Configuration

Default config path:

```text
$XDG_CONFIG_HOME/tobyscope-x11/config.toml
```

If `XDG_CONFIG_HOME` is unset, this is normally:

```text
~/.config/tobyscope-x11/config.toml
```

Start from the example:

```bash
install -Dm644 config.example.toml ~/.config/tobyscope-x11/config.toml
```

Example:

```toml
[ui]
font = "Iosevka Term 11"
show_overlay_background = true
show_workspace_number = true
show_program_name = true
rounded_corners = false
corner_radius = 14
shadows = false
shadow_offset_x = 0
shadow_offset_y = 8
shadow_radius = 18

[colors]
background = "#101418"
cell = "#202832"
cell_hover = "#2b3642"
border = "#5f6f7f"
selected = "#4ea1ff"
text = "#e8edf2"
muted = "#95a3b2"
error = "#66303a"
empty = "#151b21"
shadow = "#05080c"

[layout]
margin = 24
gap_small = 12
gap = 18
padding = 12
top_meta_height = 40
label_height = 34

[thumbnails]
refresh_after_seconds = 600
max_cache_edge = 960
```

Notes:

- `font` is a Pango/Fontconfig description, for example `Iosevka Term 11`.
  Polybar-style values such as `Iosevka Term:size=11;2` are accepted too.
- Colors must be `#RRGGBB`.
- Partial configs are allowed; missing values use built-in defaults.
- `show_overlay_background = false` avoids painting the dark fullscreen
  background. The overlay is still an X11 window, so compositor behavior can
  vary.
- `show_workspace_number = false` removes the workspace number and gives that
  space back to previews.
- `show_program_name = false` removes the lower app label and gives that space
  back to previews.
- `rounded_corners = true` draws rounded overview cards and thumbnails so the
  overview matches a picom setup with rounded corners.
- `corner_radius` controls that internal overview radius in pixels. Picom can
  round top-level X11 windows, but the overview cards are drawn inside one
  overlay window, so Tobyscope handles those rounded corners itself.
- `shadows = true` draws internal shadows behind overview cards for setups that
  use picom shadows. `shadow_offset_x`, `shadow_offset_y`, `shadow_radius`, and
  `colors.shadow` control the look.

## picom

The overlay sets `WM_CLASS` to `tobyscope-x11`. Exclude it from effects that can
make overview animations flicker:

```conf
shadow-exclude = [
  "class_g = 'tobyscope-x11'"
];

blur-background-exclude = [
  "class_g = 'tobyscope-x11'"
];

fade-exclude = [
  "class_g = 'tobyscope-x11'"
];
```

Adapt the rule to your existing picom config style if you already have these
lists.

## Release Maintainers

To publish a release:

```bash
git status --short
cargo check
cargo build --release --locked
git tag v0.1.1
git push origin v0.1.1
```

The GitHub Actions release workflow builds the Linux x86_64 tarball, creates
`SHA256SUMS`, and uploads both files to the GitHub Release for the tag.

## Troubleshooting

- `DISPLAY is not set`: run inside an X11 session. Wayland-only sessions are
  not supported.
- Missing `Composite` or `Render`: the current X server does not expose the
  required X11 extensions.
- `--list-windows --debug` should list windows from all i3 workspaces. If i3 IPC
  is unavailable, the program falls back to currently visible X11 clients and
  prints a warning in debug mode.
- Per-window `preview error`: the server rejected the XComposite/GetImage path
  for that window. Run with `--debug` to see the exact X11 error.
- Invalid config files fail fast with the config path and the TOML field that
  could not be parsed.
