//! kubide.
//!
//! Splittable pane tree, draggable dividers, keyboard focus movement, a custom
//! title bar and an acrylic backdrop. A pane holds a terminal, a file explorer,
//! a text editor, or a notice about a file we refuse to open.
//!
//! Shortcuts live in the config, not here; see config.example.toml.

#![windows_subsystem = "windows"]

mod agent;
mod content;
mod draw;
mod folders;
mod metrics;
mod palette;
mod pomodoro;
mod session;

use content::{Content, Explorer};
use metrics::{TextArea, INSET};
use palette::{Palette, Target};
use kb_gfx::{Renderer, Result};
use kb_text::TextEngine;
use kb_ui::{focus_in_dir, Axis, Dir, DividerRef, Hit, Layout, PaneId, Rect, Tree};
use kb_win::{Backdrop, Chrome, CursorShape, Handler, Mods, Window, WindowConfig};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};
use std::path::{Path, PathBuf};

struct Kubide {
    gfx: Option<Renderer>,
    text: TextEngine,
    tree: Tree,
    focus: PaneId,
    layout: Layout,
    area: Rect,
    /// Dragged divider and the last mouse position.
    dragging: Option<(DividerRef, f32, f32)>,
    hover_divider: Option<Axis>,
    /// Where the mouse last was, for deciding what the pointer should look
    /// like over that spot.
    mouse: (f32, f32),
    /// When an overlay's text box last took a keystroke. A caret that
    /// blinks from the moment you arrive is noise; one that holds still
    /// while you type and blinks once you stop is the thing everyone
    /// recognises as "this box is waiting for you".
    typed_at: Instant,
    /// The blink phase at the last tick, so the redraw happens on the flip
    /// and not sixty times a second.
    blink_last: bool,
    frame_ms: f64,
    /// What each pane holds. Panes with no entry are empty.
    content: HashMap<PaneId, Content>,
    /// Pane whose selection is being dragged.
    sel_drag: Option<PaneId>,
    /// Where and when the last click landed, for recognising a double click.
    last_click: Option<(PaneId, kb_edit::Pos, std::time::Instant)>,

    cfg: kb_cfg::Config,
    /// Why the config didn't load, shown in the status bar. A config that
    /// silently does nothing is the worst possible outcome.
    cfg_problem: Option<String>,
    cfg_watch: Option<kb_cfg::Watcher>,
    /// Watches the active theme file, so recolouring it repaints live —
    /// which is the entire feedback loop of making a theme. The theme's
    /// name rides in `cfg.theme_name`.
    theme_watch: Option<kb_cfg::Watcher>,
    /// Watches the workspace's own `.kubide\config.toml`, when it has one.
    ws_watch: Option<kb_cfg::Watcher>,
    window: Option<Window>,

    git: kb_git::Git,
    /// Ticks since the last git refresh. `git status` costs real time on a big
    /// repository, so it runs on a schedule rather than per frame — and off the
    /// UI thread either way.
    git_at: Instant,
    /// Counts up whenever a git poll reports news. Editor gutters re-read
    /// their diff marks when their own stamp falls behind this one — lazily,
    /// so a pane nobody draws never runs a diff.
    git_gen: u64,
    /// Files the focused pane has shown, most recent first — what Ctrl+Tab
    /// walks back through. In-memory only: remembering it across runs would
    /// mean reopening files the machine may no longer have.
    recent: Vec<PathBuf>,
    /// The directory kubide was opened on. Explorers root here rather than at
    /// the process's current directory, which drifts once anything chdirs.
    root: PathBuf,
    /// Loaded grammars, shared by every editor pane. Loading them per pane
    /// would repeat a fair amount of setup for no reason.
    syntax: Rc<kb_syn::Syntax>,
    /// Trigger words Tab expands, per file extension. Reloaded with the
    /// config, so editing a snippet file lands on the next config touch.
    snippets: kb_cfg::snippets::Snippets,
    /// What every editor's vim shares: registers, the last search, the
    /// last change, the macro being recorded. One per window, because
    /// yanking in one pane and putting in another is the point of
    /// registers. Its options come from `[vim]` and `:set` bends them until
    /// that table next changes.
    vim: kb_vim::Session,
    /// The overlay, when one is open. It captures every key while it is.
    palette: Option<Palette>,
    /// The folder picker, when it is open. Above the palette in every sense:
    /// it owns the keyboard and the mouse while it is up.
    folder_picker: Option<folders::Picker>,
    /// Where the picker's parts were drawn, for hit-testing clicks. Written
    /// by drawing, the same arrangement as `palette_rows`.
    picker_hits: Option<PickerHits>,
    /// A message for the status bar, e.g. a refused close.
    notice: Option<String>,
    /// What the open prompt is collecting an answer for.
    pending: Option<Pending>,
    /// The work timer. Always present, shown only when asked for.
    timer: pomodoro::Pomodoro,
    /// The last drawn value of the time-varying status segments.
    status_stamp_last: (u64, u64),
    /// The last drawn seconds count of busy agent panes, summed — the same
    /// repaint-on-change gate, for the "working 12s" in their headers.
    agent_stamp_last: u64,
    /// The status bar's shortcut hints, as drawn: hit box and what pressing
    /// one runs.
    corner_chips: Vec<(Rect, kb_cfg::Action)>,
    /// The settings button in the bottom-left corner, as drawn. `None` until
    /// the first frame places it.
    settings_btn: Option<Rect>,
    /// Whether the cursor is over it, for the hover glow.
    settings_hover: bool,
    /// Whether it is held down. The click fires on release, on the button.
    settings_pressed: bool,
    /// Where the overlay's rows were last drawn, so clicks can find them.
    /// Recorded by drawing because that is what decides the geometry.
    palette_rows: Option<PaletteRows>,
    /// Where this workspace's layout is remembered, if anywhere.
    session_path: Option<PathBuf>,
    /// When the layout was last written.
    session_at: Instant,
    /// What the chosen font can draw. Measured, not assumed.
    glyphs: Glyphs,
    /// Whether the full shortcut list is showing.
    ///
    /// Not in the config, unlike the one-line corner hint: this is something
    /// you put up while you look at it, not a panel you live with. Twenty-odd
    /// rows pinned over your code permanently is what made the first version
    /// of this annoying.
    help_open: bool,
    /// A destructive action waiting to be confirmed.
    ///
    /// There is no dialog system, and there should not be one for this: the
    /// requirement is only that unsaved work cannot vanish from one keystroke.
    /// Pressing the same thing again confirms; anything else cancels.
    confirm: Option<Confirm>,
}

/// Which decorative glyphs the current font actually has.
///
/// The font is picked from a list of candidates, so what you get depends on
/// the machine: a PC with no Nerd Font installed lands on Cascadia Code, which
/// has none of these codepoints and draws a notdef box for every one. A file
/// tree full of boxes reads as a broken editor, not as a missing font.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Glyphs {
    /// The best file-tree marker set this font can draw.
    icons: kb_fs::Icons,
    /// The Powerline branch symbol. A separate question: Cascadia Code PL and
    /// friends patch Powerline and nothing else, so one flag would either hide
    /// a symbol the font has or draw a box for one it hasn't.
    branch: bool,
    /// The arrow on the terminal's scrollback badge. Not a Nerd Font glyph —
    /// U+21E1 is ordinary Unicode — but Consolas, Courier New and Lucida
    /// Console all lack it, so it has to be asked about like the rest.
    arrow: bool,
    /// Box-drawing characters, for the frame around a question. Every font
    /// Windows ships has them, but the family comes from a list the user
    /// controls, so it gets measured like everything else.
    boxes: bool,
    /// Block elements (█), for the welcome screen's watermark. A different
    /// Unicode block than the frames, so a different question.
    blocks: bool,
}

impl Glyphs {
    /// Best set first, falling back until something renders.
    ///
    /// One representative codepoint per range rather than one per icon: font
    /// patches come whole ranges at a time, and each question costs a walk of
    /// the system font collection.
    fn detect(text: &TextEngine) -> Self {
        let icons = if text.has_glyph('\u{f07b}') {
            kb_fs::Icons::Nerd
        } else if text.has_glyph('\u{25b8}') && text.has_glyph('\u{25be}') {
            kb_fs::Icons::Shapes
        } else {
            kb_fs::Icons::Ascii
        };
        Self {
            icons,
            branch: text.has_glyph('\u{e0a0}'),
            arrow: text.has_glyph('\u{21e1}'),
            boxes: text.has_glyph('\u{2554}'),
            blocks: text.has_glyph('\u{2588}'),
        }
    }
}

/// A destructive action waiting for the same key again.
///
/// Only yes-or-no questions live here. Where there are three ways out — save,
/// discard, cancel — pressing again can offer two of them at most, and the one
/// it cannot offer is the one people usually want; those ask through the
/// overlay instead, as a list that says what each answer does.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Confirm {
    /// Saving over a file that changed on disk after we opened it.
    Overwrite(PaneId),
    /// Removing the selected path. Nothing here goes to a recycle bin.
    DeletePath,
    /// Throwing away a file's unstaged changes from the git panel. Carries
    /// the path so the confirmation can never land on a different row than
    /// the question was asked about.
    DiscardFile(PathBuf),
}

/// The drawn position of the overlay's list.
#[derive(Clone, Copy)]
pub struct PaletteRows {
    pub x: f32,
    pub width: f32,
    /// Top of the first row.
    pub y0: f32,
    pub line_h: f32,
    pub count: usize,
}

/// The drawn geometry of the folder picker, for hit-testing clicks.
///
/// Recorded by drawing, like [`PaletteRows`]: the renderer decides where
/// everything sits, and letting the mouse work it out separately would
/// drift the moment either changed.
#[derive(Clone)]
pub struct PickerHits {
    /// The whole panel. Clicks inside it that hit nothing do nothing;
    /// clicks outside it are swallowed too, like the palette's.
    pub panel: Rect,
    /// Breadcrumb segments: x range and the directory each one names.
    pub crumb_y: (f32, f32),
    pub crumbs: Vec<(f32, f32, PathBuf)>,
    /// The address box itself; a click on it that lands on no crumb opens
    /// the bar for typing, like Explorer's.
    pub addr_x: (f32, f32),
    /// The left rail: top of the first row, then one entry per drawn row —
    /// `None` for the group labels, which are furniture, not places.
    pub places_y0: f32,
    pub places_x: (f32, f32),
    pub places: Vec<Option<PathBuf>>,
    /// The folder list: top of the first row and how many rows are drawn.
    pub list_y0: f32,
    pub list_x: (f32, f32),
    pub list_count: usize,
    pub line_h: f32,
    /// How many rows fit the list, for translating a wheel turn.
    pub visible: usize,
    /// Back, forward, up — the toolbar corner.
    pub back_btn: Rect,
    pub fwd_btn: Rect,
    pub up_btn: Rect,
    /// "Select folder" and "Cancel".
    pub open_btn: Rect,
    pub cancel_btn: Rect,
}

/// What a text prompt is collecting an answer for.
enum Pending {
    NewFile(PathBuf),
    NewFolder(PathBuf),
    Rename(PathBuf),
    ProjectSearch,
    /// Replace, step one: what to look for.
    ReplaceWhat,
    /// Replace, step two, carrying step one's answer.
    ReplaceWith(String),
    /// The git panel asked for a commit message.
    CommitMessage,
    /// Closing a pane whose editor has unsaved work.
    CloseUnsaved(PaneId),
    /// Moving to another workspace with unsaved work on screen, carrying
    /// where we were going.
    SwitchUnsaved(PathBuf),
    /// Quitting with unsaved work anywhere.
    QuitUnsaved,
    /// Opening a file into a pane whose editor has unsaved work, carrying
    /// what was about to be opened there.
    ReplaceUnsaved(PaneId, PathBuf),
}

/// The answers to "what about the unsaved work", in the order they are listed.
///
/// Saving comes first because it is what people usually mean and the list
/// opens on its first row; discarding is second and spelled out rather than
/// called "No"; cancelling is last and is also what Escape does.
const SAVE: usize = 0;
const DISCARD: usize = 1;

/// How often the layout is written.
///
/// Saving only on exit loses it to a crash or a kill, which is exactly when
/// remembering it is worth something.
const SESSION_INTERVAL: Duration = Duration::from_secs(30);

/// How often git status is re-read. Often enough to feel live, rare enough to
/// cost nothing.
const GIT_INTERVAL: Duration = Duration::from_secs(2);

/// How much unsaved work there is, as a sentence can say it.
///
/// One phrase for both questions that ask about it, because they are the same
/// question — the alternative was two format strings drifting into "with 1
/// file has unsaved changes", which is what they had both drifted into.
fn unsaved_phrase(n: usize) -> String {
    let plural = if n == 1 { "file" } else { "files" };
    format!("{n} unsaved {plural}")
}

/// What the command line asked us to open.
///
/// `kubide` with no argument opens the project the shell is standing in —
/// found by walking up, so it works from a crate's own directory or from
/// `target\release` as well as from the top. Naming a directory is taken
/// literally: discovery is for when nobody said.
struct Workspace {
    dir: PathBuf,
    /// Set when the argument was a file rather than a directory.
    file: Option<PathBuf>,
    /// Whether we know where we are. A named directory and a discovered
    /// project both count; a bare `kubide` somewhere that is no project at
    /// all — which is what double-clicking the exe amounts to — does not, and
    /// gets the welcome screen instead of a file listing of wherever it
    /// happened to wake up.
    explicit: bool,
    /// `kubide workspace`: mark this folder with a `.kubide` before opening
    /// it, the way `git init` marks a repository.
    init: bool,
}

impl Workspace {
    fn from_args() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let Some(arg) = std::env::args_os().nth(1) else {
            return match kb_fs::find_root(&cwd) {
                Some(dir) => Self { dir, file: None, explicit: true, init: false },
                None => Self { dir: cwd, file: None, explicit: false, init: false },
            };
        };
        // The subcommand — unless something in this folder is literally
        // named "workspace", in which case the path wins: a word can be
        // reclaimed, a directory cannot.
        if arg == "workspace" && !cwd.join(&arg).exists() {
            return Self { dir: cwd, file: None, explicit: true, init: true };
        }
        let path = cwd.join(arg);
        // Canonicalized so the title and the git root agree with each other,
        // and so `kubide ..` shows a real name instead of "..". De-armoured
        // straight after: the \\?\ form leaks into every shell we spawn.
        let path = kb_fs::strip_verbatim(std::fs::canonicalize(&path).unwrap_or(path));
        if path.is_dir() {
            return Self { dir: path, file: None, explicit: true, init: false };
        }
        // A file names itself, not a workspace. Its folder is where to look
        // for one: `kubide crates\kb-fs\src\lib.rs` should open the project
        // beside the file, not a two-entry tree of the directory it sits in.
        let beside = path.parent().map(Path::to_path_buf).unwrap_or(cwd);
        Self {
            dir: kb_fs::find_root(&beside).unwrap_or(beside),
            file: Some(path),
            explicit: true,
            init: false,
        }
    }
}

/// The window title for a workspace root.
///
/// The folder name rather than the whole path: this goes in the taskbar and
/// Alt+Tab, where a path is truncated to uselessness, and the job is only to
/// tell two windows apart. A drive root has no file name and falls back to
/// itself, which is already short.
fn title_for(root: &Path) -> String {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    format!("{name} — kubide")
}

impl Kubide {
    fn new(workspace: &Workspace) -> Result<Self> {
        let loaded = kb_cfg::load_workspace(&workspace.dir);
        let (tree, root) = Tree::new();
        let text = TextEngine::with_fonts(&loaded.config.font.family, loaded.config.font.size)?;
        let watch = kb_cfg::Watcher::new(&loaded.path);
        let glyphs = Glyphs::detect(&text);
        // Said once, at startup, and only when there is no config error to
        // report instead — that one matters more. Falling back silently means
        // someone sits looking at an editor that does not match a single
        // screenshot of it and has no idea the fix is one font away. The first
        // keypress clears it, so it costs nothing to anyone who meant it.
        let hint = (glyphs.icons != kb_fs::Icons::Nerd && loaded.problem.is_none())
            .then(|| "no Nerd Font found — file icons are off; install one from nerdfonts.com".to_string());
        Ok(Self {
            glyphs,
            help_open: false,
            git: kb_git::Git::discover(&workspace.dir),
            git_at: Instant::now(),
            git_gen: 0,
            recent: Vec::new(),
            root: workspace.dir.clone(),
            syntax: Rc::new(kb_syn::Syntax::new()),
            snippets: kb_cfg::snippets::load(),
            vim: kb_vim::Session::new(vim_options(&loaded.config)),
            palette: None,
            folder_picker: None,
            picker_hits: None,
            pending: None,
            timer: pomodoro::Pomodoro::new(loaded.config.pomodoro),
            status_stamp_last: (0, 0),
            agent_stamp_last: 0,
            corner_chips: Vec::new(),
            settings_btn: None,
            settings_hover: false,
            settings_pressed: false,
            palette_rows: None,
            session_path: session::path_for(&workspace.dir),
            session_at: Instant::now(),
            notice: hint,
            confirm: None,
            gfx: None,
            text,
            tree,
            focus: root,
            layout: Layout::default(),
            area: Rect::default(),
            dragging: None,
            hover_divider: None,
            mouse: (0.0, 0.0),
            typed_at: Instant::now(),
            blink_last: true,
            frame_ms: 0.0,
            content: HashMap::new(),
            sel_drag: None,
            last_click: None,
            cfg: loaded.config,
            cfg_problem: loaded.problem,
            cfg_watch: watch,
            theme_watch: loaded.theme_path.as_deref().and_then(kb_cfg::Watcher::new),
            ws_watch: loaded.workspace_path.as_deref().and_then(kb_cfg::Watcher::new),
            window: None,
        })
    }

