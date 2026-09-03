# kubide

A code editor for Windows and Linux, written from scratch in Rust: raw Win32
and Direct2D on one, winit and a CPU rasteriser on the other, no UI
framework, no web view, no telemetry, no accounts. Same window, same keys,
same config on both.

On Arch: `pacman -S rust base-devel ttf-jetbrains-mono-nerd`, then
`cargo install --path crates/kubide`. Settings live in `~/.config/kubide`.

cool ide for cool people only

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) first — the rules are short and
they are not arbitrary. MIT licensed. 
