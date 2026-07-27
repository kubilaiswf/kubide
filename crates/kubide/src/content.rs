//! What a pane holds.
//!
//! One map of `PaneId -> Content` rather than one map per kind. A pane shows
//! exactly one thing, and an enum says so; parallel maps would let two of them
//! disagree about the same pane.

use std::path::{Path, PathBuf};

// The variants differ in size, and clippy would rather the big one were
// boxed. There are at most nine of these alive — one per pane — so the waste
// is a few hundred bytes total, and boxing would put an indirection on every
// single thing the program draws or types into.
#[allow(clippy::large_enum_variant)]
pub enum Content {
    Terminal(kb_term::Terminal),
    Explorer(Explorer),
    Editor(Editor),
    /// Only for files we refuse to edit: binary, too large, unreadable.
    Viewer(Viewer),
    Settings(Settings),
    Git(GitPanel),
    /// What a bare `kubide` shows when it does not recognise where it is.
    Welcome(Welcome),
}

/// One drawn line of the settings screen.
///
/// Headings are lines too, because scrolling counts what is on screen; the
/// selection counts settings. Keeping both in one list is what stops those two
/// from disagreeing about where the selected row is.
pub enum Line {
    Heading(&'static str),
    Row(kb_cfg::Setting),
    /// An action and the key it is on. Read-only: rebinding is a text editor's
    /// job, and there is no way to type a chord at a screen that is already
    /// reading arrow keys as commands.
    Control(kb_cfg::Action),
}

/// The screen as drawn: a heading wherever the section changes.
///
/// The controls come last and are not landed on. They are here rather than in
/// their own screen because "what can this thing do" and "how do I change it"
/// are the same question when you are new, and two screens means finding the
/// second one first.
pub fn settings_lines(keys: &kb_cfg::Keymap) -> Vec<Line> {
    let mut out = Vec::new();
    let mut section = "";
    for setting in kb_cfg::Setting::ALL {
        if setting.section() != section {
            section = setting.section();
            out.push(Line::Heading(section));
        }
        out.push(Line::Row(*setting));
    }

    out.push(Line::Heading("CONTROLS"));
    // Bound actions only. A command with no key on it is something the palette
    // can run, not something this list can teach you to press.
    out.extend(
        kb_cfg::Action::ALL
            .iter()
            .filter(|a| keys.binding_for(**a).is_some())
            .map(|a| Line::Control(*a)),
    );
    out
}

/// The settings screen.
///
/// A pane rather than an overlay: changes land live, and watching the padding
/// move while the file next to it stays put is the whole reason to see them
/// side by side. An overlay would cover the thing being adjusted.
pub struct Settings {
    /// Index into `Setting::ALL`, not into the drawn lines.
    pub selected: usize,
    /// First visible drawn line.
    pub top: usize,
    /// What the last save did. Cleared by the next change, so it can never
    /// claim a file matches a screen it no longer matches.
    pub status: Option<String>,
    /// Whatever the pane held before this took it over.
    ///
    /// Held rather than dropped so leaving puts the file back. Closing the
    /// pane instead would leave an empty one, and on a single-pane window that
    /// is the whole editor gone with no way back to what you were reading.
    /// Boxed because this is itself a `Content`.
    previous: Option<Box<Content>>,
    /// The selection the view last followed — the wheel-versus-follow gate.
    selection_seen: Option<usize>,
}

impl Settings {
    pub fn new(previous: Option<Box<Content>>) -> Self {
        Self { selected: 0, top: 0, status: None, previous, selection_seen: None }
    }

    /// Gives back what the pane held before. `None` when it was empty.
    pub fn take_previous(&mut self) -> Option<Content> {
        self.previous.take().map(|b| *b)
    }

    pub fn setting(&self) -> kb_cfg::Setting {
        kb_cfg::Setting::ALL[self.selected.min(kb_cfg::Setting::ALL.len() - 1)]
    }

    pub fn move_selection(&mut self, delta: i32) {
        let last = kb_cfg::Setting::ALL.len() as i32 - 1;
        self.selected = (self.selected as i32 + delta).clamp(0, last) as usize;
        self.status = None;
    }