    fn relayout(&mut self, w: f32, h: f32) {
        let pad = self.cfg.window.padding;
        let cap = self.cfg.window.caption_height;
        // The status bar is a row of text along the bottom edge, and the
        // panes stop above it. With only the padding below them, a pane's
        // last few pixels — its sideways scroll thumb — sat on the clock.
        let status = (self.text.line_height() + 8.0).max(pad);
        self.area = Rect::new(pad, cap, (w - pad * 2.0).max(1.0), (h - cap - status).max(1.0));
        self.layout = self.tree.compute(self.area);
    }

    /// Reloads the config and applies only what actually changed.
    ///
    /// The point of hot reload is that an edit lands without disturbing what
    /// you were doing, so a color change must not rebuild font atlases or
    /// restart a running shell.
    fn reload_config(&mut self) -> bool {
        let loaded = kb_cfg::load_workspace(&self.root);
        self.cfg_problem = loaded.problem;
        // Re-aimed every reload: a config edit may have renamed the theme,
        // a workspace switch changes whose .kubide is in charge, and the
        // watches have to follow the files actually in use.
        self.theme_watch = loaded.theme_path.as_deref().and_then(kb_cfg::Watcher::new);
        self.ws_watch = loaded.workspace_path.as_deref().and_then(kb_cfg::Watcher::new);
        // Snippet files ride along: they have no watcher of their own, and
        // "touch the config to reload them" is a rule people can hold.
        self.snippets = kb_cfg::snippets::load();
        self.apply_config(loaded.config)
    }

    /// Swaps in a config and applies only what actually changed.
    ///
    /// Shared by the file watcher and the settings screen so a value edited on
    /// screen lands exactly the way the same value edited in the file does.
    /// Two paths here would mean two answers to "does changing this need a new
    /// font atlas", and one of them would be wrong.
    fn apply_config(&mut self, config: kb_cfg::Config) -> bool {
        let refresh = config.refresh_from(&self.cfg);
        self.cfg = config;

        if !refresh.any() {
            // Still redraw: the problem message may have appeared or cleared.
            return true;
        }

        if refresh.vim {
            self.vim.options = vim_options(&self.cfg);
        }
        if refresh.font {
            let _ = self.text.set_fonts(&self.cfg.font.family, self.cfg.font.size);
            // A new family is a new set of codepoints. Keeping the old answer
            // would draw icons a font hasn't got, or hide ones it has.
            self.glyphs = Glyphs::detect(&self.text);
        }
        if refresh.paint {
            self.timer.set_config(self.cfg.pomodoro);
            let colors = self.cfg.theme.terminal;
            for c in self.content.values_mut() {
                if let Content::Terminal(t) = c {
                    t.set_colors(colors);
                }
            }
            // The pointer wears the theme too, and Windows only re-asks for
            // it when the mouse moves; without this poke a recolour leaves
            // the old cursor on screen until the hand twitches.
            if let Some(window) = self.window {
                kb_win::refresh_cursor(window);
            }
        }
        if let Some(window) = self.window {
            if refresh.window {
                kb_win::set_backdrop(window, backdrop_of(self.cfg.window.backdrop));
            }
            if refresh.layout {
                kb_win::set_caption_height(window, self.cfg.window.caption_height as i32);
            }
        }
        if refresh.font || refresh.layout {
            if let Some(gfx) = &self.gfx {
                let (w, h) = gfx.size();
                self.relayout(w, h);
            }
        }
        true
    }

