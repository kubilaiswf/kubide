//! Terminal session: ConPTY plus a VT state machine.
//!
//! No drawing here — this layer only holds state and hands out a readable
//! snapshot. Rendering happens in `kubide`, on our own DirectWrite stack.
//!
//! We don't write the VT core: `alacritty_terminal` already gets the grid,
//! scrollback, selection, alternate screen and scroll regions right. Writing
//! our own parser would take a year and look identical.
//!
//! The core isn't hidden behind a trait — there is only one implementation —
//! but the API is deliberately narrow (feed / resize / snapshot) so swapping
//! it later stays cheap.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;

pub use kb_cfg::theme::TerminalColors;

/// One cell — everything drawing needs and nothing more.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    /// Inside the selection. Marking the cell instead of carrying the range
    /// separately keeps the draw loop simple.
    pub selected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<kb_cfg::Color> for Rgb {
    fn from(c: kb_cfg::Color) -> Self {
        Rgb { r: c.r, g: c.g, b: c.b }
    }
}

/// Everything one frame needs. Copied out so drawing doesn't hold the terminal
/// lock — holding it through a render would stall the PTY reader thread.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Cell>,
    pub cursor: (usize, usize),
    pub cursor_visible: bool,
    /// Scrolled into history. The UI must show this, or the user won't notice
    /// they're missing live output.
    pub scrolled_back: bool,
    /// Exit code, if the shell is gone.
    pub exited: Option<i32>,
}

impl Snapshot {
    pub fn cell(&self, col: usize, row: usize) -> Option<&Cell> {
        self.cells.get(row * self.cols + col)
    }
}

/// What to launch and how much history to keep.
#[derive(Clone, Debug)]
pub struct SpawnOptions {
    pub cols: usize,
    pub rows: usize,
    pub cell_w: u16,
    pub cell_h: u16,
    /// `None` uses the system default shell.
    pub shell: Option<String>,
    pub args: Vec<String>,
    pub scrollback: usize,
    pub colors: TerminalColors,
    /// Where the shell starts. `None` inherits the editor's own working
    /// directory — which is wherever the exe happened to be launched, and
    /// almost never where the user is working.
    pub cwd: Option<std::path::PathBuf>,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            cell_w: 8,
            cell_h: 16,
            shell: None,
            args: Vec::new(),
            // Without scrollback a terminal is unusable: you can't see the top
            // of a build log. 10k lines is also Alacritty's default.
            scrollback: 10_000,
            colors: TerminalColors::default(),
            cwd: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Dims {
    cols: usize,
    rows: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Events from the PTY thread. We only track exit and "needs redraw".
#[derive(Clone, Default)]
struct Listener {
    dirty: Arc<Mutex<bool>>,
    exited: Arc<Mutex<Option<i32>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            // In 0.26 ChildExit carries ExitStatus, not i32.
            Event::ChildExit(status) => {
                *self.exited.lock().unwrap() = Some(status.code().unwrap_or(-1));
                *self.dirty.lock().unwrap() = true;
            }
            Event::Wakeup | Event::Bell | Event::Title(_) | Event::ResetTitle => {
                *self.dirty.lock().unwrap() = true;
            }
            _ => {}
        }
    }
}

pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    notifier: Notifier,
    listener: Listener,
    dims: Dims,
    colors: TerminalColors,
}

impl Terminal {
    /// Starts a shell in a fresh ConPTY.
    pub fn spawn(opts: &SpawnOptions) -> Result<Self, String> {
        let dims = Dims {
            cols: opts.cols.max(1),
            rows: opts.rows.max(1),
        };
        let listener = Listener::default();

        let window_size = WindowSize {
            num_lines: dims.rows as u16,
            num_cols: dims.cols as u16,
            cell_width: opts.cell_w.max(1),
            cell_height: opts.cell_h.max(1),
        };

        let pty_options = tty::Options {
            shell: opts
                .shell
                .as_ref()
                .map(|s| tty::Shell::new(s.clone(), opts.args.clone())),
            working_directory: opts.cwd.clone(),
            ..Default::default()
        };
        let pty = tty::new(&pty_options, window_size, 0).map_err(|e| e.to_string())?;

        let config = Config {
            scrolling_history: opts.scrollback,
            ..Config::default()
        };
        let term = Term::new(config, &dims, listener.clone());
        let term = Arc::new(FairMutex::new(term));

        let event_loop = EventLoop::new(term.clone(), listener.clone(), pty, false, false)
            .map_err(|e| e.to_string())?;
        let notifier = Notifier(event_loop.channel());
        // The thread runs on its own; shutdown goes through Msg::Shutdown, so
        // there's no JoinHandle to keep.
        let _ = event_loop.spawn();

        Ok(Self {
            term,
            notifier,
            listener,
            dims,
            colors: opts.colors,
        })
    }