    /// Where the selected setting sits among the drawn lines.
    ///
    /// Not the same number as `selected`: every heading above it pushes it
    /// down one. Scrolling works in drawn lines, so it has to ask.
    pub fn drawn_row(&self) -> usize {
        let mut section = "";
        let mut row = 0;
        for (i, setting) in kb_cfg::Setting::ALL.iter().enumerate() {
            if setting.section() != section {
                section = setting.section();
                row += 1;
            }
            if i == self.selected {
                return row;
            }
            row += 1;
        }
        row
    }

    /// Scrolls just enough to keep the selection on screen.
    pub fn ensure_visible(&mut self, keys: &kb_cfg::Keymap, visible: usize) {
        if visible == 0 {
            return;
        }
        // Only when the selection moved, or the wheel loses every time.
        if self.selection_seen == Some(self.selected) {
            return;
        }
        self.selection_seen = Some(self.selected);
        let row = self.drawn_row();
        if row < self.top {
            // Far enough to bring the section heading with it, or you land on
            // a row with no idea what it belongs to.
            self.top = row.saturating_sub(1);
        } else if row >= self.top + visible {
            self.top = row + 1 - visible;
        }
        let total = settings_lines(keys).len();
        self.top = self.top.min(total.saturating_sub(visible));
    }

    pub fn scroll(&mut self, keys: &kb_cfg::Keymap, delta: i32, visible: usize) {
        let max_top = settings_lines(keys).len().saturating_sub(visible.max(1));
        let next = self.top as i32 - delta;
        self.top = next.clamp(0, max_top as i32) as usize;
    }
}

impl Content {
    pub fn as_terminal(&self) -> Option<&kb_term::Terminal> {
        match self {
            Content::Terminal(t) => Some(t),
            _ => None,
        }
    }

    /// Opens a path as the right kind of pane.
    ///
    /// A refusal is still a pane rather than nothing happening: pressing Enter
    /// on a file and seeing no reaction reads as a broken editor.
    pub fn open_path(path: &Path) -> Content {
        match Viewer::reason_to_refuse(path) {
            Some(note) => Content::Viewer(Viewer::refused(path, note)),
            None => match kb_edit::Buffer::open(path) {
                Ok(buffer) => Content::Editor(Editor::new(buffer)),
                Err(e) => Content::Viewer(Viewer::refused(path, e.to_string())),
            },
        }
    }
}

pub struct Editor {
    pub buffer: kb_edit::Buffer,
    /// First visible line.
    pub top: usize,
    /// First visible column. Long lines scroll sideways rather than wrapping:
    /// wrapping breaks the one-line-one-row assumption the whole renderer and
    /// the line-number gutter are built on.
    pub left: usize,
    /// Transient message, e.g. the result of a save.
    pub status: Option<String>,
    /// `None` for a language we have no grammar for.
    lang: Option<kb_syn::Lang>,
    /// Highlight spans per line, and the buffer revision they were built from.
    ///
    /// Re-parsing on every frame would burn a whole core on an idle window;
    /// re-parsing on every keystroke is fine, because that only happens when
    /// the text actually changed.
    highlights: Vec<Vec<kb_syn::Span>>,
    highlighted_at: Option<u64>,
    /// Longest line in characters, and the buffer revision it was measured at.
    ///
    /// The sideways indicator needs it on every frame, and walking every line
    /// of a large file sixty times a second for a number that only moves when
    /// the text does is exactly the cost the highlight cache exists to avoid.
    widest: usize,
    widest_at: Option<u64>,
    /// Which lines differ from HEAD, for the gutter. Refreshed by the owner
    /// when git reports news — the editor cannot ask git itself, and marks
    /// that quietly went stale would point at the wrong lines.
    pub marks: Vec<(usize, kb_git::LineChange)>,
    /// The git generation `marks` was computed at. `None` means never.
    pub marks_at: Option<u64>,
    /// The caret position and revision the view last followed. The gate
    /// that keeps caret-following out of wheel scrolling's way.
    caret_seen: Option<(kb_edit::Pos, u64)>,
}

impl Editor {
    pub fn new(buffer: kb_edit::Buffer) -> Self {
        let lang = buffer.path().and_then(kb_syn::Lang::of);
        Self {
            buffer,
            top: 0,
            left: 0,
            status: None,
            lang,
            highlights: Vec::new(),
            highlighted_at: None,
            widest: 0,
            widest_at: None,
            marks: Vec::new(),
            marks_at: None,
            caret_seen: None,
        }
    }