    /// The file name in a pane, for asking about it by name. A question that
    /// says which file it means is the difference between answering it and
    /// guessing.
    fn file_name_in(&self, pane: PaneId) -> String {
        match self.content.get(&pane) {
            Some(Content::Editor(e)) => e
                .buffer
                .path()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "This file".into()),
            _ => "This file".into(),
        }
    }

    /// Whether a pane holds an editor with unsaved changes.
    fn unsaved_in(&self, pane: PaneId) -> bool {
        matches!(self.content.get(&pane), Some(Content::Editor(e)) if e.buffer.modified())
    }

    fn unsaved_count(&self) -> usize {
        self.content
            .values()
            .filter(|c| matches!(c, Content::Editor(e) if e.buffer.modified()))
            .count()
    }

    /// Shows a warning in the status bar.
    ///
    /// One place, not two. Putting it on the pane as well meant reading the
    /// same sentence twice, and the pane header is short enough that a long
    /// warning gets pushed against the edge.
    fn warn(&mut self, message: &str) {
        self.notice = Some(message.to_string());
    }

    /// True when quitting is allowed. Asks once if anything is unsaved.
    fn confirm_quit(&mut self) -> bool {
        let n = self.unsaved_count();
        if n == 0 {
            return true;
        }
        self.pending = Some(Pending::QuitUnsaved);
        self.palette = Some(Palette::ask(
            "Unsaved Changes",
            &format!("Quit with {}?", unsaved_phrase(n)),
            &["Save all", "Discard", "Cancel"],
        ));
        false
    }

    /// Moves the divider that changes the focused pane's size.
    ///
    /// Which divider that is depends on where the pane sits: one against the
    /// window edge has no divider on that side, so the opposite one moves
    /// instead and the sign flips. That is exactly why the actions are named
    /// for what happens to the pane — "wider" is true wherever it sits, where
    /// "move the boundary right" would grow a sidebar and shrink whatever was
    /// against the right edge.
    fn resize_pane(&mut self, grow: bool, along: Axis) -> bool {
        /// One press. Small enough to tune with, big enough to see.
        const STEP: f32 = 24.0;

        let (trailing, leading) = match along {
            Axis::Horizontal => (Dir::Right, Dir::Left),
            Axis::Vertical => (Dir::Down, Dir::Up),
        };
        // The trailing edge first. `Tree::drag` grows whatever sits before the
        // divider, so a divider on that side takes the delta as it comes and
        // one on the leading side takes it reversed.
        let found = kb_ui::divider_in_dir(&self.layout, self.focus, trailing)
            .map(|d| (d, 1.0))
            .or_else(|| {
                kb_ui::divider_in_dir(&self.layout, self.focus, leading).map(|d| (d, -1.0))
            });
        let Some((divider, sign)) = found else {
            // A single pane fills the window; there is nothing to move.
            return false;
        };

        self.tree.drag(divider, STEP * sign * if grow { 1.0 } else { -1.0 }, self.area);
        self.layout = self.tree.compute(self.area);
        true
    }

    fn move_focus(&mut self, dir: Dir) -> bool {
        if let Some(p) = focus_in_dir(&self.layout, self.focus, dir) {
            self.focus = p;
            return true;
        }
        false
    }

    /// Fits terminals to their pane.
    ///
    /// Resizing is expensive and delicate on the ConPTY side (reflow bugs come
    /// from here), so `Terminal::resize` only does work when the column or row
    /// count actually changed.
    fn sync_terms(&mut self) {
        let (cw, ch) = self.text.cell_size();
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        for (pane, r) in &self.layout.panes {
            if let Some(Content::Terminal(t)) = self.content.get_mut(pane) {
                let cols = ((r.w - INSET * 2.0) / cw).floor().max(1.0) as usize;
                let rows = ((r.h - INSET * 2.0) / ch).floor().max(1.0) as usize;
                t.resize(cols, rows, cw as u16, ch as u16);
            }
        }
    }

    fn terminal(&self, pane: PaneId) -> Option<&kb_term::Terminal> {
        self.content.get(&pane).and_then(Content::as_terminal)
    }

    /// Text geometry of a pane, or `None` if it has no rect yet.
    fn text_area(&self, pane: PaneId, top: usize) -> Option<TextArea> {
        let r = self.layout.rect_of(pane)?;
        let (cw, _) = self.text.cell_size();
        Some(TextArea::new(r, self.text.line_height(), cw, top))
    }

    /// How many content rows fit in a pane. Drawing and input have to agree on
    /// this, or a wheel scroll moves by a different amount than it looks.
    fn visible_rows(&self, pane: PaneId) -> usize {
        self.text_area(pane, 0).map(|a| a.visible).unwrap_or(1)
    }

    /// Screen point to a buffer position in an editor pane.
    fn editor_pos_at(&self, pane: PaneId, x: f32, y: f32) -> Option<kb_edit::Pos> {
        let Some(Content::Editor(e)) = self.content.get(&pane) else { return None };
        let area = self.text_area(pane, e.top)?;
        let (row, col) = area.cell_at(x, y);
        // The view may be scrolled sideways, so a column on screen is not a
        // column in the line.
        let col = col + e.left;
        // Clamping happens in the buffer: it knows the line lengths, and a
        // click past the last line should land on the last line, not nowhere.
        Some(kb_edit::Pos::new(e.top + row, col))
    }

    /// Copies the focused pane's selection, optionally removing it.
    fn copy_from_focus(&mut self, cut: bool) {
        match self.content.get_mut(&self.focus) {
            Some(Content::Terminal(t)) => {
                if let Some(s) = t.selection_text() {
                    let _ = kb_win::clipboard::set_text(&s);
                    t.select_clear();
                }
            }
            Some(Content::Editor(e)) => {
                // In vim mode the selection is visual mode's, and a cut is
                // its `d`, so the registers see it too.
                if self.cfg.vim.enabled {
                    let Some(s) = e.vim.selection_text(&e.buffer) else { return };
                    if kb_win::clipboard::set_text(&s).is_ok() && cut {
                        self.vim_key(kb_vim::Key::Char('d'));
                    }
                    return;
                }
                let Some(s) = e.buffer.selected_text() else { return };
                // Only remove the text once the clipboard actually took it,
                // or a failed cut destroys the selection with no copy of it.
                if kb_win::clipboard::set_text(&s).is_ok() && cut {
                    e.buffer.insert("");
                }
            }
            _ => {}
        }
    }

    /// The vim state of the focused pane, when it is an editor and vim mode
    /// is on. `None` is "not vim's business", which is what every caller
    /// wants to know.
    pub(crate) fn focused_vim(&self) -> Option<&kb_vim::Vim> {
        if !self.cfg.vim.enabled {
            return None;
        }
        match self.content.get(&self.focus) {
            Some(Content::Editor(e)) => Some(&e.vim),
            _ => None,
        }
    }

    /// Hands a key to the focused editor's vim. `None` when vim mode is off
    /// or the focus is not an editor; otherwise whether vim took it.
    fn vim_key(&mut self, key: kb_vim::Key) -> Option<bool> {
        if !self.cfg.vim.enabled {
            return None;
        }
        let visible = self.visible_rows(self.focus);
        let auto_close = self.cfg.editor.auto_close;
        let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) else {
            return None;
        };
        let ctx = kb_vim::Ctx { top: e.top, visible, auto_close };
        let outcome = e.vim.key(key, &mut e.buffer, &mut self.vim, &mut Clipboard, ctx);
        match outcome {
            kb_vim::Outcome::Pass => Some(false),
            kb_vim::Outcome::Handled(fx) => {
                // Typing means the user moved on from whatever was being
                // confirmed, the same as the plain editor path.
                e.status = None;
                self.confirm = None;
                self.notice = None;
                for f in fx {
                    self.vim_effect(f);
                }
                Some(true)
            }
        }
    }

    /// Does what vim asked for and the buffer could not.
    fn vim_effect(&mut self, effect: kb_vim::Effect) {
        use kb_vim::Effect::*;
        match effect {
            Save => {
                self.run(kb_cfg::Action::Save);
            }
            SaveAll => {
                self.save_every_editor();
            }
            SaveClose => {
                self.run(kb_cfg::Action::Save);
                // Close asks if the save did not take — a read-only file
                // must not vanish from the screen on `:wq`.
                self.run(kb_cfg::Action::ClosePane);
            }
            ClosePane => {
                self.run(kb_cfg::Action::ClosePane);
            }
            ClosePaneForce => self.close_pane(self.focus),
            Quit => {
                self.run(kb_cfg::Action::Quit);
            }
            QuitForce => {
                self.save_session();
                kb_win::quit();
            }
            SplitRight => {
                self.run(kb_cfg::Action::SplitRight);
            }
            SplitDown => {
                self.run(kb_cfg::Action::SplitDown);
            }
            Focus(dir) => {
                self.move_focus(match dir {
                    kb_vim::Dir::Left => Dir::Left,
                    kb_vim::Dir::Right => Dir::Right,
                    kb_vim::Dir::Up => Dir::Up,
                    kb_vim::Dir::Down => Dir::Down,
                });
            }
            OpenTerminal => {
                self.open_terminal();
            }
            OpenFile(name) => {
                // Relative to the workspace, the way `:e src/main.rs` reads
                // from a shell standing at the project root.
                let path = self.root.join(name);
                if !path.is_file() {
                    self.warn(&format!("not a file: {}", path.display()));
                    return;
                }
                self.open_at(path, kb_edit::Pos::new(0, 0));
            }
            ScrollTo(top) => {
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.top = top.min(e.buffer.len().saturating_sub(1));
                }
            }
        }
    }

    /// After a click or a drag: vim's mode follows what the mouse did.
    fn vim_mouse_sync(&mut self, pane: PaneId) {
        if !self.cfg.vim.enabled {
            return;
        }
        if let Some(Content::Editor(e)) = self.content.get_mut(&pane) {
            e.vim.mouse_sync(&mut e.buffer);
        }
    }

    /// Pastes the clipboard into the focused pane.
    ///
    /// For a terminal it's wrapped in bracketed paste, so the shell sees pasted
    /// text as pasted rather than typed. Without it, pasting several lines runs
    /// each one as a command — which makes pasting a script dangerous.
    fn paste_into_focus(&mut self) {
        let Some(text) = kb_win::clipboard::get_text() else { return };
        match self.content.get_mut(&self.focus) {
            Some(Content::Terminal(t)) => {
                t.write(b"\x1b[200~");
                t.write(text.replace("\r\n", "\r").as_bytes());
                t.write(b"\x1b[201~");
            }
            Some(Content::Editor(e)) => {
                // Normalize endings: pasting CRLF into an LF buffer would
                // leave stray carriage returns rendered as garbage.
                e.buffer.insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
            }
            Some(Content::Agent(a)) => a.paste(&text),
            _ => {}
        }
    }

    /// Screen coordinate to terminal cell.
    ///
    /// Clamped to the bounds so a drag slightly past the edge doesn't drop the
    /// selection.
    fn term_cell_at(&self, pane: PaneId, x: f32, y: f32) -> Option<(usize, usize)> {
        let term = self.terminal(pane)?;
        let r = self.layout.rect_of(pane)?;
        let (cw, ch) = self.text.cell_size();
        if cw <= 0.0 || ch <= 0.0 {
            return None;
        }
        let snap = term.snapshot();
        let col = (((x - (r.x + INSET)) / cw).floor() as i64).clamp(0, snap.cols as i64 - 1);
        let row = (((y - (r.y + INSET)) / ch).floor() as i64).clamp(0, snap.rows as i64 - 1);
        Some((col as usize, row as usize))
    }

    /// Which explorer row is under the cursor, if any.
    fn explorer_row_at(&self, pane: PaneId, y: f32) -> Option<usize> {
        let Some(Content::Explorer(e)) = self.content.get(&pane) else { return None };
        let r = self.layout.rect_of(pane)?;
        let lh = self.text.line_height();
        let y0 = r.y + INSET + lh * 1.6;
        if y < y0 {
            return None;
        }
        let index = e.top + ((y - y0) / lh).floor() as usize;
        (index < e.tree.rows().len()).then_some(index)
    }

    /// The pane holding a shell, if one is on screen.
    fn terminal_pane(&self) -> Option<PaneId> {
        self.content
            .iter()
            .find(|(_, c)| matches!(c, Content::Terminal(_)))
            .map(|(p, _)| *p)
    }

    /// The pane the code lives in: where a strip under the work belongs.
    ///
    /// The focused pane when it holds a file or nothing; otherwise the
    /// first file pane in reading order, then the first pane that is not
    /// furniture. The tree, the agent and a shell are things beside the
    /// code, and a strip under one of those is a strip nobody asked for.
    fn code_pane(&self) -> PaneId {
        let is_code = |c: Option<&Content>| {
            matches!(c, None | Some(Content::Editor(_) | Content::Viewer(_) | Content::Welcome(_)))
        };
        if is_code(self.content.get(&self.focus)) {
            return self.focus;
        }
        let order = kb_ui::panes_in_reading_order(&self.layout);
        order
            .iter()
            .copied()
            .find(|p| is_code(self.content.get(p)))
            .or_else(|| {
                order.iter().copied().find(|p| {
                    !matches!(
                        self.content.get(p),
                        Some(Content::Explorer(_) | Content::Agent(_) | Content::Terminal(_))
                    )
                })
            })
            .unwrap_or(self.focus)
    }

    /// Opens a terminal, or goes to the one that is open.
    ///
    /// One shell on screen is the shell they meant, the same as the tree:
    /// pressing the key from anywhere lands in it rather than growing a
    /// second one under whatever had focus.
    fn open_terminal(&mut self) -> bool {
        if let Some(pane) = self.terminal_pane() {
            self.focus = pane;
            return true;
        }
        // An empty pane takes the shell whole. Otherwise it is a strip
        // along the bottom of the code — the terminal is an accessory to
        // the code, not a replacement for it, and it goes under the code
        // wherever focus happens to be. This used to refuse outright on a
        // full pane, which read as a dead key.
        let target = if !self.content.contains_key(&self.focus) {
            self.focus
        } else {
            let base = self.code_pane();
            if !self.content.contains_key(&base) {
                base
            } else {
                // The bottom third, give or take: enough for a build log,
                // not enough to evict the code above it.
                match self.tree.split_at(base, Axis::Vertical, 0.68) {
                    Some(p) => {
                        self.layout = self.tree.compute(self.area);
                        p
                    }
                    None => return false,
                }
            }
        };
        let (cw, ch) = self.text.cell_size();
        let r = self.layout.rect_of(target).unwrap_or(self.area);
        let opts = kb_term::SpawnOptions {
            cols: ((r.w - INSET * 2.0) / cw).floor().max(1.0) as usize,
            rows: ((r.h - INSET * 2.0) / ch).floor().max(1.0) as usize,
            cell_w: cw as u16,
            cell_h: ch as u16,
            shell: self.cfg.terminal.shell.clone(),
            args: self.cfg.terminal.args.clone(),
            scrollback: self.cfg.terminal.scrollback,
            colors: self.cfg.theme.terminal,
            // The workspace root, not wherever the exe was launched from:
            // a shell that opens in the wrong project is a trap with a
            // prompt.
            cwd: Some(self.root.clone()),
        };
        match kb_term::Terminal::spawn(&opts) {
            Ok(t) => {
                self.content.insert(target, Content::Terminal(t));
                self.focus = target;
                true
            }
            Err(_) => false,
        }
    }

    /// The pane holding the agent, if one is on screen.
    fn agent_pane(&self) -> Option<PaneId> {
        self.content
            .iter()
            .find(|(_, c)| matches!(c, Content::Agent(_)))
            .map(|(p, _)| *p)
    }

    /// Opens the agent pane, or goes to it if one is up.
    ///
    /// One per window. A second conversation is a second process with its
    /// own idea of the working tree, and two of them editing the same
    /// files is a race nobody asked for. Placement follows the terminal:
    /// an empty pane takes it whole, a full one gets a column beside the
    /// work rather than losing the code under it.
    fn open_agent(&mut self) {
        if let Some(pane) = self.agent_pane() {
            self.focus = pane;
            return;
        }
        let target = if !self.content.contains_key(&self.focus) {
            self.focus
        } else {
            let base = self.workspace_pane();
            if !self.content.contains_key(&base) {
                base
            } else {
                match self.tree.split_at(base, Axis::Horizontal, 0.6) {
                    Some(p) => {
                        self.layout = self.tree.compute(self.area);
                        p
                    }
                    None => {
                        self.warn("no room for another pane");
                        return;
                    }
                }
            }
        };
        let cfg = &self.cfg.agent;
        let opts = kb_agent::Options {
            command: cfg.command.clone(),
            // The workspace, not wherever the exe was launched from: the
            // CLI reads CLAUDE.md there and scopes its tools to it.
            cwd: self.root.clone(),
            model: cfg.model.clone(),
            permission_mode: cfg.permission_mode.clone(),
            allowed_tools: cfg.allowed_tools.clone(),
            resume: None,
        };
        self.content
            .insert(target, Content::Agent(agent::AgentPane::new(opts)));
        self.focus = target;
        self.typed_at = Instant::now();
    }

    /// Re-reads every editor whose file changed on disk and that has no
    /// unsaved work of its own. One with edits keeps them; saving it asks
    /// first, which is the standing rule for a file another program
    /// touched.
    fn reload_clean_editors(&mut self) {
        for c in self.content.values_mut() {
            if let Content::Editor(e) = c {
                if !e.buffer.modified() && e.buffer.changed_on_disk() && e.reload().is_ok() {
                    e.status = Some("reloaded \u{b7} changed by the agent".into());
                }
            }
        }
    }

    /// The pane holding the file tree, if one is on screen.
    fn explorer_pane(&self) -> Option<PaneId> {
        self.content
            .iter()
            .find(|(_, c)| matches!(c, Content::Explorer(_)))
            .map(|(p, _)| *p)
    }

    /// Opens the explorer in the focused pane, replacing an existing explorer
    /// or viewer but never a running terminal — that would kill a shell.
    ///
    /// A tree already on screen is the tree they meant: focus goes there
    /// rather than a second one being built, which also means the key does
    /// something when the focused pane is a shell. With no tree anywhere and
    /// a shell under the cursor there is nothing safe to do, so it says so —
    /// the same lesson the terminal key already learned about dead keys.
    fn open_explorer(&mut self) -> bool {
        if let Some(pane) = self.explorer_pane() {
            self.focus = pane;
            return true;
        }
        if self.content.get(&self.focus).is_some_and(Content::is_live) {
            let key = self
                .cfg
                .keys
                .binding_for(kb_cfg::Action::ToggleExplorer)
                .map(|c| c.to_string())
                .unwrap_or_else(|| "the toggle".to_string());
            self.warn(&format!("something is running here — {key} puts the tree beside it"));
            return false;
        }
        let root = self.root.clone();
        self.content
            .insert(self.focus, Content::Explorer(Explorer::new(root)));
        true
    }

    /// Opens the settings screen in the focused pane.
    ///
    /// Never over a terminal, which would kill a shell, and never over unsaved
    /// work. Pressing it again while it is already open does nothing rather
    /// than resetting the selection to the top.
    fn open_settings(&mut self) -> bool {
        // Never over the explorer: opened from the tree, it goes next door.
        let target = self.workspace_pane();
        match self.content.get(&target) {
            // Already open, so this is someone pressing it again to get out.
            Some(Content::Settings(_)) => {
                self.focus = target;
                return self.close_settings();
            }
            // Said out loud, like the git panel's refusal: a key that quietly
            // does nothing reads as a broken key.
            Some(c) if c.is_live() => {
                self.warn("settings will not replace a running process — use another pane");
                return false;
            }
            _ if self.unsaved_in(target) => {
                self.warn("unsaved changes there — save first, or use another pane");
                return false;
            }
            _ => {}
        }
        // What was here comes with it rather than being dropped: leaving has
        // to put the file back, not hand over an empty pane.
        let previous = self.content.remove(&target).map(Box::new);
        self.content
            .insert(target, Content::Settings(content::Settings::new(previous)));
        self.focus = target;
        true
    }

    /// The pane where content should land when the focused one is off
    /// limits.
    ///
    /// The explorer is furniture, not a workspace: a file, a panel or a
    /// settings screen opened while the tree has focus goes next door —
    /// right, then down, skipping shells — and when there is no next door,
    /// a new split is made rather than eating the tree. A window that lost
    /// its navigation to the thing it was navigating to is backwards.
    fn workspace_pane(&mut self) -> PaneId {
        if !matches!(self.content.get(&self.focus), Some(Content::Explorer(_))) {
            return self.focus;
        }
        if let Some(p) = focus_in_dir(&self.layout, self.focus, Dir::Right)
            .or_else(|| focus_in_dir(&self.layout, self.focus, Dir::Down))
            .filter(|p| !self.content.get(p).is_some_and(Content::is_live))
        {
            return p;
        }
        // The tree is alone, or everything else is a running shell.
        match self.tree.split(self.focus, Axis::Horizontal) {
            Some(p) => {
                self.layout = self.tree.compute(self.area);
                p
            }
            None => self.focus,
        }
    }

    /// Opens the git panel, or closes it if it is already up — the same
    /// in-and-out the settings screen has. Never over the explorer: pressed
    /// from the tree, the panel opens next door.
    fn toggle_git_panel(&mut self) {
        if !self.git.is_repo() {
            self.warn("not a git repository");
            return;
        }
        let target = self.workspace_pane();
        match self.content.get(&target) {
            Some(Content::Git(_)) => {
                self.focus = target;
                self.close_git_panel();
                return;
            }
            Some(c) if c.is_live() => {
                self.warn("the git panel will not replace a running process — use another pane");
                return;
            }
            _ if self.unsaved_in(target) => {
                self.warn("unsaved changes there — save first, or use another pane");
                return;
            }
            _ => {}
        }
        let entries = self.git.entries();
        let previous = self.content.remove(&target).map(Box::new);
        self.content
            .insert(target, Content::Git(content::GitPanel::new(previous, entries)));
        self.focus = target;
    }

    /// Leaves the git panel, putting back whatever it covered.
    fn close_git_panel(&mut self) -> bool {
        let Some(Content::Git(g)) = self.content.get_mut(&self.focus) else {
            return false;
        };
        match g.take_previous() {
            Some(previous) => {
                self.content.insert(self.focus, previous);
            }
            None => {
                self.content.remove(&self.focus);
            }
        }
        true
    }

    /// Re-reads the file list after anything that could have changed it, and
    /// nudges the async status along so the tree and the gutters follow.
    fn refresh_git_panel(&mut self) {
        let entries = self.git.entries();
        if let Some(Content::Git(g)) = self.content.get_mut(&self.focus) {
            g.set_entries(entries);
        }
        self.git.refresh();
    }

    /// Leaves the settings screen, putting back whatever it covered.
    fn close_settings(&mut self) -> bool {
        let Some(Content::Settings(s)) = self.content.get_mut(&self.focus) else {
            return false;
        };
        match s.take_previous() {
            Some(previous) => {
                self.content.insert(self.focus, previous);
            }
            // It opened on an empty pane, so an empty pane is where it goes
            // back to — with the hints on it saying what to press next.
            None => {
                self.content.remove(&self.focus);
            }
        }
        true
    }

    /// Writes the current config to disk.
    ///
    /// Only what differs from the defaults, so the file stays a list of what
    /// you changed rather than a frozen copy of every colour in the theme.
    /// The watcher will see the write and reload; that is a no-op, because
    /// what it reads back is what is already applied.
    fn save_config(&mut self) {
        let path = kb_cfg::config_path();
        let result = kb_cfg::save_named(&self.cfg, self.cfg.theme_name.as_deref(), &path);
        if let Some(Content::Settings(s)) = self.content.get_mut(&self.focus) {
            s.status = Some(match &result {
                Ok(()) => format!("written to {}", path.display()),
                Err(e) => format!("could not write: {e}"),
            });
        }
        if let Err(e) = result {
            self.warn(&format!("config: {e}"));
        }
    }

    /// The `[cursor]` settings as a concrete pointer. A non-empty `file` —
    /// a `.cur` or `.ani` from any cursor pack — wins over the drawn shape,
    /// with the drawing as its fallback; `fallback` itself is the system
    /// pointer used when custom is off entirely.
    ///
    /// The colour string is parsed on every call and the parse is two
    /// instructions deep — not worth a cache that could go stale on a
    /// config reload.
    fn themed_or_file(
        &self,
        kind: kb_win::ThemedKind,
        file: &str,
        fallback: CursorShape,
    ) -> CursorShape {
        let cc = &self.cfg.cursor;
        if !cc.custom {
            return fallback;
        }
        // A role's own colour when one is set, the shared one otherwise —
        // and "accent", or anything unparseable, harmlessly follows the
        // theme.
        let role_color = match kind {
            kb_win::ThemedKind::Arrow
            | kb_win::ThemedKind::Dart
            | kb_win::ThemedKind::Triangle
            | kb_win::ThemedKind::Temple
            // The hand is the pointer's sibling — same dress code.
            | kb_win::ThemedKind::Hand => cc.pointer_color.as_str(),
            kb_win::ThemedKind::IBeam | kb_win::ThemedKind::Bar => cc.text_color.as_str(),
            _ => "",
        };
        let spec = if role_color.is_empty() { cc.color.as_str() } else { role_color };
        let rgb = spec
            .strip_prefix('#')
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .filter(|_| spec.len() == 7)
            .unwrap_or_else(|| {
                let c = self.cfg.theme.accent;
                ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32
            });
        let desc = kb_win::ThemedCursor {
            kind,
            size: cc.size.min(128) as u16,
            rgb,
        };
        if file.is_empty() {
            CursorShape::Themed(desc)
        } else {
            CursorShape::File { path: PathBuf::from(file), fallback: desc }
        }
    }

    /// Whether an overlay's caret is showing this frame.
    ///
    /// Solid for a moment after each keystroke, blinking after that: a caret
    /// that flickers while you are mid-word is what makes a text box feel
    /// cheap, and one that never moves does not read as a text box at all.
    pub(crate) fn caret_on(&self) -> bool {
        const HOLD: Duration = Duration::from_millis(500);
        const PHASE: u128 = 530; // the Windows default, near enough
        let since = self.typed_at.elapsed();
        since < HOLD || (since.as_millis() / PHASE).is_multiple_of(2)
    }

    /// Whether the folder picker has something clickable at this point —
    /// the same regions `picker_click` answers to, asked without clicking.
    /// This is what turns the pointer into a hand: a row that changes the
    /// cursor is a row that looks pressable before it is pressed.
    fn picker_clickable(&self, x: f32, y: f32) -> bool {
        let Some(h) = &self.picker_hits else { return false };
        if h.open_btn.contains(x, y)
            || h.cancel_btn.contains(x, y)
            || h.back_btn.contains(x, y)
            || h.fwd_btn.contains(x, y)
            || h.up_btn.contains(x, y)
        {
            return true;
        }
        if y >= h.crumb_y.0 && y < h.crumb_y.1 {
            if h.crumbs.iter().any(|(x0, x1, _)| x >= *x0 && x < *x1) {
                return true;
            }
            // The bar itself opens for typing, so it counts too.
            if x >= h.addr_x.0 && x < h.addr_x.1 {
                return true;
            }
        }
        if x >= h.places_x.0 && x < h.places_x.1 && y >= h.places_y0 {
            let row = ((y - h.places_y0) / h.line_h).floor() as usize;
            if matches!(h.places.get(row), Some(Some(_))) {
                return true;
            }
        }
        if x >= h.list_x.0 && x < h.list_x.1 && y >= h.list_y0 {
            let row = ((y - h.list_y0) / h.line_h).floor() as usize;
            if row < h.list_count {
                return true;
            }
        }
        false
    }

    /// The pointer for text or not-text, in the configured style.
    fn pointer(&self, over_text: bool) -> CursorShape {
        let cc = &self.cfg.cursor;
        if over_text {
            let kind = match cc.text {
                kb_cfg::TextPointerStyle::Ibeam => kb_win::ThemedKind::IBeam,
                kb_cfg::TextPointerStyle::Bar => kb_win::ThemedKind::Bar,
            };
            self.themed_or_file(kind, &cc.text_file, CursorShape::Text)
        } else {
            let kind = match cc.pointer {
                kb_cfg::PointerStyle::Arrow => kb_win::ThemedKind::Arrow,
                kb_cfg::PointerStyle::Dart => kb_win::ThemedKind::Dart,
                kb_cfg::PointerStyle::Triangle => kb_win::ThemedKind::Triangle,
                kb_cfg::PointerStyle::Temple => kb_win::ThemedKind::Temple,
            };
            self.themed_or_file(kind, &cc.pointer_file, CursorShape::Arrow)
        }
    }

    /// The line-comment marker for the focused file's language.
    ///
    /// Guessed from the extension, same as highlighting. A wrong marker is
    /// harmless — it comments with the wrong characters and toggles straight
    /// back off — where a missing one does nothing at all and reads as broken.
    fn comment_marker(&self) -> String {
        let lang = match self.content.get(&self.focus) {
            Some(Content::Editor(e)) => e.buffer.path().and_then(kb_syn::Lang::of),
            _ => None,
        };
        match lang {
            Some(
                kb_syn::Lang::Toml | kb_syn::Lang::Python | kb_syn::Lang::Yaml | kb_syn::Lang::Bash,
            ) => "#".into(),
            // Markdown and JSON have no line comment; neither do HTML and
            // CSS, whose comments are block-only and out of reach for a
            // line-wise toggle. `//` is what JSON-with-comments uses and what
            // people expect to type, and a wrong marker toggles back off.
            _ => "//".into(),
        }
    }

    /// Opens the file finder.
    ///
    /// git first, because it already honours .gitignore. Walking the directory
    /// instead would hand back a `target/` full of build artefacts, and
    /// reimplementing .gitignore to avoid that is a project of its own.
    fn open_palette_files(&mut self) {
        const LIMIT: usize = 20_000;
        // Stripped against wherever the list actually came from. Git hands
        // back paths under the repository root, which may sit above the
        // workspace — started from target\release, every row was wearing
        // its full absolute path because the strip never matched.
        let (files, base) = match self.git.list_files().filter(|f| !f.is_empty()) {
            Some(files) => (files, self.git.root().unwrap_or(&self.root).to_path_buf()),
            None => (kb_fs::list_files(&self.root, LIMIT), self.root.clone()),
        };
        self.palette = Some(Palette::files(files, &base));
    }

    /// A click while the overlay is open.
    ///
    /// Inside a row selects it, and a second click on the row already selected
    /// takes it — the same rule the file tree uses, so there is one behaviour
    /// to learn rather than two.
    fn palette_click(&mut self, x: f32, y: f32) {
        let Some(rows) = self.palette_rows else { return };
        let Some(p) = &mut self.palette else { return };

        if x < rows.x || x > rows.x + rows.width || y < rows.y0 {
            return;
        }
        let row = ((y - rows.y0) / rows.line_h).floor() as usize;
        if row >= rows.count {
            return;
        }
        // The drawn row is an offset into what is on screen, and the list
        // scrolls, so the match it points at is that far past the top.
        let index = p.top + row;
        let already = p.selected == index;
        p.selected = index;
        if already {
            self.palette_accept();
        }
    }

    /// Acting on a picker row: a folder is somewhere to go, a file is
    /// something to open — the dialog being copied does exactly this.
    fn picker_activate(&mut self) {
        let Some(p) = &mut self.folder_picker else { return };
        let Some((row, path)) = p.selected_entry() else { return };
        if row.is_dir {
            p.navigate(path);
            return;
        }
        self.folder_picker = None;
        // The same landing rules as the file finder: never over the
        // explorer, never over unsaved work.
        let target = self.workspace_pane();
        self.open_over(target, path);
    }

    /// Opens a file into a pane, asking first when that pane holds unsaved
    /// work.
    ///
    /// Every way of opening a file — the tree, the finder, the picker —
    /// lands here, so they all ask the same question in the same box as
    /// closing a pane does: Save, Discard, Cancel. A press-again warning
    /// used to stand here, and it could not offer the answer people
    /// usually want, which is to save.
    fn open_over(&mut self, target: PaneId, path: PathBuf) {
        if self.unsaved_in(target) {
            let name = self.file_name_in(target);
            self.pending = Some(Pending::ReplaceUnsaved(target, path));
            self.palette = Some(Palette::ask(
                "Unsaved Changes",
                &format!("Replace {name} without saving it?"),
                &["Save", "Discard", "Cancel"],
            ));
            return;
        }
        self.confirm = None;
        self.notice = None;
        self.content.insert(target, Content::open_path(&path));
        self.focus = target;
    }

    /// Keys while the folder picker is open. Explorer's grammar throughout:
    /// Enter opens what is selected, Backspace goes back, Alt+Up goes up —
    /// with Ctrl+Enter kept as the fast "select this folder", answering with
    /// whatever the footer field is showing.
    fn picker_key(&mut self, vk: u8, ctrl: bool, alt: bool) -> bool {
        let Some(p) = &mut self.folder_picker else { return false };
        // While the address bar is open it owns the keys: Enter goes to the
        // typed place, Escape only closes the bar — the dialog under it
        // survives a mistyped path.
        self.typed_at = Instant::now();
        if p.address.is_some() {
            match vk {
                0x1B => p.address = None,
                0x4C if ctrl => p.address = None,
                // Ctrl+A selects the line, so the next keystroke replaces
                // the whole path rather than appending to it.
                0x41 if ctrl => p.select_all = true,
                0x0D => {
                    let text = p.address.clone().unwrap_or_default();
                    let text = text.trim().trim_matches('"').to_string();
                    let path = std::path::PathBuf::from(&text);
                    let dest = if path.is_dir() {
                        Some(path)
                    } else {
                        // A file's path means the folder holding it.
                        path.parent().filter(|q| q.is_dir()).map(Path::to_path_buf)
                    };
                    match dest {
                        Some(d) => p.navigate(d),
                        // Nowhere: the crumbs come back and say where you
                        // still are.
                        None => p.address = None,
                    }
                }
                0x08 => {
                    if let Some(a) = &mut p.address {
                        a.pop();
                    }
                }
                0x56 if ctrl => {
                    p.clear_selected();
                    if let (Some(a), Some(text)) =
                        (&mut p.address, kb_win::clipboard::get_text())
                    {
                        a.extend(
                            text.trim().trim_matches('"').chars().filter(|c| !c.is_control()),
                        );
                    }
                }
                // Unhandled, so WM_CHAR still arrives and types the path.
                _ => return false,
            }
            return true;
        }
        match vk {
            0x1B => self.folder_picker = None,
            // Ctrl+L, the browser reflex, and Alt+D, Explorer's own: the
            // address bar opens for typing with the path filled in.
            0x4C if ctrl => p.edit_address(),
            0x44 if alt => p.edit_address(),
            // Ctrl+A over the search box selects what is in it, so the next
            // keystroke starts a fresh search instead of extending the old
            // one. With the box already empty there is nothing to select and
            // the address bar is the more useful answer to the reflex.
            0x41 if ctrl => {
                if p.filter.is_empty() {
                    p.edit_address();
                } else {
                    p.select_all = true;
                }
            }
            0x0D if ctrl => {
                let dir = p.chosen();
                self.folder_picker = None;
                self.switch_workspace(dir);
            }
            // Alt+arrows are Explorer's own navigation set.
            0x26 if alt => p.up(),
            0x25 if alt => p.back(),
            0x27 if alt => p.forward(),
            // Ctrl+V: a pasted absolute path jumps straight there — Explorer's
            // address bar takes one, and clicking a way down to AppData
            // through hidden folders is nobody's idea of an address. A file's
            // path means the folder holding it; anything that is not a path
            // lands in the search box like typed text.
            0x56 if ctrl => {
                if let Some(text) = kb_win::clipboard::get_text() {
                    let text = text.trim().trim_matches('"').to_string();
                    let path = std::path::PathBuf::from(&text);
                    if path.is_absolute() && path.is_dir() {
                        p.navigate(path);
                    } else if path.is_absolute()
                        && path.parent().is_some_and(|q| q.is_dir())
                    {
                        p.navigate(path.parent().unwrap().to_path_buf());
                    } else {
                        for c in text.chars().filter(|c| !c.is_control()) {
                            p.push(c);
                        }
                    }
                }
            }
            // Escape hatch for a selected search box: clear it and carry on.
            0x2E => {
                p.clear_selected();
            }
            // Enter, Tab and Right all open the selection: whichever reflex
            // arrives — dialog, shell or tree — the box does what was meant.
            // Unless the search box holds an absolute path, typed or pasted:
            // then it is an address, and Enter goes there.
            0x0D | 0x09 | 0x27 => {
                let typed = std::path::PathBuf::from(p.filter.trim().trim_matches('"'));
                if typed.is_absolute() && typed.is_dir() {
                    p.navigate(typed);
                } else {
                    self.picker_activate();
                }
            }
            0x25 => p.back(),  // left, the browser reflex
            0x26 => p.move_selection(-1),
            0x28 => p.move_selection(1),
            0x24 => p.move_selection(i32::MIN / 2), // home
            0x23 => p.move_selection(i32::MAX / 2), // end
            // Backspace un-types the search while there is one; after that
            // it goes back, which is what it does in Explorer itself.
            0x08 => {
                if p.filter.is_empty() {
                    p.back();
                } else {
                    p.backspace();
                }
            }
            // Unhandled, so WM_CHAR still arrives and types into the search.
            _ => return false,
        }
        true
    }

    /// A click while the folder picker is open.
    ///
    /// Rows follow the tree's rule — one click selects, a second on the
    /// same row opens — and everything else is a button that does what its
    /// picture says: the arrows navigate, crumbs jump, places jump, the two
    /// footer buttons answer the dialog.
    fn picker_click(&mut self, x: f32, y: f32) {
        let Some(hits) = self.picker_hits.clone() else { return };
        let Some(p) = &mut self.folder_picker else { return };

        if hits.open_btn.contains(x, y) {
            let dir = p.chosen();
            self.folder_picker = None;
            self.switch_workspace(dir);
            return;
        }
        if hits.cancel_btn.contains(x, y) {
            self.folder_picker = None;
            return;
        }
        if hits.back_btn.contains(x, y) {
            p.back();
            return;
        }
        if hits.fwd_btn.contains(x, y) {
            p.forward();
            return;
        }
        if hits.up_btn.contains(x, y) {
            p.up();
            return;
        }
        if y >= hits.crumb_y.0 && y < hits.crumb_y.1 {
            if let Some((.., dir)) = hits
                .crumbs
                .iter()
                .find(|(x0, x1, _)| x >= *x0 && x < *x1)
            {
                p.navigate(dir.clone());
            } else if x >= hits.addr_x.0 && x < hits.addr_x.1 {
                // The bar itself: open it for typing, like Explorer.
                p.edit_address();
            }
            return;
        }
        // A click anywhere else closes an open address bar and then counts
        // as itself — the same promise Explorer keeps.
        p.address = None;
        if x >= hits.places_x.0 && x < hits.places_x.1 && y >= hits.places_y0 {
            let row = ((y - hits.places_y0) / hits.line_h).floor() as usize;
            if let Some(Some(dir)) = hits.places.get(row) {
                p.navigate(dir.clone());
            }
            return;
        }
        if x >= hits.list_x.0 && x < hits.list_x.1 && y >= hits.list_y0 {
            let row = ((y - hits.list_y0) / hits.line_h).floor() as usize;
            if row < hits.list_count && p.select_visible(row) {
                self.picker_activate();
            }
        }
        // Anywhere else — inside the panel or off it — is not an answer.
        // The palette ignores those clicks too; Escape and Cancel both say
        // "no" unambiguously, a slipped click should not.
    }

    /// Keys while the overlay is open.
    fn palette_key(&mut self, vk: u8) -> bool {
        if !palette::consumes(vk) {
            // Reported as unhandled so the character this key produces still
            // arrives. Ownership holds anyway: the action lookup is never
            // reached while a palette is open, and `on_char` drops control
            // characters, so an unbound chord does nothing.
            return false;
        }
        match vk {
            0x1B => {
                // Escape drops the prompt and whatever it was collecting for,
                // or the operation would fire on the next unrelated prompt.
                self.palette = None;
                self.pending = None;
            }
            0x08 => {
                if let Some(p) = &mut self.palette {
                    p.backspace();
                }
            }
            0x26 => {
                if let Some(p) = &mut self.palette {
                    p.move_selection(-1);
                }
            }
            0x28 => {
                if let Some(p) = &mut self.palette {
                    p.move_selection(1);
                }
            }
            // A question lays its answers out side by side, so the arrows that
            // move between them are the sideways pair. Ignored elsewhere: the
            // lists run downwards.
            0x25 | 0x27 => {
                if let Some(p) = &mut self.palette {
                    if p.mode == palette::Mode::Choice {
                        p.move_selection(if vk == 0x25 { -1 } else { 1 });
                    }
                }
            }
            0x0D => self.palette_accept(),
            _ => {}
        }
        true
    }

    /// Enter in the overlay.
    fn palette_accept(&mut self) {
        let Some(p) = &self.palette else { return };
        let Some(target) = p.chosen() else {
            // Nothing to accept — an unparseable line number, or no match.
            // Leaving the overlay open lets the user fix the query rather than
            // retyping it from scratch.
            return;
        };
        self.palette = None;

        match target {
            Target::Path(path) => {
                // Never over the explorer, and the same protection as the
                // tree: opening must not discard work.
                let target = self.workspace_pane();
                self.open_over(target, path);
            }
            Target::Text(answer) => self.apply_prompt(answer),
            Target::Answer(index) => self.apply_answer(index),
            Target::Location(path, pos) => self.open_at(path, pos),
            Target::Run(action) => {
                // Runs through the same path a key press would, so a command
                // and its shortcut can never drift apart.
                self.run(action);
            }
            Target::Pos(pos) => {
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.buffer.move_to(pos, false);
                    // Centre it: landing on the last visible row means seeing
                    // no context after what you searched for.
                    e.top = pos.line.saturating_sub(4);
                }
            }
        }
    }

    /// A number that changes exactly when the clock or countdown text does.
    ///
    /// Minutes for the clock, seconds for the countdown, and zero for either
    /// when it is switched off — so a hidden segment cannot cause a repaint.
    fn status_stamp(&self) -> (u64, u64) {
        let minute = if self.cfg.status.clock {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() / 60)
                .unwrap_or(0)
        } else {
            0
        };
        let countdown = if self.cfg.status.pomodoro {
            self.timer.remaining().as_secs()
        } else {
            0
        };
        (minute, countdown)
    }

    /// Describes the current layout for saving.
    fn session(&self) -> session::Session {
        let panes = self
            .tree
            .panes()
            .into_iter()
            .map(|id| {
                let what = match self.content.get(&id) {
                    Some(Content::Explorer(_)) => session::Pane::Explorer,
                    Some(Content::Terminal(_)) => session::Pane::Terminal,
                    Some(Content::Editor(e)) => match e.buffer.path() {
                        Some(p) => session::Pane::File(
                            p.to_path_buf(),
                            e.buffer.cursor.line,
                            e.buffer.cursor.col,
                        ),
                        None => session::Pane::Empty,
                    },
                    // A refusal notice is not worth restoring; you would only
                    // be told again that the file cannot be opened.
                    _ => session::Pane::Empty,
                };
                (id, what)
            })
            .collect();
        session::Session {
            layout: self.tree.describe(),
            panes,
            focus: self.focus,
        }
    }

    fn save_session(&self) {
        // Before the welcome check, and outside the per-workspace file: a
        // window someone sized is a window they sized, whatever was in it.
        if let Some(window) = self.window {
            if let Some(place) = kb_win::placement(window) {
                session::note_window_place(place);
            }
        }
        // The welcome screen is a doorway, not a layout: remembering it
        // would seed session files for wherever the exe happened to wake up,
        // which is exactly the noise it exists to avoid.
        if self.content.values().any(|c| matches!(c, Content::Welcome(_))) {
            return;
        }
        let Some(path) = &self.session_path else { return };
        // Best effort. A layout that failed to save is not worth a message.
        let _ = self.session().save(path, &self.root);
    }

    /// Rebuilds a saved layout. Returns false when there was nothing usable.
    fn restore_session(&mut self) -> bool {
        let Some(path) = self.session_path.clone() else { return false };
        let Some(saved) = session::Session::load(&path, &self.root) else { return false };
        // Furniture-only sessions are left where they lie. Every kubide
        // before the welcome screen saved one for any folder it was so much
        // as started in, and restoring those hands back a listing of
        // wherever the exe woke up — the exact thing welcome replaces.
        if !saved.worth_restoring() {
            return false;
        }

        let (tree, panes) = Tree::from_desc(&saved.layout);
        if panes.len() < 2 {
            // A single empty pane is not worth restoring over the normal
            // startup, which at least opens the tree.
            return false;
        }
        self.tree = tree;
        self.layout = self.tree.compute(self.area);
        self.content.clear();

        for (id, what) in saved.panes {
            if !panes.contains(&id) {
                continue;
            }
            match what {
                session::Pane::Explorer => {
                    self.content
                        .insert(id, Content::Explorer(Explorer::new(self.root.clone())));
                }
                // Terminals are not restarted. A shell is a live process with
                // history and a working directory; pretending to bring one
                // back would be a different shell wearing its clothes.
                session::Pane::Terminal | session::Pane::Empty => {}
                session::Pane::File(path, line, col) => {
                    // Files move and get deleted between runs; a missing one
                    // leaves an empty pane rather than an error.
                    if path.is_file() {
                        let mut content = Content::open_path(&path);
                        if let Content::Editor(e) = &mut content {
                            e.buffer.move_to(kb_edit::Pos::new(line, col), false);
                            e.top = line.saturating_sub(4);
                        }
                        self.content.insert(id, content);
                    }
                }
            }
        }

        self.focus = if panes.contains(&saved.focus) {
            saved.focus
        } else {
            panes[0]
        };
        true
    }

    /// Opens a file and puts the caret on a line, for a search result.
    fn open_at(&mut self, path: PathBuf, pos: kb_edit::Pos) {
        // Never over the explorer — a search result landing in the tree's
        // pane would trade the navigation for one match.
        let target = self.workspace_pane();
        let already = matches!(
            self.content.get(&target),
            Some(Content::Editor(e)) if e.buffer.path() == Some(path.as_path())
        );
        if !already {
            if self.unsaved_in(target) {
                self.warn("unsaved changes there — save first, or use another pane");
                return;
            }
            self.content.insert(target, Content::open_path(&path));
        }
        if let Some(Content::Editor(e)) = self.content.get_mut(&target) {
            e.buffer.move_to(pos, false);
            // Centred, so the match has context above and below it.
            e.top = pos.line.saturating_sub(4);
        }
        self.focus = target;
    }

    /// Notes which file the focused pane is showing, most recent first.
    ///
    /// Polled from the tick rather than hooked into every open: moving focus
    /// onto a pane is as much "I was in that file" as opening it was, and a
    /// single observation point cannot miss a case that a scattering of
    /// hooks would.
    fn note_recent(&mut self) {
        let Some(Content::Editor(e)) = self.content.get(&self.focus) else { return };
        let Some(path) = e.buffer.path() else { return };
        if self.recent.first().map(PathBuf::as_path) == Some(path) {
            return;
        }
        let path = path.to_path_buf();
        self.recent.retain(|p| *p != path);
        self.recent.insert(0, path);
        // Enough to walk back through, not a history feature.
        self.recent.truncate(20);
    }

    /// Ctrl+Tab: the focused pane goes back to the file it showed before.
    ///
    /// Skips anything already open in another pane — the same file in two
    /// editors is two buffers drifting apart, which is worse than staying
    /// put — and anything deleted since it was noted.
    fn open_last_file(&mut self) {
        // Never over the explorer: pressed from the tree, the file goes
        // next door, same as every other way of opening one.
        let pane = self.workspace_pane();
        if self.content.get(&pane).is_some_and(Content::is_live) {
            self.warn("something is running in this pane — no file to switch back to");
            return;
        }
        let current: Option<PathBuf> = match self.content.get(&pane) {
            Some(Content::Editor(e)) => e.buffer.path().map(Path::to_path_buf),
            _ => None,
        };
        let elsewhere: Vec<PathBuf> = self
            .content
            .iter()
            .filter(|(p, _)| **p != pane)
            .filter_map(|(_, c)| match c {
                Content::Editor(e) => e.buffer.path().map(Path::to_path_buf),
                _ => None,
            })
            .collect();
        let target = self
            .recent
            .iter()
            .find(|p| {
                Some(p.as_path()) != current.as_deref() && !elsewhere.contains(p) && p.exists()
            })
            .cloned();
        let Some(path) = target else {
            self.warn("no earlier file to switch back to");
            return;
        };
        // The same protection every other open has.
        if self.unsaved_in(pane) {
            self.warn("unsaved changes there — save first, or close the pane");
            return;
        }
        self.content.insert(pane, Content::open_path(&path));
        self.focus = pane;
    }

    /// Replaces every occurrence in the focused editor, as one undo step.
    ///
    /// The occurrences come from the same matcher Find uses, so this changes
    /// exactly the set of places Find would have highlighted — smart-case,
    /// literal, no surprises between the two features.
    fn replace_all(&mut self, needle: &str, with: &str) {
        let chars = needle.chars().count();
        let ranges: Vec<(kb_edit::Pos, usize)> = match self.content.get(&self.focus) {
            Some(Content::Editor(e)) => e
                .buffer
                .lines()
                .iter()
                .enumerate()
                .flat_map(|(line, text)| {
                    kb_find::occurrences(needle, text)
                        .into_iter()
                        .map(move |col| (kb_edit::Pos::new(line, col), chars))
                })
                .collect(),
            _ => return,
        };
        if ranges.is_empty() {
            self.warn(&format!("no match for '{needle}'"));
            return;
        }
        if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
            let n = e.buffer.replace_ranges(&ranges, with);
            e.status = Some(format!(
                "replaced {n} occurrence{}",
                if n == 1 { "" } else { "s" }
            ));
        }
    }

    /// The selected path in the focused explorer.
    fn explorer_selection(&self) -> Option<PathBuf> {
        let Some(Content::Explorer(e)) = self.content.get(&self.focus) else { return None };
        Some(e.tree.selected_row()?.path.clone())
    }

    /// Where a new entry should go: the selected directory, or the parent of
    /// the selected file. Creating a sibling of what you are looking at is
    /// what people mean, and it is what every file manager does.
    fn explorer_target_dir(&self) -> Option<PathBuf> {
        let Some(Content::Explorer(e)) = self.content.get(&self.focus) else {
            // Not in the tree: beside the file being edited, or at the
            // workspace root. Ctrl+N must not require a detour through the
            // explorer to mean anything.
            return match self.content.get(&self.focus) {
                Some(Content::Editor(ed)) => ed
                    .buffer
                    .path()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .or_else(|| Some(self.root.clone())),
                _ => Some(self.root.clone()),
            };
        };
        let Some(row) = e.tree.selected_row() else {
            return Some(e.tree.root().to_path_buf());
        };
        Some(if row.is_dir {
            row.path.clone()
        } else {
            row.path.parent().map(Path::to_path_buf).unwrap_or_else(|| e.tree.root().to_path_buf())
        })
    }

    /// Moves the workspace to another directory.
    ///
    /// The file tree used to move its own root and nothing else, which left
    /// Ctrl+P, project search, git and the session pointing at the directory
    /// kubide was started in: Backspace showed `C:\` in the tree while the
    /// finder still listed the old repository. There is one root, and
    /// everything keyed to it moves together.
    fn set_workspace_root(&mut self, root: PathBuf) {
        if root == self.root {
            return;
        }
        // Written before the path changes: the layout on screen belongs to the
        // workspace being left, and a moment later there is no way to name it.
        self.save_session();
        let previous = std::mem::replace(&mut self.root, root);

        // Rediscovered rather than adjusted. The new root may be a different
        // repository, a parent holding several, or no repository at all, and
        // only git can say which. Discovery refreshes, so the clock resets too
        // or the next tick would immediately run a second status.
        self.git = kb_git::Git::discover(&self.root);
        self.git_at = Instant::now();
        // From here the layout is remembered against the new workspace, and
        // the layout it had remembered before is overwritten rather than
        // restored. Deliberate: Backspace is a navigation key, not "open
        // workspace", and rearranging someone's panes under them because they
        // stepped up a directory would be far worse than losing a layout they
        // have not looked at.
        self.session_path = session::path_for(&self.root);
        self.session_at = Instant::now();

        // Every explorer follows, not just the focused one. Leaving the others
        // where they were would put the divergence back, one pane over.
        let root = self.root.clone();
        for c in self.content.values_mut() {
            if let Content::Explorer(e) = c {
                e.move_root(root.clone(), &previous);
            }
        }

        if let Some(window) = self.window {
            kb_win::set_title(window, &title_for(&self.root));
        }
        // Backspace is one key and a directory listing looks much the same on
        // either side of it, so the move has to be said out loud. Losing the
        // branch from the status bar is otherwise the only clue.
        self.warn(&format!("workspace root is now {}", self.root.display()));
    }

    fn refresh_explorers(&mut self) {
        for c in self.content.values_mut() {
            if let Content::Explorer(e) = c {
                e.tree.refresh();
            }
        }
        self.git.refresh();
    }

    /// Acts on a choice.
    ///
    /// Anything that is not save or discard is a cancel, which includes
    /// Escape: the palette drops the pending question with it, so an answer
    /// can never arrive at the wrong operation later.
    fn apply_answer(&mut self, index: usize) {
        let Some(op) = self.pending.take() else { return };
        match op {
            Pending::ReplaceUnsaved(pane, path) => {
                if index == SAVE {
                    if let Some(Content::Editor(e)) = self.content.get_mut(&pane) {
                        e.save();
                        if e.buffer.modified() {
                            self.warn("still unsaved — nothing was opened over it");
                            return;
                        }
                    }
                } else if index != DISCARD {
                    return;
                }
                // Saved or given up on; either way the pane is free now.
                self.content.insert(pane, Content::open_path(&path));
                self.focus = pane;
            }
            Pending::CloseUnsaved(pane) => {
                if index == SAVE {
                    if let Some(Content::Editor(e)) = self.content.get_mut(&pane) {
                        e.save();
                        // A failed save must not close the pane; that is the
                        // one case where discarding was never asked for.
                        if e.buffer.modified() {
                            self.warn("still unsaved — the pane is staying open");
                            return;
                        }
                    }
                } else if index != DISCARD {
                    return;
                }
                self.close_pane(pane);
            }
            Pending::SwitchUnsaved(dir) => {
                if index == SAVE {
                    let failed = self.save_every_editor();
                    // A failed save must not take the workspace down with it:
                    // the panes holding that work are about to be thrown away.
                    if failed > 0 {
                        let plural = if failed == 1 { "file" } else { "files" };
                        self.warn(&format!("{failed} {plural} would not save — staying put"));
                        return;
                    }
                } else if index != DISCARD {
                    return;
                }
                self.open_workspace(dir);
            }
            Pending::QuitUnsaved => {
                if index == SAVE {
                    let failed = self.save_every_editor();
                    if failed > 0 {
                        let plural = if failed == 1 { "file" } else { "files" };
                        self.warn(&format!("{failed} {plural} would not save — still here"));
                        return;
                    }
                } else if index != DISCARD {
                    return;
                }
                self.save_session();
                kb_win::quit();
            }
            // The text prompts answer through `apply_prompt`; putting one
            // back would lose it, so they are dropped here deliberately.
            _ => {}
        }
    }

    /// Saves every modified editor. Returns how many refused.
    fn save_every_editor(&mut self) -> usize {
        let mut failed = 0;
        for content in self.content.values_mut() {
            if let Content::Editor(e) = content {
                if e.buffer.modified() {
                    e.save();
                    if e.buffer.modified() {
                        failed += 1;
                    }
                }
            }
        }
        failed
    }

    /// Closes a pane and moves focus somewhere that still exists.
    ///
    /// Split out because closing now happens from two places: the key, and the
    /// answer to what should become of the unsaved work in it.
    fn close_pane(&mut self, pane: PaneId) {
        // Found before closing; afterwards the pane is gone and so is the
        // question of what is next to it.
        let next = focus_in_dir(&self.layout, pane, Dir::Left)
            .or_else(|| focus_in_dir(&self.layout, pane, Dir::Right))
            .or_else(|| focus_in_dir(&self.layout, pane, Dir::Up))
            .or_else(|| focus_in_dir(&self.layout, pane, Dir::Down));
        // Dropping the content shuts a PTY down; leaving it would keep a shell
        // running with no way to reach it.
        self.content.remove(&pane);
        if self.tree.close(pane) {
            if let Some(n) = next {
                self.focus = n;
            }
            self.layout = self.tree.compute(self.area);
        }
        // The last pane cannot be removed — there would be nothing left to
        // draw — so closing it empties it instead. Refusing outright would
        // mean pressing close twice and watching nothing happen, which reads
        // as a broken key.
    }

    /// Acts on a prompt's answer.
    fn apply_prompt(&mut self, answer: String) {
        let Some(op) = self.pending.take() else { return };
        let (result, created) = match op {
            Pending::ProjectSearch => {
                // Capped: a search that matches everything should not turn
                // into a list nobody can scroll.
                const LIMIT: usize = 500;
                let hits = self.git.grep(&answer, LIMIT).unwrap_or_default();
                self.palette = Some(Palette::results(hits, &self.root));
                return;
            }
            Pending::ReplaceWhat => {
                // Step two rides on step one's answer, and the label carries
                // it so the box says what it is about to touch. An empty
                // second answer cancels, like every prompt — replacing with
                // nothing is the one thing this flow cannot express.
                self.pending = Some(Pending::ReplaceWith(answer.clone()));
                self.palette = Some(Palette::prompt(&format!("replace '{answer}' with"), ""));
                return;
            }
            Pending::ReplaceWith(needle) => {
                self.replace_all(&needle, &answer);
                return;
            }
            Pending::CommitMessage => {
                let result = self.git.commit(&answer);
                if let Some(Content::Git(g)) = self.content.get_mut(&self.focus) {
                    g.status = Some(match &result {
                        Ok(summary) => summary.clone(),
                        Err(e) => format!("commit failed: {e}"),
                    });
                }
                self.refresh_git_panel();
                return;
            }
            Pending::NewFile(dir) => {
                let path = dir.join(&answer);
                (kb_fs::ops::create_file(&path), Some(path))
            }
            Pending::NewFolder(dir) => (kb_fs::ops::create_dir(&dir.join(&answer)), None),
            Pending::Rename(from) => (kb_fs::ops::rename(&from, &answer).map(|_| ()), None),
            // Answered by picking a row, not by typing. Reaching here would
            // mean a choice was left waiting while a text prompt opened, which
            // cannot happen — but dropping it beats acting on the wrong one.
            Pending::CloseUnsaved(_)
            | Pending::QuitUnsaved
            | Pending::SwitchUnsaved(_)
            | Pending::ReplaceUnsaved(..) => return,
        };

        match result {
            Ok(()) => {
                self.refresh_explorers();
                // A new file opens straight away: making one and then having
                // to find it in the tree is a step nobody wants. The tree
                // keeps its pane; anyone else gets it where they stand — and
                // never over a shell or unsaved work.
                if let Some(path) = created {
                    let target = self.workspace_pane();
                    let occupied_by_shell = self.content.get(&target).is_some_and(Content::is_live);
                    if !occupied_by_shell && !self.unsaved_in(target) {
                        self.content.insert(target, Content::open_path(&path));
                        self.focus = target;
                    }
                }
            }
            Err(e) => self.warn(&e),
        }
    }

    /// Shows or hides the file tree.
    ///
    /// Hiding closes its pane outright rather than shrinking it to nothing: a
    /// zero-width pane is still in the layout, still takes divider hit-tests,
    /// and is impossible to grab again.
    fn toggle_explorer(&mut self) -> bool {
        if let Some(pane) = self.explorer_pane() {
            // Never leave focus on a pane that no longer exists.
            let next = focus_in_dir(&self.layout, pane, Dir::Right)
                .or_else(|| focus_in_dir(&self.layout, pane, Dir::Down))
                .or_else(|| focus_in_dir(&self.layout, pane, Dir::Left));
            self.content.remove(&pane);
            if self.tree.close(pane) {
                if let Some(n) = next {
                    self.focus = n;
                }
                self.layout = self.tree.compute(self.area);
            }
            return true;
        }

        // Put it on the left, which means splitting the leftmost pane and
        // handing its content to the new right-hand half.
        let leftmost = self
            .layout
            .panes
            .iter()
            .min_by(|(_, a), (_, b)| a.x.total_cmp(&b.x))
            .map(|(p, _)| *p)
            .unwrap_or(self.focus);

        let Some(right) = self.tree.split_at(leftmost, Axis::Horizontal, 0.25) else {
            return false;
        };
        if let Some(moved) = self.content.remove(&leftmost) {
            self.content.insert(right, moved);
        }
        self.content
            .insert(leftmost, Content::Explorer(Explorer::new(self.root.clone())));
        // Focus follows the content the user was working in, not the tree.
        self.focus = right;
        self.layout = self.tree.compute(self.area);
        true
    }

    /// Activates the explorer selection: expand a directory, or open a file.
    ///
    /// The file opens in the neighbouring pane when there is one, so the tree
    /// stays visible. That's the whole reason to have a pane tree.
    fn activate_explorer(&mut self) -> bool {
        let Some(Content::Explorer(e)) = self.content.get_mut(&self.focus) else {
            return false;
        };
        if e.tree.toggle_selected() {
            return true;
        }
        let Some(row) = e.tree.selected_row() else { return true };
        let path = row.path.clone();

        // Next door, or a fresh split — never into the tree's own pane,
        // which used to be the fallback when the tree was alone.
        let target = self.workspace_pane();
        // Opening a file over unsaved work is the same loss as closing the
        // pane, and it happens far more easily — one Enter in the tree.
        self.open_over(target, path);
        true
    }

    /// Editor keys: movement and the two deletes. Text itself arrives through
    /// `on_char`, which has the keyboard layout applied.
    ///
    /// Shift extends the selection everywhere, so it is read once here rather
    /// than repeated in every arm.
    fn editor_key(&mut self, vk: u8, mods: Mods) -> bool {
        let visible = self.visible_rows(self.focus);
        let auto_close = self.cfg.editor.auto_close;
        let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) else {
            return false;
        };
        let extend = mods.shift;
        let b = &mut e.buffer;
        match (vk, mods.ctrl) {
            (0x25, false) => b.move_left(extend),
            (0x27, false) => b.move_right(extend),
            (0x25, true) => b.move_word_left(extend),
            (0x27, true) => b.move_word_right(extend),
            (0x26, _) => b.move_vertical(-1, extend),
            (0x28, _) => b.move_vertical(1, extend),
            (0x21, _) => b.move_vertical(-(visible as isize), extend),
            (0x22, _) => b.move_vertical(visible as isize, extend),
            (0x24, false) => b.move_line_start(extend),
            (0x23, false) => b.move_line_end(extend),
            (0x24, true) => b.move_doc_start(extend),
            (0x23, true) => b.move_doc_end(extend),
            (0x08, false) => {
                e.status = None;
                if auto_close {
                    // A pair dies whole when the caret sits inside it.
                    b.backspace_pair();
                } else {
                    b.backspace();
                }
            }
            (0x08, true) => {
                e.status = None;
                b.delete_word_left();
            }
            (0x2E, false) => {
                e.status = None;
                b.delete();
            }
            (0x2E, true) => {
                e.status = None;
                b.delete_word_right();
            }
            // Tab is handled here rather than in on_char, because Shift+Tab
            // produces no character at all and would otherwise be unreachable.
            (0x09, _) => {
                e.status = None;
                if extend {
                    b.dedent(4);
                } else if b.selection().is_some() {
                    b.indent(4);
                } else {
                    // No selection: Tab is an indent at the caret, not a block
                    // shift, or pressing it mid-line would move the whole line.
                    b.insert("    ");
                }
            }
            // Enter comes through on_char, where the layout is applied.
            _ => return false,
        }
        true
    }

    /// Settings keys. Up and down pick a row, left and right change it.
    ///
    /// The change lands immediately rather than on a confirm: turning the
    /// clock on and having to press something else before seeing it is how you
    /// end up unsure whether the switch did anything. The file is the separate
    /// step, because that one is not reversible by pressing the arrow back.
    fn settings_key(&mut self, vk: u8) -> bool {
        let visible = self.visible_rows(self.focus);
        let Some(Content::Settings(s)) = self.content.get_mut(&self.focus) else {
            return false;
        };
        let delta = match vk {
            0x26 => return step_selection(s, -1),               // up
            0x28 => return step_selection(s, 1),                // down
            0x21 => return step_selection(s, -(visible as i32)), // page up
            0x22 => return step_selection(s, visible as i32),   // page down
            0x24 => return step_selection(s, i32::MIN / 2),     // home
            0x23 => return step_selection(s, i32::MAX / 2),     // end
            0x25 => -1,             // left
            0x27 | 0x0D | 0x20 => 1, // right, enter, space
            // Escape leaves. Deferred because putting the old pane back needs
            // `&mut self` again, which cannot happen while this one is still
            // borrowed out of the map.
            0x1B => return self.close_settings(),
            _ => return false,
        };

        let setting = s.setting();
        s.status = None;
        let mut next = self.cfg.clone();
        setting.step(&mut next, delta);
        self.apply_config(next);
        true
    }

    /// Welcome screen keys: pick a place, go there. Everything else falls
    /// through, so the global shortcuts all still work from the doorway.
    fn welcome_key(&mut self, vk: u8) -> bool {
        let chosen = {
            let Some(Content::Welcome(w)) = self.content.get_mut(&self.focus) else {
                return false;
            };
            match vk {
                0x26 => {
                    w.move_selection(-1);
                    return true;
                }
                0x28 => {
                    w.move_selection(1);
                    return true;
                }
                0x24 => {
                    w.move_selection(i32::MIN / 2);
                    return true;
                }
                0x23 => {
                    w.move_selection(i32::MAX / 2);
                    return true;
                }
                0x0D => match w.chosen() {
                    Some(dir) => dir,
                    None => return true,
                },
                _ => return false,
            }
        };
        self.switch_workspace(chosen);
        true
    }

    /// Moves to another workspace, asking first if that would cost work.
    ///
    /// The one way in, so that every route to another folder — the picker,
    /// the tree, the welcome screen — behaves the same. Refusing to switch
    /// while anything is unsaved used to be the answer, which left the person
    /// to find and save each file themselves; being asked is what a person
    /// would do.
    fn switch_workspace(&mut self, dir: PathBuf) {
        // Canonicalised because a row can be reached by a typed `..\`, and
        // this path goes on to be the window title, the session's name and
        // every new shell's working directory. De-armoured straight after,
        // the same way the command line's is.
        let dir = kb_fs::strip_verbatim(std::fs::canonicalize(&dir).unwrap_or(dir));
        if !dir.is_dir() {
            // Listed a moment ago and gone now, or typed by hand and never
            // there at all.
            self.warn(&format!("not a folder: {}", dir.display()));
            return;
        }
        if dir == self.root {
            return;
        }
        let n = self.unsaved_count();
        if n == 0 {
            self.open_workspace(dir);
            return;
        }
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        self.pending = Some(Pending::SwitchUnsaved(dir));
        self.palette = Some(Palette::ask(
            "Unsaved Changes",
            &format!("Open {name} with {}?", unsaved_phrase(n)),
            &["Save all", "Discard", "Cancel"],
        ));
    }

    /// Moves to another workspace: its remembered layout when it has one,
    /// the standard tree-beside-work start when it does not.
    ///
    /// The caller has already made sure nothing unsaved is on screen. The
    /// old layout goes entirely — shells included — because half a window
    /// of one project next to half of another is not a workspace, it is an
    /// accident waiting for a save.
    fn open_workspace(&mut self, dir: PathBuf) {
        self.set_workspace_root(dir);
        session::note_workspace(&self.root);
        // The new root may carry its own .kubide overrides — a per-project
        // theme has to land with the project, not one restart later.
        self.reload_config();
        if !self.restore_session() {
            let (tree, first) = Tree::new();
            self.tree = tree;
            self.content.clear();
            self.focus = first;
            self.open_explorer();
            if let Some(right) = self.tree.split_at(self.focus, Axis::Horizontal, 0.25) {
                self.focus = right;
            }
            self.layout = self.tree.compute(self.area);
        }
        self.save_session();
    }

    /// Git panel keys.
    ///
    /// Deferred actions, the same shape as the explorer: acting needs
    /// `&mut self` again — git, the palette, the pane map — which cannot
    /// happen while the panel is still borrowed out of the map.
    /// Keys the agent pane owns: the box at the bottom and the transcript
    /// above it.
    fn agent_key(&mut self, vk: u8, mods: Mods) -> bool {
        let visible = self
            .layout
            .rect_of(self.focus)
            .map(|r| agent::visible_rows(r.h, self.text.line_height()))
            .unwrap_or(1);
        let said = {
            let Some(Content::Agent(a)) = self.content.get_mut(&self.focus) else {
                return false;
            };
            // A question up in the pane takes the keys a question takes —
            // and only from this pane. Focus elsewhere cannot answer it,
            // which is the point: the person looking at it decides. The
            // transcript still scrolls, so what is being asked about can
            // be read.
            if a.needs_answer() {
                match vk {
                    0x0D => a.ask_answer(),
                    0x1B => a.ask_deny(),
                    0x25 => a.ask_move(-1),
                    0x27 => a.ask_move(1),
                    0x26 => a.scroll(3, visible),
                    0x28 => a.scroll(-3, visible),
                    0x21 => a.scroll(visible as i32, visible),
                    0x22 => a.scroll(-(visible as i32), visible),
                    _ => return false,
                }
                self.typed_at = Instant::now();
                return true;
            }
            // The completion list, while `/` is being typed: up and down
            // walk it, Tab or Right take the pick, Enter takes it too and
            // sends on the next press. Everything else types on.
            if !a.completions().is_empty() {
                match vk {
                    0x26 => {
                        a.complete_move(-1);
                        return true;
                    }
                    0x28 => {
                        a.complete_move(1);
                        return true;
                    }
                    0x09 | 0x27 => {
                        a.complete();
                        return true;
                    }
                    _ => {}
                }
            }
            match vk {
                0x0D => {
                    a.send();
                    None
                }
                0x1B => {
                    a.cancel();
                    None
                }
                0x08 => {
                    a.backspace(mods.ctrl);
                    None
                }
                0x26 => {
                    a.scroll(3, visible);
                    None
                }
                0x28 => {
                    a.scroll(-3, visible);
                    None
                }
                0x21 => {
                    a.scroll(visible as i32, visible);
                    None
                }
                0x22 => {
                    a.scroll(-(visible as i32), visible);
                    None
                }
                _ => return false,
            }
        };
        self.typed_at = Instant::now();
        if let Some(said) = said {
            self.warn(said);
        }
        true
    }

    fn git_key(&mut self, vk: u8, mods: Mods) -> bool {
        use content::GitView;

        // An unbound Ctrl chord must fall through as unhandled, not land
        // here as its bare letter — Ctrl+C arriving as C would commit.
        if mods.ctrl {
            return false;
        }

        enum Do {
            Toggle(kb_git::Entry),
            Diff(kb_git::Entry),
            Commit,
            Log,
            ShowCommit(kb_git::Commit),
            Refresh,
            Close,
            Discard(kb_git::Entry),
            Remote(kb_git::RemoteOp),
        }

        let visible = self.visible_rows(self.focus);
        // Any key but the second X means the user moved on; a discard
        // confirmation must not survive to swallow a later press.
        let pending = self.confirm.take();
        let act = {
            let Some(Content::Git(g)) = self.content.get_mut(&self.focus) else {
                return false;
            };
            match vk {
                0x26 => {
                    g.move_selection(-1, visible);
                    return true;
                }
                0x28 => {
                    g.move_selection(1, visible);
                    return true;
                }
                0x21 => {
                    g.move_selection(-(visible as i32), visible);
                    return true;
                }
                0x22 => {
                    g.move_selection(visible as i32, visible);
                    return true;
                }
                0x24 => {
                    g.move_selection(i32::MIN / 2, visible);
                    return true;
                }
                0x23 => {
                    g.move_selection(i32::MAX / 2, visible);
                    return true;
                }
                // One screen back; off the first screen, out. Left and
                // backspace walk back too but never close — leaving should
                // take the deliberate key, not the one held for scrolling
                // history in some other pane a moment ago.
                0x1B | 0x25 | 0x08 => {
                    if g.back() {
                        return true;
                    }
                    if vk == 0x1B {
                        Do::Close
                    } else {
                        return true;
                    }
                }
                // Space: the whole point of the panel.
                0x20 => match (g.view, g.selected_entry()) {
                    (GitView::Status, Some(e)) => Do::Toggle(e.clone()),
                    _ => return true,
                },
                0x0D => match g.view {
                    GitView::Status => match g.selected_entry() {
                        Some(e) => Do::Diff(e.clone()),
                        None => return true,
                    },
                    GitView::Log => match g.selected_commit() {
                        Some(c) => Do::ShowCommit(c.clone()),
                        None => return true,
                    },
                    GitView::Diff => return true,
                },
                0x43 => Do::Commit,  // c
                0x4C => Do::Log,     // l
                0x52 => Do::Refresh, // r
                // Discard, on the file list only: X elsewhere is nothing.
                0x58 => match (g.view, g.selected_entry()) {
                    (GitView::Status, Some(e)) => Do::Discard(e.clone()),
                    _ => return true,
                },
                // P pushes; with shift it pulls, the pairing lazygit taught.
                0x50 => Do::Remote(if mods.shift {
                    kb_git::RemoteOp::Pull
                } else {
                    kb_git::RemoteOp::Push
                }),
                _ => return false,
            }
        };

        match act {
            Do::Toggle(e) => {
                let result = if e.staged {
                    self.git.unstage(&e.path)
                } else {
                    self.git.stage(&e.path)
                };
                let said = match result {
                    Ok(()) => {
                        format!("{} {}", if e.staged { "unstaged" } else { "staged" }, e.rel)
                    }
                    Err(err) => err,
                };
                self.refresh_git_panel();
                if let Some(Content::Git(g)) = self.content.get_mut(&self.focus) {
                    g.status = Some(said);
                }
            }
            Do::Diff(e) => {
                let lines = if e.status == kb_git::Status::Untracked {
                    // Untracked means git has nothing to compare against: the
                    // whole file is the change, so it is shown as one.
                    match std::fs::read_to_string(&e.path) {
                        Ok(text) => text
                            .lines()
                            .map(|l| (kb_git::DiffKind::Add, format!("+{l}")))
                            .collect(),
                        Err(err) => vec![(kb_git::DiffKind::Meta, format!("unreadable: {err}"))],
                    }
                } else {
                    self.git.diff_file(&e.path, e.staged)
                };
                let title = if e.staged { format!("{} \u{b7} staged", e.rel) } else { e.rel.clone() };
                if let Some(Content::Git(g)) = self.content.get_mut(&self.focus) {
                    if lines.is_empty() {
                        // Binary, or the working tree caught up with the
                        // index. An empty screen would read as a hang.
                        g.status = Some(format!("no diff to show for {}", e.rel));
                    } else {
                        g.show_diff(title, lines, false);
                    }
                }
            }
            Do::Commit => {
                let staged = matches!(
                    self.content.get(&self.focus),
                    Some(Content::Git(g)) if g.entries.iter().any(|e| e.staged)
                );
                if staged {
                    self.pending = Some(Pending::CommitMessage);
                    self.palette = Some(Palette::prompt("commit message", ""));
                } else if let Some(Content::Git(g)) = self.content.get_mut(&self.focus) {
                    g.status = Some("nothing staged — Space stages the selected file".into());
                }
            }
            Do::Log => {
                let commits = self.git.log(200);
                if let Some(Content::Git(g)) = self.content.get_mut(&self.focus) {
                    if commits.is_empty() {
                        g.status = Some("no commits yet".into());
                    } else {
                        g.log_selected = g.log_selected.min(commits.len() - 1);
                        g.commits = commits;
                        g.view = GitView::Log;
                    }
                }
            }
            Do::ShowCommit(c) => {
                let lines = self.git.show(&c.hash);
                if let Some(Content::Git(g)) = self.content.get_mut(&self.focus) {
                    g.show_diff(format!("{} \u{b7} {}", c.hash, c.subject), lines, true);
                }
            }
            Do::Refresh => {
                self.refresh_git_panel();
                if let Some(Content::Git(g)) = self.content.get_mut(&self.focus) {
                    g.status = Some("refreshed".into());
                }
            }
            Do::Close => {
                self.close_git_panel();
            }
            Do::Discard(e) => {
                let said = if e.staged {
                    "staged — Space unstages it first, then X discards".to_string()
                } else if e.status == kb_git::Status::Untracked {
                    // Restore has no "before" for a file git never saw;
                    // deleting it is the tree's job, with its own confirm.
                    "untracked — nothing to restore; delete it in the tree".to_string()
                } else if pending == Some(Confirm::DiscardFile(e.path.clone())) {
                    let said = match self.git.discard(&e.path) {
                        Ok(()) => format!("discarded changes to {}", e.rel),
                        Err(err) => err,
                    };
                    self.refresh_git_panel();
                    said
                } else {
                    self.confirm = Some(Confirm::DiscardFile(e.path.clone()));
                    format!("discard changes to {}? this cannot be undone — X again", e.rel)
                };
                if let Some(Content::Git(g)) = self.content.get_mut(&self.focus) {
                    g.status = Some(said);
                }
            }
            Do::Remote(op) => {
                // Off the UI thread: a push can sit on the network for
                // seconds, and the result lands through the tick.
                let said = match self.git.start_remote(op) {
                    Ok(name) => format!("{name}ing\u{2026}"),
                    Err(err) => err,
                };
                if let Some(Content::Git(g)) = self.content.get_mut(&self.focus) {
                    g.status = Some(said);
                }
            }
        }
        true
    }

    /// Explorer keys. Returns false for anything it doesn't own, so the normal
    /// shortcuts still work.
    fn explorer_key(&mut self, vk: u8) -> bool {
        let visible = self.visible_rows(self.focus);
        // Anything but activation means the user moved on, so a pending
        // confirmation must not survive to swallow a later Enter.
        if !matches!(vk, 0x0D | 0x27) {
            self.confirm = None;
            self.notice = None;
        }
        // Deferred: these need `&mut self` again, which can't happen while the
        // explorer is still borrowed out of the map.
        let mut activate = false;
        let mut refresh_git = false;
        let mut go_up = None;
        {
            let Some(Content::Explorer(e)) = self.content.get_mut(&self.focus) else {
                return false;
            };
            match vk {
                0x26 => e.tree.move_selection(-1), // up
                0x28 => e.tree.move_selection(1),  // down
                0x21 => e.tree.move_selection(-(visible as i32)), // page up
                0x22 => e.tree.move_selection(visible as i32), // page down
                0x24 => e.tree.select(0),          // home
                0x23 => e.tree.select(usize::MAX), // end
                0x25 => e.tree.collapse_or_parent(), // left
                // Right expands but never collapses: on a directory that is
                // already open it steps into it rather than closing what you
                // just opened.
                0x27 => {
                    if e.tree.selected_row().is_some_and(|r| r.is_dir && r.open) {
                        e.tree.move_selection(1);
                    } else {
                        activate = true;
                    }
                }
                0x0D => activate = true, // enter
                0x74 => {
                    // F5 refreshes both: a file that just changed on disk
                    // usually changed its git status too.
                    e.tree.refresh();
                    refresh_git = true;
                }
                0x08 => {
                    // Backspace moves the whole workspace up a level, not just
                    // this tree. `None` at a drive root, where the only thing
                    // above is nothing.
                    go_up = e.tree.root().parent().map(Path::to_path_buf);
                }
                _ => return false,
            }
        }
        if refresh_git {
            self.git.refresh();
        }
        if let Some(parent) = go_up {
            self.set_workspace_root(parent);
        }
        if activate {
            self.activate_explorer();
        }
        true
    }
}

