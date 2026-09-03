//! Vim emulation over a [`kb_edit::Buffer`].
//!
//! The editor stays what it was — a buffer, a caret, a selection — and this
//! sits in front of the keyboard translating vim's grammar into calls on it.
//! Nothing here draws, reads the clipboard or touches a file: the owner passes
//! in a [`Host`] for the clipboard and gets back [`Effect`]s for anything that
//! reaches past the buffer (save, close the pane, split), so all of it runs in
//! a test.
//!
//! Two pieces of state, because vim has two: a [`Vim`] per editor pane holds
//! the mode, the pending keys and the marks, and one [`Session`] per window
//! holds what every pane shares — registers, the last search, the last change
//! for `.`, the macro being recorded. Yanking in one pane and pasting in
//! another is the whole point of registers, and per-pane registers would have
//! broken it.

mod ex;
mod motion;
mod ops;
mod parse;
mod regex;

use std::collections::HashMap;

use kb_edit::{Buffer, Pos};
use parse::{Cmd, CmdKind, Op, Parse};
pub use regex::Regex;

/// One keystroke, after the keyboard layout has been applied to it.
///
/// Characters come with the layout applied because that is the only way a
/// Turkish `ş` or a German `ö` can be typed into a file; the named keys are
/// what has no character. `Ctrl` carries the letter on the key, layout
/// independent, so `Ctrl+R` is redo on every keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Esc,
    Enter,
    Backspace,
    Delete,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Replace,
    Visual,
    VisualLine,
    /// The `:` / `/` / `?` line is being typed.
    Command,
}

impl Mode {
    /// What vim prints at the bottom: `-- INSERT --` and friends.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Replace => "REPLACE",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "VISUAL LINE",
            Mode::Command => "COMMAND",
        }
    }
}

/// How the caret should be drawn for the mode. Vim users read the caret
/// before they read the mode line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

/// Something the pane's owner has to do because the buffer cannot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Effect {
    /// `:w`.
    Save,
    /// `:wa`.
    SaveAll,
    /// `:wq`, `:x`, `ZZ`: save, then close the pane.
    SaveClose,
    /// `:q`: close the pane, asking about unsaved work.
    ClosePane,
    /// `:q!`, `ZQ`: close the pane, discarding.
    ClosePaneForce,
    /// `:qa`: quit, asking.
    Quit,
    /// `:qa!`: quit, discarding.
    QuitForce,
    SplitRight,
    SplitDown,
    Focus(Dir),
    OpenTerminal,
    /// `:e path`.
    OpenFile(String),
    /// The view should scroll so this is the first visible line.
    ScrollTo(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// What the owner provides: the system clipboard, behind the `+` and `*`
/// registers. A trait rather than two functions so a test can be a `Vec`.
pub trait Host {
    fn clipboard(&mut self) -> Option<String>;
    fn set_clipboard(&mut self, text: &str);
}

/// What vim needs to know about the view for one keystroke.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    /// First visible line.
    pub top: usize,
    /// Lines that fit.
    pub visible: usize,
    /// Whether typing `(` should bring `)` — the editor's own option, obeyed
    /// in insert mode so vim on and vim off type the same.
    pub auto_close: bool,
}

/// What a keystroke came to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Not vim's business; the editor should do what it would have done.
    Pass,
    Handled(Vec<Effect>),
}

/// The options `:set` can change. Seeded from the config, and changed at
/// runtime by `:set`, until the next config reload.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    pub ignorecase: bool,
    pub smartcase: bool,
    pub hlsearch: bool,
    /// `clipboard=unnamedplus`: the unnamed register is the system clipboard.
    pub clipboard: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { ignorecase: false, smartcase: false, hlsearch: true, clipboard: false }
    }
}

/// One register's contents. Linewise text is stored without its final
/// newline; the flag says how to put it back.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Register {
    pub text: String,
    pub linewise: bool,
}

/// A change that `.` can repeat: the command, and whatever was typed in the
/// insert mode it opened.
#[derive(Clone, Debug)]
struct Change {
    cmd: Cmd,
    insert: Vec<Key>,
    /// For a change made in visual mode: how big the selection was, so `.`
    /// can select the same amount from the cursor.
    visual: Option<Extent>,
}

#[derive(Clone, Copy, Debug)]
struct Extent {
    lines: usize,
    cols: usize,
    linewise: bool,
}

/// What every pane shares.
#[derive(Default)]
pub struct Session {
    pub options: Options,
    registers: HashMap<char, Register>,
    /// Pattern and direction of the last `/`, `?`, `*` or `:s`.
    last_search: Option<(String, bool)>,
    /// The last search, compiled, while `hlsearch` wants it drawn. `:noh`
    /// clears it without forgetting the pattern.
    hl: Option<Regex>,
    /// The last `f`/`t`, for `;` and `,`.
    last_find: Option<(char, bool, bool)>,
    last_change: Option<Change>,
    recording: Option<(char, Vec<Key>)>,
    last_macro: Option<char>,
    cmd_history: Vec<String>,
    search_history: Vec<String>,
    /// Pattern, replacement and flags of the last `:s`, for `&` and `:s`
    /// with no pattern.
    last_sub: Option<(String, String, String)>,
    last_insert_text: String,
}