    /// Characters in the longest line, remeasured only when the text changed.
    ///
    /// Characters rather than pixels: the font is monospace and every column
    /// is one cell, so this converts to a width by multiplying, and counting
    /// bytes instead would overstate every line holding a non-ASCII character.
    pub fn widest(&mut self) -> usize {
        if self.widest_at != Some(self.buffer.revision()) {
            self.widest = self
                .buffer
                .lines()
                .iter()
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(0);
            self.widest_at = Some(self.buffer.revision());
        }
        self.widest
    }

    /// Re-highlights if the text changed since last time.
    ///
    /// Whole-file parse rather than an incremental one. tree-sitter supports
    /// incremental parsing but it needs byte-level edit tracking threaded
    /// through the buffer; at the file sizes this editor accepts, a full parse
    /// is a millisecond or two and not worth that complexity yet.
    pub fn sync_highlights(&mut self, syntax: &kb_syn::Syntax) {
        let Some(lang) = self.lang else { return };
        if self.highlighted_at == Some(self.buffer.revision()) {
            return;
        }
        self.highlights = syntax.highlight(lang, &self.buffer.to_text());
        self.highlighted_at = Some(self.buffer.revision());
    }

    /// Spans for one line, empty when there is no highlighting for it.
    pub fn spans(&self, line: usize) -> &[kb_syn::Span] {
        self.highlights.get(line).map(Vec::as_slice).unwrap_or(&[])
    }
}

impl Editor {
    /// Scrolls so the cursor stays on screen, with a little margin.
    ///
    /// Called from drawing, because only the renderer knows how many lines
    /// fit — but it acts only when the caret moved or the text changed.
    /// Following the caret is for typing and jumps; a wheel scroll is
    /// neither, and without this gate every frame yanked the view straight
    /// back to the caret. That read as "scrolling is broken": it moved,
    /// then snapped home before the next glance.
    pub fn ensure_visible(&mut self, visible: usize, cols: usize) {
        if visible == 0 {
            return;
        }
        let now = (self.buffer.cursor, self.buffer.revision());
        if self.caret_seen == Some(now) {
            // The bounds still hold when the file or the pane shrank.
            self.top = self.top.min(self.buffer.len().saturating_sub(1));
            return;
        }
        self.caret_seen = Some(now);
        // Keeping a couple of lines of context below the cursor is the
        // difference between typing at the bottom edge and typing in a window.
        let margin = 2.min(visible / 4);
        let line = self.buffer.cursor.line;
        if line < self.top + margin {
            self.top = line.saturating_sub(margin);
        } else if line + margin >= self.top + visible {
            self.top = line + margin + 1 - visible;
        }
        let max_top = self.buffer.len().saturating_sub(1);
        self.top = self.top.min(max_top);

        // Sideways, with a wider margin: horizontal scrolling is disorienting,
        // so it should happen in jumps rather than one column at a time.
        if cols == 0 {
            return;
        }
        let col = self.buffer.cursor.col;
        let hmargin = 8.min(cols / 4);
        if col < self.left + hmargin {
            self.left = col.saturating_sub(hmargin);
        } else if col + hmargin >= self.left + cols {
            self.left = col + hmargin + 1 - cols;
        }
    }

    pub fn scroll(&mut self, delta: i32, visible: usize) {
        let max_top = self.buffer.len().saturating_sub(visible.max(1));
        let next = self.top as i32 - delta;
        self.top = next.clamp(0, max_top as i32) as usize;
    }

