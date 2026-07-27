# TODO

Working notes. Roughly ordered by how much they'd change day-to-day use.

## Editing

- **Multiple cursors.** Ctrl+D is bound to duplicate-line here; in VS Code it
  is select-next-occurrence. Worth revisiting once multi-cursor exists.
- **Replace is whole-file only.** Ctrl+H replaces every occurrence in one
  undo step, same smart-case literal match as Find. Still missing: stepping
  through matches one at a time, regex, and replacing with an empty string
  (an empty prompt answer means cancel).
- **Auto-close exists; matching does not.** `(` brings `)`, closers step
  over themselves, backspace inside a pair takes both, a selection wraps —
  all behind `[editor] auto_close`. Still missing: highlighting the matching
  bracket under the caret.
- **Snippets exist** (`[editor] snippets`): trigger word + Tab, per file
  extension, from `%APPDATA%\kubide\snippets\<ext>.toml`, seeded with
  starters. `$0` marks the caret. Missing: multiple placeholders with
  Tab-through (`$1`, `$2`), and reload without touching the config.
- **Word wrap.** Deliberately absent: the renderer and the line-number gutter
  are built on one-line-one-row. Changing that is a real piece of work, not a
  flag.
- **Undo history is per session.** Closing a file loses it.

## Panes and files

- **No tabs.** One pane holds one file. Opening a second file replaces the
  first, which is why there are so many "unsaved changes" guards. Ctrl+Tab
  now swaps the pane back to the file it held before — the piece of tabs
  worth having — but there is no picker over the whole recent list yet.
- **Terminals are not restored** with a session. Deliberate — a shell is a live
  process — but a note saying "there was a terminal here" would be honest.
- **No drag and drop**, from the tree or from Explorer.

## Git

**The git panel exists** (Ctrl+Shift+G): staged and unstaged file lists,
Space to stage/unstage, C to commit with the message typed into the overlay,
Enter for a coloured per-file diff, L for the log and Enter there for a
commit's diff. Alongside it: file colours in the tree, the branch in the
status bar, and per-line gutter marks in the editor (against HEAD, hidden
while the buffer has unsaved edits — they would point at the wrong lines).

The panel also pushes (P), pulls (Shift+P, `--ff-only` — a pull that needs
a merge is a decision, not a side effect), and discards a file's unstaged
changes (X, ask-once like delete in the tree). Push and pull run on their
own thread and report back through the tick.

Still missing from the panel:

- Hunk-level staging (`git add -p` territory) — it is file-at-a-time today.
- Branches and stash. Fetch exists only as part of pull.
- Discarding an untracked file (today: delete it in the tree instead).
- A word-level diff highlight; the diff is line-level colour only.

## Keyboard-only

The point is to never need the mouse.

- **Hint mode**: letter labels over every clickable thing, Vimium style.

## Theming

**Theme files exist.** `theme = "gruvbox"` in the config loads
`%APPDATA%\kubide\themes\gruvbox.toml`; the folder is seeded on first start
with the built-ins (gruvbox, catppuccin, tokyonight, nord, rose-pine), a
dropped file wins over its built-in namesake, and the active theme file is
watched — recolour, save, see it, no restart. The settings screen writes the
name back rather than the resolved colours, so the config keeps following
the theme.

Still wanted: a theme picker on the settings screen (left/right through
whatever the folder holds), and a light theme that actually works over a
bright wallpaper — see "Measurements never taken".

## Measurements never taken

- `spikes/olc.ps1` measures what Acrylic costs on the GPU, but only the
  backdrop-on half was ever run. Without the backdrop-off baseline the number
  means nothing.
- Readability over a bright wallpaper has never been checked. Every test so far
  has been against a dark one.

## Working on another machine

```powershell
git clone https://github.com/kubilaiswf/kubide
cd kubide
cargo install --path crates/kubide   # puts kubide.exe on PATH
kubide                               # opens the current directory
```

Needs Rust 1.97+, the MSVC toolchain, and Visual Studio Build Tools with
**Desktop development with C++** — the tree-sitter grammars compile C.

State lives outside the repo:

- `%APPDATA%\kubide\config.toml` — settings. Copy `config.example.toml` to
  start; it documents every option and is checked against the defaults by a
  test.
- `%APPDATA%\kubide\sessions\*.session` — one remembered layout per project.
  Deleting them is harmless.

Before pushing:

```powershell
cargo clippy --all-targets    # leave no warnings
cargo test
cargo run -p kb-term --example dump    # terminal layer
cargo run -p kb-git --example status   # git layer
```
