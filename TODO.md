# TODO

Working notes. Roughly ordered by how much they'd change day-to-day use.

## Editing

- **Multiple cursors.** Ctrl+D is bound to duplicate-line here; in VS Code it
  is select-next-occurrence. Worth revisiting once multi-cursor exists.
- **Replace is whole-file only.** Ctrl+H replaces every occurrence in one
  undo step, same smart-case literal match as Find. Still missing: stepping
  through matches one at a time, regex, and replacing with an empty string
  (an empty prompt answer means cancel).
- **Auto-close and bracket matching exist.** `(` brings `)`, closers step
  over themselves, backspace inside a pair takes both, a selection wraps —
  all behind `[editor] auto_close` — and the pair the caret touches is
  highlighted, and Ctrl+M jumps between the two. The match is textual, so a
  bracket inside a string counts; knowing better needs the syntax tree.
  Still missing: an unmatched-bracket colour.
- **Snippets exist** (`[editor] snippets`): trigger word + Tab, per file
  extension, from `%APPDATA%\kubide\snippets\<ext>.toml`, seeded with
  starters. `$0` marks the caret. Missing: multiple placeholders with
  Tab-through (`$1`, `$2`), and reload without touching the config.
- **Word wrap.** Deliberately absent: the renderer and the line-number gutter
  are built on one-line-one-row. Changing that is a real piece of work, not a
  flag.
- **Undo history is per session.** Closing a file loses it.
- **Highlighting covers 15 grammars** (Rust, JS/TS/JSX/TSX, Python, C, C++,
  Go, HTML, CSS, JSON, TOML, YAML, Bash, Markdown block + inline), and an
  injection resolves through the same table — markdown fences and HTML
  `<script>`/`<style>` colour as their real language, common fence aliases
  (`py`, `js`, `sh`, `c++`) included. Notes: `.h` is guessed as C, which is
  wrong for C++-only headers but degrades better than the reverse; all
  grammars load at startup (tens of ms) rather than lazily; no Java, C#,
  Lua, Ruby, PHP — add a grammar crate and two match arms when one is
  actually missed.

## Panes and files

- **No tabs.** One pane holds one file. Opening a second file replaces the
  first, which is why there are so many "unsaved changes" guards — every
  way of opening one over unsaved work (tree, finder, picker) now asks
  Save / Discard / Cancel in the same box closing a pane does. Ctrl+Tab
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

## Agent

**The agent pane exists** (Ctrl+Shift+A): Claude Code in a pane. It is the
`claude` CLI itself, spawned in print mode with stream-json on both ends —
the seam the official editor extensions sit on — so CLAUDE.md, skills, MCP
servers, hooks and the subscription login all apply exactly as in the
terminal. The transcript streams; tool calls show as one row each with
their result under them; a successful Edit/Write re-reads every clean
editor from disk and nudges git and the tree. On a subscription the header
shows the 5-hour and 7-day windows and when the short one resets, read
from the CLI's `rate_limit_event`; the dollar estimate only appears with
no windows in sight (an API key) or in paid overage.