    /// Swaps the palette in place, for config reload.
    pub fn set_colors(&mut self, colors: TerminalColors) {
        self.colors = colors;
        *self.listener.dirty.lock().unwrap() = true;
    }

    /// Sends user input to the shell.
    ///
    /// Typing scrolls back to the bottom, like every terminal does. Pressing a
    /// key and seeing nothing feels broken.
    pub fn write(&self, bytes: &[u8]) {
        use alacritty_terminal::event::Notify;
        use alacritty_terminal::grid::Scroll;
        self.term.lock().scroll_display(Scroll::Bottom);
        self.notifier.notify(bytes.to_vec());
        *self.listener.dirty.lock().unwrap() = true;
    }

    pub fn resize(&mut self, cols: usize, rows: usize, cell_w: u16, cell_h: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.dims.cols == cols && self.dims.rows == rows {
            return;
        }
        self.dims = Dims { cols, rows };
        let size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: cell_w.max(1),
            cell_height: cell_h.max(1),
        };
        let _ = self.notifier.0.send(Msg::Resize(size));
        self.term.lock().resize(self.dims);
    }

    /// Begins a mouse selection.
    ///
    /// `row` is a viewport row (0 = top of screen). Selections live in buffer
    /// space, so we subtract the display offset — that way the selection
    /// scrolls with the content instead of sticking to the screen.
    pub fn select_start(&self, col: usize, row: usize) {
        use alacritty_terminal::index::Side;
        use alacritty_terminal::selection::{Selection, SelectionType};
        let mut term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        let point = Point::new(Line(row as i32 - offset), Column(col));
        term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
        *self.listener.dirty.lock().unwrap() = true;
    }

    /// Moves the loose end while dragging.
    pub fn select_update(&self, col: usize, row: usize) {
        use alacritty_terminal::index::Side;
        let mut term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        let point = Point::new(Line(row as i32 - offset), Column(col));
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, Side::Right);
        }
        *self.listener.dirty.lock().unwrap() = true;
    }

    pub fn select_clear(&self) {
        let mut term = self.term.lock();
        if term.selection.take().is_some() {
            *self.listener.dirty.lock().unwrap() = true;
        }
    }

    pub fn has_selection(&self) -> bool {
        self.term.lock().selection.is_some()
    }

    /// Selected text, or `None` when there's no selection.
    pub fn selection_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|s| !s.is_empty())
    }

    /// Scrolls through history. Positive is towards the past.
    pub fn scroll(&self, lines: i32) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().scroll_display(Scroll::Delta(lines));
        *self.listener.dirty.lock().unwrap() = true;
    }

    /// On the alternate screen (vim, btop and other full-screen TUIs) there is
    /// no scrollback; the wheel event has to be forwarded to the app instead.
    pub fn in_alt_screen(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.lock().mode().contains(TermMode::ALT_SCREEN)
    }

    /// Anything changed since the last draw? Lets us render on demand — an
    /// idle terminal shouldn't touch the GPU.
    pub fn take_dirty(&self) -> bool {
        let mut d = self.listener.dirty.lock().unwrap();
        std::mem::replace(&mut d, false)
    }

    pub fn snapshot(&self) -> Snapshot {
        let term = self.term.lock();
        let grid = term.grid();
        let cols = grid.columns();
        let rows = grid.screen_lines();

        // display_offset has to be applied BY HAND: `grid[Line(n)]` indexes
        // `raw[n]` directly and ignores the scroll position. Line(0) is the top
        // of the live screen, negative lines are history — which is exactly
        // what `display_iter()` does internally.
        //
        // Without this, scrollback looks completely dead: the wheel moves the
        // offset and nothing on screen changes.
        let offset = grid.display_offset() as i32;

        // The selection range is in buffer space; compute it once so each cell
        // is a cheap containment test.
        let selection = term.selection.as_ref().and_then(|s| s.to_range(&term));

        let mut cells = Vec::with_capacity(cols * rows);
        for row in 0..rows {
            for col in 0..cols {
                let point = Point::new(Line(row as i32 - offset), Column(col));
                let c = &grid[point];
                let selected = selection.is_some_and(|r| r.contains(point));
                let flags = c.flags;
                cells.push(Cell {
                    ch: c.c,
                    fg: self.to_rgb(c.fg),
                    bg: self.to_rgb(c.bg),
                    bold: flags.contains(alacritty_terminal::term::cell::Flags::BOLD),
                    italic: flags.contains(alacritty_terminal::term::cell::Flags::ITALIC),
                    underline: flags
                        .contains(alacritty_terminal::term::cell::Flags::UNDERLINE),
                    inverse: flags.contains(alacritty_terminal::term::cell::Flags::INVERSE),
                    selected,
                });
            }
        }

        // The cursor moves into viewport space too: scrolled into history it
        // leaves the screen and must not be drawn.
        let cursor_point = grid.cursor.point;
        let cursor_row = cursor_point.line.0 + offset;
        let cursor_visible = cursor_row >= 0 && (cursor_row as usize) < rows;

        Snapshot {
            cols,
            rows,
            cells,
            cursor: (cursor_point.column.0, cursor_row.max(0) as usize),
            cursor_visible,
            scrolled_back: offset > 0,
            exited: *self.listener.exited.lock().unwrap(),
        }
    }

    /// Flattens alacritty's color type to plain RGB using the theme palette.
    fn to_rgb(&self, color: alacritty_terminal::vte::ansi::Color) -> Rgb {
        use alacritty_terminal::vte::ansi::{Color, NamedColor};
        let ansi = &self.colors.ansi;
        match color {
            Color::Spec(c) => Rgb { r: c.r, g: c.g, b: c.b },
            Color::Indexed(i) => self.indexed(i),
            Color::Named(n) => match n {
                NamedColor::Black => ansi.black.into(),
                NamedColor::Red => ansi.red.into(),
                NamedColor::Green => ansi.green.into(),
                NamedColor::Yellow => ansi.yellow.into(),
                NamedColor::Blue => ansi.blue.into(),
                NamedColor::Magenta => ansi.magenta.into(),
                NamedColor::Cyan => ansi.cyan.into(),
                NamedColor::White => ansi.white.into(),
                NamedColor::BrightBlack => ansi.bright_black.into(),
                NamedColor::BrightRed => ansi.bright_red.into(),
                NamedColor::BrightGreen => ansi.bright_green.into(),
                NamedColor::BrightYellow => ansi.bright_yellow.into(),
                NamedColor::BrightBlue => ansi.bright_blue.into(),
                NamedColor::BrightMagenta => ansi.bright_magenta.into(),
                NamedColor::BrightCyan => ansi.bright_cyan.into(),
                NamedColor::BrightWhite => ansi.bright_white.into(),
                NamedColor::Background => self.colors.background.into(),
                NamedColor::Cursor => self.colors.cursor.into(),
                _ => self.colors.foreground.into(),
            },
        }
    }

    /// The standard xterm 256-color cube. Only the first 16 are themeable;
    /// the cube and greyscale ramp are fixed by the spec.
    fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.colors.ansi.get(i).into(),
            16..=231 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { v * 40 + 55 };
                Rgb {
                    r: level(i / 36),
                    g: level((i % 36) / 6),
                    b: level(i % 6),
                }
            }
            232..=255 => {
                let v = (i - 232) * 10 + 8;
                Rgb { r: v, g: v, b: v }
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Shut the shell and reader thread down, or the process won't exit.
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}