impl Handler for Kubide {
    fn on_create(&mut self, window: Window) {
        self.window = Some(window);
    }

    /// The close button, Alt+F4 and the taskbar all come through here, which is
    /// why the confirmation cannot live only in the quit shortcut.
    fn on_close(&mut self) -> bool {
        let closing = self.confirm_quit();
        if closing {
            self.save_session();
        }
        closing
    }

    fn on_paint(&mut self, window: Window, chrome: &Chrome) {
        let _ = self.render(window, chrome);
    }

    fn on_resize(&mut self, width: u32, height: u32) {
        if let Some(gfx) = &mut self.gfx {
            let _ = gfx.resize(width, height);
        }
        self.relayout(width as f32, height as f32);
    }

    /// Text input. WM_CHAR has the keyboard layout applied, which is the only
    /// way ğ/ş/İ arrive correctly on a Turkish layout.
    fn on_char(&mut self, c: char) -> bool {
        if let Some(p) = &mut self.folder_picker {
            // Control characters are chords, not filter text.
            if (c as u32) >= 0x20 {
                p.push(c);
                // A caret holds still while a word is being typed.
                self.typed_at = Instant::now();
            }
            return true;
        }
        if let Some(p) = &mut self.palette {
            // A question has no text box, and letting a stray keystroke reach
            // the query would filter answers out of a list of three.
            if p.mode != palette::Mode::Choice && (c as u32) >= 0x20 {
                p.push(c);
                self.typed_at = Instant::now();
            }
            return true;
        }
        // Typing means the user moved on from whatever was being confirmed.
        self.confirm = None;
        self.notice = None;
        match self.content.get_mut(&self.focus) {
            Some(Content::Terminal(t)) => {
                // Typing clears the selection, like every terminal does.
                // Leaving it up makes it ambiguous what a copy would take.
                t.select_clear();
                // Ctrl+letter arrives here as a control character already
                // (Ctrl+C = 0x03), so nothing needs encoding. Enter arrives as
                // \r, which is what the shell expects.
                let mut buf = [0u8; 4];
                t.write(c.encode_utf8(&mut buf).as_bytes());
                true
            }
            Some(Content::Editor(_)) => {
                // Ctrl+[ arrives as the escape character, and to a vim user
                // it is Esc; every other control character is a chord that
                // was not bound, and typing it would put garbage in the file.
                if self.cfg.vim.enabled {
                    let key = match c {
                        '\u{1b}' => Some(kb_vim::Key::Esc),
                        '\r' => Some(kb_vim::Key::Enter),
                        '\t' => Some(kb_vim::Key::Tab),
                        c if (c as u32) < 0x20 => None,
                        c => Some(kb_vim::Key::Char(c)),
                    };
                    match key.and_then(|k| self.vim_key(k)) {
                        Some(true) => return true,
                        Some(false) => {}
                        None => return false,
                    }
                }
                let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) else {
                    return false;
                };
                if (c as u32) < 0x20 && c != '\r' && c != '\t' {
                    return false;
                }
                e.status = None;
                match c {
                    '\r' => e.buffer.insert_newline(),
                    // Snippets first: `print` plus Tab is a question, and
                    // four spaces is the answer only when nothing matched.
                    '\t' => {
                        let trigger = if self.cfg.editor.snippets && e.buffer.selection().is_none()
                        {
                            e.buffer.word_before_cursor()
                        } else {
                            String::new()
                        };
                        let ext = e
                            .buffer
                            .path()
                            .and_then(|p| p.extension())
                            .and_then(|x| x.to_str())
                            .map(str::to_lowercase)
                            .unwrap_or_default();
                        let body = (!trigger.is_empty())
                            .then(|| self.snippets.get(&ext, &trigger))
                            .flatten()
                            .map(str::to_string);
                        match body {
                            Some(body) => {
                                e.buffer.expand_snippet(trigger.chars().count(), &body)
                            }
                            // Spaces, not a tab character: the renderer is a
                            // cell grid with no tab stops, so a real tab
                            // would not line up.
                            None => e.buffer.insert("    "),
                        }
                    }
                    // The pairing lives behind its switch: people who hate
                    // auto-closing hate it completely.
                    _ if self.cfg.editor.auto_close => e.buffer.type_char(c),
                    _ => e.buffer.insert_char(c),
                }
                true
            }
            Some(Content::Agent(a)) => {
                // Enter, Escape and Backspace arrive as keys and are handled
                // there; their control characters are not text. A question
                // has no text box, so typing at it goes nowhere.
                if (c as u32) < 0x20 || a.needs_answer() {
                    return false;
                }
                a.push(c);
                self.typed_at = Instant::now();
                true
            }
            _ => false,
        }
    }

    fn on_wheel(&mut self, x: f32, y: f32, lines: i32) -> bool {
        // The picker owns the wheel while it is up, wherever the cursor is:
        // it also owns every key and click, and a wheel that scrolls the
        // editor behind an overlay scrolls something nobody is looking at.
        if self.folder_picker.is_some() {
            let visible = self.picker_hits.as_ref().map(|h| h.visible).unwrap_or(10);
            if let Some(p) = &mut self.folder_picker {
                p.scroll(lines, visible);
            }
            return true;
        }
        // The wheel goes to the pane under the cursor, not the focused one —
        // the mouse already said what it meant.
        let target = match self.tree.hit(&self.layout, x, y) {
            Some(Hit::Pane(p)) => p,
            _ => self.focus,
        };
        let visible = self.visible_rows(target);
        let agent_visible = self
            .layout
            .rect_of(target)
            .map(|r| agent::visible_rows(r.h, self.text.line_height()))
            .unwrap_or(1);
        let keys = self.cfg.keys.clone();
        match self.content.get_mut(&target) {
            Some(Content::Agent(a)) => {
                a.scroll(lines, agent_visible);
                true
            }
            Some(Content::Terminal(t)) => {
                // On the alternate screen (vim, btop) there is no scrollback,
                // so arrow keys are the correct translation.
                if t.in_alt_screen() {
                    let key: &[u8] = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
                    for _ in 0..lines.abs().min(5) {
                        t.write(key);
                    }
                } else {
                    t.scroll(lines);
                }
                true
            }
            Some(Content::Explorer(e)) => {
                e.scroll(lines, visible);
                true
            }
            Some(Content::Viewer(v)) => {
                v.scroll(lines, visible);
                true
            }
            Some(Content::Editor(e)) => {
                e.scroll(lines, visible);
                true
            }
            Some(Content::Settings(s)) => {
                s.scroll(&keys, lines, visible);
                true
            }
            Some(Content::Git(g)) => {
                g.scroll(lines, visible);
                true
            }
            // Eight rows at most; there is nothing to scroll to.
            Some(Content::Welcome(_)) => false,
            None => false,
        }
    }

    fn on_tick(&mut self) -> bool {
        // The theme file counts as config: recolour, save, see it — that
        // round trip is the whole point of the themes folder.
        if self.cfg_watch.as_ref().is_some_and(|w| w.changed())
            || self.theme_watch.as_ref().is_some_and(|w| w.changed())
            || self.ws_watch.as_ref().is_some_and(|w| w.changed())
        {
            return self.reload_config();
        }

        // Elapsed time, not a tick count. Windows throttles a background
        // window's timer hard — counting ticks turned "every two seconds" into
        // "every few minutes" the moment the window lost focus.
        if self.session_at.elapsed() >= SESSION_INTERVAL {
            self.session_at = Instant::now();
            self.save_session();
        }

        let mut tree_changed = false;
        if self.git_at.elapsed() >= GIT_INTERVAL {
            self.git_at = Instant::now();
            self.git.refresh();
            // The tree goes with it. Creating a file in the terminal and not
            // seeing it appear makes the explorer look stale and untrusted,
            // and this costs one directory read per open folder. The change
            // report feeds the redraw below — a refresh that stays silent
            // leaves the new file invisible until an unrelated repaint.
            for c in self.content.values_mut() {
                if let Content::Explorer(e) = c {
                    tree_changed |= e.tree.refresh();
                }
            }
        }
        // Only redraws when the status actually changed, so a clean tree costs
        // nothing between refreshes.
        let git_changed = self.git.poll();
        if git_changed {
            // Editor gutters re-read their diff marks against this stamp the
            // next time they draw — lazily, so hidden panes never pay.
            self.git_gen += 1;
        }
        // Cheap enough to do every tick: one path comparison, almost always
        // equal. Watching from here is what lets Ctrl+Tab count a focus
        // change as "I was in that file" without hooks on every open.
        self.note_recent();

        // A push or pull that finished while we were drawing frames. The
        // result goes to every open panel — and to the status bar when the
        // panel was closed before the network came back.
        let remote_done = if let Some((name, result)) = self.git.poll_remote() {
            let said = match result {
                Ok(line) => format!("{name}: {line}"),
                Err(err) => format!("{name} failed: {err}"),
            };
            let entries = self.git.entries();
            let mut told = false;
            for c in self.content.values_mut() {
                if let Content::Git(g) = c {
                    g.status = Some(said.clone());
                    g.set_entries(entries.clone());
                    told = true;
                }
            }
            if !told {
                self.warn(&said);
            }
            // A pull moved files; the tree colours and gutters follow.
            self.git.refresh();
            true
        } else {
            false
        };

        // The agent's stream lands on its own schedule too. A tool that
        // wrote a file moved the working tree under the open editors: the
        // clean ones re-read, and the tree colours and gutters follow.
        let mut agent_changed = false;
        let mut edited = false;
        let mut busy_secs = 0;
        for c in self.content.values_mut() {
            if let Content::Agent(a) = c {
                let polled = a.poll();
                agent_changed |= polled.changed;
                edited |= polled.edited;
                // Streamed text is let out a few characters a frame; the
                // frame that moves it is owed a repaint.
                agent_changed |= a.animate();
                busy_secs += a.busy_secs().unwrap_or(0);
            }
        }
        // The running count in a busy pane's header moves once a second;
        // a repaint then and not sixty times a second in between.
        if busy_secs != self.agent_stamp_last {
            self.agent_stamp_last = busy_secs;
            agent_changed = true;
        }
        if edited {
            self.reload_clean_editors();
            self.git.refresh();
            for c in self.content.values_mut() {
                if let Content::Explorer(e) = c {
                    tree_changed |= e.tree.refresh();
                }
            }
        }

        // Redraw for the clock and the countdown only when the text they
        // produce would actually differ. Treating "enabled" as "changed"
        // repaints sixty times a second for a display that moves once — which
        // measured at three percent of a core doing nothing.
        let stamp = self.status_stamp();
        let ticking = self.timer.poll() || stamp != self.status_stamp_last;
        self.status_stamp_last = stamp;

        // Terminal output arrives on its own schedule; draw only on change so
        // an idle kubide never touches the GPU.
        let dirty = self
            .content
            .values()
            .filter_map(Content::as_terminal)
            .any(|t| t.take_dirty());

        // The caret in an open overlay, on the flip only — a blink is two
        // repaints a second, not sixty.
        let blink = self.caret_on();
        let agent_focused = matches!(self.content.get(&self.focus), Some(Content::Agent(_)));
        let blinked = (self.folder_picker.is_some() || self.palette.is_some() || agent_focused)
            && blink != self.blink_last;
        self.blink_last = blink;

        dirty || git_changed || ticking || remote_done || tree_changed || blinked || agent_changed
    }

    fn on_mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.mouse = (x, y);
        if let Some((d, lx, ly)) = self.dragging {
            let axis = self
                .layout
                .dividers
                .iter()
                .find(|(dd, _, _)| *dd == d)
                .map(|(_, a, _)| *a);
            let delta = match axis {
                Some(Axis::Horizontal) => x - lx,
                _ => y - ly,
            };
            self.tree.drag(d, delta, self.area);
            self.layout = self.tree.compute(self.area);
            self.dragging = Some((d, x, y));
            return true;
        }

        if let Some(pane) = self.sel_drag {
            if let (Some((c, r)), Some(t)) = (self.term_cell_at(pane, x, y), self.terminal(pane)) {
                t.select_update(c, r);
                return true;
            }
            if let Some(pos) = self.editor_pos_at(pane, x, y) {
                if let Some(Content::Editor(e)) = self.content.get_mut(&pane) {
                    // `extend` keeps the anchor, which is what makes a drag a
                    // selection rather than a series of cursor jumps.
                    e.buffer.move_to(pos, true);
                }
                return true;
            }
        }

        // The picker's rows and buttons light up under the mouse, so while
        // it is open every move is worth a frame. Only the picker: the rest
        // of the window repaints on state changes, not on travel.
        if self.folder_picker.is_some() {
            return true;
        }

        let prev = self.hover_divider;
        self.hover_divider = match self.tree.hit(&self.layout, x, y) {
            Some(Hit::Divider(_, axis)) => Some(axis),
            _ => None,
        };

        // The settings button lights up under the cursor — the one visual
        // promise a clickable thing makes. Redrawn only on the crossing.
        let hover = self.settings_btn.is_some_and(|r| r.contains(x, y));
        let crossed = hover != self.settings_hover;
        self.settings_hover = hover;

        prev != self.hover_divider || crossed
    }

    fn on_mouse_down(&mut self, x: f32, y: f32) -> bool {
        // While an overlay is up it owns the mouse too. Without this the
        // click lands on a pane underneath and moves focus behind a list the
        // user is still looking at.
        if self.folder_picker.is_some() {
            self.picker_click(x, y);
            return false;
        }
        if self.palette.is_some() {
            self.palette_click(x, y);
            return false;
        }
        // The settings button presses like a button: down marks it, and the
        // action fires on release, only if the cursor is still on it — the
        // way every button since the first one has let go of a mis-click.
        if self.settings_btn.is_some_and(|r| r.contains(x, y)) {
            self.settings_pressed = true;
            return true; // capture, so the release comes back here
        }
        // The corner chips sit over the panes, so they are asked first.
        if let Some(action) = self
            .corner_chips
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, a)| *a)
        {
            self.run(action);
            return false;
        }
        match self.tree.hit(&self.layout, x, y) {
            Some(Hit::Divider(d, _)) => {
                self.dragging = Some((d, x, y));
                true // capture the mouse
            }
            Some(Hit::Pane(p)) => {
                self.focus = p;
                if let Some(row) = self.explorer_row_at(p, y) {
                    if let Some(Content::Explorer(e)) = self.content.get_mut(&p) {
                        let already = e.tree.selected() == row;
                        e.tree.select(row);
                        // Click selects; clicking the selected row activates.
                        // Double-click would need timing state for the same
                        // result, and this stays predictable.
                        if already {
                            self.activate_explorer();
                        }
                    }
                    return false;
                }
                if let (Some(t), Some((c, r))) = (self.terminal(p), self.term_cell_at(p, x, y)) {
                    t.select_start(c, r);
                    self.sel_drag = Some(p);
                    // Capture, or a drag past the pane edge breaks.
                    return true;
                }
                if let Some(pos) = self.editor_pos_at(p, x, y) {
                    // A second click in the same spot takes the word. The
                    // interval comes from Windows: it is a user setting, and
                    // inventing our own number would quietly ignore it.
                    let again = self.last_click.is_some_and(|(lp, lpos, at)| {
                        lp == p
                            && lpos == pos
                            && at.elapsed().as_millis() <= kb_win::double_click_ms() as u128
                    });
                    if let Some(Content::Editor(e)) = self.content.get_mut(&p) {
                        if again {
                            e.buffer.select_word_at(pos);
                        } else {
                            e.buffer.move_to(pos, false);
                        }
                        e.status = None;
                    }
                    // Now, not on release: a click alone must land in
                    // normal mode at once. The drag that may follow is
                    // synced when the button comes up.
                    self.vim_mouse_sync(p);
                    self.last_click = Some((p, pos, std::time::Instant::now()));
                    self.sel_drag = Some(p);
                    return true;
                }
                false
            }
            None => false,
        }
    }

    /// The Windows console behaviour: copy and clear if there's a selection,
    /// otherwise paste. Two jobs on one button, but never ambiguous — the
    /// presence of a selection says which one was meant.
    fn on_right_click(&mut self, x: f32, y: f32) -> bool {
        let pane = match self.tree.hit(&self.layout, x, y) {
            Some(Hit::Pane(p)) => p,
            _ => return false,
        };
        self.focus = pane;
        let Some(t) = self.terminal(pane) else { return false };

        match t.selection_text() {
            Some(s) => {
                let _ = kb_win::clipboard::set_text(&s);
                t.select_clear();
            }
            None => self.paste_into_focus(),
        }
        true
    }

    fn on_mouse_up(&mut self, x: f32, y: f32) -> bool {
        if self.settings_pressed {
            self.settings_pressed = false;
            if self.settings_btn.is_some_and(|r| r.contains(x, y)) {
                self.open_settings();
            }
            return true;
        }
        // The selection survives the release — it's about to be copied.
        // In vim mode it becomes a visual selection, which is the same
        // thing spelled vim's way.
        if let Some(pane) = self.sel_drag.take() {
            self.vim_mouse_sync(pane);
        }
        self.dragging.take().is_some()
    }

    fn cursor(&self) -> CursorShape {
        let axis = self.dragging.map(|(d, _, _)| {
            self.layout
                .dividers
                .iter()
                .find(|(dd, _, _)| *dd == d)
                .map(|(_, a, _)| *a)
                .unwrap_or(Axis::Horizontal)
        });
        // The dividers too: a themed window with a stock Windows arrow over
        // the one thing you drag reads as a seam in the costume.
        if let Some(axis) = axis.or(self.hover_divider) {
            let cc = &self.cfg.cursor;
            return match axis {
                Axis::Horizontal => self.themed_or_file(
                    kb_win::ThemedKind::SizeWE,
                    &cc.resize_we_file.clone(),
                    CursorShape::SizeWE,
                ),
                Axis::Vertical => self.themed_or_file(
                    kb_win::ThemedKind::SizeNS,
                    &cc.resize_ns_file.clone(),
                    CursorShape::SizeNS,
                ),
            };
        }
        // While an overlay is up it owns the mouse. Over the picker's rows
        // and buttons the pointer becomes a hand — the other half of the
        // hover highlight's promise that this is a thing you can press.
        if self.folder_picker.is_some() {
            let (x, y) = self.mouse;
            if self.picker_clickable(x, y) {
                let file = self.cfg.cursor.hand_file.clone();
                return self.themed_or_file(kb_win::ThemedKind::Hand, &file, CursorShape::Hand);
            }
            return self.pointer(false);
        }
        if self.palette.is_some() {
            return self.pointer(false);
        }
        // Over the stuff you can type into, the pointer says so. Editors and
        // terminals both: a shell is a place text goes.
        let (x, y) = self.mouse;
        let over_text = self
            .layout
            .panes
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .is_some_and(|(p, _)| {
                matches!(
                    self.content.get(p),
                    Some(Content::Editor(_) | Content::Terminal(_) | Content::Agent(_))
                )
            });
        self.pointer(over_text)
    }

    fn on_key(&mut self, vk: u8, mods: Mods) -> bool {
        // The overlays take every key while one is open. Anything less lets
        // a stray keystroke through to the editor underneath, which is how a
        // find box ends up typing into the file it was searching.
        if self.folder_picker.is_some() {
            return self.picker_key(vk, mods.ctrl, mods.alt);
        }
        if self.palette.is_some() {
            return self.palette_key(vk);
        }
        // Vim's own Ctrl chords come before the bindings when the config
        // says so: a vim user pressing Ctrl+R wants redo, not whatever the
        // table put there, and Ctrl+D in normal mode is half a page. Only
        // the chords vim would act on in its current mode; the rest fall
        // through to the table as usual.
        if self.cfg.vim.ctrl_keys && mods.ctrl && !mods.shift && !mods.alt && vk.is_ascii_uppercase() {
            let c = (vk as char).to_ascii_lowercase();
            if self.focused_vim().is_some_and(|v| v.wants_ctrl(c)) {
                if let Some(true) = self.vim_key(kb_vim::Key::Ctrl(c)) {
                    return true;
                }
            }
        }
        // Bound actions win over pane content. That ordering is the whole
        // reason shortcuts kept breaking before: a terminal that swallows every
        // key makes anything checked after it dead code.
        if let Some(action) = self.cfg.keys.lookup(vk, mods.ctrl, mods.shift, mods.alt) {
            // Scoped actions step aside rather than claiming a key from
            // whatever has focus. Editor actions give way to a terminal —
            // Ctrl+A, Ctrl+Z and Ctrl+X are control characters a shell needs —
            // and explorer actions only fire on the tree, so the editor keeps
            // its Delete key.
            let applies = match action.scope() {
                kb_cfg::Scope::Global => true,
                kb_cfg::Scope::Editor => self.terminal(self.focus).is_none(),
                kb_cfg::Scope::Explorer => {
                    matches!(self.content.get(&self.focus), Some(Content::Explorer(_)))
                }
            };
            if applies {
                return self.run(action);
            }
        }

        // Keys the pane content owns. Alt is excluded so focus movement can't
        // be swallowed by whatever is focused.
        if mods.alt {
            return false;
        }
        if self.settings_key(vk) {
            return true;
        }
        if self.welcome_key(vk) {
            return true;
        }
        if self.git_key(vk, mods) {
            return true;
        }
        if self.agent_key(vk, mods) {
            return true;
        }
        if self.explorer_key(vk) {
            return true;
        }
        // The named keys go to vim first — Esc is how insert mode ends —
        // and what it declines (arrows in insert mode, Tab for a snippet)
        // takes the ordinary path below.
        if let Some(key) = vim_named_key(vk, mods) {
            if let Some(true) = self.vim_key(key) {
                return true;
            }
        }
        if self.editor_key(vk, mods) {
            return true;
        }
        if let Some(t) = self.terminal(self.focus) {
            if let Some(bytes) = Self::term_key_bytes(vk, mods.ctrl) {
                t.write(bytes);
                return true;
            }
            return false;
        }
        let visible = self.visible_rows(self.focus);
        let step = match vk {
            0x26 => 3,                 // up
            0x28 => -3,                // down
            0x21 => visible as i32,    // page up
            0x22 => -(visible as i32), // page down
            _ => return false,
        };
        if let Some(Content::Viewer(v)) = self.content.get_mut(&self.focus) {
            v.scroll(step, visible);
            return true;
        }
        false
    }
}