impl Session {
    pub fn new(options: Options) -> Self {
        Self { options, ..Default::default() }
    }

    /// The register a macro is being recorded into, for the status line.
    pub fn recording(&self) -> Option<char> {
        self.recording.as_ref().map(|(c, _)| *c)
    }

    /// The pattern to highlight, when there is one to draw.
    pub fn highlight(&self) -> Option<&Regex> {
        self.hl.as_ref()
    }

    pub fn register(&self, name: char) -> Option<&Register> {
        self.registers.get(&name)
    }

    /// The current pattern compiled with the case options as they stand.
    fn compile(&self, pattern: &str) -> Result<Regex, String> {
        let ignore = self.options.ignorecase && !(self.options.smartcase && Regex::pattern_has_upper(pattern));
        Regex::new(pattern, ignore)
    }

    fn set_search(&mut self, pattern: &str, forward: bool) {
        self.last_search = Some((pattern.to_string(), forward));
        self.hl = if self.options.hlsearch { self.compile(pattern).ok() } else { None };
    }
}

/// An insert session, from `i` to `Esc`.
#[derive(Clone, Debug)]
struct Insert {
    /// What was typed, for repeating with a count and for `.`.
    keys: Vec<Key>,
    count: usize,
    /// `o`/`O`: repeating means opening another line first.
    reopen: Option<bool>,
}

struct CmdLine {
    prefix: char,
    text: String,
    /// Where in the history Up has walked to.
    hist: Option<usize>,
    /// The mode to go back to. `/` in visual mode extends the selection.
    back_to: Mode,
    /// Ctrl+R was pressed: the next key names a register to insert.
    reg_pending: bool,
}

/// The per-pane state.
pub struct Vim {
    mode: Mode,
    /// Keys of the normal-mode command being typed.
    keys: Vec<Key>,
    visual_anchor: Pos,
    cmdline: Option<CmdLine>,
    /// The column vertical movement aims for; `usize::MAX` after `$`.
    want_col: Option<usize>,
    marks: HashMap<char, Pos>,
    jumps: Vec<Pos>,
    jump_at: usize,
    insert: Option<Insert>,
    /// Replace mode: what each typed character covered, so Backspace can put
    /// it back. `None` for a character typed past the end of the line.
    replaced: Vec<Option<char>>,
    /// Ctrl+R in insert mode: waiting for a register name.
    insert_reg: bool,
    message: Option<(String, bool)>,
    /// Anchor, cursor and kind of the last visual selection, for `gv`, `'<`
    /// and `'>`.
    last_visual: Option<(Pos, Pos, bool)>,
    /// Ctrl+O from insert mode: one normal command, then back.
    one_shot: bool,
    /// Depth of `.` replay, during which nothing is recorded as the next
    /// thing to repeat.
    dot_depth: u32,
    /// Depth of macro replay, during which keys are not recorded again.
    macro_depth: u32,
    /// The change in progress, waiting for its insert session to end.
    change: Option<Change>,
    /// The visual selection `.` is replaying, if any.
    dot_extent: Option<Extent>,
    /// Whether the last key failed — a motion that could not move, an
    /// error. Vim beeps there, and a macro or `.` stops at the beep.
    failed: bool,
    /// `d/`: the operator waiting for the search being typed.
    pending_op: Option<(Op, Option<char>, usize)>,
}

impl Default for Vim {
    fn default() -> Self {
        Self::new()
    }
}

/// The span an operator works on. `end` is exclusive; for a linewise range
/// the columns are meaningless and only the lines count.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Range {
    pub start: Pos,
    pub end: Pos,
    pub linewise: bool,
}

impl Range {
    pub(crate) fn lines(first: usize, last: usize) -> Self {
        Range { start: Pos::new(first, 0), end: Pos::new(last, 0), linewise: true }
    }
}