    pub fn save(&mut self) {
        self.status = Some(match self.buffer.save() {
            Ok(()) => "saved".to_string(),
            // Read-only files and permission errors are common enough that
            // silently doing nothing would be a genuine trap.
            Err(e) => format!("save failed: {e}"),
        });
    }
}

/// The screen a bare `kubide` opens on when launched somewhere it has no
/// session for — a double-click on the exe, mostly. A quiet wordmark, the
/// keys that matter, and the places you have been: the welcome tab of the
/// big editors, reduced to what this one believes in. Opening a workspace
/// from here replaces it; it is a hallway, not a room.
pub struct Welcome {
    /// `(label, directory)` — the folder kubide was started in first, so
    /// Enter always has an answer, then the remembered workspaces.
    pub rows: Vec<(String, PathBuf)>,
    pub selected: usize,
}

impl Welcome {
    pub fn new(cwd: &Path, recents: Vec<PathBuf>) -> Self {
        let name = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        };
        let mut rows = vec![(format!("{} (this folder)", name(cwd)), cwd.to_path_buf())];
        rows.extend(
            recents
                .into_iter()
                .filter(|p| p != cwd)
                .map(|p| (name(&p), p)),
        );
        Self { rows, selected: 0 }
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as i32 - 1;
        self.selected = (self.selected as i32).saturating_add(delta).clamp(0, last) as usize;
    }

    pub fn chosen(&self) -> Option<PathBuf> {
        self.rows.get(self.selected).map(|(_, p)| p.clone())
    }
}

/// Which of the git panel's three screens is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitView {
    /// The changed files, staged and not.
    Status,
    /// One diff — a file's or a commit's.
    Diff,
    /// Recent commits.
    Log,
}

/// One drawn line of the status screen. Headings are lines here for the same
/// reason they are on the settings screen: scrolling counts what is on
/// screen, selection counts entries, and one list keeps them agreeing.
pub enum GitLine {
    Heading(String),
    /// Index into the panel's entries.
    Entry(usize),
}

/// The status screen as drawn: a heading over each half that exists.
pub fn git_lines(entries: &[kb_git::Entry]) -> Vec<GitLine> {
    let staged = entries.iter().filter(|e| e.staged).count();
    let unstaged = entries.len() - staged;
    let mut out = Vec::new();
    if staged > 0 {
        out.push(GitLine::Heading(format!("STAGED \u{b7} {staged}")));
        out.extend((0..staged).map(GitLine::Entry));
    }
    if unstaged > 0 {
        out.push(GitLine::Heading(format!("CHANGES \u{b7} {unstaged}")));
        out.extend((staged..staged + unstaged).map(GitLine::Entry));
    }
    out
}

/// The git panel: what changed, what is staged, and the recent history.
///
/// A pane like the settings screen rather than an overlay, because staging
/// is a conversation with the working tree — act, look at what that did,
/// act again — and a dialog that vanished on every action would make it a
/// chore. Held state is flat rather than an enum per screen: Esc has to put
/// the log back exactly as it was left, and rebuilding it from scratch on
/// every return would lose the scroll and the selection.
pub struct GitPanel {
    pub entries: Vec<kb_git::Entry>,
    /// Index into `entries` — headings are skipped over, not landed on.
    pub selected: usize,
    /// First visible drawn line of the status screen.
    pub top: usize,
    pub view: GitView,
    /// Whose diff is showing, said over the list: "src/main.rs (staged)" or
    /// a commit's hash and subject.
    pub diff_title: String,
    pub diff: Vec<(kb_git::DiffKind, String)>,
    pub diff_top: usize,
    /// Where Esc goes from the diff: back to the log when the diff came
    /// from there, back to the files otherwise.
    pub diff_from_log: bool,
    pub commits: Vec<kb_git::Commit>,
    pub log_selected: usize,
    pub log_top: usize,
    /// What the last action said — "staged x", a commit summary, git's own
    /// error. Cleared by the next movement so it cannot go stale.
    pub status: Option<String>,
    /// Whatever the pane held before, put back on close — same contract as
    /// the settings screen.
    previous: Option<Box<Content>>,
    /// What the view last followed, per screen — the wheel-versus-follow
    /// gate the editor keeps for its caret.
    followed: Option<(GitView, usize, usize)>,
}

impl GitPanel {
    pub fn new(previous: Option<Box<Content>>, entries: Vec<kb_git::Entry>) -> Self {
        Self {
            entries,
            selected: 0,
            top: 0,
            view: GitView::Status,
            diff_title: String::new(),
            diff: Vec::new(),
            diff_top: 0,
            diff_from_log: false,
            commits: Vec::new(),
            log_selected: 0,
            log_top: 0,
            status: None,
            previous,
            followed: None,
        }
    }

    pub fn take_previous(&mut self) -> Option<Content> {
        self.previous.take().map(|b| *b)
    }