impl Kubide {
    /// Runs a bound action.
    ///
    /// Always returns `true`, even when the action couldn't do anything: the
    /// key was handled by US, so its WM_CHAR must be swallowed. Returning
    /// `false` from a failed copy would send 0x03 — SIGINT — to the shell,
    /// which is the worst kind of silent bug.
    fn run(&mut self, action: kb_cfg::Action) -> bool {
        use kb_cfg::Action::*;
        // Taken up front: doing anything else means the user moved on, and a
        // confirmation that survives an unrelated action is a trap.
        let pending = self.confirm.take();
        self.notice = None;
        match action {
            SplitRight | SplitDown => {
                let axis = if action == SplitRight { Axis::Horizontal } else { Axis::Vertical };
                if let Some(p) = self.tree.split(self.focus, axis) {
                    self.focus = p;
                }
                self.layout = self.tree.compute(self.area);
            }
            ClosePane => {
                // Unsaved work must not disappear from one keystroke. Asked as
                // a list rather than a press-again, because there are three
                // ways out of this and a press-again can only offer two.
                if self.unsaved_in(self.focus) {
                    let name = self.file_name_in(self.focus);
                    self.pending = Some(Pending::CloseUnsaved(self.focus));
                    self.palette = Some(Palette::ask(
                        "Unsaved Changes",
                        &format!("Close {name} without saving it?"),
                        &["Save", "Discard", "Cancel"],
                    ));
                    return true;
                }
                self.close_pane(self.focus);
            }
            OpenTerminal => {
                self.open_terminal();
            }
            OpenExplorer => {
                self.open_explorer();
            }
            ToggleExplorer => {
                self.toggle_explorer();
            }
            OpenSettings => {
                self.open_settings();
            }
            GitPanel => self.toggle_git_panel(),
            OpenAgent => self.open_agent(),
            WorkspaceHere => {
                let Some(path) = self.explorer_selection() else { return true };
                // A file means its folder: "make the project this thing's
                // home" is what pressing this on a file can only mean.
                let dir = if path.is_dir() {
                    path
                } else {
                    match path.parent() {
                        Some(p) => p.to_path_buf(),
                        None => return true,
                    }
                };
                self.switch_workspace(dir);
            }
            OpenFolder => {
                // Opened on the current root: the next project is usually a
                // sibling, one Left away. The palette drops so two overlays
                // never fight over the keyboard.
                self.palette = None;
                self.pending = None;
                self.folder_picker = Some(folders::Picker::new(
                    &self.root,
                    session::recent_workspaces(),
                    kb_win::quick_access(),
                ));
            }
            ToggleHelp => self.help_open = !self.help_open,
            ToggleVim => {
                // Live, like a settings-screen change: the config file is
                // the separate step, from the screen or by hand.
                let mut next = self.cfg.clone();
                next.vim.enabled = !next.vim.enabled;
                self.apply_config(next);
                self.warn(if self.cfg.vim.enabled { "vim mode on" } else { "vim mode off" });
            }
            NewFile | NewFolder => {
                let Some(dir) = self.explorer_target_dir() else { return true };
                self.pending = Some(if action == NewFile {
                    Pending::NewFile(dir)
                } else {
                    Pending::NewFolder(dir)
                });
                let label = if action == NewFile { "new file" } else { "new folder" };
                self.palette = Some(Palette::prompt(label, ""));
            }
            Rename => {
                let Some(path) = self.explorer_selection() else { return true };
                let old = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.pending = Some(Pending::Rename(path));
                // Prefilled: renaming usually means changing part of a name.
                self.palette = Some(Palette::prompt("rename", &old));
            }
            Delete => {
                let Some(path) = self.explorer_selection() else { return true };
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // Nothing here goes to a recycle bin, so it gets the same
                // ask-once treatment as discarding unsaved work.
                if pending != Some(Confirm::DeletePath) {
                    self.confirm = Some(Confirm::DeletePath);
                    self.warn(&format!("delete {name}? press delete again"));
                    return true;
                }
                match kb_fs::ops::delete(&path) {
                    Ok(()) => self.refresh_explorers(),
                    Err(e) => self.warn(&e),
                }
            }
            Commands => self.palette = Some(Palette::commands(&self.cfg.keys)),
            GoToFile => self.open_palette_files(),
            LastFile => self.open_last_file(),
            Find => {
                let lines = match self.content.get(&self.focus) {
                    Some(Content::Editor(e)) => e.buffer.lines().to_vec(),
                    _ => return true,
                };
                self.palette = Some(Palette::find(&lines));
            }
            Replace => {
                let Some(Content::Editor(e)) = self.content.get(&self.focus) else {
                    return true;
                };
                // Prefilled with the selection when it fits on one line:
                // replacing the thing you are looking at is the usual case.
                let initial = e
                    .buffer
                    .selected_text()
                    .filter(|s| !s.contains('\n'))
                    .unwrap_or_default();
                self.pending = Some(Pending::ReplaceWhat);
                self.palette = Some(Palette::prompt("replace", &initial));
            }
            GoToLine => self.palette = Some(Palette::line()),
            FindInProject => {
                if !self.git.is_repo() {
                    // Without a repository there is no .gitignore to honour
                    // and no fast index; saying so beats scanning target/.
                    self.warn("project search needs a git repository");
                    return true;
                }
                self.pending = Some(Pending::ProjectSearch);
                self.palette = Some(Palette::prompt("search", ""));
            }
            ToggleComment => {
                let marker = self.comment_marker();
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.buffer.toggle_comment(&marker);
                }
            }
            MoveLineUp | MoveLineDown => {
                let down = action == MoveLineDown;
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.buffer.move_lines(down);
                }
            }
            DuplicateLine => {
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.buffer.duplicate_lines();
                }
            }
            DeleteLine => {
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.buffer.delete_lines();
                }
            }
            SelectLine => {
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.buffer.select_line();
                }
            }
            GoToBracket => {
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    if let Some((_, other)) = e.buffer.matching_bracket() {
                        e.buffer.move_to(other, false);
                    }
                }
            }
            FocusLeft => {
                self.move_focus(Dir::Left);
            }
            FocusRight => {
                self.move_focus(Dir::Right);
            }
            FocusUp => {
                self.move_focus(Dir::Up);
            }
            FocusDown => {
                self.move_focus(Dir::Down);
            }
            FocusPane1 | FocusPane2 | FocusPane3 | FocusPane4 | FocusPane5 | FocusPane6
            | FocusPane7 | FocusPane8 | FocusPane9 => {
                // Counted where they are drawn, not where they are in the
                // tree: the number has to mean the same pane every time, and
                // tree order changes when something elsewhere is closed.
                let order = kb_ui::panes_in_reading_order(&self.layout);
                if let Some(p) = action.pane_index().and_then(|i| order.get(i)) {
                    self.focus = *p;
                }
                // A number past the end does nothing. Warning about it would
                // be noise on a key you press by habit.
            }
            GrowPaneWidth => {
                self.resize_pane(true, Axis::Horizontal);
            }
            ShrinkPaneWidth => {
                self.resize_pane(false, Axis::Horizontal);
            }
            GrowPaneHeight => {
                self.resize_pane(true, Axis::Vertical);
            }
            ShrinkPaneHeight => {
                self.resize_pane(false, Axis::Vertical);
            }
            MinimizeWindow => {
                if let Some(window) = self.window {
                    kb_win::minimize(window);
                }
            }
            ToggleMaximize => {
                if let Some(window) = self.window {
                    kb_win::toggle_maximize(window);
                }
            }
            Copy => self.copy_from_focus(false),
            Cut => self.copy_from_focus(true),
            Paste => self.paste_into_focus(),
            Save => {
                if matches!(self.content.get(&self.focus), Some(Content::Settings(_))) {
                    self.save_config();
                    return true;
                }
                let stale = matches!(
                    self.content.get(&self.focus),
                    Some(Content::Editor(e)) if e.buffer.changed_on_disk()
                );
                // Overwriting another program's change cannot be undone — not
                // by us, and not by whatever wrote it.
                if stale && pending != Some(Confirm::Overwrite(self.focus)) {
                    self.confirm = Some(Confirm::Overwrite(self.focus));
                    self.warn("changed on disk since you opened it — press save again to overwrite");
                    return true;
                }
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.save();
                }
            }
            Undo => {
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.buffer.undo();
                }
            }
            Redo => {
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.buffer.redo();
                }
            }
            SelectAll => {
                if let Some(Content::Editor(e)) = self.content.get_mut(&self.focus) {
                    e.buffer.select_all();
                }
            }
            PomodoroToggle => self.timer.toggle(),
            PomodoroReset => self.timer.reset(),
            PomodoroSkip => self.timer.advance(),
            FontLarger => {
                let _ = self.text.set_size(self.text.size() + 1.0);
            }
            FontSmaller => {
                let _ = self.text.set_size(self.text.size() - 1.0);
            }
            Quit => {
                if !self.confirm_quit() {
                    return true;
                }
                self.save_session();
                kb_win::quit();
            }
        }
        true
    }
}