impl Vim {
    pub fn new() -> Self {
        Self {
            mode: Mode::Normal,
            keys: Vec::new(),
            visual_anchor: Pos::default(),
            cmdline: None,
            want_col: None,
            marks: HashMap::new(),
            jumps: Vec::new(),
            jump_at: 0,
            insert: None,
            replaced: Vec::new(),
            insert_reg: false,
            message: None,
            last_visual: None,
            one_shot: false,
            dot_depth: 0,
            macro_depth: 0,
            change: None,
            dot_extent: None,
            failed: false,
            pending_op: None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn cursor_shape(&self) -> CursorShape {
        match self.mode {
            Mode::Insert => CursorShape::Bar,
            Mode::Replace => CursorShape::Underline,
            _ => CursorShape::Block,
        }
    }

    /// The command line as typed so far, prefix included, while one is open.
    pub fn cmdline(&self) -> Option<String> {
        self.cmdline.as_ref().map(|c| format!("{}{}", c.prefix, c.text))
    }

    /// The last message, and whether it is an error. Cleared by the next key.
    pub fn message(&self) -> Option<(&str, bool)> {
        self.message.as_ref().map(|(m, e)| (m.as_str(), *e))
    }

    /// The keys of the command being typed — vim's `showcmd`.
    pub fn pending(&self) -> String {
        self.keys.iter().map(|k| key_label(*k)).collect()
    }

    /// The visual selection as the editor draws selections: an ordered pair
    /// with an exclusive end. `None` outside visual mode.
    pub fn selection(&self, buf: &Buffer) -> Option<(Pos, Pos)> {
        if !self.in_visual() {
            return None;
        }
        let r = self.visual_range(buf, false);
        if r.linewise {
            let last = r.end.line;
            let end = if last + 1 < buf.len() { Pos::new(last + 1, 0) } else { Pos::new(last, buf.line_len(last)) };
            return Some((Pos::new(r.start.line, 0), end));
        }
        Some((r.start, r.end))
    }

    /// The text of the visual selection, for a copy made by the owner.
    pub fn selection_text(&self, buf: &Buffer) -> Option<String> {
        if !self.in_visual() {
            return None;
        }
        let r = self.visual_range(buf, false);
        Some(if r.linewise {
            let mut s = buf.lines()[r.start.line..=r.end.line].join("\n");
            s.push('\n');
            s
        } else {
            buf.text_in(r.start, r.end)
        })
    }

    /// Whether a Ctrl chord is one vim would act on right now, so the owner
    /// can decide whether its own binding or vim gets it.
    pub fn wants_ctrl(&self, c: char) -> bool {
        match self.mode {
            Mode::Normal | Mode::Visual | Mode::VisualLine => {
                !self.keys.is_empty()
                    || c == 'w'
                    || matches!(c, 'r' | 'd' | 'u' | 'f' | 'b' | 'e' | 'y' | 'a' | 'x' | 'o' | 'i' | 'g' | 'c' | 'v' | 'h' | 'j' | 'n' | 'p' | 'm' | '[')
            }
            Mode::Insert | Mode::Replace => {
                self.insert_reg || matches!(c, 'w' | 'u' | 'o' | 'r' | 'h' | 'j' | 'm' | 't' | 'd' | 'e' | 'y' | 'c' | '[' | 'a')
            }
            Mode::Command => matches!(c, 'u' | 'w' | 'r' | 'c' | 'h' | '[' | 'j' | 'm'),
        }
    }

    /// The owner moved the caret or made a selection with the mouse; bring
    /// the mode in line with it.
    pub fn mouse_sync(&mut self, buf: &mut Buffer) {
        if matches!(self.mode, Mode::Insert | Mode::Replace | Mode::Command) {
            return;
        }
        if let Some((a, b)) = buf.selection() {
            // The buffer's selection is exclusive; visual mode is inclusive.
            let (anchor, cursor) = if buf.cursor == b { (a, back_one(buf, b)) } else { (back_one(buf, a), b) };
            self.visual_anchor = anchor;
            buf.set_cursor(cursor);
            self.mode = Mode::Visual;
        } else {
            self.exit_visual(buf);
            self.clamp_normal(buf);
        }
        buf.clear_selection();
        self.keys.clear();
    }

    /// One keystroke.
    pub fn key(&mut self, key: Key, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx) -> Outcome {
        if self.macro_depth == 0 {
            if let Some((_, keys)) = &mut s.recording {
                keys.push(key);
            }
        }
        self.message = None;
        self.failed = false;
        match self.mode {
            Mode::Insert | Mode::Replace => self.insert_key(key, buf, s, host, ctx),
            Mode::Command => Outcome::Handled(self.cmdline_key(key, buf, s, host, ctx)),
            Mode::Normal | Mode::Visual | Mode::VisualLine => self.normal_key(key, buf, s, host, ctx),
        }
    }

    /// Feeds a string of keys, the way `:normal`, `.` and macros do.
    fn feed(&mut self, keys: &[Key], buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx) -> Vec<Effect> {
        let mut fx = Vec::new();
        for k in keys {
            if let Outcome::Handled(more) = self.key(*k, buf, s, host, ctx) {
                fx.extend(more);
            } else if let Key::Tab = k {
                // The owner would have expanded a snippet; with nobody
                // listening, a tab is four spaces, as everywhere else here.
                buf.insert("    ");
            }
            // A step that failed ends the whole sequence, as in vim: that
            // is what makes `100@a` stop where the file runs out.
            if self.failed {
                break;
            }
        }
        fx
    }

    fn normal_key(&mut self, key: Key, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx) -> Outcome {
        self.keys.push(key);
        match parse::parse(&self.keys, self.in_visual(), s.recording.is_some()) {
            Parse::Incomplete => Outcome::Handled(Vec::new()),
            Parse::Invalid => {
                self.keys.clear();
                Outcome::Handled(Vec::new())
            }
            Parse::Complete(cmd) => {
                self.keys.clear();
                let fx = self.execute(cmd, buf, s, host, ctx);
                Outcome::Handled(fx)
            }
        }
    }

    /// Runs a parsed command.
    fn execute(&mut self, cmd: Cmd, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx) -> Vec<Effect> {
        let mut fx = Vec::new();
        let visual = self.in_visual();
        let count = cmd.count;
        let n = count.unwrap_or(1).max(1);

        match cmd.kind.clone() {
            CmdKind::Move(m) => {
                let from = buf.cursor;
                match self.motion(buf, s, &ctx, m, count, false) {
                    Some(dest) => {
                        if motion::is_jump(m) {
                            self.push_jump(from);
                        }
                        buf.set_cursor(dest.pos);
                        if !visual && !self.one_shot {
                            self.clamp_normal(buf);
                        }
                    }
                    None => {
                        // A failed motion cancels: vim beeps, and `3dw` at the
                        // end of a file does nothing rather than half of it.
                        self.failed = true;
                    }
                }
                self.after_motion(m, buf);
            }
            CmdKind::Op { op, target } => {
                if let parse::Target::Motion { motion: parse::Motion::SearchPrompt { forward }, .. } = target {
                    // The range ends where the search lands; the operator
                    // waits for the line to be typed.
                    self.pending_op = Some((op, cmd.reg, n));
                    self.open_cmdline(if forward { '/' } else { '?' }, "", buf);
                } else if let Some(range) = self.op_range(buf, s, &ctx, op, &target, count) {
                    self.note_change(&cmd, None);
                    fx.extend(self.apply_op(op, range, cmd.reg, 1, buf, s, host, &ctx));
                } else {
                    self.failed = true;
                }
            }
            CmdKind::VisualOp { op, linewise } => {
                let range = self.visual_range(buf, linewise);
                let extent = self.extent(buf);
                self.exit_visual(buf);
                if op != Op::Yank {
                    self.note_change(&cmd, Some(extent));
                }
                fx.extend(self.apply_op(op, range, cmd.reg, n, buf, s, host, &ctx));
            }
            CmdKind::Object(obj) => {
                // With a selection already made, an object extends it by the
                // next one along, the way a second `iw` takes the following
                // space; from a single character it selects the object.
                let extending = self.visual_anchor != buf.cursor && self.mode == Mode::Visual;
                let from = buf.cursor;
                if extending {
                    let len = buf.line_len(from.line);
                    if from.col + 1 < len {
                        buf.set_cursor(Pos::new(from.line, from.col + 1));
                    } else if from.line + 1 < buf.len() {
                        buf.set_cursor(Pos::new(from.line + 1, 0));
                    }
                }
                match self.object_range(buf, obj, n) {
                    Some(r) if r.linewise => {
                        self.mode = Mode::VisualLine;
                        if !extending {
                            self.visual_anchor = Pos::new(r.start.line, 0);
                        }
                        buf.set_cursor(Pos::new(r.end.line, 0));
                    }
                    Some(r) => {
                        if !extending {
                            self.visual_anchor = r.start;
                        }
                        buf.set_cursor(back_one(buf, r.end));
                    }
                    None => {
                        buf.set_cursor(from);
                        self.failed = true;
                    }
                }
            }
            CmdKind::Act(act) => {
                fx.extend(self.act(act, &cmd, buf, s, host, ctx));
            }
        }

        // A change that did not open insert mode is complete now.
        if !matches!(self.mode, Mode::Insert | Mode::Replace) {
            if let Some(change) = self.change.take() {
                if self.dot_depth == 0 {
                    s.last_change = Some(change);
                }
            }
        }
        // Ctrl+O from insert mode: the one command is done, go back.
        if self.one_shot && self.mode == Mode::Normal && self.keys.is_empty() {
            self.one_shot = false;
            self.mode = Mode::Insert;
            buf.begin_undo_group();
        }
        fx
    }

    /// Remembers a command as the thing `.` repeats, unless `.` itself is
    /// what is running it.
    fn note_change(&mut self, cmd: &Cmd, visual: Option<Extent>) {
        if self.dot_depth > 0 {
            return;
        }
        self.change = Some(Change { cmd: cmd.clone(), insert: Vec::new(), visual });
    }

    /// Repeats the last change, with `count` in place of its own when given.
    fn repeat_last(&mut self, count: Option<usize>, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx) -> Vec<Effect> {
        let Some(change) = s.last_change.clone() else { return Vec::new() };
        let mut cmd = change.cmd.clone();
        if count.is_some() {
            cmd.count = count;
        }
        self.dot_depth += 1;
        let mut fx = Vec::new();
        if let Some(extent) = change.visual {
            // Reselect the same amount from the cursor, then run the
            // operator on it as visual mode would have.
            self.dot_extent = Some(extent);
            let from = buf.cursor;
            self.visual_anchor = from;
            self.mode = if extent.linewise { Mode::VisualLine } else { Mode::Visual };
            let last = buf.len() - 1;
            let line = (from.line + extent.lines.saturating_sub(1)).min(last);
            let col = if extent.lines <= 1 { from.col + extent.cols.saturating_sub(1) } else { extent.cols.saturating_sub(1) };
            buf.set_cursor(Pos::new(line, col.min(buf.line_len(line).saturating_sub(1))));
            self.dot_extent = None;
        }
        fx.extend(self.execute(cmd, buf, s, host, ctx));
        if matches!(self.mode, Mode::Insert | Mode::Replace) {
            fx.extend(self.feed(&change.insert, buf, s, host, ctx));
            fx.extend(self.feed(&[Key::Esc], buf, s, host, ctx));
        }
        self.dot_depth -= 1;
        fx
    }

    fn in_visual(&self) -> bool {
        matches!(self.mode, Mode::Visual | Mode::VisualLine)
    }

    /// Normal mode's caret sits on a character, never after the last one.
    fn clamp_normal(&self, buf: &mut Buffer) {
        let len = buf.line_len(buf.cursor.line);
        if len > 0 && buf.cursor.col >= len {
            buf.set_cursor(Pos::new(buf.cursor.line, len - 1));
        }
    }

    /// Keeps the wanted column across vertical motions and drops it after
    /// anything else.
    fn after_motion(&mut self, m: parse::Motion, _buf: &Buffer) {
        use parse::Motion::*;
        match m {
            Up | Down => {}
            LineEnd => self.want_col = Some(usize::MAX),
            _ => self.want_col = None,
        }
    }

    fn visual_range(&self, buf: &Buffer, force_linewise: bool) -> Range {
        let (a, b) = order(self.visual_anchor, buf.cursor);
        if self.mode == Mode::VisualLine || force_linewise {
            return Range::lines(a.line, b.line);
        }
        let len = buf.line_len(b.line);
        let end = if b.col < len {
            Pos::new(b.line, b.col + 1)
        } else if b.line + 1 < buf.len() {
            // The caret on the line break selects the line break.
            Pos::new(b.line + 1, 0)
        } else {
            Pos::new(b.line, len)
        };
        Range { start: a, end, linewise: false }
    }

    /// The size of the visual selection, for `.` to reproduce.
    fn extent(&self, buf: &Buffer) -> Extent {
        let (a, b) = order(self.visual_anchor, buf.cursor);
        Extent {
            lines: b.line - a.line + 1,
            cols: if a.line == b.line { b.col - a.col + 1 } else { b.col + 1 },
            linewise: self.mode == Mode::VisualLine,
        }
    }

    fn enter_visual(&mut self, buf: &Buffer, linewise: bool) {
        self.visual_anchor = buf.cursor;
        self.mode = if linewise { Mode::VisualLine } else { Mode::Visual };
    }

    fn exit_visual(&mut self, buf: &Buffer) {
        if self.in_visual() {
            self.last_visual = Some((self.visual_anchor, buf.cursor, self.mode == Mode::VisualLine));
            self.marks.insert('<', order(self.visual_anchor, buf.cursor).0);
            self.marks.insert('>', order(self.visual_anchor, buf.cursor).1);
        }
        self.mode = Mode::Normal;
    }

    fn push_jump(&mut self, from: Pos) {
        self.marks.insert('\'', from);
        self.jumps.truncate(self.jump_at);
        if self.jumps.last() != Some(&from) {
            self.jumps.push(from);
        }
        self.jump_at = self.jumps.len();
    }

    /// Opens an insert session. Everything typed until Esc is one undo step.
    fn enter_insert(&mut self, buf: &mut Buffer, count: usize, reopen: Option<bool>, replace: bool) {
        buf.begin_undo_group();
        self.insert = Some(Insert { keys: Vec::new(), count: count.max(1), reopen });
        self.replaced.clear();
        self.mode = if replace { Mode::Replace } else { Mode::Insert };
        self.want_col = None;
    }

    fn insert_key(&mut self, key: Key, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx) -> Outcome {
        let replace = self.mode == Mode::Replace;
        if self.insert_reg {
            self.insert_reg = false;
            if let Key::Char(c) = key {
                if let Some(r) = self.fetch(s, host, Some(c)) {
                    buf.insert(&r.text);
                }
            }
            return Outcome::Handled(Vec::new());
        }
        match key {
            Key::Esc | Key::Ctrl('c') | Key::Ctrl('[') => {
                return Outcome::Handled(self.leave_insert(buf, s, host, ctx));
            }
            Key::Char(c) => {
                self.record_insert(key);
                if replace {
                    self.replace_char(buf, c);
                } else if ctx.auto_close {
                    buf.type_char(c);
                } else {
                    buf.insert_char(c);
                }
            }
            Key::Enter | Key::Ctrl('j') | Key::Ctrl('m') => {
                self.record_insert(Key::Enter);
                if replace {
                    self.replaced.push(None);
                }
                buf.insert_newline();
            }
            Key::Tab => {
                self.record_insert(key);
                if self.dot_depth > 0 || self.macro_depth > 0 {
                    buf.insert("    ");
                } else {
                    // The owner expands snippets on Tab; the key is recorded
                    // so a repeat still produces an indent.
                    return Outcome::Pass;
                }
            }
            Key::Backspace | Key::Ctrl('h') => {
                self.record_insert(Key::Backspace);
                if replace {
                    self.unreplace(buf);
                } else if ctx.auto_close {
                    buf.backspace_pair();
                } else {
                    buf.backspace();
                }
            }
            Key::Delete => {
                self.record_insert(key);
                buf.delete();
            }
            Key::Ctrl('w') => {
                self.record_insert(key);
                buf.delete_word_left();
            }
            Key::Ctrl('u') => {
                self.record_insert(key);
                let c = buf.cursor;
                let indent = first_non_blank(buf, c.line);
                let to = if c.col > indent { indent } else { 0 };
                buf.edit(Pos::new(c.line, to), c, "");
            }
            Key::Ctrl('t') | Key::Ctrl('d') => {
                self.record_insert(key);
                let c = buf.cursor;
                let before = buf.line_len(c.line);
                if key == Key::Ctrl('t') {
                    buf.indent(4);
                } else {
                    buf.dedent(4);
                }
                let after = buf.line_len(c.line);
                let col = (c.col as i64 + after as i64 - before as i64).max(0) as usize;
                buf.set_cursor(Pos::new(c.line, col));
            }
            Key::Ctrl('e') | Key::Ctrl('y') => {
                // The character below or above the caret, copied in.
                let c = buf.cursor;
                let other = if key == Key::Ctrl('e') { c.line + 1 } else { c.line.wrapping_sub(1) };
                if let Some(ch) = buf.lines().get(other).and_then(|l| l.chars().nth(c.col)) {
                    self.record_insert(Key::Char(ch));
                    buf.insert_char(ch);
                }
            }
            Key::Ctrl('r') => {
                self.insert_reg = true;
            }
            Key::Ctrl('o') => {
                buf.end_undo_group();
                self.one_shot = true;
                self.mode = Mode::Normal;
            }
            Key::Ctrl('a') => {
                let text = s.last_insert_text.clone();
                buf.insert(&text);
            }
            Key::Up | Key::Down | Key::Left | Key::Right | Key::Home | Key::End | Key::PageUp | Key::PageDown => {
                // Moving starts a new undo step and a new repeatable insert,
                // exactly as in vim: `.` repeats what was typed after the
                // arrow, not before it.
                buf.end_undo_group();
                buf.begin_undo_group();
                if let Some(i) = &mut self.insert {
                    i.keys.clear();
                    i.count = 1;
                    i.reopen = None;
                }
                return Outcome::Pass;
            }
            Key::Ctrl(_) => return Outcome::Pass,
        }
        Outcome::Handled(Vec::new())
    }

    fn record_insert(&mut self, key: Key) {
        if let Some(i) = &mut self.insert {
            i.keys.push(key);
        }
    }

    /// Replace mode's typing: over the character under the caret, or past
    /// the end of the line.
    fn replace_char(&mut self, buf: &mut Buffer, c: char) {
        let at = buf.cursor;
        let under = buf.line(at.line).chars().nth(at.col);
        self.replaced.push(under);
        let end = if under.is_some() { Pos::new(at.line, at.col + 1) } else { at };
        let mut tmp = [0u8; 4];
        buf.edit(at, end, c.encode_utf8(&mut tmp));
    }

    fn unreplace(&mut self, buf: &mut Buffer) {
        let Some(was) = self.replaced.pop() else {
            // Before the session started: Backspace only moves, as in vim.
            if buf.cursor.col > 0 {
                buf.set_cursor(Pos::new(buf.cursor.line, buf.cursor.col - 1));
            }
            return;
        };
        let at = buf.cursor;
        match was {
            Some(ch) if at.col > 0 => {
                let mut tmp = [0u8; 4];
                buf.edit(Pos::new(at.line, at.col - 1), at, ch.encode_utf8(&mut tmp));
                buf.set_cursor(Pos::new(at.line, at.col - 1));
            }
            _ => buf.backspace(),
        }
    }

    /// Esc: repeat the insert for its count, close the undo step, step the
    /// caret back onto a character, and file the change for `.`.
    fn leave_insert(&mut self, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx) -> Vec<Effect> {
        let mut fx = Vec::new();
        let Some(ins) = self.insert.take() else {
            self.mode = Mode::Normal;
            return fx;
        };
        if ins.count > 1 && !ins.keys.is_empty() {
            // Replays go through a throwaway session so they are not
            // recorded a second time.
            self.macro_depth += 1;
            for _ in 1..ins.count {
                self.insert = Some(Insert { keys: Vec::new(), count: 1, reopen: None });
                if let Some(below) = ins.reopen {
                    self.open_line(buf, below);
                }
                fx.extend(self.feed(&ins.keys, buf, s, host, ctx));
                self.insert = None;
            }
            self.macro_depth -= 1;
        }
        buf.end_undo_group();
        self.replaced.clear();
        self.insert_reg = false;
        self.mode = Mode::Normal;
        self.one_shot = false;
        self.marks.insert('^', buf.cursor);
        s.last_insert_text = typed_text(&ins.keys);
        if buf.cursor.col > 0 {
            buf.set_cursor(Pos::new(buf.cursor.line, buf.cursor.col - 1));
        }
        self.clamp_normal(buf);
        if let Some(mut change) = self.change.take() {
            change.insert = ins.keys;
            if self.dot_depth == 0 {
                s.last_change = Some(change);
            }
        }
        fx
    }

    /// Opens a new line below or above the caret's, carrying the indent, and
    /// puts the caret on it.
    fn open_line(&mut self, buf: &mut Buffer, below: bool) {
        let line = buf.cursor.line;
        let indent: String = buf.line(line).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        if below {
            let end = Pos::new(line, buf.line_len(line));
            buf.edit(end, end, &format!("\n{indent}"));
            buf.set_cursor(Pos::new(line + 1, indent.chars().count()));
        } else {
            let start = Pos::new(line, 0);
            buf.edit(start, start, &format!("{indent}\n"));
            buf.set_cursor(Pos::new(line, indent.chars().count()));
        }
    }

    fn cmdline_key(&mut self, key: Key, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx) -> Vec<Effect> {
        let Some(cl) = &mut self.cmdline else {
            self.mode = Mode::Normal;
            return Vec::new();
        };
        if cl.reg_pending {
            cl.reg_pending = false;
            if let Key::Char(c) = key {
                if let Some(r) = self.fetch(s, host, Some(c)) {
                    if let Some(cl) = &mut self.cmdline {
                        cl.text.push_str(r.text.lines().next().unwrap_or(""));
                    }
                }
            }
            return Vec::new();
        }
        match key {
            Key::Char(c) => cl.text.push(c),
            Key::Backspace | Key::Ctrl('h') => {
                if cl.text.pop().is_none() {
                    self.cancel_cmdline(buf);
                }
            }
            Key::Ctrl('u') => cl.text.clear(),
            Key::Ctrl('w') => {
                let trimmed = cl.text.trim_end().to_string();
                let cut = trimmed
                    .char_indices()
                    .rev()
                    .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                cl.text.truncate(cut);
            }
            Key::Ctrl('r') => cl.reg_pending = true,
            Key::Esc | Key::Ctrl('c') | Key::Ctrl('[') => self.cancel_cmdline(buf),
            Key::Up | Key::Down => {
                let history = if cl.prefix == ':' { &s.cmd_history } else { &s.search_history };
                let prefix = cl.text.clone();
                let matches: Vec<usize> = history
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| cl.hist.is_some() || h.starts_with(&prefix))
                    .map(|(i, _)| i)
                    .collect();
                let at = match (key, cl.hist) {
                    (Key::Up, None) => matches.last().copied(),
                    (Key::Up, Some(i)) => matches.iter().rev().find(|m| **m < i).copied(),
                    (Key::Down, Some(i)) => matches.iter().find(|m| **m > i).copied(),
                    _ => None,
                };
                if let Some(i) = at {
                    cl.text = history[i].clone();
                    cl.hist = Some(i);
                } else if key == Key::Down {
                    cl.text.clear();
                    cl.hist = None;
                }
            }
            Key::Enter | Key::Ctrl('j') | Key::Ctrl('m') => {
                let Some(cl) = self.cmdline.take() else { return Vec::new() };
                self.mode = cl.back_to;
                let text = cl.text;
                return match cl.prefix {
                    ':' => {
                        if !text.is_empty() {
                            s.cmd_history.retain(|h| *h != text);
                            s.cmd_history.push(text.clone());
                        }
                        let fx = self.ex(&text, buf, s, host, ctx);
                        if self.mode == Mode::Command {
                            // A command opened another prompt.
                        } else if !self.in_visual() && !matches!(self.mode, Mode::Insert | Mode::Replace) {
                            self.clamp_normal(buf);
                        }
                        fx
                    }
                    _ => {
                        let forward = cl.prefix == '/';
                        if !text.is_empty() {
                            s.search_history.retain(|h| *h != text);
                            s.search_history.push(text.clone());
                        }
                        let pattern = if text.is_empty() {
                            match &s.last_search {
                                Some((p, _)) => p.clone(),
                                None => {
                                    self.error("E35: No previous regular expression");
                                    return Vec::new();
                                }
                            }
                        } else {
                            text
                        };
                        s.set_search(&pattern, forward);
                        let from = buf.cursor;
                        let pending = self.pending_op.take();
                        let Some(pos) = self.search(buf, s, &pattern, forward, from, 1) else {
                            return Vec::new();
                        };
                        if let Some((op, reg, n)) = pending {
                            let (a, b) = order(from, pos);
                            let range = ops::exclusive_range(buf, a, b);
                            return self.apply_op(op, range, reg, n, buf, s, host, &ctx);
                        }
                        self.push_jump(from);
                        buf.set_cursor(pos);
                        Vec::new()
                    }
                };
            }
            _ => {}
        }
        Vec::new()
    }

    fn cancel_cmdline(&mut self, _buf: &mut Buffer) {
        self.pending_op = None;
        if let Some(cl) = self.cmdline.take() {
            self.mode = cl.back_to;
        }
    }

    fn open_cmdline(&mut self, prefix: char, text: &str, buf: &Buffer) {
        let back_to = if self.in_visual() { self.mode } else { Mode::Normal };
        // `:` leaves visual mode behind — the range it typed stands in for
        // the selection — but `/` keeps it so the search extends it.
        if prefix == ':' && self.in_visual() {
            self.exit_visual(buf);
        }
        self.cmdline = Some(CmdLine {
            prefix,
            text: text.to_string(),
            hist: None,
            back_to: if prefix == ':' { Mode::Normal } else { back_to },
            reg_pending: false,
        });
        self.mode = Mode::Command;
    }

    pub(crate) fn error(&mut self, text: &str) {
        self.message = Some((text.to_string(), true));
        self.failed = true;
    }

    pub(crate) fn say(&mut self, text: &str) {
        self.message = Some((text.to_string(), false));
    }
}

/// An ordered pair.
pub(crate) fn order(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b { (a, b) } else { (b, a) }
}

/// The position one character before `p`, crossing onto the previous line's
/// end; `p` itself at the start of the buffer.
pub(crate) fn back_one(buf: &Buffer, p: Pos) -> Pos {
    if p.col > 0 {
        Pos::new(p.line, p.col - 1)
    } else if p.line > 0 {
        let len = buf.line_len(p.line - 1);
        Pos::new(p.line - 1, len.saturating_sub(1))
    } else {
        p
    }
}

pub(crate) fn first_non_blank(buf: &Buffer, line: usize) -> usize {
    buf.line(line).chars().take_while(|c| c.is_whitespace()).count()
}

/// What an insert session typed, as text — the `.` register.
fn typed_text(keys: &[Key]) -> String {
    let mut out = String::new();
    for k in keys {
        match k {
            Key::Char(c) => out.push(*c),
            Key::Enter => out.push('\n'),
            Key::Tab => out.push_str("    "),
            Key::Backspace => {
                out.pop();
            }
            _ => {}
        }
    }
    out
}

fn key_label(k: Key) -> String {
    match k {
        Key::Char(c) => c.to_string(),
        Key::Ctrl(c) => format!("^{}", c.to_ascii_uppercase()),
        _ => String::new(),
    }
}

/// Keys as text, for storing a macro in a register where it can be seen,
/// pasted and edited like in vim. Named keys become the control characters
/// vim itself uses; the arrows take private-use code points nobody types.
pub(crate) fn keys_to_text(keys: &[Key]) -> String {
    keys.iter()
        .map(|k| match *k {
            Key::Char(c) => c,
            Key::Ctrl(c) => ((c.to_ascii_lowercase() as u8) & 0x1f) as char,
            Key::Esc => '\u{1b}',
            Key::Enter => '\r',
            Key::Backspace => '\u{8}',
            Key::Tab => '\t',
            Key::Delete => '\u{7f}',
            Key::Up => '\u{e000}',
            Key::Down => '\u{e001}',
            Key::Left => '\u{e002}',
            Key::Right => '\u{e003}',
            Key::Home => '\u{e004}',
            Key::End => '\u{e005}',
            Key::PageUp => '\u{e006}',
            Key::PageDown => '\u{e007}',
        })
        .collect()
}

pub(crate) fn text_to_keys(text: &str) -> Vec<Key> {
    text.chars()
        .map(|c| match c {
            '\u{1b}' => Key::Esc,
            '\r' | '\n' => Key::Enter,
            '\u{8}' => Key::Backspace,
            '\t' => Key::Tab,
            '\u{7f}' => Key::Delete,
            '\u{e000}' => Key::Up,
            '\u{e001}' => Key::Down,
            '\u{e002}' => Key::Left,
            '\u{e003}' => Key::Right,
            '\u{e004}' => Key::Home,
            '\u{e005}' => Key::End,
            '\u{e006}' => Key::PageUp,
            '\u{e007}' => Key::PageDown,
            c if (c as u32) < 0x20 => Key::Ctrl((c as u8 + b'`') as char),
            c => Key::Char(c),
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests;