    /// Fresh entries after an action, keeping the selection near where it
    /// was: staging the third file should leave you at the third row, which
    /// is now the next thing to decide about.
    pub fn set_entries(&mut self, entries: Vec<kb_git::Entry>) {
        self.entries = entries;
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
    }

    pub fn selected_entry(&self) -> Option<&kb_git::Entry> {
        self.entries.get(self.selected)
    }

    pub fn selected_commit(&self) -> Option<&kb_git::Commit> {
        self.commits.get(self.log_selected)
    }

    /// Moves whatever the current screen moves: the file selection, the
    /// commit selection, or the diff scroll.
    pub fn move_selection(&mut self, delta: i32, visible: usize) {
        self.status = None;
        match self.view {
            GitView::Status => {
                if self.entries.is_empty() {
                    return;
                }
                let last = self.entries.len() as i32 - 1;
                self.selected = (self.selected as i32).saturating_add(delta).clamp(0, last) as usize;
            }
            GitView::Log => {
                if self.commits.is_empty() {
                    return;
                }
                let last = self.commits.len() as i32 - 1;
                self.log_selected =
                    (self.log_selected as i32).saturating_add(delta).clamp(0, last) as usize;
            }
            GitView::Diff => {
                let max_top = self.diff.len().saturating_sub(visible.max(1)) as i32;
                self.diff_top =
                    (self.diff_top as i32).saturating_add(delta).clamp(0, max_top.max(0)) as usize;
            }
        }
    }

    /// Where the selected entry sits among the drawn lines — every heading
    /// above it pushes it down one.
    fn drawn_row(&self) -> usize {
        let staged = self.entries.iter().filter(|e| e.staged).count();
        let mut row = self.selected;
        if staged > 0 {
            row += 1; // the STAGED heading
        }
        if self.selected >= staged {
            row += 1; // the CHANGES heading
        }
        row
    }

    /// Scrolls the current screen just enough to keep its selection on
    /// screen. Called from drawing, which is what knows how many rows fit —
    /// and acting only when the selection moved, so the wheel is not
    /// snapped back on the very next frame.
    pub fn ensure_visible(&mut self, visible: usize) {
        if visible == 0 {
            return;
        }
        let now = (self.view, self.selected, self.log_selected);
        if self.followed == Some(now) {
            return;
        }
        self.followed = Some(now);
        match self.view {
            GitView::Status => {
                let row = self.drawn_row();
                if row < self.top {
                    // Far enough to bring the heading along, or the row lands
                    // with no idea which half it belongs to.
                    self.top = row.saturating_sub(1);
                } else if row >= self.top + visible {
                    self.top = row + 1 - visible;
                }
                let total = git_lines(&self.entries).len();
                self.top = self.top.min(total.saturating_sub(visible));
            }
            GitView::Log => {
                if self.log_selected < self.log_top {
                    self.log_top = self.log_selected;
                } else if self.log_selected >= self.log_top + visible {
                    self.log_top = self.log_selected + 1 - visible;
                }
                self.log_top = self.log_top.min(self.commits.len().saturating_sub(visible));
            }
            GitView::Diff => {
                self.diff_top = self.diff_top.min(self.diff.len().saturating_sub(visible.max(1)));
            }
        }
    }

    /// Wheel scrolling, which moves the view without moving the selection —
    /// the same contract every other pane keeps.
    pub fn scroll(&mut self, delta: i32, visible: usize) {
        let (top, total) = match self.view {
            GitView::Status => (&mut self.top, git_lines(&self.entries).len()),
            GitView::Log => (&mut self.log_top, self.commits.len()),
            GitView::Diff => (&mut self.diff_top, self.diff.len()),
        };
        let max_top = total.saturating_sub(visible.max(1));
        let next = *top as i32 - delta;
        *top = next.clamp(0, max_top as i32) as usize;
    }

    /// Opens a diff over the current screen.
    pub fn show_diff(&mut self, title: String, lines: Vec<(kb_git::DiffKind, String)>, from_log: bool) {
        self.diff_title = title;
        self.diff = lines;
        self.diff_top = 0;
        self.diff_from_log = from_log;
        self.view = GitView::Diff;
    }