impl Kubide {
    /// Terminal encoding for special keys. Printable characters come through
    /// `on_char` with the layout applied, so only keys with an escape sequence
    /// belong here.
    fn term_key_bytes(vk: u8, ctrl: bool) -> Option<&'static [u8]> {
        Some(match vk {
            // The modern terminal contract, backwards from the labels on
            // the keys: plain Backspace sends DEL (0x7f) and Ctrl+Backspace
            // sends BS (0x08). Left on the character path this arrived as
            // 0x08, which PSReadLine reads as Ctrl+Backspace — and one
            // press ate the whole word.
            0x08 if ctrl => b"\x08",
            0x08 => b"\x7f",
            0x25 => b"\x1b[D",  // left
            0x26 => b"\x1b[A",  // up
            0x27 => b"\x1b[C",  // right
            0x28 => b"\x1b[B",  // down
            0x24 => b"\x1b[H",  // home
            0x23 => b"\x1b[F",  // end
            0x2E => b"\x1b[3~", // delete
            0x21 => b"\x1b[5~", // page up
            0x22 => b"\x1b[6~", // page down
            _ => return None,
        })
    }
}

/// The clipboard as vim's `"+` register sees it.
struct Clipboard;

impl kb_vim::Host for Clipboard {
    fn clipboard(&mut self) -> Option<String> {
        kb_win::clipboard::get_text()
    }
    fn set_clipboard(&mut self, text: &str) {
        let _ = kb_win::clipboard::set_text(text);
    }
}

