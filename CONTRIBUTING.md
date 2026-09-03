# Contributing

Read this before opening a PR. It's short on purpose.

## Three words

kubide is **native, light, yours**. They settle most arguments:

- **Native** — raw Win32 + Direct2D, nothing in between. UI frameworks,
  web views, embedded browsers: closed on sight.
- **Light** — binary size and idle memory are tracked numbers. A new crate
  must say what it buys and why writing it by hand is worse.
- **Yours** — no telemetry, no cloud, no accounts, no auto-update, no AI.
  Not a preference; the definition of the project.

If your PR fights one of these words, it loses.

## Code

- Comments explain **why**, never what. Which Windows quirk forced the
  line, what was tried and abandoned, where the magic number came from.
  Cite the KB/issue/doc for platform quirks.
- Everything is English — comments, identifiers, docs, commits.
- Pure logic gets tests. `kb-ui` is dependency-free on purpose — if your
  change can be verified without opening a window, write the test.

## Commits

Kernel style. Look at `git log` and match it:

```
draw: keep the caret visible while the pane scrolls

The caret was drawn before the scroll offset was applied, so any
scrolled pane painted it one line off. Apply the offset first.

Signed-off-by: Your Name <you@example.com>
```

- Subject: `area: what the change does` — lowercase, imperative, no
  period, under 72 characters. The area is the crate or file carrying
  the change (`draw`, `folders`, `kb-ui`, `kb-term`, `config`, …).
- Body: the **why**, wrapped at 72 columns. Short is fine; missing is
  not, unless the diff truly speaks for itself.
- Sign off your work (`git commit -s`). It says you have the right to
  submit it under MIT.
- One commit, one change. One PR, one topic. Open an issue before
  starting anything large.

## Before you send it

```powershell
cargo build --release
cargo clippy --release --all-targets    # zero warnings
cargo test
cargo run -p kb-term --example dump     # terminal layer sanity check
```

Then **run the app**. A clean build proves nothing here — a bug that
broke scrollback entirely compiled without a warning and was only caught
on screen. Touched anything visual? Attach a screenshot.

## Bug reports

Windows version and build (`winver`), GPU and driver, display scaling
and refresh rate, **keyboard layout** — shortcut bugs are
layout-dependent more often than you'd think — and a screenshot if it's
visual.

## Environment

- Rust 1.97+, `x86_64-pc-windows-msvc`
- Visual Studio Build Tools, **Desktop development with C++**
- Windows 11 build 22621+
- Linux (Arch, any X11 or Wayland desktop): Rust 1.97+ and a C compiler.
  The window is winit, the surface is tiny-skia over shared memory, text
  is cosmic-text; everything visible is still drawn by `draw.rs`, so a
  change there lands on both. Check both builds before sending.

Every option lives in [config.example.toml](config.example.toml). Set
`KUBIDE_CONFIG` to try one without touching your real config. If you add
an option, add it there too — a test asserts the two match.

## License

MIT. Opening a PR means you accept that.