The control channel is wired: `--permission-prompt-tool stdio` turns the
CLI's questions into `control_request` lines, and each comes up in the
same choice box the editor uses for unsaved work, drawn inside the agent
pane above its input — Allow, Always allow (the CLI's own suggested rule,
saved to the project's `.claude/settings.local.json`), Deny; Esc is a no.
Only the focused agent pane answers: the box takes Left/Right/Enter/Esc
from that pane and nothing else, so a question can never be answered by
a keystroke meant for the editor next to it. Esc in the pane sends
`interrupt`, which ends the turn cleanly and keeps the process; a second
Esc kills it, and the next Enter resumes the session. A message typed
mid-turn queues and goes out when the turn ends. `/mode plan` and
`/model x` switch the session in place; other slash commands go to the
CLI. Typing `/` lists what is on offer — the pane's own three, then the
skills and commands found under `.claude/` in the home folder and the
project — with Up/Down, Tab or Enter to take one; the CLI's built-ins
(`/compact` and friends) are not listed because whether they work in
print mode has not been checked. None of this channel is in the CLI's documentation — it is what the
TypeScript SDK speaks, checked against 2.1.258 — so pin the CLI version
before a release. `[agent]` in the user config picks the executable,
model, permission mode and `--allowedTools`; a workspace `.kubide` file
cannot, because a clone must not grant itself anything.

Still missing:

- **The question shows one line.** A multi-line command or an edit's
  diff needs a bigger box than the choice box draws today.
- **Plan-mode approval** (`ExitPlanMode`) comes through the same channel
  and gets the generic "Use a tool?" wording.
- **Not restored** with a session, like terminals. The session id is known,
  so a "there was a conversation here — Enter resumes it" pane is possible.
- **One line of input.** Multi-line prompts get flattened on paste.
- **Markdown is drawn raw.** Fences and lists read fine in a monospace
  pane; headings and emphasis show their markers.
- **No selection context.** "This function" means nothing to it yet; the
  next step is folding the focused file and selection into the message,
  then a small stdio MCP server via `--mcp-config` for open/selection.
- **No copy** out of the transcript.

## Vim

**Vim mode exists** (`[vim] enabled`, or "Vim mode on or off" from F1). It
is the `kb-vim` crate: a grammar over the buffer, tested without a window
(`cargo test -p kb-vim`, 118 cases written as key sequences). Normal,
insert, replace, visual and visual-line modes; counts, registers (named,
numbered, `"-`, `"0`, `"+`/`"*`, `"_`, append with capitals), the
operators `d c y > < g~ gu gU` over every motion and the text objects
(`iw aw iW aW is as ip ap i( a( i[ i{ i< i" i' i` it at`), `x X s S D C
Y p P gp J gJ ~ r R u Ctrl+R .`, `f t ; ,`, `% { } ( ) H M L gg G`,
marks and the jump list, macros (`q` `@` `@@`, stored as text in the
register), `Ctrl+A/X` on numbers, `zz zt zb Ctrl+D/U/F/B/E/Y`, search
with vim's pattern dialect (`\v`, `\<`, `\(`, `\{n,m}`, `\c`) plus `n N *
#` and `hlsearch`, and a command line: `:w :q :wq :x :qa :e :s :& :g :v
:normal :d :y :pu :m :t :j :> :< :sort :noh :set :sp :vs :reg :marks
:jumps :u :red :term`, with `%`, `.`, `$`, marks, `/pat/` and offsets as
ranges. `Ctrl+W v s h j k l q` work the panes; `ZZ` and `ZQ` too.

Deliberately absent, or not yet:

- **Visual block mode** (`Ctrl+V`). The renderer draws one contiguous
  selection; a rectangle is a different drawing and a different edit.
- **`=`** re-indents nothing: there is no formatter to hand lines to.
- **`:s///c`** (confirm each) — no prompt to ask with. Run it plain and
  undo.
- **Regex look-around** (`\@=`) and `\zs`/`\ze`, `~` in patterns.
- **Search offsets** (`/pat/e`), `:s` with `\=`, `gq`, `K`, folds,
  `:map` — mappings are the `[keys]` table's job.
- **The `.` register and `Ctrl+A` in insert mode** hold typed text only;
  a snippet expanded by Tab is not in them.
- **Undo is per session**, as everywhere here; `u` past a reopen is gone.

## Keyboard-only

The point is to never need the mouse.

- **Hint mode**: letter labels over every clickable thing, Vimium style.

## Theming

**Theme files exist.** `theme = "gruvbox"` in the config loads
`%APPDATA%\kubide\themes\gruvbox.toml`; the folder is seeded on first start
with the built-ins, a dropped file wins over its built-in namesake, and the
active theme file is watched — recolour, save, see it, no restart. The
settings screen writes the name back rather than the resolved colours, so
the config keeps following the theme.

Seventeen ship built in: the five ports (gruvbox, catppuccin, tokyonight,
nord, rose-pine) and twelve grown from colorhunt.co's most liked palettes
(midnight-teal, deep-ocean, neon-sushi, steel, coral-reef, desert-night,
moss, mulberry, harbor, aurora, evergreen, paper). Those twelve extrapolate
a four-colour palette into ANSI and syntax colours — judged by eye against
the parse test, not on screen per language, so a colour that reads badly in
anger is a one-line fix in its file.

Still wanted: a theme picker on the settings screen (left/right through
whatever the folder holds), and a light theme *proven* over a bright
wallpaper — `paper` exists now, but see "Measurements never taken".

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

On Arch Linux the same commands work with `pacman -S rust base-devel` (a C
compiler for the grammars; the window, text and clipboard crates are pure
Rust and dlopen X11/Wayland at run time, so no `-dev` packages). Put a Nerd
Font first — `ttf-jetbrains-mono-nerd` — or the tree falls back to plain
markers exactly as on Windows. X11 needs `libxkbcommon-x11` installed, which
a desktop always has. Whether the window is blurred behind is the
compositor's call: Hyprland and KWin blur windows with alpha when told to,
GNOME draws them plain over the wallpaper; the tint is ours either way.

State lives outside the repo, in `%APPDATA%\kubide` on Windows and
`~/.config/kubide` on Linux:

- `config.toml` — settings. Copy `config.example.toml` to start; it
  documents every option and is checked against the defaults by a test.
- `sessions\*.session` — one remembered layout per project. Deleting them
  is harmless.

Before pushing:

```powershell
cargo clippy --all-targets    # leave no warnings
cargo test
cargo run -p kb-term --example dump    # terminal layer
cargo run -p kb-git --example status   # git layer
cargo run -p kb-agent --example chat   # one live turn through the claude CLI
```