/// Vim's options as the config spells them.
fn vim_options(cfg: &kb_cfg::Config) -> kb_vim::Options {
    kb_vim::Options {
        ignorecase: cfg.vim.ignorecase,
        smartcase: cfg.vim.smartcase,
        hlsearch: cfg.vim.hlsearch,
        clipboard: cfg.vim.clipboard,
    }
}

/// The keys that reach vim through `on_key` rather than `on_char`: the ones
/// with no character, and Ctrl+letter, which the layout cannot move.
/// Letters and digits come through `on_char` with the layout applied, so a
/// Turkish `ş` types as itself.
fn vim_named_key(vk: u8, mods: Mods) -> Option<kb_vim::Key> {
    use kb_vim::Key::*;
    if mods.ctrl && vk.is_ascii_uppercase() && !mods.shift {
        return Some(Ctrl((vk as char).to_ascii_lowercase()));
    }
    Some(match vk {
        0x1B => Esc,
        0x0D => Enter,
        0x08 => Backspace,
        0x2E => Delete,
        0x09 => Tab,
        0x25 => Left,
        0x26 => Up,
        0x27 => Right,
        0x28 => Down,
        0x24 => Home,
        0x23 => End,
        0x21 => PageUp,
        0x22 => PageDown,
        _ => return None,
    })
}