    /// Esc: one screen back. Says whether there was anywhere to go back to —
    /// Esc on the file list means "close the panel", which is the caller's
    /// job, not this struct's.
    pub fn back(&mut self) -> bool {
        match self.view {
            GitView::Diff => {
                self.view = if self.diff_from_log { GitView::Log } else { GitView::Status };
                true
            }
            GitView::Log => {
                self.view = GitView::Status;
                true
            }
            GitView::Status => false,
        }
    }
}

pub struct Explorer {
    pub tree: kb_fs::FileTree,
    /// First visible row. Kept here rather than in pixels: the explorer scrolls
    /// by whole rows, and a pixel offset would let a row sit half cut off.
    pub top: usize,
    /// The selection the view last followed — the same wheel-versus-follow
    /// gate the editor keeps for its caret.
    selection_seen: Option<usize>,
}

impl Explorer {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            tree: kb_fs::FileTree::new(root),
            top: 0,
            selection_seen: None,
        }
    }

    /// Moves this explorer to another root, landing on `from` when that path
    /// is in the new listing.
    ///
    /// Never called on its own: the root belongs to the workspace, not to one
    /// tree, so this goes through `Kubide::set_workspace_root` along with git,
    /// the finder and the session. A tree that moved by itself is exactly the
    /// bug this replaced.
    ///
    /// Landing on `from` because stepping out of a directory and finding the
    /// selection at the top of an unfamiliar list reads as the tree jumping
    /// somewhere random.
    pub fn move_root(&mut self, root: PathBuf, from: &Path) {
        self.tree.set_root(root);
        self.tree.select_path(from);
        self.top = 0;
    }

    /// Scrolls just enough to keep the selection on screen.
    ///
    /// Called from drawing, because only the renderer knows how many rows fit.
    /// Doing it on every key press instead would need the row count duplicated
    /// in two places, and they would drift.
    pub fn ensure_visible(&mut self, visible_rows: usize) {
        if visible_rows == 0 {
            return;
        }
        let sel = self.tree.selected();
        // Only when the selection moved: following it on every frame would
        // snap a wheel scroll straight back.
        if self.selection_seen == Some(sel) {
            self.top = self.top.min(self.tree.rows().len().saturating_sub(1));
            return;
        }
        self.selection_seen = Some(sel);
        if sel < self.top {
            self.top = sel;
        } else if sel >= self.top + visible_rows {
            self.top = sel + 1 - visible_rows;
        }
        let max_top = self.tree.rows().len().saturating_sub(visible_rows);
        self.top = self.top.min(max_top);
    }

    pub fn scroll(&mut self, delta: i32, visible_rows: usize) {
        let max_top = self.tree.rows().len().saturating_sub(visible_rows.max(1));
        let next = self.top as i32 - delta;
        self.top = next.clamp(0, max_top as i32) as usize;
    }
}

/// Read-only file view. Not the editor — no buffer, no undo, no editing.
/// It exists so the explorer leads somewhere.
pub struct Viewer {
    pub path: PathBuf,
    pub lines: Vec<String>,
    /// Why the content is incomplete, if it is. Shown in the pane: silently
    /// truncating a file and letting someone read half of it is worse than
    /// refusing to open it.
    pub note: Option<String>,
    pub top: usize,
}

/// Refuse rather than freeze. A gigabyte of minified JSON would be shaped line
/// by line by DirectWrite and lock the UI for minutes.
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LINES: usize = 50_000;

impl Viewer {
    /// Why this file must not be opened for editing, if it must not be.
    ///
    /// Checked before reading the whole thing, so a huge file never gets loaded
    /// just to be rejected.
    pub fn reason_to_refuse(path: &Path) -> Option<String> {
        let size = std::fs::metadata(path).ok()?.len();
        if size > MAX_BYTES {
            return Some(format!(
                "too large to open ({:.1} MB, limit {} MB)",
                size as f64 / 1_048_576.0,
                MAX_BYTES / 1_048_576
            ));
        }
        // Only the head: enough to catch binaries, cheap on a large file.
        let head = std::fs::read(path).ok()?;
        if is_binary(&head) {
            return Some("binary file".into());
        }
        if head.iter().filter(|b| **b == b'\n').count() > MAX_LINES {
            return Some(format!("more than {MAX_LINES} lines"));
        }
        None
    }

