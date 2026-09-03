# kubide

A code editor for Windows and Linux, written from scratch in Rust: raw Win32
and Direct2D on one, winit and a CPU rasteriser on the other, no UI
framework, no web view, no telemetry, no accounts. Same window, same keys,
same config on both.

cool ide for cool people only

## Installing on Arch Linux

```sh
sudo pacman -S --needed rust base-devel git ttf-jetbrains-mono-nerd
git clone https://github.com/kubilaiswf/kubide
cd kubide
cargo install --path crates/kubide      # puts `kubide` in ~/.cargo/bin
kubide                                  # opens the project the shell is in
```

- `base-devel` is for the C compiler the tree-sitter grammars need; the
  window, text and clipboard layers are pure Rust and open X11 or Wayland at
  run time, so no `-dev` packages.
- The Nerd Font gives the file tree its icons; without one the tree falls
  back to plain markers, as on Windows. Any family from `[font]` in the
  config works.
- X11 sessions need `libxkbcommon-x11` (installed with every X desktop).
  Wayland needs nothing extra.
- Settings, themes, snippets and sessions live in `~/.config/kubide`. Copy
  `config.example.toml` there as `config.toml` to start.
- Blur behind the window is the compositor's job: Hyprland and KWin blur
  windows with alpha when configured to, GNOME draws them plain.

## Installing on Windows

```powershell
git clone https://github.com/kubilaiswf/kubide
cd kubide
cargo install --path crates/kubide
```

Needs the MSVC toolchain and Visual Studio Build Tools with **Desktop
development with C++**. Settings live in `%APPDATA%\kubide`.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) first — the rules are short and
they are not arbitrary. MIT licensed. 