/// Moves the settings selection. A free function so the borrow of the pane
/// ends before the caller needs `&mut self` again.
fn step_selection(settings: &mut content::Settings, delta: i32) -> bool {
    settings.move_selection(delta);
    true
}

/// Config enum to the platform enum. kb-cfg stays platform-free, so the
/// mapping lives here rather than in the config crate.
fn backdrop_of(b: kb_cfg::Backdrop) -> Backdrop {
    match b {
        kb_cfg::Backdrop::None => Backdrop::None,
        kb_cfg::Backdrop::Mica => Backdrop::Mica,
        kb_cfg::Backdrop::MicaAlt => Backdrop::MicaAlt,
        kb_cfg::Backdrop::Acrylic => Backdrop::Acrylic,
    }
}

fn main() -> Result<()> {
    // Before the config loads, so a `theme = "gruvbox"` on a fresh machine
    // finds a real file — and so there is always something in the themes
    // folder to copy a new theme from.
    kb_cfg::seed_themes();
    kb_cfg::snippets::seed_snippets();
    let workspace = Workspace::from_args();
    if workspace.init {
        // `kubide workspace`: the mark goes down before the config loads,
        // so the new file is read and watched from the very first frame.
        kb_cfg::seed_workspace(&workspace.dir);
    }
    let mut app = Kubide::new(&workspace)?;

    // A named file is an instruction, so it wins over whatever was open last
    // time. Without one, pick up where the last session left off.
    let restored = workspace.file.is_none() && app.restore_session();
    // Nowhere named, nothing remembered, no mark on the folder: this is the
    // exe double-clicked, or a first `kubide` somewhere new. A file listing
    // of wherever the process happened to wake up helps nobody; the welcome
    // screen offers the folder and the places already worked in, and lets
    // the person say which. A `.kubide` counts as being named — that is
    // what the mark is for.
    let welcome =
        !restored && !workspace.explicit && !workspace.dir.join(".kubide").is_dir();
    if welcome {
        app.content.insert(
            app.focus,
            Content::Welcome(content::Welcome::new(&workspace.dir, session::recent_workspaces())),
        );
    } else if !restored {
        // Tree on the left, work on the right, focus on the right. A window
        // that opens as one full-width list of file names is a file manager;
        // the tree is meant to be the thing beside what you are doing, and
        // Ctrl+B takes it away when it is not wanted.
        //
        // A quarter of the width: half a window of file names is not a sidebar.
        app.open_explorer();
        if let Some(right) = app.tree.split_at(app.focus, Axis::Horizontal, 0.25) {
            app.focus = right;
        }
        if let Some(file) = &workspace.file {
            // `kubide file.rs` opens the file in that right-hand pane, so the
            // project it came from stays visible next to it.
            app.content.insert(app.focus, Content::open_path(file));
        }
    } else if let Some(file) = &workspace.file {
        app.content.insert(app.focus, Content::open_path(file));
    }

    if !welcome {
        session::note_workspace(&workspace.dir);
        // Written once at startup as well as periodically: a crash in the
        // first half minute should still leave a layout behind, and it proves
        // the path is writable while there is still a person watching. Not
        // for the welcome screen, though — remembering a session for
        // wherever the exe woke up is exactly the noise it exists to avoid.
        app.save_session();
    }

    let window = WindowConfig {
        title: title_for(&workspace.dir),
        backdrop: backdrop_of(app.cfg.window.backdrop),
        caption_h: app.cfg.window.caption_height as i32,
        // Opened where it was closed. The default size stays as the answer
        // for a first run and for a place that no longer exists.
        place: session::window_place(),
        ..Default::default()
    };
    Ok(kb_win::run(window, Box::new(app))?)
}