    pub fn refused(path: &Path, note: String) -> Self {
        Self {
            path: path.to_path_buf(),
            lines: Vec::new(),
            note: Some(note),
            top: 0,
        }
    }

    pub fn scroll(&mut self, delta: i32, visible_rows: usize) {
        let max_top = self.lines.len().saturating_sub(visible_rows.max(1));
        let next = self.top as i32 - delta;
        self.top = next.clamp(0, max_top as i32) as usize;
    }
}

/// A NUL byte in the first few KB. The heuristic every diff tool uses, and it
/// beats guessing from the extension.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_keeps_the_selection_visible() {
        let dir = std::env::temp_dir().join("kubide-content-scroll");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..30 {
            std::fs::write(dir.join(format!("f{i:02}.txt")), "").unwrap();
        }

        let mut e = Explorer::new(&dir);
        e.tree.move_selection(25);
        e.ensure_visible(10);
        assert!(e.top <= e.tree.selected());
        assert!(e.tree.selected() < e.top + 10);

        e.tree.move_selection(-25);
        e.ensure_visible(10);
        assert_eq!(e.top, 0, "going back to the top must scroll back up");
    }

    #[test]
    fn moving_the_root_up_lands_on_the_directory_we_left() {
        let base = std::env::temp_dir().join("kubide-content-root");
        let _ = std::fs::remove_dir_all(&base);
        for name in ["alpha", "beta", "gamma"] {
            std::fs::create_dir_all(base.join(name)).unwrap();
        }

        let mut e = Explorer::new(base.join("gamma"));
        e.top = 5;
        e.move_root(base.clone(), &base.join("gamma"));

        assert_eq!(e.tree.root(), base.as_path());
        assert_eq!(e.tree.selected_row().map(|r| r.name.as_str()), Some("gamma"));
        assert_eq!(e.top, 0, "the old scroll offset means nothing in a new listing");
    }

    #[test]
    fn a_root_with_nothing_to_come_back_to_still_moves() {
        // The path we came from is not always in the new listing — a sibling
        // may have been deleted, or the new root may not contain it at all.
        // Landing on the first row beats refusing to move.
        let base = std::env::temp_dir().join("kubide-content-root-missing");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("only")).unwrap();

        let mut e = Explorer::new(base.join("only"));
        e.move_root(base.clone(), Path::new("nowhere-at-all"));

        assert_eq!(e.tree.root(), base.as_path());
        // What move_root owes the caller when `from` is not there: a moved
        // root and a selection that still points at a row. Asserting index 0
        // would only be restating what set_root does on its own.
        assert!(e.tree.selected_row().is_some(), "the move must leave a usable selection");
    }

    #[test]
    fn a_binary_file_is_refused_with_a_reason() {
        let p = std::env::temp_dir().join("kubide-content-bin");
        std::fs::write(&p, [0x00, 0x01, 0x02, 0x00]).unwrap();
        assert_eq!(Viewer::reason_to_refuse(&p).as_deref(), Some("binary file"));
        assert!(matches!(Content::open_path(&p), Content::Viewer(_)));
    }

    #[test]
    fn a_text_file_opens_as_an_editor() {
        let p = std::env::temp_dir().join("kubide-content-text.txt");
        std::fs::write(&p, "hello\n").unwrap();
        assert_eq!(Viewer::reason_to_refuse(&p), None);
        assert!(matches!(Content::open_path(&p), Content::Editor(_)));
    }

    #[test]
    fn a_missing_file_still_opens_a_pane_that_says_why() {
        // Pressing Enter and having nothing happen reads as a broken editor.
        match Content::open_path(Path::new("no-such-file-anywhere.txt")) {
            Content::Viewer(v) => assert!(v.note.is_some()),
            _ => panic!("a missing file must not open as an editor"),
        }
    }

    #[test]
    fn the_view_follows_the_cursor_down_and_back_up() {
        let mut e = Editor::new(kb_edit::Buffer::from_text(&"line\n".repeat(200)));
        e.buffer.move_vertical(150, false);
        e.ensure_visible(20, 80);
        assert!(e.top <= e.buffer.cursor.line);
        assert!(e.buffer.cursor.line < e.top + 20);

        e.buffer.move_vertical(-150, false);
        e.ensure_visible(20, 80);
        assert_eq!(e.top, 0);
    }
}
