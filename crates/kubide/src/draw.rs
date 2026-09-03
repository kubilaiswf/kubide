//! All drawing.
//!
//! Split from `main.rs` so input handling and rendering stay readable
//! separately. Everything here is `impl Kubide`; the state lives in one place.

use kb_gfx::{rgba, Canvas, Color, Point, RoundedRect};
// The pixel rectangle draw.rs fills and clips with; kb_ui::Rect stays the
// pane geometry, so the drawing one gets a name of its own here.
use kb_gfx::Rect as Bounds;
use kb_ui::{PaneId, Rect};
use kb_win::{CaptionButton, Chrome};
use kb_gfx::Result;

use crate::content::{self, Content};
use crate::metrics::{self, TextArea, INSET, PAD};
use crate::{Kubide, Renderer};

fn to_color(c: kb_term::Rgb, a: f32) -> Color {
    rgba(c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, a)
}

/// Theme color to a drawing color, with an extra alpha multiplier for focus fades.
pub fn themed(c: kb_cfg::Color, alpha: f32) -> Color {
    let (r, g, b, a) = c.f32s();
    rgba(r, g, b, a * alpha)
}

/// How solid an overlay's backdrop has to be, whatever its theme says.
///
/// Below this a light line of code behind the panel contributes enough to be
/// read, and a list of file names sitting on top of a diff becomes two texts
/// at once. The colour is the theme's business; this is not — a theme file
/// that asks for less is describing a bug, and old seeded copies of the
/// built-ins ask for exactly that.
const OVERLAY_FLOOR: f32 = 0.95;

/// Rows a choice box takes: frame, title, rule, air, question, air,
/// answers, air, frame. Known up front so a box can be placed by its
/// bottom edge before it is built.
const CHOICE_ROWS: usize = 9;

/// The backdrop of an overlay: the theme's colour, never thinner than
/// [`OVERLAY_FLOOR`]. A theme is free to ask for more.
fn overlay(c: kb_cfg::Color) -> Color {
    let (r, g, b, a) = c.f32s();
    rgba(r, g, b, a.max(OVERLAY_FLOOR))
}

/// One explorer line, copied out of the tree so the shaping cache can borrow
/// `self` mutably while drawing.
struct Row {
    name: String,
    is_dir: bool,
    depth: usize,
    open: bool,
    git: Option<kb_git::Status>,
}

/// Local wall-clock time.
///
/// Computed from the system clock rather than pulling in a date library: this
/// needs hours and minutes in the local zone and nothing else, and the
/// platform layer hands exactly that over.
fn clock(twenty_four: bool) -> String {
    let (hour, minute) = kb_win::local_clock();
    if twenty_four {
        return format!("{hour:02}:{minute:02}");
    }
    let (hour, suffix) = match hour {
        0 => (12, "am"),
        h if h < 12 => (h, "am"),
        12 => (12, "pm"),
        h => (h - 12, "pm"),
    };
    format!("{hour}:{minute:02} {suffix}")
}

fn syntax_color(c: &kb_cfg::SyntaxColors, k: kb_syn::Kind) -> kb_cfg::Color {
    use kb_syn::Kind::*;
    match k {
        Keyword => c.keyword,
        Function => c.function,
        Type => c.type_,
        String => c.string,
        Number => c.number,
        Comment => c.comment,
        Constant => c.constant,
        Operator => c.operator,
        Punctuation => c.punctuation,
        Variable => c.variable,
        Property => c.property,
        Attribute => c.attribute,
    }
}

fn git_color(c: &kb_cfg::GitColors, s: kb_git::Status) -> kb_cfg::Color {
    match s {
        kb_git::Status::Modified => c.modified,
        kb_git::Status::Added => c.added,
        kb_git::Status::Deleted => c.deleted,
        kb_git::Status::Renamed => c.modified,
        kb_git::Status::Untracked => c.untracked,
        kb_git::Status::Conflicted => c.conflicted,
    }
}

/// What the corner shortcut list says, in order.
///
/// `(action, stands for, label)`. An empty middle field means the line shows
/// whatever that action is bound to; a filled one means the line stands for a
/// whole family and says so in one go — nine numbered jumps and four resize
/// chords spelled out would be half the list and none of the interest. The
/// action is still consulted either way, so a family nobody has bound drops
/// out with it.
///
/// Curated rather than every action: this is the list you scan when you have
/// forgotten a chord, and the full one is a keystroke away in the settings
/// screen. Ordered by what a person reaches for, not alphabetically.
type Cheat = (kb_cfg::Action, &'static str, &'static str);
const CHEAT_SHEET: &[Cheat] = &[
    (kb_cfg::Action::Commands, "", "All commands"),
    (kb_cfg::Action::OpenSettings, "", "Settings"),
    (kb_cfg::Action::OpenFolder, "", "Open folder"),
    (kb_cfg::Action::GoToFile, "", "Go to file"),
    (kb_cfg::Action::LastFile, "", "Switch to last file"),
    (kb_cfg::Action::Find, "", "Find in file"),
    (kb_cfg::Action::Replace, "", "Replace in file"),
    (kb_cfg::Action::FindInProject, "", "Find in project"),
    (kb_cfg::Action::GoToLine, "", "Go to line"),
    (kb_cfg::Action::GitPanel, "", "Git panel"),
    (kb_cfg::Action::OpenAgent, "", "Claude agent"),
    (kb_cfg::Action::Save, "", "Save"),
    (kb_cfg::Action::ToggleComment, "", "Toggle comment"),
    (kb_cfg::Action::DuplicateLine, "", "Duplicate line"),
    (kb_cfg::Action::DeleteLine, "", "Delete line"),
    (kb_cfg::Action::MoveLineUp, "Ctrl+Shift+\u{2191}\u{2193}", "Move line"),
    (kb_cfg::Action::ToggleExplorer, "", "File tree"),
    (kb_cfg::Action::OpenTerminal, "", "Terminal"),
    (kb_cfg::Action::SplitRight, "", "Split side by side"),
    (kb_cfg::Action::SplitDown, "", "Split stacked"),
    (kb_cfg::Action::ClosePane, "", "Close pane"),
    (kb_cfg::Action::FocusLeft, "Alt+\u{2190}\u{2192}\u{2191}\u{2193}", "Move focus"),
    (kb_cfg::Action::FocusPane1, "Alt+1\u{2026}9", "Focus pane by number"),
    (
        kb_cfg::Action::GrowPaneWidth,
        "Ctrl+Alt+\u{2190}\u{2192}\u{2191}\u{2193}",
        "Resize pane",
    ),
    (kb_cfg::Action::NewFile, "", "New file (in the tree)"),
    (kb_cfg::Action::Rename, "", "Rename (in the tree)"),
    (kb_cfg::Action::WorkspaceHere, "", "Open folder as workspace (in the tree)"),
    (kb_cfg::Action::PomodoroToggle, "", "Work timer"),
    (kb_cfg::Action::ToggleMaximize, "", "Maximise or restore"),
    (kb_cfg::Action::Quit, "", "Quit"),
];

/// Background a cell should be drawn with. Selection wins over inverse, so the
/// highlight stays visible whatever colors the content uses.
fn cell_bg(c: &kb_term::Cell, selection: kb_term::Rgb) -> kb_term::Rgb {
    if c.selected {
        selection
    } else if c.inverse {
        c.fg
    } else {
        c.bg
    }
}

impl Kubide {
    pub(crate) fn render(&mut self, window: kb_win::Window, chrome: &Chrome) -> Result<()> {
        if self.gfx.is_none() {
            let (w, h) = kb_win::client_size(window);
            self.gfx = Some(Renderer::new(window, w, h)?);
            self.relayout(w as f32, h as f32);
        }
        let Some(mut gfx) = self.gfx.take() else { return Ok(()) };

        let t0 = std::time::Instant::now();
        let (w, h) = gfx.size();
        if self.layout.panes.is_empty() {
            self.relayout(w, h);
        }
        // A frame that cannot start is not a reason to lose the renderer.
        let dc = match gfx.begin() {
            Ok(dc) => dc,
            Err(e) => {
                self.gfx = Some(gfx);
                return Err(e);
            }
        };

        self.sync_terms();
        let panes: Vec<(PaneId, Rect)> = self.layout.panes.clone();
        for (pane, r) in panes {
            let focused = pane == self.focus && chrome.active;
            match self.content.get(&pane) {
                Some(Content::Terminal(_)) => self.draw_terminal(&dc, pane, r, focused)?,
                Some(Content::Explorer(_)) => self.draw_explorer(&dc, pane, r, focused)?,
                Some(Content::Editor(_)) => self.draw_editor(&dc, pane, r, focused)?,
                Some(Content::Viewer(_)) => self.draw_viewer(&dc, pane, r, focused)?,
                Some(Content::Settings(_)) => self.draw_settings(&dc, pane, r, focused)?,
                Some(Content::Git(_)) => self.draw_git(&dc, pane, r, focused)?,
                Some(Content::Agent(_)) => self.draw_agent(&dc, pane, r, focused)?,
                Some(Content::Welcome(_)) => self.draw_welcome(&dc, pane, r, focused)?,
                None => self.draw_placeholder(&dc, pane, r, focused)?,
            }
        }

        let hair = dc.solid(themed(self.cfg.theme.divider, 1.0))?;
        for (_, _, r) in self.layout.dividers.clone() {
            dc.fill_rect(
                &Bounds { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() },
                &hair,
            );
        }

        self.draw_status(&dc, h, chrome)?;
        self.draw_help(&dc, w, h)?;
        self.draw_palette(&dc, w, h)?;
        // Above the palette: opening the picker closes the palette, but
        // drawing order should say the same thing the input order does.
        self.draw_folder_picker(&dc, w, h)?;

        self.draw_caption(&dc, chrome)?;
        gfx.end(dc)?;
        self.frame_ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.gfx = Some(gfx);
        Ok(())
    }

    /// Where you are in content taller or wider than the pane.
    ///
    /// A thumb and no track. A track is a permanent line down the side of
    /// every pane, and on a translucent surface that reads as a crack in the
    /// window; the thumb alone says both things a scrollbar is read for — how
    /// much of the whole you are seeing, and whereabouts it sits.
    ///
    /// Nothing is drawn when the content fits, so a short file carries no
    /// furniture at all. Not draggable on purpose: the point of this editor is
    /// never to need the mouse, and a grabbable thumb is a promise to support
    /// grabbing everything else too.
    ///
    /// `y0` is the top of the content area — below the pane header, which the
    /// vertical bar must not run across.
    fn draw_scroll_marks(
        &self,
        dc: &Canvas,
        r: Rect,
        y0: f32,
        focused: bool,
        down: (usize, usize, usize),
        across: Option<(f32, usize, usize, usize)>,
    ) -> Result<()> {
        const THICK: f32 = 3.0;
        let color = themed(self.cfg.theme.dim, if focused { 0.55 } else { 0.28 });

        let brush = dc.solid(color)?;

        let (first, visible, total) = down;
        let track = (r.bottom() - INSET - y0).max(0.0);
        if let Some((offset, length)) = metrics::thumb(track, first, visible, total) {
            let x = r.right() - INSET * 0.5 - THICK;
            dc.fill_rect(
                &Bounds {
                    left: x,
                    top: y0 + offset,
                    right: x + THICK,
                    bottom: y0 + offset + length,
                },
                &brush,
            );
        }

        // Sideways, when there is one. Starting at the text rather than at
        // the pane edge: the gutter never scrolls, so a bar under it would
        // claim ground that does not move.
        if let Some((x0, first, visible, total)) = across {
            let track = (r.right() - INSET - x0).max(0.0);
            if let Some((offset, length)) = metrics::thumb(track, first, visible, total) {
                let y = r.bottom() - INSET * 0.5 - THICK;
                dc.fill_rect(
                    &Bounds {
                        left: x0 + offset,
                        top: y,
                        right: x0 + offset + length,
                        bottom: y + THICK,
                    },
                    &brush,
                );
            }
        }
                Ok(())
    }

    /// The focus marker: an accent notch beside the pane's title.
    ///
    /// Third try. A border reads as noise on a translucent surface; the
    /// 26-pixel corner tick had to be searched for; the full-width top line
    /// glued itself to the caption on the top row of panes and read as
    /// window chrome. The notch sits where the eye already goes to ask
    /// "what is this pane" — beside the title — so the question and the
    /// answer share a corner, and its height matches the title row, so it
    /// belongs to the pane and not to the window.
    fn draw_focus_mark(&self, dc: &Canvas, r: Rect, focused: bool) -> Result<()> {
        if !focused {
            return Ok(());
        }
        let lh = self.text.line_height();
        let accent = dc.solid(themed(self.cfg.theme.accent, 0.9))?;
        dc.fill_rounded(
            &RoundedRect {
                rect: Bounds {
                    left: r.x + 2.0,
                    top: r.y + INSET,
                    right: r.x + 5.0,
                    bottom: r.y + INSET + lh,
                },
                radius_x: 1.5,
                radius_y: 1.5,
            },
            &accent,
        );
                Ok(())
    }

    /// Terminal cell grid.
    ///
    /// Unlike text, this draws **cells**: every one has its own background and
    /// glyphs must sit on the grid. Two passes — backgrounds, then text —
    /// because one pass would mean a DrawTextLayout per cell. Merging
    /// neighbours into runs is what keeps the call count down.
    fn draw_terminal(
        &mut self,
        dc: &Canvas,
        pane: PaneId,
        r: Rect,
        focused: bool,
    ) -> Result<()> {
        let Some(term) = self.content.get(&pane).and_then(Content::as_terminal) else {
            return Ok(());
        };
        let snap = term.snapshot();
        let (cw, ch) = self.text.cell_size();
        let ox = r.x + INSET;
        let oy = r.y + INSET;

        let theme = self.cfg.theme;
        let sel: kb_term::Rgb = theme.terminal.selection.into();
        let default_bg: kb_term::Rgb = theme.terminal.background.into();

        // 1) Background runs, skipping the default background so the
        //    acrylic shows through.
        for row in 0..snap.rows {
            let mut col = 0;
            while col < snap.cols {
                let Some(c) = snap.cell(col, row) else { break };
                let bg = cell_bg(c, sel);
                let mut run = 1;
                while col + run < snap.cols {
                    let Some(n) = snap.cell(col + run, row) else { break };
                    if cell_bg(n, sel) != bg {
                        break;
                    }
                    run += 1;
                }
                if bg != default_bg {
                    let brush = dc.solid(to_color(bg, 1.0))?;
                    dc.fill_rect(
                        &Bounds {
                            left: ox + col as f32 * cw,
                            top: oy + row as f32 * ch,
                            right: ox + (col + run) as f32 * cw,
                            bottom: oy + (row + 1) as f32 * ch,
                        },
                        &brush,
                    );
                }
                col += run;
            }
        }

        // 2) Cursor — before the text, so glyphs draw on top.
        if focused && snap.cursor_visible && snap.cursor.1 < snap.rows {
            let brush = dc.solid(themed(theme.terminal.cursor, 1.0))?;
            dc.fill_rect(
                &Bounds {
                    left: ox + snap.cursor.0 as f32 * cw,
                    top: oy + snap.cursor.1 as f32 * ch,
                    right: ox + (snap.cursor.0 + 1) as f32 * cw,
                    bottom: oy + (snap.cursor.1 + 1) as f32 * ch,
                },
                &brush,
            );
        }

        // 3) Text runs: consecutive same-colored cells in one call.
        let alpha = if focused { 1.0 } else { 0.72 };
        for row in 0..snap.rows {
            let mut col = 0;
            while col < snap.cols {
                let Some(c) = snap.cell(col, row) else { break };
                let fg = if c.inverse { c.bg } else { c.fg };
                let mut s = String::new();
                let mut run = 0;
                while col + run < snap.cols {
                    let Some(n) = snap.cell(col + run, row) else { break };
                    let nfg = if n.inverse { n.bg } else { n.fg };
                    if nfg != fg {
                        break;
                    }
                    s.push(if n.ch == '\0' { ' ' } else { n.ch });
                    run += 1;
                }
                if !s.trim().is_empty() {
                    let brush = dc.solid(to_color(fg, alpha))?;
                    let layout = self.text.line(&s)?;
                    dc.text(
                        Point { x: ox + col as f32 * cw, y: oy + row as f32 * ch },
                        &layout,
                        &brush,
                    );
                }
                col += run.max(1);
            }
        }

        // Being scrolled into history has to be visible, or the user misses
        // live output and thinks the terminal froze.
        if snap.scrolled_back {
            let brush = dc.solid(themed(theme.warning, 0.85))?;
            let badge = if self.glyphs.arrow { "⇡ history" } else { "history" };
            let x = (r.right() - INSET - self.text.width_of(badge)).max(r.x + INSET);
            let layout = self.text.volatile(badge)?;
            dc.text(
                Point { x, y: r.y + 6.0 },
                &layout,
                &brush,
            );
        }

        if let Some(code) = snap.exited {
            let brush = dc.solid(themed(theme.error, 0.9))?;
            let layout = self.text.volatile(&format!("[shell exited — code {code}]"))?;
            dc.text(
                Point { x: ox, y: oy + snap.rows as f32 * ch + 4.0 },
                &layout,
                &brush,
            );
        }
                self.draw_focus_mark(dc, r, focused)
    }

    /// File explorer.
    ///
    /// The visible rows are copied out before drawing: the renderer needs
    /// `&mut self` for the shaping cache, and holding a borrow of the tree
    /// across that would not compile. The copy is bounded by the pane height.
    fn draw_explorer(
        &mut self,
        dc: &Canvas,
        pane: PaneId,
        r: Rect,
        focused: bool,
    ) -> Result<()> {
        let lh = self.text.line_height();
        let visible = (((r.h - INSET * 2.0 - lh * 1.6) / lh).floor()).max(1.0) as usize;
        let y0 = r.y + INSET + lh * 1.6;

        let git = self.git.snapshot().clone();
        let (rows, selected, top, root, problem, total) = {
            let Some(Content::Explorer(e)) = self.content.get_mut(&pane) else {
                return Ok(());
            };
            e.ensure_visible(visible);
            let top = e.top;
            let total = e.tree.rows().len();
            let rows: Vec<Row> = e
                .tree
                .rows()
                .iter()
                .skip(top)
                .take(visible)
                .map(|row| Row {
                    name: row.name.clone(),
                    is_dir: row.is_dir,
                    depth: row.depth,
                    open: row.open,
                    git: git.status_of(&row.path),
                })
                .collect();
            (
                rows,
                e.tree.selected(),
                top,
                e.tree
                    .root()
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| e.tree.root().display().to_string()),
                e.tree.problem().map(str::to_owned),
                total,
            )
        };

        let theme = self.cfg.theme;
        let dim = dc.solid(themed(theme.dim, if focused { 1.0 } else { 0.5 }))?;
        let fg = dc.solid(themed(theme.fg, if focused { 0.95 } else { 0.62 }))?;
        let accent = dc.solid(themed(theme.accent, if focused { 1.0 } else { 0.6 }))?;

        // Same marker set as the rows below, so there is no way to draw a
        // glyph nobody checked for — and the same two spaces, or the
        // header's name sits a column left of every name under it.
        let mark = self.glyphs.icons.of(&root, true, true);
        let header = self.text.volatile(&format!("{mark}  {root}"))?;
        dc.text(
            Point { x: r.x + 10.0, y: r.y + INSET },
            &header,
            &dim,
        );

        if let Some(p) = problem {
            let err = dc.solid(themed(theme.error, 0.9))?;
            let layout = self.text.volatile(&p)?;
            dc.text(
                Point { x: r.x + 10.0, y: r.y + INSET + lh * 1.6 },
                &layout,
                &err,
            );
            return self.draw_focus_mark(dc, r, focused);
        }

        dc.push_clip(&Bounds { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() });

        for (i, row) in rows.iter().enumerate() {
            let y = y0 + i as f32 * lh;
            let is_selected = top + i == selected;

            if is_selected {
                // The selection stays marked when the pane loses focus,
                // just fainter: coming back and having lost your place is
                // worse than a little extra ink.
                let bg = dc.solid(themed(theme.accent, if focused { 0.22 } else { 0.10 }))?;
                dc.fill_rect(
                    &Bounds { left: r.x + 2.0, top: y, right: r.right() - 2.0, bottom: y + lh },
                    &bg,
                );
            }

            let indent = 10.0 + row.depth as f32 * 14.0;
            let glyph = self.glyphs.icons.of(&row.name, row.is_dir, row.open);
            let line = format!("{glyph}  {}", row.name);
            let layout = self.text.line(&line)?;

            // Git status wins over the plain file/directory color: the
            // point of the color is that changes stand out at a glance.
            let brush = match row.git {
                Some(s) => &dc.solid(themed(git_color(&theme.git, s), if focused { 1.0 } else { 0.6 }))?,
                None if row.is_dir => &accent,
                None => &fg,
            };
            dc.text(
                Point { x: r.x + indent, y },
                &layout,
                brush,
            );
        }
        dc.pop_clip();
                // Names are clipped at the pane edge rather than scrolled, so there is
        // nothing sideways to report.
        self.draw_scroll_marks(dc, r, y0, focused, (top, visible, total), None)?;
        self.draw_focus_mark(dc, r, focused)
    }

    /// Text editor: gutter, selection, cursor, text.
    ///
    /// Order matters. Selection is a filled rect behind the glyphs, the cursor
    /// goes under the text too — a caret drawn on top of a glyph hides the
    /// character you are about to type over.
    fn draw_editor(
        &mut self,
        dc: &Canvas,
        pane: PaneId,
        r: Rect,
        focused: bool,
    ) -> Result<()> {
        let lh = self.text.line_height();
        let (cw, _) = self.text.cell_size();
        let top_now = match self.content.get(&pane) {
            Some(Content::Editor(e)) => e.top,
            _ => 0,
        };
        let area = TextArea::new(r, lh, cw, top_now);
        let visible = area.visible;

        // Gutter marks, re-read when git has reported news since they were
        // built. Refreshed here rather than in the tick because drawing is
        // the only consumer: a pane nobody draws never runs a diff.
        let marks_stale = matches!(
            self.content.get(&pane),
            Some(Content::Editor(e)) if e.marks_at != Some(self.git_gen)
        );
        if marks_stale {
            let path = match self.content.get(&pane) {
                Some(Content::Editor(e)) => e.buffer.path().map(|p| p.to_path_buf()),
                _ => None,
            };
            let marks = path.map(|p| self.git.diff_marks(&p)).unwrap_or_default();
            if let Some(Content::Editor(e)) = self.content.get_mut(&pane) {
                e.marks = marks;
                e.marks_at = Some(self.git_gen);
            }
        }

        let syntax = self.syntax.clone();
        let vim_on = self.cfg.vim.enabled;
        // The last search, lit on every visible line while vim wants it.
        let hl = if vim_on { self.vim.highlight().cloned() } else { None };
        let (title, modified, status, lines, top, left, total, cursor, selection, brackets, spans, widest, marks, shape) = {
            let Some(Content::Editor(e)) = self.content.get_mut(&pane) else {
                return Ok(());
            };
            e.ensure_visible(visible, area.cols);
            e.sync_highlights(&syntax);
            let widest = e.widest();
            let b = &e.buffer;
            let title = b
                .path()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into());
            let lines: Vec<String> = b
                .lines()
                .iter()
                .skip(e.top)
                .take(visible)
                .cloned()
                .collect();
            let spans: Vec<Vec<kb_syn::Span>> = (e.top..e.top + visible)
                .map(|i| e.spans(i).to_vec())
                .collect();
            (
                title,
                b.modified(),
                e.status.clone(),
                lines,
                e.top,
                e.left,
                b.len(),
                b.cursor,
                // Visual mode's selection is vim's to describe; the buffer's
                // own anchor is only used with vim off.
                if vim_on { e.vim.selection(b) } else { b.selection() },
                b.matching_bracket(),
                spans,
                widest,
                e.marks.clone(),
                if vim_on { e.vim.cursor_shape() } else { kb_vim::CursorShape::Bar },
            )
        };

        let theme = self.cfg.theme;
        // Recomputed with the scrolled `top`: ensure_visible may have moved it.
        let area = TextArea::new(r, lh, cw, top);
        let (digits, text_x, y0) = (area.digits, area.text_x, area.y0);

        let dim = dc.solid(themed(theme.dim, if focused { 1.0 } else { 0.5 }))?;
        let fg = dc.solid(themed(theme.fg, if focused { 0.95 } else { 0.62 }))?;

        let header = format!(
            "{}{}   ·   {total} lines",
            title,
            if modified { " ●" } else { "" }
        );
        let layout = self.text.volatile(&header)?;
        dc.text(
            Point { x: r.x + 10.0, y: r.y + INSET },
            &layout,
            &dim,
        );

        if let Some(s) = &status {
            let brush = dc.solid(themed(if s.starts_with("save failed") { theme.error } else { theme.accent }, 0.9))?;
            // Measured, not guessed: a fixed offset clips the message
            // mid-word as soon as it is longer than the guess.
            let x = (r.right() - INSET - self.text.width_of(s)).max(r.x + INSET);
            let layout = self.text.volatile(s)?;
            dc.text(
                Point { x, y: r.y + INSET },
                &layout,
                &brush,
            );
        }

        dc.push_clip(&Bounds { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() });

        // Search matches, under the selection: what `n` will land on.
        if let Some(re) = &hl {
            let brush = dc.solid(themed(theme.warning, if focused { 0.22 } else { 0.12 }))?;
            for (i, line) in lines.iter().enumerate() {
                let chars: Vec<char> = line.chars().collect();
                let y = y0 + i as f32 * lh;
                for m in re.find_all(&chars) {
                    // An empty match has nothing to light.
                    if m.end <= m.start || m.end <= left {
                        continue;
                    }
                    dc.fill_rect(
                        &Bounds {
                            left: text_x + m.start.saturating_sub(left) as f32 * cw,
                            top: y,
                            right: text_x + (m.end - left) as f32 * cw,
                            bottom: y + lh,
                        },
                        &brush,
                    );
                }
            }
        }

        // Selection, per visible line. Columns are character counts, so
        // the x positions come from the cell width, not from measuring.
        if let Some((start, end)) = selection {
            let brush = dc.solid(themed(theme.accent, if focused { 0.28 } else { 0.14 }))?;
            for (i, line) in lines.iter().enumerate() {
                let ln = top + i;
                if ln < start.line || ln > end.line {
                    continue;
                }
                let len = line.chars().count();
                let from = if ln == start.line { start.col } else { 0 };
                // A selected line break is shown as one extra cell, so an
                // empty selected line is still visible.
                let to = if ln == end.line { end.col } else { len + 1 };
                if to <= left {
                    continue;
                }
                let y = y0 + i as f32 * lh;
                dc.fill_rect(
                    &Bounds {
                        left: text_x + from.saturating_sub(left) as f32 * cw,
                        top: y,
                        right: text_x + (to - left) as f32 * cw,
                        bottom: y + lh,
                    },
                    &brush,
                );
            }
        }

        // The bracket pair the caret is touching, tinted like a faint
        // selection. Only while the pane is focused: the highlight
        // follows the caret, and the caret is not drawn either.
        if let Some(pair) = brackets.filter(|_| focused) {
            let brush = dc.solid(themed(theme.accent, 0.30))?;
            for p in [pair.0, pair.1] {
                if p.line < top || p.line >= top + visible || p.col < left {
                    continue;
                }
                let x = text_x + (p.col - left) as f32 * cw;
                let y = y0 + (p.line - top) as f32 * lh;
                dc.fill_rect(
                    &Bounds { left: x, top: y, right: x + cw, bottom: y + lh },
                    &brush,
                );
            }
        }

        if focused && cursor.line >= top && cursor.line < top + visible && cursor.col >= left {
            let x = text_x + (cursor.col - left) as f32 * cw;
            let y = y0 + (cursor.line - top) as f32 * lh;
            // The shape is the mode: a bar between characters for
            // typing, a block on a character for normal mode, an
            // underline for replace mode. The block is translucent
            // because the glyph is drawn over it and has to stay
            // readable — a vim user reads the character under the
            // cursor more than any other.
            let (rect, alpha) = match shape {
                kb_vim::CursorShape::Bar => (Bounds { left: x, top: y, right: x + 2.0, bottom: y + lh }, 1.0),
                kb_vim::CursorShape::Block => (Bounds { left: x, top: y, right: x + cw, bottom: y + lh }, 0.55),
                kb_vim::CursorShape::Underline => {
                    (Bounds { left: x, top: y + lh - 2.0, right: x + cw, bottom: y + lh }, 1.0)
                }
            };
            let brush = dc.solid(themed(theme.terminal.cursor, alpha))?;
            dc.fill_rect(&rect, &brush);
        }

        for (i, line) in lines.iter().enumerate() {
            let y = y0 + i as f32 * lh;
            let num = self.text.volatile(&format!("{:>width$}", top + i + 1, width = digits))?;
            dc.text(
                Point { x: r.x + 10.0, y },
                &num,
                &dim,
            );

            // What changed since the last commit: a thin bar in the strip
            // left of the numbers, the same colours the file tree uses.
            // Hidden while the buffer is modified — unsaved edits shift
            // lines, and a mark on the wrong line is worse than none.
            if !modified {
                for (_, change) in marks.iter().filter(|(l, _)| *l == top + i) {
                    let brush = dc.solid(themed(
                            git_color(
                                &theme.git,
                                match change {
                                    kb_git::LineChange::Added => kb_git::Status::Added,
                                    kb_git::LineChange::Modified => kb_git::Status::Modified,
                                    kb_git::LineChange::Deleted => kb_git::Status::Deleted,
                                },
                            ),
                            if focused { 0.9 } else { 0.45 },
                        ))?;
                    let rect = if *change == kb_git::LineChange::Deleted {
                        // The vanished lines have no line to mark, so the
                        // boundary they left gets a short tick instead.
                        Bounds { left: r.x + 4.0, top: y - 1.0, right: r.x + 9.0, bottom: y + 1.5 }
                    } else {
                        Bounds { left: r.x + 4.0, top: y, right: r.x + 6.5, bottom: y + lh }
                    };
                    dc.fill_rect(&rect, &brush);
                }
            }

            if line.trim().is_empty() {
                continue;
            }

            let line_spans = spans.get(i).map(Vec::as_slice).unwrap_or(&[]);
            if line_spans.is_empty() {
                // No grammar, or nothing captured: one call for the line.
                let visible_text: String = line.chars().skip(left).collect();
                if visible_text.trim().is_empty() {
                    continue;
                }
                let layout = self.text.line(&visible_text)?;
                dc.text(
                    Point { x: text_x, y },
                    &layout,
                    &fg,
                );
                continue;
            }

            // Coloured runs, positioned by column. The font is monospace,
            // so a column is exactly one cell wide and each run can be
            // placed without measuring what came before it.
            let chars: Vec<char> = line.chars().collect();
            let mut col = 0usize;
            for span in line_spans {
                let start = span.start.min(chars.len());
                let end = span.end.min(chars.len());
                if start > col {
                    // Text between spans keeps the default colour.
                    let from = col.max(left);
                    let gap: String = chars[from.min(start)..start].iter().collect();
                    if !gap.trim().is_empty() {
                        let layout = self.text.line(&gap)?;
                        dc.text(
                            Point { x: text_x + (from - left) as f32 * cw, y },
                            &layout,
                            &fg,
                        );
                    }
                }
                if end > start.max(left) {
                    let start = start.max(left);
                    let run: String = chars[start..end].iter().collect();
                    if !run.trim().is_empty() {
                        let brush = dc.solid(themed(
                                syntax_color(&theme.syntax, span.kind),
                                if focused { 1.0 } else { 0.62 },
                            ))?;
                        let layout = self.text.line(&run)?;
                        dc.text(
                            Point { x: text_x + (start - left) as f32 * cw, y },
                            &layout,
                            &brush,
                        );
                    }
                }
                col = col.max(end);
            }
            if col < chars.len() {
                let from = col.max(left);
                let rest: String = chars[from.min(chars.len())..].iter().collect();
                if !rest.trim().is_empty() {
                    let layout = self.text.line(&rest)?;
                    dc.text(
                        Point { x: text_x + (from - left) as f32 * cw, y },
                        &layout,
                        &fg,
                    );
                }
            }
        }
        dc.pop_clip();
                self.draw_scroll_marks(
            dc,
            r,
            y0,
            focused,
            (top, visible, total),
            Some((text_x, left, area.cols, widest)),
        )?;
        self.draw_focus_mark(dc, r, focused)
    }

    /// Read-only file view with a line-number gutter.
    fn draw_viewer(
        &mut self,
        dc: &Canvas,
        pane: PaneId,
        r: Rect,
        focused: bool,
    ) -> Result<()> {
        let lh = self.text.line_height();
        let (cw, _) = self.text.cell_size();
        let visible = (((r.h - INSET * 2.0 - lh * 1.6) / lh).floor()).max(1.0) as usize;
        let y0 = r.y + INSET + lh * 1.6;

        let (title, note, lines, top, total) = {
            let Some(Content::Viewer(v)) = self.content.get(&pane) else {
                return Ok(());
            };
            let title = v
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| v.path.display().to_string());
            let lines: Vec<String> = v.lines.iter().skip(v.top).take(visible).cloned().collect();
            (title, v.note.clone(), lines, v.top, v.lines.len())
        };

        let theme = self.cfg.theme;
        // Gutter width from the widest line number actually shown, so a short
        // file doesn't pay for a long one's gutter.
        let digits = ((top + visible).max(1) as f64).log10().floor() as usize + 1;
        let gutter = (digits as f32 + 1.0) * cw;

        let dim = dc.solid(themed(theme.dim, if focused { 1.0 } else { 0.5 }))?;
        let fg = dc.solid(themed(theme.fg, if focused { 0.95 } else { 0.62 }))?;

        let header = self.text.volatile(&format!("{title}   ·   {total} lines"))?;
        dc.text(
            Point { x: r.x + 10.0, y: r.y + INSET },
            &header,
            &dim,
        );

        if let Some(n) = note {
            let warn = dc.solid(themed(theme.warning, 0.9))?;
            let x = (r.right() - INSET - self.text.width_of(&n)).max(r.x + INSET);
            let layout = self.text.volatile(&n)?;
            dc.text(
                Point { x, y: r.y + INSET },
                &layout,
                &warn,
            );
        }

        dc.push_clip(&Bounds { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() });
        for (i, line) in lines.iter().enumerate() {
            let y = y0 + i as f32 * lh;
            let num = self.text.volatile(&format!("{:>width$}", top + i + 1, width = digits))?;
            dc.text(
                Point { x: r.x + 10.0, y },
                &num,
                &dim,
            );
            if !line.trim().is_empty() {
                let layout = self.text.line(line)?;
                dc.text(
                    Point { x: r.x + 10.0 + gutter, y },
                    &layout,
                    &fg,
                );
            }
        }
        dc.pop_clip();
                // No sideways bar: the viewer does not scroll sideways, so one would
        // be furniture for a movement that cannot happen.
        self.draw_scroll_marks(dc, r, y0, focused, (top, visible, total), None)?;
        self.draw_focus_mark(dc, r, focused)
    }

    /// The settings screen.
    ///
    /// Two columns and a marker, which is what an options screen has been
    /// since it was a list on a CRT: the name on the left, the value on the
    /// right, and no doubt about which line the arrows will move. The value
    /// column is right-aligned to the pane rather than to the longest label,
    /// so it stays put while the list scrolls under it.
    fn draw_settings(
        &mut self,
        dc: &Canvas,
        pane: PaneId,
        r: Rect,
        focused: bool,
    ) -> Result<()> {
        use crate::content::Line;

        let lh = self.text.line_height();
        let visible = (((r.h - INSET * 2.0 - lh * 1.6) / lh).floor()).max(1.0) as usize;
        let y0 = r.y + INSET + lh * 1.6;

        let keys = self.cfg.keys.clone();
        let (selected, top, status) = {
            let Some(Content::Settings(s)) = self.content.get_mut(&pane) else {
                return Ok(());
            };
            s.ensure_visible(&keys, visible);
            (s.setting(), s.top, s.status.clone())
        };

        let lines = crate::content::settings_lines(&keys);
        let total = lines.len();
        let theme = self.cfg.theme;
        let cfg = self.cfg.clone();

        let dim = dc.solid(themed(theme.dim, if focused { 1.0 } else { 0.5 }))?;
        let fg = dc.solid(themed(theme.fg, if focused { 0.95 } else { 0.62 }))?;
        let accent = dc.solid(themed(theme.accent, if focused { 1.0 } else { 0.6 }))?;

        // Clipped from the header down, not just the rows: the hint is a
        // whole sentence and a narrow pane would spill it over the divider
        // into whatever is next door.
        dc.push_clip(&Bounds { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() });

        let title = "SETTINGS";
        let header = self.text.volatile(title)?;
        dc.text(
            Point { x: r.x + 10.0, y: r.y + INSET },
            &header,
            &accent,
        );

        // What the arrows do, and what writes the file. An options screen
        // that does not say how to leave it is a puzzle.
        // Esc comes first: a screen that takes over a pane and does not
        // say how to leave it is a trap, and that is worse than not
        // knowing what the arrows do.
        let hint = status.unwrap_or_else(|| {
            "Esc closes  \u{b7}  \u{2190}\u{2192} change  \u{b7}  \
             Ctrl+S writes config.toml (comments not kept)"
                .to_string()
        });
        // Clamped past the title rather than to the pane edge: in a narrow
        // pane a long message would otherwise be drawn straight over it.
        let after_title = r.x + 10.0 + self.text.width_of(title) + self.text.cell_size().0 * 2.0;
        let x = (r.right() - INSET - self.text.width_of(&hint)).max(after_title);
        let layout = self.text.volatile(&hint)?;
        dc.text(
            Point { x, y: r.y + INSET },
            &layout,
            &dim,
        );

        for (i, line) in lines.iter().skip(top).take(visible).enumerate() {
            let y = y0 + i as f32 * lh;
            match line {
                Line::Heading(title) => {
                    let layout = self.text.line(title)?;
                    dc.text(
                        Point { x: r.x + 12.0, y },
                        &layout,
                        &accent,
                    );
                }
                Line::Row(setting) => {
                    let is_selected = *setting == selected;
                    if is_selected {
                        let bg = dc.solid(themed(theme.accent, if focused { 0.22 } else { 0.10 }))?;
                        dc.fill_rect(
                            &Bounds {
                                left: r.x + 2.0,
                                top: y,
                                right: r.right() - 2.0,
                                bottom: y + lh,
                            },
                            &bg,
                        );
                    }

                    let label = format!(
                        "{} {}",
                        if is_selected { "\u{25b8}" } else { " " },
                        setting.label()
                    );
                    let layout = self.text.line(&label)?;
                    dc.text(
                        Point { x: r.x + 26.0, y },
                        &layout,
                        if is_selected { &fg } else { &dim });

                    // A switch that is on is the one thing worth colouring:
                    // it is the difference the eye is scanning for.
                    let value = setting.value(&cfg);
                    let brush = if value == "ON" { &accent } else if is_selected { &fg } else { &dim };
                    let vx = (r.right() - INSET - 4.0 - self.text.width_of(&value))
                        .max(r.x + 26.0);
                    let layout = self.text.line(&value)?;
                    dc.text(
                        Point { x: vx, y },
                        &layout,
                        brush,
                    );
                }
                Line::Control(action) => {
                    let layout = self.text.line(action.title())?;
                    dc.text(
                        Point { x: r.x + 26.0, y },
                        &layout,
                        &dim,
                    );
                    // Right-aligned in the same column as the values, so
                    // the whole screen reads as one two-column list.
                    let chord = match keys.binding_for(*action) {
                        Some(c) => c.to_string(),
                        None => continue,
                    };
                    let vx = (r.right() - INSET - 4.0 - self.text.width_of(&chord))
                        .max(r.x + 26.0);
                    let layout = self.text.line(&chord)?;
                    dc.text(
                        Point { x: vx, y },
                        &layout,
                        &fg,
                    );
                }
            }
        }
        dc.pop_clip();
                self.draw_scroll_marks(dc, r, y0, focused, (top, visible, total), None)?;
        self.draw_focus_mark(dc, r, focused)
    }

    /// The git panel: the changed files, one diff, or the log — whichever
    /// screen the panel is on, drawn with the same two-column discipline as
    /// the settings screen and the same colours as the file tree.
    fn draw_git(
        &mut self,
        dc: &Canvas,
        pane: PaneId,
        r: Rect,
        focused: bool,
    ) -> Result<()> {
        use crate::content::{git_lines, GitLine, GitView};

        fn letter(s: kb_git::Status) -> &'static str {
            match s {
                kb_git::Status::Modified => "M",
                kb_git::Status::Added => "A",
                kb_git::Status::Deleted => "D",
                kb_git::Status::Renamed => "R",
                kb_git::Status::Untracked => "?",
                // Loud on purpose, like the tree: it needs a decision.
                kb_git::Status::Conflicted => "!",
            }
        }

        let lh = self.text.line_height();
        let (cw, _) = self.text.cell_size();
        let visible = (((r.h - INSET * 2.0 - lh * 1.6) / lh).floor()).max(1.0) as usize;
        let y0 = r.y + INSET + lh * 1.6;

        let branch = self
            .git
            .snapshot()
            .branch
            .clone()
            .unwrap_or_else(|| "no branch".to_string());

        // Copied out per screen — and only the visible slice of the big
        // lists, because a long diff cloned whole on every frame is exactly
        // the per-frame cost this renderer keeps refusing to pay.
        enum Screen {
            Status { entries: Vec<kb_git::Entry>, selected: usize, top: usize },
            Diff { title: String, rows: Vec<(kb_git::DiffKind, String)>, top: usize, total: usize },
            Log { rows: Vec<kb_git::Commit>, selected: usize, top: usize, total: usize },
        }

        let (screen, status) = {
            let Some(Content::Git(g)) = self.content.get_mut(&pane) else {
                return Ok(());
            };
            g.ensure_visible(visible);
            let screen = match g.view {
                GitView::Status => Screen::Status {
                    entries: g.entries.clone(),
                    selected: g.selected,
                    top: g.top,
                },
                GitView::Diff => Screen::Diff {
                    title: g.diff_title.clone(),
                    rows: g.diff.iter().skip(g.diff_top).take(visible).cloned().collect(),
                    top: g.diff_top,
                    total: g.diff.len(),
                },
                GitView::Log => Screen::Log {
                    rows: g.commits.iter().skip(g.log_top).take(visible).cloned().collect(),
                    selected: g.log_selected,
                    top: g.log_top,
                    total: g.commits.len(),
                },
            };
            (screen, g.status.clone())
        };

        let theme = self.cfg.theme;
        let dim = dc.solid(themed(theme.dim, if focused { 1.0 } else { 0.5 }))?;
        let fg = dc.solid(themed(theme.fg, if focused { 0.95 } else { 0.62 }))?;
        let accent = dc.solid(themed(theme.accent, if focused { 1.0 } else { 0.6 }))?;

        dc.push_clip(&Bounds { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() });

        // The header names the screen, so Esc always has a visible answer
        // to "back out of what?".
        let title = match &screen {
            Screen::Status { .. } => format!("GIT \u{b7} {branch}"),
            Screen::Diff { title, .. } => format!("GIT \u{b7} {branch} \u{b7} {title}"),
            Screen::Log { .. } => format!("GIT \u{b7} {branch} \u{b7} log"),
        };
        let header = self.text.volatile(&title)?;
        dc.text(
            Point { x: r.x + 10.0, y: r.y + INSET },
            &header,
            &accent,
        );

        // The last action's result outranks the standing key hint, same
        // contract as the settings screen.
        let hint = status.unwrap_or_else(|| {
            match &screen {
                // R refresh and Shift+P pull exist too, but the hint has
                // to survive a half-width pane; these are the keys least
                // missed by whoever has not found them yet.
                Screen::Status { .. } => {
                    "Space stages \u{b7} Enter diff \u{b7} C commit \u{b7} \
                     X discards \u{b7} P pushes \u{b7} L log"
                }
                Screen::Diff { .. } => "\u{2191}\u{2193} scroll \u{b7} Esc back",
                Screen::Log { .. } => "Enter shows the commit \u{b7} Esc back",
            }
            .to_string()
        });
        let after_title = r.x + 10.0 + self.text.width_of(&title) + cw * 2.0;
        let hx = (r.right() - INSET - self.text.width_of(&hint)).max(after_title);
        let layout = self.text.volatile(&hint)?;
        dc.text(
            Point { x: hx, y: r.y + INSET },
            &layout,
            &dim,
        );

        let (top, total) = match &screen {
            Screen::Status { entries, selected, top } => {
                let lines = git_lines(entries);
                if entries.is_empty() {
                    let layout = self.text.volatile("working tree clean \u{b7} L shows the log")?;
                    dc.text(
                        Point { x: r.x + 12.0, y: y0 },
                        &layout,
                        &dim,
                    );
                }
                for (i, line) in lines.iter().skip(*top).take(visible).enumerate() {
                    let y = y0 + i as f32 * lh;
                    match line {
                        GitLine::Heading(t) => {
                            let layout = self.text.line(t)?;
                            dc.text(
                                Point { x: r.x + 12.0, y },
                                &layout,
                                &accent,
                            );
                        }
                        GitLine::Entry(idx) => {
                            let e = &entries[*idx];
                            let is_selected = idx == selected;
                            if is_selected {
                                let bg = dc.solid(themed(theme.accent, if focused { 0.22 } else { 0.10 }))?;
                                dc.fill_rect(
                                    &Bounds {
                                        left: r.x + 2.0,
                                        top: y,
                                        right: r.right() - 2.0,
                                        bottom: y + lh,
                                    },
                                    &bg,
                                );
                            }
                            // Letter and name both in the tree's colour
                            // for that state: the colour IS the
                            // information here, and a grey list with one
                            // tinted letter read as a grey list.
                            let mark = dc.solid(themed(git_color(&theme.git, e.status), if focused { 1.0 } else { 0.6 }))?;
                            let layout = self.text.line(letter(e.status))?;
                            dc.text(
                                Point { x: r.x + 26.0, y },
                                &layout,
                                &mark,
                            );
                            let layout = self.text.line(&e.rel)?;
                            dc.text(
                                Point { x: r.x + 26.0 + cw * 2.0, y },
                                &layout,
                                if is_selected { &fg } else { &mark });
                        }
                    }
                }
                (*top, lines.len())
            }
            Screen::Diff { rows, top, total, .. } => {
                for (i, (kind, text)) in rows.iter().enumerate() {
                    let y = y0 + i as f32 * lh;
                    let color = match kind {
                        kb_git::DiffKind::Add => themed(theme.git.added, if focused { 0.95 } else { 0.6 }),
                        kb_git::DiffKind::Del => themed(theme.git.deleted, if focused { 0.95 } else { 0.6 }),
                        kb_git::DiffKind::Hunk => themed(theme.accent, if focused { 0.9 } else { 0.55 }),
                        kb_git::DiffKind::Meta => themed(theme.dim, if focused { 0.8 } else { 0.4 }),
                        kb_git::DiffKind::Context => themed(theme.fg, if focused { 0.8 } else { 0.5 }),
                    };
                    if text.trim().is_empty() {
                        continue;
                    }
                    let brush = dc.solid(color)?;
                    let layout = self.text.line(text)?;
                    dc.text(
                        Point { x: r.x + 12.0, y },
                        &layout,
                        &brush,
                    );
                }
                (*top, *total)
            }
            Screen::Log { rows, selected, top, total } => {
                for (i, c) in rows.iter().enumerate() {
                    let y = y0 + i as f32 * lh;
                    let is_selected = top + i == *selected;
                    if is_selected {
                        let bg = dc.solid(themed(theme.accent, if focused { 0.22 } else { 0.10 }))?;
                        dc.fill_rect(
                            &Bounds {
                                left: r.x + 2.0,
                                top: y,
                                right: r.right() - 2.0,
                                bottom: y + lh,
                            },
                            &bg,
                        );
                    }
                    let layout = self.text.line(&c.hash)?;
                    dc.text(
                        Point { x: r.x + 12.0, y },
                        &layout,
                        &accent,
                    );
                    let subject_x = r.x + 12.0 + (c.hash.chars().count() as f32 + 2.0) * cw;
                    // Subjects in the text colour, not the furniture
                    // grey: they are the content of this screen.
                    let layout = self.text.line(&c.subject)?;
                    dc.text(
                        Point { x: subject_x, y },
                        &layout,
                        &fg,
                    );
                    // Age and author on the right, clamped clear of the
                    // subject in a narrow pane.
                    let meta = format!("{} \u{b7} {}", c.when, c.author);
                    let mx = (r.right() - INSET - self.text.width_of(&meta))
                        .max(subject_x + self.text.width_of(&c.subject) + cw * 2.0);
                    let layout = self.text.volatile(&meta)?;
                    dc.text(
                        Point { x: mx, y },
                        &layout,
                        &dim,
                    );
                }
                (*top, *total)
            }
        };

        dc.pop_clip();
        self.draw_scroll_marks(dc, r, y0, focused, (top, visible, total), None)?;
                self.draw_focus_mark(dc, r, focused)
    }

    /// The agent pane: a header, the wrapped transcript, and the box at the
    /// bottom. The rows come already wrapped from the pane, so this draws
    /// lines the way every other list does — where a line breaks is the
    /// pane's business, and the one thing the renderer knows that it does
    /// not is how many columns there are.
    fn draw_agent(
        &mut self,
        dc: &Canvas,
        pane: PaneId,
        r: Rect,
        focused: bool,
    ) -> Result<()> {
        use crate::agent::{self, Row as AgentRow, Style};

        let lh = self.text.line_height();
        let (cw, _) = self.text.cell_size();
        let visible = agent::visible_rows(r.h, lh);
        let y0 = r.y + INSET + lh * 1.6;
        let x0 = r.x + 12.0;
        let cols = (((r.right() - INSET - x0) / cw).floor()).max(8.0) as usize;

        let (header, hint, rows, top, total, input, busy, ask, completions) = {
            let Some(Content::Agent(a)) = self.content.get_mut(&pane) else {
                return Ok(());
            };
            let total = a.rows(cols).len();
            a.ensure_visible(visible);
            let top = a.top;
            let rows: Vec<AgentRow> = a.rows(cols).iter().skip(top).take(visible).cloned().collect();
            // The frame needs six columns of its own around the line.
            let ask = a
                .ask_text(cols.saturating_sub(6))
                .map(|(title, line)| (title, line, a.answers(), a.pick()));
            let completions = (a.completions(), a.pick_cmd());
            (a.header(), a.hint(), rows, top, total, a.input().to_string(), a.busy(), ask, completions)
        };
        let (header, hint, rows, top, total, input, busy, ask, (completions, pick_cmd)) =
            (header, hint, rows, top, total, input, busy, ask, completions);

        let theme = self.cfg.theme;
        let fade = if focused { 1.0 } else { 0.6 };
        let dim = dc.solid(themed(theme.dim, if focused { 1.0 } else { 0.5 }))?;
        let faint = dc.solid(themed(theme.dim, if focused { 0.65 } else { 0.35 }))?;
        let fg = dc.solid(themed(theme.fg, if focused { 0.95 } else { 0.62 }))?;
        let accent = dc.solid(themed(theme.accent, fade))?;
        let bad = dc.solid(themed(theme.git.deleted, fade))?;
        let warn = dc.solid(themed(theme.warning, fade))?;

        dc.push_clip(&Bounds { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() });

        let layout = self.text.volatile(&header)?;
        dc.text(
            Point { x: r.x + 10.0, y: r.y + INSET },
            &layout,
            &accent,
        );
        let after_title = r.x + 10.0 + self.text.width_of(&header) + cw * 2.0;
        let hx = (r.right() - INSET - self.text.width_of(&hint)).max(after_title);
        let layout = self.text.volatile(&hint)?;
        dc.text(
            Point { x: hx, y: r.y + INSET },
            &layout,
            &dim,
        );

        // The transcript. Streaming rows go through the uncached path:
        // every frame of a growing line is a new string, and the line
        // cache never forgets one.
        for (i, row) in rows.iter().enumerate() {
            if row.text.trim().is_empty() {
                continue;
            }
            let y = y0 + i as f32 * lh;
            let brush = match row.style {
                Style::User => &accent,
                Style::Text => &fg,
                Style::Tool => &dim,
                Style::ToolResult | Style::Note => &faint,
                Style::ToolError => &bad,
                Style::Error => &warn,
            };
            let layout = if row.volatile {
                self.text.volatile(&row.text)?
            } else {
                self.text.line(&row.text)?
            };
            dc.text(
                Point { x: x0, y },
                &layout,
                brush,
            );
        }

        // The box: a hairline, then the prompt and whatever is typed.
        // Long input shows its tail, which is where the caret is.
        let y_in = r.bottom() - INSET - lh;
        let rule = dc.solid(themed(theme.divider, 1.0))?;
        dc.fill_rect(
            &Bounds { left: r.x + 6.0, top: y_in - 4.0, right: r.right() - 6.0, bottom: y_in - 3.0 },
            &rule,
        );
        let prompt = "\u{203a} ";
        let layout = self.text.line(prompt)?;
        dc.text(
            Point { x: x0, y: y_in },
            &layout,
            &accent,
        );
        let text_x = x0 + self.text.width_of(prompt);
        let fits = cols.saturating_sub(3).max(1);
        let shown: String = if input.chars().count() > fits {
            input.chars().rev().take(fits).collect::<Vec<_>>().into_iter().rev().collect()
        } else {
            input.clone()
        };
        if shown.is_empty() {
            let placeholder = if busy { "working\u{2026}" } else { "ask Claude" };
            let layout = self.text.line(placeholder)?;
            dc.text(
                Point { x: text_x, y: y_in },
                &layout,
                &faint,
            );
        } else {
            let layout = self.text.volatile(&shown)?;
            dc.text(
                Point { x: text_x, y: y_in },
                &layout,
                &fg,
            );
        }
        if focused && self.caret_on() && ask.is_none() {
            let cx = text_x + self.text.advance_of(&shown);
            dc.fill_rect(
                &Bounds { left: cx, top: y_in + 2.0, right: cx + 2.0, bottom: y_in + lh - 2.0 },
                &accent,
            );
        }

        // The completion list while `/` is being typed: a short list
        // above the box, the pick highlighted the way the palette's
        // is, the description in the furniture grey beside the name.
        if !completions.is_empty() && ask.is_none() {
            const MAX_SHOWN: usize = 8;
            let shown: Vec<_> = completions.iter().take(MAX_SHOWN).collect();
            let name_w = shown.iter().map(|c| c.name.chars().count()).max().unwrap_or(0) + 1;
            let y_list = y_in - 6.0 - shown.len() as f32 * lh;
            let bg = dc.solid(overlay(theme.overlay))?;
            dc.fill_rect(
                &Bounds { left: r.x + 6.0, top: y_list - 2.0, right: r.right() - 6.0, bottom: y_in - 4.0 },
                &bg,
            );
            let picked = dc.solid(themed(theme.accent, if focused { 0.22 } else { 0.10 }))?;
            for (i, c) in shown.iter().enumerate() {
                let y = y_list + i as f32 * lh;
                if i == pick_cmd {
                    dc.fill_rect(
                        &Bounds { left: r.x + 6.0, top: y, right: r.right() - 6.0, bottom: y + lh },
                        &picked,
                    );
                }
                let layout = self.text.line(&format!("/{}", c.name))?;
                dc.text(
                    Point { x: x0, y },
                    &layout,
                    if i == pick_cmd { &fg } else { &accent });
                if !c.description.is_empty() {
                    let room = cols.saturating_sub(name_w + 4);
                    let desc: String = c.description.chars().take(room).collect();
                    let layout = self.text.line(&desc)?;
                    dc.text(
                        Point { x: x0 + (name_w + 2) as f32 * cw, y },
                        &layout,
                        &faint,
                    );
                }
            }
        }

        // A question sits in its own frame just above the box, inside
        // this pane and nowhere else: it is this conversation's to
        // answer, and only while this pane has the keyboard.
        if let Some((title, line, answers, pick)) = ask {
            let y_box = y_in - 6.0 - CHOICE_ROWS as f32 * lh;
            let answers: Vec<String> = answers.iter().map(|a| a.to_string()).collect();
            self.draw_choice_box(dc, (r.x + r.w * 0.5, y_box), &title, &line, &answers, pick)?;
        }

        dc.pop_clip();
        self.draw_scroll_marks(dc, r, y0, focused, (top, visible, total), None)?;
                self.draw_focus_mark(dc, r, focused)
    }

    /// The welcome screen: a quiet wordmark, the keys that matter, and the
    /// places already worked in. Centred as one block, because it is the
    /// only thing on screen and a corner would make it look like debris.
    fn draw_welcome(
        &mut self,
        dc: &Canvas,
        pane: PaneId,
        r: Rect,
        focused: bool,
    ) -> Result<()> {
        let lh = self.text.line_height();
        let (cw, _) = self.text.cell_size();

        let (rows, selected) = {
            let Some(Content::Welcome(w)) = self.content.get(&pane) else {
                return Ok(());
            };
            (w.rows.clone(), w.selected)
        };

        // Read from the keymap, like the empty pane's hints: after a rebind
        // a fixed list would advertise a key that does nothing. Same list as
        // the empty pane's, from the one place that holds it.
        let hints: Vec<(String, &str)> = content::STARTER_KEYS
            .iter()
            .filter_map(|(action, label)| {
                Some((self.cfg.keys.binding_for(*action)?.to_string(), *label))
            })
            .collect();

        // One column width for everything, so the block reads as one thing.
        let chord_w = hints.iter().map(|(c, _)| c.chars().count()).max().unwrap_or(0);
        let widest = rows
            .iter()
            .map(|(label, _)| label.chars().count() + 2)
            .chain(hints.iter().map(|(_, l)| chord_w + 3 + l.chars().count()))
            .max()
            .unwrap_or(24)
            .max(24);

        // The watermark: kubIDE's K in block art, barely there — the trick
        // every editor's empty window pulls, done in the materials a
        // character grid actually owns. ANSI-shadow style where the font
        // can draw it, plain hashes where it cannot.
        let watermark: &[&str] = if self.glyphs.blocks && self.glyphs.boxes {
            &[
                "\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2557}",
                "\u{2588}\u{2588}\u{2551} \u{2588}\u{2588}\u{2554}\u{255d}",
                "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d} ",
                "\u{2588}\u{2588}\u{2554}\u{2550}\u{2588}\u{2588}\u{2557} ",
                "\u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2557}",
                "\u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d}",
            ]
        } else {
            &["##  ##", "## ## ", "####  ", "## ## ", "##  ##"]
        };

        let block_w = widest as f32 * cw;
        let x = r.x + ((r.w - block_w) * 0.5).max(INSET);
        // The block hangs just above centre; dead centre reads as sinking.
        let total_rows = watermark.len() + 4 + hints.len()
            + if rows.is_empty() { 0 } else { rows.len() + 2 };
        let mut y = r.y + ((r.h - total_rows as f32 * lh) * 0.38).max(INSET);

        let theme = self.cfg.theme;
        dc.push_clip(&Bounds { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() });

        // Fainter than the wordmark under it: a watermark that competes
        // with the content is a poster, not a watermark.
        //
        // Drawn in two passes, body then shadow, because that is what the
        // style is: the box-drawing characters are a shadow cast by the
        // blocks. Given one flat alpha they weigh the same as the letter,
        // and a hairline stroke beside a solid block at 11% reads as a
        // smudged K rather than a shaded one.
        let ghost = dc.solid(themed(theme.fg, if focused { 0.11 } else { 0.06 }))?;
        let shade = dc.solid(themed(theme.fg, if focused { 0.05 } else { 0.03 }))?;
        // One left edge for every row, taken from the character grid
        // rather than each row's measured width: rows hold different
        // numbers of blocks and box characters, and centring them one by
        // one lets the shape drift a fraction of a cell per line.
        let cols = watermark.iter().map(|row| row.chars().count()).max().unwrap_or(0);
        let mark_x = r.x + (r.w - cols as f32 * cw) * 0.5;
        let is_body = |c: char| c == '\u{2588}' || c == '#';
        for row in watermark {
            for (brush, keep) in [(&ghost, true), (&shade, false)] {
                let part: String =
                    row.chars().map(|c| if is_body(c) == keep { c } else { ' ' }).collect();
                if part.trim().is_empty() {
                    continue;
                }
                let layout = self.text.line(&part)?;
                dc.text(
                    Point { x: mark_x, y },
                    &layout,
                    brush,
                );
            }
            y += lh;
        }
        y += lh;

        // The wordmark: the name, spaced out and barely there. A logo
        // drawn any louder would be the only decoration in the program.
        let faint = dc.solid(themed(theme.fg, if focused { 0.30 } else { 0.18 }))?;
        // The capitals carry the joke: an IDE named kub-something.
        let mark = "k u b I D E";
        let mark_w = self.text.width_of(mark);
        let layout = self.text.volatile(mark)?;
        dc.text(
            Point { x: r.x + (r.w - mark_w) * 0.5, y },
            &layout,
            &faint,
        );
        y += lh * 2.0;

        let dim = dc.solid(themed(theme.dim, if focused { 0.9 } else { 0.5 }))?;
        let fg = dc.solid(themed(theme.fg, if focused { 0.92 } else { 0.6 }))?;
        let accent = dc.solid(themed(theme.accent, if focused { 0.95 } else { 0.55 }))?;

        for (chord, label) in &hints {
            let layout = self.text.line(&format!("{chord:>chord_w$}"))?;
            dc.text(Point { x, y }, &layout, &accent);
            let layout = self.text.line(label)?;
            dc.text(
                Point { x: x + (chord_w as f32 + 3.0) * cw, y },
                &layout,
                &dim,
            );
            y += lh;
        }

        if !rows.is_empty() {
            y += lh;
            let layout = self.text.volatile("OPEN \u{b7} Enter")?;
            dc.text(Point { x, y }, &layout, &accent);
            y += lh;

            for (i, (label, _)) in rows.iter().enumerate() {
                let is_selected = i == selected;
                if is_selected {
                    let bg = dc.solid(themed(theme.accent, if focused { 0.22 } else { 0.10 }))?;
                    dc.fill_rounded(
                        &RoundedRect {
                            rect: Bounds {
                                left: x - 8.0,
                                top: y,
                                right: x + block_w + 8.0,
                                bottom: y + lh,
                            },
                            radius_x: 5.0,
                            radius_y: 5.0,
                        },
                        &bg,
                    );
                }
                let marker = if is_selected { "\u{25b8} " } else { "  " };
                let layout = self.text.line(&format!("{marker}{label}"))?;
                dc.text(
                    Point { x, y },
                    &layout,
                    if is_selected { &fg } else { &dim });
                y += lh;
            }
        }
        dc.pop_clip();
                self.draw_focus_mark(dc, r, focused)
    }

    /// An empty pane. Says what to press instead of sitting there blank.
    fn draw_placeholder(
        &mut self,
        dc: &Canvas,
        pane: PaneId,
        r: Rect,
        focused: bool,
    ) -> Result<()> {
        let theme = self.cfg.theme;
        let lh = self.text.line_height();

        // Read from the keymap rather than hardcoded: after a rebind a fixed
        // list would be telling the user to press a key that does nothing.
        // The same starter keys the welcome screen shows, then the two that
        // only make sense inside a pane, with a blank line between the group
        // that gets you anywhere and the group that is about this box.
        let line = |chord: &kb_cfg::Chord, label: &str| format!("{:<14} {label}", chord.to_string());
        let hints: Vec<String> = content::STARTER_KEYS
            .iter()
            .filter_map(|(action, label)| Some(line(&self.cfg.keys.binding_for(*action)?, label)))
            .chain(std::iter::once(String::new()))
            .chain(content::PANE_KEYS.iter().filter_map(|(action, label)| {
                Some(line(&self.cfg.keys.binding_for(*action)?, label))
            }))
            .collect();
        let dim = dc.solid(themed(theme.dim, if focused { 0.9 } else { 0.45 }))?;
        let label = self.text.volatile(&format!("pane {}", pane.0))?;
        dc.text(
            Point { x: r.x + 10.0, y: r.y + INSET },
            &label,
            &dim,
        );

        dc.push_clip(&Bounds { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() });
        let y0 = r.y + INSET + lh * 2.0;
        for (i, h) in hints.iter().enumerate() {
            let layout = self.text.line(h.as_str())?;
            dc.text(
                Point { x: r.x + 10.0, y: y0 + i as f32 * lh },
                &layout,
                &dim,
            );
        }
        dc.pop_clip();
                self.draw_focus_mark(dc, r, focused)
    }

    /// The overlay, drawn over everything but the title bar.
    ///
    /// Centred and fixed-width rather than filling the window: a list of file
    /// names does not get more readable at 2000 pixels wide, and a box that
    /// covers everything makes it unclear what you are about to act on.
    /// A question, as a bordered box with its answers side by side.
    ///
    /// Not the list the other overlays use. A list is for picking one of many
    /// and reads downwards; this is one decision with two or three ways out,
    /// and laying them across is what every settings program did when the
    /// screen was a character grid — which is also what this renderer is.
    ///
    /// Composed as text lines rather than rectangles, because the font is
    /// monospace and a cell grid is what the border characters were drawn for.
    fn draw_choice(&mut self, dc: &Canvas, w: f32, h: f32) -> Result<()> {
        let Some(p) = &self.palette else { return Ok(()) };
        let title = p.label.clone().unwrap_or_default();
        let question = p.question.clone().unwrap_or_default();
        let answers: Vec<String> = p.answers().to_vec();
        let selected = p.selected;

        let lh = self.text.line_height();
        let y = ((h - CHOICE_ROWS as f32 * lh) * 0.4).max(self.cfg.window.caption_height);
        self.draw_choice_box(dc, (w * 0.5, y), &title, &question, &answers, selected)?;
        // Nothing to hit-test: a question is answered with the arrows and
        // Enter, so the click handler must not think it has rows here.
        self.palette_rows = None;
        Ok(())
    }

    /// A question in a frame: title, one line, the answers side by side,
    /// centred on `cx` with its top at `y`. One drawing for the overlay
    /// and for a pane that asks on its own behalf, so the two can never
    /// look like different kinds of question.
    fn draw_choice_box(
        &mut self,
        dc: &Canvas,
        (cx, y): (f32, f32),
        title: &str,
        question: &str,
        answers: &[String],
        selected: usize,
    ) -> Result<()> {
        // `[ Save ]  [ Discard ]`, and where each one starts, so the highlight
        // lands on a button rather than near it.
        let mut buttons = String::new();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for (i, answer) in answers.iter().enumerate() {
            if i > 0 {
                buttons.push_str("  ");
            }
            let start = buttons.chars().count();
            let label = format!("[ {answer} ]");
            let len = label.chars().count();
            buttons.push_str(&label);
            spans.push((start, len));
        }

        let inner = question
            .chars()
            .count()
            .max(buttons.chars().count())
            .max(title.chars().count())
            + 4;

        // Fall back to ASCII where the font has no border characters. Every
        // font that ships with Windows has them, but the family is chosen from
        // a list and the list is the user's.
        let b: [&str; 8] = if self.glyphs.boxes {
            ["\u{2554}", "\u{2557}", "\u{255a}", "\u{255d}", "\u{2550}", "\u{2551}", "\u{2560}", "\u{2563}"]
        } else {
            ["+", "+", "+", "+", "-", "|", "+", "+"]
        };
        let rule = b[4].repeat(inner);
        let centred = |text: &str| {
            let pad = inner.saturating_sub(text.chars().count());
            format!("{}{text}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2))
        };

        let lines = [
            format!("{}{rule}{}", b[0], b[1]),
            format!("{}{}{}", b[5], centred(title), b[5]),
            format!("{}{rule}{}", b[6], b[7]),
            format!("{}{}{}", b[5], " ".repeat(inner), b[5]),
            format!("{}{}{}", b[5], centred(question), b[5]),
            format!("{}{}{}", b[5], " ".repeat(inner), b[5]),
            format!("{}{}{}", b[5], centred(&buttons), b[5]),
            format!("{}{}{}", b[5], " ".repeat(inner), b[5]),
            format!("{}{rule}{}", b[2], b[3]),
        ];
        debug_assert_eq!(lines.len(), CHOICE_ROWS);

        let lh = self.text.line_height();
        let (cw, _) = self.text.cell_size();
        let box_w = (inner + 2) as f32 * cw;
        let x = (cx - box_w * 0.5).max(0.0);
        // Where the buttons line starts, for putting the highlight on it.
        let buttons_y = y + 6.0 * lh;
        let buttons_x = x + cw + ((inner - buttons.chars().count()) / 2) as f32 * cw;

        let theme = self.cfg.theme;
        let bg = dc.solid(overlay(theme.overlay))?;
        let edge = dc.solid(themed(theme.accent, 0.85))?;
        let fg = dc.solid(themed(theme.fg, 1.0))?;
        let picked = dc.solid(themed(theme.accent, 1.0))?;
        let on_picked = dc.solid(overlay(theme.overlay))?;

        dc.fill_rect(
            &Bounds {
                left: x,
                top: y,
                right: x + box_w,
                bottom: y + lines.len() as f32 * lh,
            },
            &bg,
        );

        for (i, line) in lines.iter().enumerate() {
            // The frame in the accent colour, the words in the text
            // colour: the border is furniture and should not read as loud
            // as the question.
            let brush = if i == 0 || i == 2 || i == lines.len() - 1 { &edge } else { &fg };
            let layout = self.text.line(line)?;
            dc.text(
                Point { x, y: y + i as f32 * lh },
                &layout,
                brush,
            );
        }

        // The chosen answer, drawn over the top of itself in reverse. A
        // filled block is what a character grid has instead of a raised
        // button, and it is unmistakable at a glance.
        if let Some((start, len)) = spans.get(selected).copied() {
            let bx = buttons_x + start as f32 * cw;
            dc.fill_rect(
                &Bounds {
                    left: bx,
                    top: buttons_y,
                    right: bx + len as f32 * cw,
                    bottom: buttons_y + lh,
                },
                &picked,
            );
            let label = format!("[ {} ]", answers[selected]);
            let layout = self.text.line(&label)?;
            dc.text(
                Point { x: bx, y: buttons_y },
                &layout,
                &on_picked,
            );
        }
                Ok(())
    }

    /// The folder picker: Explorer's open dialog, stroke for stroke — the
    /// navigation corner, the boxed address bar, the search box, a details
    /// list under column headings, places down the left, the field-and-two-
    /// buttons footer — in the editor's own material and colours.
    fn draw_folder_picker(&mut self, dc: &Canvas, w: f32, h: f32) -> Result<()> {
        if self.folder_picker.is_none() {
            self.picker_hits = None;
            return Ok(());
        }

        let lh = self.text.line_height();
        let (cell_w, _) = self.text.cell_size();
        // Everything clickable lights up under the mouse. The highlight and
        // the hand cursor make the same promise from two directions; a row
        // that answers to a click but sits inert under the pointer reads as
        // furniture.
        let (mx, my) = self.mouse;
        let width = (w * 0.7).clamp(560.0, 1040.0).min(w - 24.0);
        let height = (h * 0.66).clamp(340.0, 680.0).min(h - 48.0);
        let x = (w - width) * 0.5;
        let y = self.cfg.window.caption_height + ((h - height) * 0.35).max(16.0);
        const PAD: f32 = 14.0;

        // The reference dialog's four bands, top to bottom: toolbar,
        // headings, the listing, the footer.
        let toolbar_h = lh * 2.0;
        let header_y = y + toolbar_h;
        let body_y = header_y + lh * 1.3;
        let footer_h = lh * 2.2;
        let rail_w = (width * 0.22).clamp(130.0, 200.0);
        let list_x = x + rail_w + PAD;
        let body_h = y + height - footer_h - body_y;
        let rows_wanted = (body_h / lh).floor().max(1.0) as usize;

        if let Some(p) = &mut self.folder_picker {
            p.ensure_visible(rows_wanted);
        }
        let Some(p) = &self.folder_picker else { return Ok(()) };

        let theme = self.cfg.theme;
        let crumbs = p.crumbs();
        let rows = p.rows(rows_wanted);
        let more = p.len().saturating_sub(p.top + rows.len());
        let selected_row = p.selected_row();
        let chosen = p.chosen();
        let filter = p.filter.clone();
        let address = p.address.clone();
        let select_all = p.select_all;
        let caret_on = self.caret_on();
        // The rail is labelled as one list rather than three: a `Documents` in
        // quick access and a different `Documents` in the recents is the same
        // coin toss as two `release`s, and only a whole-rail view can see it.
        let (quick, recents, drives) = {
            let all: Vec<std::path::PathBuf> = p
                .quick
                .iter()
                .chain(&p.recents)
                .chain(&p.drives)
                .cloned()
                .collect();
            let mut labels = kb_fs::distinct_labels(&all).into_iter();
            let mut take = |group: &[std::path::PathBuf]| -> Vec<(std::path::PathBuf, String)> {
                group
                    .iter()
                    .map(|path| (path.clone(), labels.next().unwrap_or_default()))
                    .collect()
            };
            (take(&p.quick), take(&p.recents), take(&p.drives))
        };
        let current = p.dir.clone();
        let dir_label = crumbs.last().map(|c| c.name.clone()).unwrap_or_default();
        let (can_back, can_fwd, can_up) =
            (p.can_back(), p.can_forward(), p.dir.parent().is_some());
        let icons = self.glyphs.icons;

        // The same material as the palette: translucent over the
        // acrylic, hairline edge, nothing louder than the content.
        let bg = dc.solid(overlay(theme.overlay))?;
        let edge = dc.solid(themed(theme.divider, 1.6))?;
        let fg = dc.solid(themed(theme.fg, 1.0))?;
        let dim = dc.solid(themed(theme.dim, 1.0))?;
        let faint = dc.solid(themed(theme.dim, 0.55))?;
        let hit = dc.solid(themed(theme.accent, 1.0))?;

        let rect = RoundedRect {
            rect: Bounds { left: x, top: y, right: x + width, bottom: y + height },
            radius_x: 10.0,
            radius_y: 10.0,
        };
        dc.fill_rounded(&rect, &bg);
        dc.stroke_rounded(&rect, &edge, 1.0);
        dc.push_clip(&Bounds { left: x, top: y, right: x + width, bottom: y + height });

        // A boxed control, the visual grammar every band below reuses.
        let boxed = |bx: f32, by: f32, bw: f32, bh: f32| -> Result<()> {
            let r = RoundedRect {
                rect: Bounds { left: bx, top: by, right: bx + bw, bottom: by + bh },
                radius_x: 6.0,
                radius_y: 6.0,
            };
            dc.stroke_rounded(&r, &edge, 1.0);
            Ok(())
        };

        // ── Toolbar: back, forward, up, then the address bar, then the
        // search box — the reference dialog's top row, control for
        // control.
        let btn = lh * 1.5;
        let btn_y = y + (toolbar_h - btn) * 0.5;
        let text_y = btn_y + (btn - lh) * 0.5;
        let arrows: [(&str, bool); 3] = if self.glyphs.arrow {
            [("\u{2190}", can_back), ("\u{2192}", can_fwd), ("\u{2191}", can_up)]
        } else {
            [("<", can_back), (">", can_fwd), ("^", can_up)]
        };
        let mut bx = x + PAD;
        let mut nav_rects = [kb_ui::Rect::default(); 3];
        for (i, (glyph, alive)) in arrows.iter().enumerate() {
            nav_rects[i] = kb_ui::Rect::new(bx, btn_y, btn, btn);
            if *alive && nav_rects[i].contains(mx, my) {
                let hover = dc.solid(themed(theme.fg, 0.08))?;
                dc.fill_rounded(
                    &RoundedRect {
                        rect: Bounds { left: bx, top: btn_y, right: bx + btn, bottom: btn_y + btn },
                        radius_x: 5.0,
                        radius_y: 5.0,
                    },
                    &hover,
                );
            }
            let gw = self.text.width_of(glyph);
            let layout = self.text.volatile(glyph)?;
            dc.text(
                Point { x: bx + (btn - gw) * 0.5, y: text_y },
                &layout,
                if *alive { &dim } else { &faint });
            bx += btn + 6.0;
        }

        let search_w = (width * 0.24).clamp(150.0, 240.0);
        let search_x = x + width - PAD - search_w;
        let addr_x = bx + PAD * 0.5;
        let addr_w = search_x - PAD - addr_x;
        boxed(addr_x, btn_y, addr_w, btn)?;
        boxed(search_x, btn_y, search_w, btn)?;

        let mut crumb_hits: Vec<(f32, f32, std::path::PathBuf)> = Vec::new();
        if let Some(addr) = &address {
            // The bar in its editable state: the raw path with a caret,
            // crumbs gone until Enter or Escape. Cut from the left when
            // it overflows — the end of a path is the part being typed.
            let avail = addr_w - PAD * 1.4 - 4.0;
            let mut shown: String = addr.clone();
            while self.text.width_of(&shown) > avail && shown.chars().count() > 1 {
                shown.remove(0);
            }
            let tx = addr_x + PAD * 0.7;
            let tw = self.text.width_of(&shown);
            // Selected whole: the highlight behind it says the next
            // keystroke replaces the path rather than extending it.
            if select_all && !shown.is_empty() {
                let sel = dc.solid(themed(theme.accent, 0.30))?;
                dc.fill_rect(
                    &Bounds {
                        left: tx - 1.0,
                        top: text_y,
                        right: tx + tw + 1.0,
                        bottom: text_y + lh,
                    },
                    &sel,
                );
            }
            let layout = self.text.volatile(&shown)?;
            dc.text(
                Point { x: tx, y: text_y },
                &layout,
                &fg,
            );
            if caret_on {
                let caret_x = tx + tw + 1.0;
                let caret = dc.solid(themed(theme.accent, 0.95))?;
                dc.fill_rect(
                    &Bounds {
                        left: caret_x,
                        top: text_y,
                        right: caret_x + 2.0,
                        bottom: text_y + lh,
                    },
                    &caret,
                );
            }
        } else {
            // The path as one clickable segment per directory, current
            // one in full colour. Cut from the left when it runs long:
            // the segments that matter are where you stand.
            let sep = if icons == kb_fs::Icons::Ascii { " > " } else { " \u{203a} " };
            let sep_w = self.text.width_of(sep);
            let mut widths: Vec<f32> =
                crumbs.iter().map(|c| self.text.width_of(&c.name)).collect();
            let mut start = 0;
            while widths.iter().sum::<f32>()
                + sep_w * widths.len().saturating_sub(1) as f32
                > addr_w - PAD * 2.0
                && widths.len() > 1
            {
                widths.remove(0);
                start += 1;
            }
            let mut cx = addr_x + PAD * 0.7;
            for (i, crumb) in crumbs.iter().enumerate().skip(start) {
                let last = i + 1 == crumbs.len();
                let cw = self.text.width_of(&crumb.name);
                let layout = self.text.volatile(&crumb.name)?;
                dc.text(
                    Point { x: cx, y: text_y },
                    &layout,
                    if last { &fg } else { &dim });
                crumb_hits.push((cx, cx + cw, crumb.dir.clone()));
                cx += cw;
                if !last {
                    let layout = self.text.volatile(sep)?;
                    dc.text(Point { x: cx, y: text_y }, &layout, &faint);
                    cx += sep_w;
                }
            }
        }

        // The search box narrows the listing, which is exactly what the
        // reference dialog's does; typing goes here, nowhere else.
        let (search_text, searching) = if filter.is_empty() {
            (format!("search {dir_label}"), false)
        } else {
            (filter.clone(), true)
        };
        let stx = search_x + PAD * 0.7;
        if select_all && address.is_none() && searching {
            let sel = dc.solid(themed(theme.accent, 0.30))?;
            dc.fill_rect(
                &Bounds {
                    left: stx - 1.0,
                    top: text_y,
                    right: stx + self.text.width_of(&search_text) + 1.0,
                    bottom: text_y + lh,
                },
                &sel,
            );
        }
        // The placeholder stands aside for the caret rather than sitting
        // under it; typed text starts at the box's edge as usual.
        let placeholder_gap = if searching { 0.0 } else { 7.0 };
        let layout = self.text.volatile(&search_text)?;
        dc.text(
            Point { x: stx + placeholder_gap, y: text_y },
            &layout,
            if searching { &fg } else { &faint });
        // A blinking caret here even when nothing has been typed: this
        // box takes every keystroke the dialog does not claim, and a
        // grey word alone reads as a label rather than as an invitation.
        // It steps aside while the address bar is the one being typed in
        // — two carets would be two claims on the same keyboard.
        if caret_on && address.is_none() {
            let caret_x = if searching {
                stx + self.text.width_of(&search_text) + 1.0
            } else {
                stx
            };
            let caret = dc.solid(themed(theme.accent, 0.95))?;
            dc.fill_rect(
                &Bounds {
                    left: caret_x,
                    top: text_y,
                    right: caret_x + 2.0,
                    bottom: text_y + lh,
                },
                &caret,
            );
        }
        dc.fill_rect(
            &Bounds { left: x, top: y + toolbar_h, right: x + width, bottom: y + toolbar_h + 1.0 },
            &edge,
        );

        // ── The rail: quick access, recents, drives, each group under
        // a faint label — the reference dialog's rail, in its order:
        // the home folders first, then places chosen, then places that
        // merely exist.
        let places_y0 = header_y + lh * 0.3;
        let place_bottom = y + height - footer_h;
        let mut place_hits: Vec<Option<std::path::PathBuf>> = Vec::new();
        let mut py = places_y0;
        let mut draw_group = |title: &str,
                              group: &[(std::path::PathBuf, String)],
                              place_hits: &mut Vec<Option<std::path::PathBuf>>,
                              py: &mut f32|
         -> Result<()> {
            if group.is_empty() || *py + lh > place_bottom {
                return Ok(());
            }
            let layout = self.text.volatile(title)?;
            dc.text(Point { x: x + PAD, y: *py }, &layout, &faint);
            place_hits.push(None);
            *py += lh;
            for (place, name) in group {
                if *py + lh > place_bottom {
                    break;
                }
                let row_rect = Bounds { left: x + PAD * 0.4, top: *py, right: x + rail_w - PAD * 0.4, bottom: *py + lh };
                let hovered =
                    mx >= row_rect.left && mx < row_rect.right && my >= *py && my < *py + lh;
                if *place == current || hovered {
                    let a = if *place == current { 0.18 } else { 0.10 };
                    let sel = dc.solid(themed(theme.accent, a))?;
                    dc.fill_rounded(
                        &RoundedRect { rect: row_rect, radius_x: 5.0, radius_y: 5.0 },
                        &sel,
                    );
                }
                let layout = self.text.line(name)?;
                dc.text(
                    Point { x: x + PAD * 1.6, y: *py },
                    &layout,
                    if *place == current { &fg } else { &dim });
                place_hits.push(Some(place.clone()));
                *py += lh;
            }
            Ok(())
        };
        draw_group("quick access", &quick, &mut place_hits, &mut py)?;
        draw_group("recent", &recents, &mut place_hits, &mut py)?;
        draw_group("this pc", &drives, &mut place_hits, &mut py)?;
        dc.fill_rect(
            &Bounds { left: x + rail_w, top: y + toolbar_h, right: x + rail_w + 1.0, bottom: place_bottom },
            &edge,
        );

        // ── Column headings, and the widths the rows under them share.
        // Columns give way from the right when the box gets narrow: the
        // name is the one that can never go.
        let area_w = x + width - PAD - list_x;
        let stamp_w = 17.0 * cell_w;
        let kind_w = 8.0 * cell_w;
        let size_w = 9.0 * cell_w;
        let gap = cell_w * 2.0;
        let show_size = area_w > 34.0 * cell_w + stamp_w;
        let show_kind = area_w > 26.0 * cell_w + stamp_w;
        let show_stamp = area_w > 24.0 * cell_w;
        let mut right = x + width - PAD;
        let size_x = { right -= if show_size { size_w + gap } else { 0.0 }; right + gap };
        let kind_x = { right -= if show_kind { kind_w + gap } else { 0.0 }; right + gap };
        let stamp_x = { right -= if show_stamp { stamp_w + gap } else { 0.0 }; right + gap };
        let name_w = right - list_x;

        let head_y = header_y + lh * 0.15;
        for (label, cx, on) in [
            ("Name", list_x, true),
            ("Modified", stamp_x, show_stamp),
            ("Type", kind_x, show_kind),
            ("Size", size_x, show_size),
        ] {
            if on {
                let layout = self.text.volatile(label)?;
                dc.text(Point { x: cx, y: head_y }, &layout, &faint);
            }
        }
        dc.fill_rect(
            &Bounds { left: list_x - PAD * 0.5, top: body_y - lh * 0.15, right: x + width - PAD * 0.5, bottom: body_y - lh * 0.15 + 1.0 },
            &edge,
        );

        // ── The listing.
        let list_y0 = body_y;
        if rows.is_empty() {
            let text = if filter.is_empty() { "this folder is empty" } else { "no match" };
            let layout = self.text.volatile(text)?;
            dc.text(Point { x: list_x, y: list_y0 }, &layout, &dim);
        }
        // Room for the icon and its two spaces, then the name.
        let name_chars = ((name_w / cell_w) as usize).saturating_sub(4);
        for (i, (row, hits)) in rows.iter().enumerate() {
            let ry = list_y0 + i as f32 * lh;
            let picked = Some(i) == selected_row;
            let row_rect = Bounds { left: list_x - PAD * 0.5, top: ry, right: x + width - PAD * 0.5, bottom: ry + lh };
            let hovered =
                mx >= row_rect.left && mx < row_rect.right && my >= ry && my < ry + lh;
            if picked || hovered {
                let a = if picked { 0.18 } else { 0.10 };
                let sel = dc.solid(themed(theme.accent, a))?;
                dc.fill_rounded(
                    &RoundedRect { rect: row_rect, radius_x: 5.0, radius_y: 5.0 },
                    &sel,
                );
            }
            // Folders in text colour, files a shade back: both are rows,
            // but this dialog's answer is usually a folder.
            let ink = if picked {
                &fg
            } else if row.is_dir {
                &dim
            } else {
                &faint
            };
            let glyph = icons.of(&row.name, row.is_dir, false);
            let mut name: String = row.name.chars().take(name_chars).collect();
            if name.len() < row.name.len() {
                name.pop();
                name.push('\u{2026}');
            }
            let layout = self.text.line(&format!("{glyph}  {name}"))?;
            dc.text(Point { x: list_x, y: ry }, &layout, ink);
            // The matched letters, overdrawn in accent — the palette's
            // trick, shifted past the icon and its two spaces.
            for pos in hits {
                if *pos >= name_chars {
                    break;
                }
                let Some(ch) = row.name.chars().nth(*pos) else { continue };
                let one = self.text.volatile(&ch.to_string())?;
                dc.text(
                    Point { x: list_x + (3 + pos) as f32 * cell_w, y: ry },
                    &one,
                    &hit,
                );
            }
            for (text, cx, on) in [
                (&row.modified, stamp_x, show_stamp),
                (&row.kind, kind_x, show_kind),
            ] {
                if on && !text.is_empty() {
                    let layout = self.text.volatile(text)?;
                    dc.text(Point { x: cx, y: ry }, &layout, &faint);
                }
            }
            if show_size && !row.size.is_empty() {
                // Right-aligned, like every size column ever printed.
                let sx = size_x + size_w - self.text.width_of(&row.size);
                let layout = self.text.volatile(&row.size)?;
                dc.text(Point { x: sx, y: ry }, &layout, &faint);
            }
        }
        if more > 0 {
            let text = format!("\u{2026} {more} more — type to narrow");
            let layout = self.text.volatile(&text)?;
            dc.text(
                Point { x: list_x, y: list_y0 + rows.len() as f32 * lh },
                &layout,
                &faint,
            );
        }

        // ── Footer: the field on the left, the two verbs on the right —
        // the reference dialog's bottom row. The field reports rather
        // than edits: typing already has one home, the search box.
        let fy = y + height - footer_h;
        dc.fill_rect(
            &Bounds { left: x, top: fy, right: x + width, bottom: fy + 1.0 },
            &edge,
        );
        let bh = lh * 1.5;
        let by = fy + (footer_h - bh) * 0.5;
        let bty = by + (bh - lh) * 0.5;
        let open_label = "Select folder";
        let cancel_label = "Cancel";
        let open_w = self.text.width_of(open_label) + PAD * 2.0;
        let cancel_w = self.text.width_of(cancel_label) + PAD * 2.0;
        let cancel_x = x + width - PAD - cancel_w;
        let open_x = cancel_x - PAD * 0.7 - open_w;

        let field_label = "Folder:";
        let flw = self.text.width_of(field_label);
        let layout = self.text.volatile(field_label)?;
        dc.text(Point { x: x + PAD, y: bty }, &layout, &dim);
        let field_x = x + PAD + flw + PAD * 0.7;
        let field_w = open_x - PAD - field_x;
        boxed(field_x, by, field_w, bh)?;
        // The field names exactly what the button will answer with — a
        // drive root has no file name of its own, so it borrows the
        // crumb's label.
        let field_text = chosen
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(dir_label);
        let layout = self.text.volatile(&field_text)?;
        dc.text(Point { x: field_x + PAD * 0.7, y: bty }, &layout, &fg);

        let over_open =
            mx >= open_x && mx < open_x + open_w && my >= by && my < by + bh;
        let accent_fill =
            dc.solid(themed(theme.accent, if over_open { 0.34 } else { 0.22 }))?;
        let open_rect = RoundedRect {
            rect: Bounds { left: open_x, top: by, right: open_x + open_w, bottom: by + bh },
            radius_x: 6.0,
            radius_y: 6.0,
        };
        dc.fill_rounded(&open_rect, &accent_fill);
        dc.stroke_rounded(&open_rect, &hit, 1.0);
        let layout = self.text.volatile(open_label)?;
        dc.text(Point { x: open_x + PAD, y: bty }, &layout, &fg);
        if mx >= cancel_x && mx < cancel_x + cancel_w && my >= by && my < by + bh {
            let hover = dc.solid(themed(theme.fg, 0.08))?;
            dc.fill_rounded(
                &RoundedRect {
                    rect: Bounds { left: cancel_x, top: by, right: cancel_x + cancel_w, bottom: by + bh },
                    radius_x: 6.0,
                    radius_y: 6.0,
                },
                &hover,
            );
        }
        boxed(cancel_x, by, cancel_w, bh)?;
        let layout = self.text.volatile(cancel_label)?;
        dc.text(Point { x: cancel_x + PAD, y: bty }, &layout, &dim);

        dc.pop_clip();

        self.picker_hits = Some(crate::PickerHits {
            panel: kb_ui::Rect::new(x, y, width, height),
            crumb_y: (btn_y, btn_y + btn),
            crumbs: crumb_hits,
            addr_x: (addr_x, addr_x + addr_w),
            places_y0,
            places_x: (x, x + rail_w),
            places: place_hits,
            list_y0,
            list_x: (list_x - PAD * 0.5, x + width),
            list_count: rows.len(),
            line_h: lh,
            visible: rows_wanted,
            back_btn: nav_rects[0],
            fwd_btn: nav_rects[1],
            up_btn: nav_rects[2],
            open_btn: kb_ui::Rect::new(open_x, by, open_w, bh),
            cancel_btn: kb_ui::Rect::new(cancel_x, by, cancel_w, bh),
        });
                Ok(())
    }

    fn draw_palette(&mut self, dc: &Canvas, w: f32, h: f32) -> Result<()> {
        match self.palette.as_ref().map(|p| p.mode) {
            None => {
                self.palette_rows = None;
                return Ok(());
            }
            // A question is a box, not a list.
            Some(crate::palette::Mode::Choice) => return self.draw_choice(dc, w, h),
            Some(_) => {}
        }

        let lh = self.text.line_height();
        let (cw, _) = self.text.cell_size();
        let width = (w * 0.6).clamp(360.0, 900.0);
        let x = (w - width) * 0.5;
        let y = self.cfg.window.caption_height + 24.0;
        let rows_wanted = ((h * 0.5) / lh).floor().max(1.0) as usize;

        // Scrolled before it is measured: how many rows fit is decided here,
        // and the list has to follow the selection rather than the selection
        // being limited to the first screenful.
        if let Some(p) = &mut self.palette {
            p.ensure_visible(rows_wanted);
        }
        let Some(p) = &self.palette else { return Ok(()) };

        let rows = p.rows(rows_wanted);
        // Said out loud when the list runs past the box, or a list that stops
        // at the edge reads as a list that ends there.
        let more = p.len().saturating_sub(p.top + rows.len());
        let scrolled_past = p.top;
        // One more line when the list runs past the box, for the count.
        let body = if rows.is_empty() { 1 } else { rows.len() + usize::from(more > 0) };
        let height = lh * (body as f32 + 2.8);

        let theme = self.cfg.theme;
        let note = p.note.clone();
        let selected = p.selected;
        let blinking = self.caret_on();

        // Translucent, not a slab. The window is acrylic and the whole
        // look rests on things sitting *in* that material; an opaque panel
        // over a blurred backdrop reads as a dialog bolted onto something
        // else.
        let bg = dc.solid(overlay(theme.overlay))?;
        // A hairline, the same one the pane dividers use. An accent-blue
        // outline makes the frame the loudest thing on screen, which is
        // backwards — the list is what matters, not its edge.
        let edge = dc.solid(themed(theme.divider, 1.6))?;
        let fg = dc.solid(themed(theme.fg, 1.0))?;
        let dim = dc.solid(themed(theme.dim, 1.0))?;
        let faint = dc.solid(themed(theme.dim, 0.55))?;
        let hit = dc.solid(themed(theme.accent, 1.0))?;

        let rect = RoundedRect {
            rect: Bounds { left: x, top: y, right: x + width, bottom: y + height },
            radius_x: 10.0,
            radius_y: 10.0,
        };
        dc.fill_rounded(&rect, &bg);
        dc.stroke_rounded(&rect, &edge, 1.0);

        // Everything inside stays inside: a long path or a wide query
        // would otherwise run straight over the rounded edge and out of
        // the box.
        dc.push_clip(&Bounds { left: x, top: y, right: x + width, bottom: y + height });

        // The label is furniture and the query is the answer, so they are
        // not the same colour. One string in one weight makes the prompt
        // compete with what was typed into it.
        // The gap after the label is a measured two cells, not two spaces:
        // DirectWrite drops trailing whitespace from a layout's width, so
        // padding the string would draw a gap the query then starts inside.
        let label = p.label.as_deref().unwrap_or_else(|| p.mode.prompt()).to_string();
        let label_w = self.text.width_of(&label) + self.text.cell_size().0 * 2.0;
        let layout = self.text.volatile(&label)?;
        dc.text(
            Point { x: x + PAD, y: y + lh * 0.5 },
            &layout,
            &faint,
        );
        let query = p.query.clone();
        let layout = self.text.volatile(&query)?;
        dc.text(
            Point { x: x + PAD + label_w, y: y + lh * 0.5 },
            &layout,
            &fg,
        );

        // A caret after the query, so an empty box still looks like
        // input — blinking, because a bar that never moves reads as a
        // rule someone drew rather than as a box waiting for a word.
        if blinking {
            let caret_x = x + PAD + label_w + self.text.width_of(&query) + 1.0;
            dc.fill_rect(
                &Bounds {
                    left: caret_x,
                    top: y + lh * 0.55,
                    right: caret_x + 2.0,
                    bottom: y + lh * 1.45,
                },
                &hit,
            );
        }

        // Hairline under the input: it separates what you type from what
        // the typing produced, which is the one distinction in the box.
        dc.fill_rect(
            &Bounds {
                left: x + PAD,
                top: y + lh * 1.8,
                right: x + width - PAD,
                bottom: y + lh * 1.8 + 1.0,
            },
            &edge,
        );

        let y0 = y + lh * 2.15;
        // Recorded for hit-testing: drawing decides the geometry, so
        // letting the mouse work it out separately would drift.
        self.palette_rows = Some(crate::PaletteRows {
            x,
            width,
            y0,
            line_h: lh,
            count: rows.len(),
        });

        if let Some(n) = note {
            let layout = self.text.volatile(&n)?;
            dc.text(
                Point { x: x + PAD, y: y0 },
                &layout,
                &dim,
            );
            dc.pop_clip();
            return Ok(());
        }

        for (i, row) in rows.iter().enumerate() {
            let ry = y0 + i as f32 * lh;
            if i == selected {
                // Rounded and inset to match the box it sits in. A
                // full-bleed bar with square ends belongs to a different
                // decade of user interface.
                let sel = dc.solid(themed(theme.accent, 0.18))?;
                dc.fill_rounded(
                    &RoundedRect {
                        rect: Bounds {
                            left: x + PAD * 0.5,
                            top: ry,
                            right: x + width - PAD * 0.5,
                            bottom: ry + lh,
                        },
                        radius_x: 5.0,
                        radius_y: 5.0,
                    },
                    &sel,
                );
            }
            let layout = self.text.line(&row.text)?;
            dc.text(
                Point { x: x + PAD, y: ry },
                &layout,
                if i == selected { &fg } else { &dim });

            // The matched characters, redrawn on top in the accent colour.
            // Underlining would need per-range formatting on the layout;
            // overdrawing single characters is exact and costs nothing at
            // this list size.
            for pos in &row.hits {
                let Some(ch) = row.text.chars().nth(*pos) else { continue };
                let one = self.text.volatile(&ch.to_string())?;
                dc.text(
                    Point { x: x + PAD + *pos as f32 * cw, y: ry },
                    &one,
                    &hit,
                );
            }

            // The chord, right-aligned into a column of its own — the
            // same two-column read as the settings screen. Fainter than
            // the names, because the list is scanned by name and the
            // chord is the answer once the name is found; the selected
            // row's chord takes the accent, since that is the one about
            // to matter.
            if let Some(d) = &row.detail {
                let dx = x + width - PAD - self.text.width_of(d);
                let layout = self.text.volatile(d)?;
                dc.text(
                    Point { x: dx, y: ry },
                    &layout,
                    if i == selected { &hit } else { &faint });
            }
        }

        // How much is off the bottom, and how much is already behind you.
        // Without it the box reads as the whole answer rather than as a
        // window onto one.
        if more > 0 {
            let note = if scrolled_past > 0 {
                format!("{scrolled_past} above  \u{b7}  {more} below")
            } else {
                format!("{more} more below")
            };
            let layout = self.text.volatile(&note)?;
            dc.text(
                Point { x: x + 14.0, y: y0 + rows.len() as f32 * lh },
                &layout,
                &dim,
            );
        }
        dc.pop_clip();
                Ok(())
    }

    /// The shortcut list pinned in the bottom right.
    ///
    /// Not the command palette and not the settings screen: both of those are
    /// somewhere you go, and the thing you actually want when you have
    /// forgotten a chord is to be told without leaving what you were doing.
    ///
    /// Bottom right because that corner is the emptiest — the caret works down
    /// from the top left, and the status bar already owns the bottom left.
    fn draw_help(&mut self, dc: &Canvas, w: f32, h: f32) -> Result<()> {
        let keys = self.cfg.keys.clone();

        // Closed, this is one quiet line on the status bar rather than nothing
        // at all. It used to be two rounded chips floating above the corner,
        // and they read as controls — dressed-up buttons over the code, the
        // loudest thing in a window built out of translucency. The reminder
        // belongs with the other readouts: same baseline, same grey, and the
        // bar's separator between the two halves. Still clickable, because
        // the first thing anyone does with a label naming a key is press it.
        if !self.help_open {
            self.corner_chips.clear();
            if !self.cfg.help.visible {
                return Ok(());
            }

            let lh = self.text.line_height();
            let y = h - lh - 2.0;

            // The status bar's own grey, a step fainter: this is read
            // once and then lived next to. Only the command list now —
            // settings has its own button at the other end of the bar.
            let fg = dc.solid(themed(self.cfg.theme.dim, 0.7))?;
            if let Some(chord) = keys.binding_for(kb_cfg::Action::Commands) {
                let text = format!("{chord} commands");
                let tw = self.text.width_of(&text);
                let x = w - self.cfg.window.padding - tw;
                let layout = self.text.volatile(&text)?;
                dc.text(
                    Point { x, y },
                    &layout,
                    &fg,
                );
                // Recorded here because drawing is what decides where it
                // ended up; working the position out again for the mouse
                // is how the two drift apart.
                self.corner_chips
                    .push((Rect::new(x - 4.0, y - 2.0, tw + 8.0, lh + 4.0), kb_cfg::Action::Commands));
            }
                        return Ok(());
        }
        self.corner_chips.clear();

        let mut rows: Vec<(String, &str)> = CHEAT_SHEET
            .iter()
            .filter_map(|(action, stands_for, label)| {
                let chord = if stands_for.is_empty() {
                    keys.binding_for(*action)?.to_string()
                } else {
                    // A line standing in for a family: nine numbered jumps
                    // spelled out would be a third of the list and none of the
                    // interest. Still keyed off a real action, so a family
                    // nobody has bound disappears with it.
                    keys.binding_for(*action)?;
                    (*stands_for).to_string()
                };
                Some((chord, *label))
            })
            .collect();
        if rows.is_empty() {
            return Ok(());
        }

        let lh = self.text.line_height();
        let (cw, _) = self.text.cell_size();

        // Cut to what the window can hold. Without this a short window gets a
        // list taller than itself, drawn off the top edge and over everything
        // — which is exactly what a panel that is on by default must not do.
        let room = h - lh - 12.0 - self.cfg.window.caption_height - 12.0;
        let fits = ((room / lh).floor() as usize).saturating_sub(1);
        let dropped = rows.len().saturating_sub(fits);
        if dropped > 0 {
            rows.truncate(fits.saturating_sub(1));
            // Said out loud rather than silently shortened: a list that stops
            // early with no sign of it teaches you the rest do not exist.
            rows.push((String::new(), "…and more, in the settings screen"));
        }
        if rows.is_empty() {
            return Ok(());
        }

        // Sized to the widest line rather than a guess, or the longest label
        // is the one that gets clipped.
        let widest = rows
            .iter()
            .map(|(chord, label)| chord.chars().count() + label.chars().count() + 3)
            .max()
            .unwrap_or(20);
        let pad = 12.0;
        let box_w = (widest as f32 * cw + pad * 2.0).min(w - self.cfg.window.padding * 2.0);
        let box_h = (rows.len() as f32 + 1.0) * lh + pad;
        // Above the status bar, not over it.
        let x = w - self.cfg.window.padding - box_w;
        let y = h - lh - 6.0 - box_h - 6.0;

        let theme = self.cfg.theme;
        let bg = dc.solid(overlay(theme.overlay))?;
        let edge = dc.solid(themed(theme.accent, 0.4))?;
        let key = dc.solid(themed(theme.accent, 0.95))?;
        let label = dc.solid(themed(theme.fg, 0.85))?;
        let dim = dc.solid(themed(theme.dim, 1.0))?;

        let rect = RoundedRect {
            rect: Bounds { left: x, top: y, right: x + box_w, bottom: y + box_h },
            radius_x: 8.0,
            radius_y: 8.0,
        };
        dc.fill_rounded(&rect, &bg);
        dc.stroke_rounded(&rect, &edge, 1.0);

        dc.push_clip(&Bounds { left: x, top: y, right: x + box_w, bottom: y + box_h });

        let title = match keys.binding_for(kb_cfg::Action::ToggleHelp) {
            Some(c) => format!("SHORTCUTS \u{b7} {c} hides this"),
            None => "SHORTCUTS".to_string(),
        };
        let layout = self.text.volatile(&title)?;
        dc.text(
            Point { x: x + pad, y: y + pad * 0.5 },
            &layout,
            &dim,
        );

        for (i, (chord, text)) in rows.iter().enumerate() {
            let row_y = y + pad * 0.5 + (i as f32 + 1.0) * lh;
            let layout = self.text.line(chord)?;
            dc.text(
                Point { x: x + pad, y: row_y },
                &layout,
                &key,
            );
            // The labels start in one column so the eye can run down them,
            // which is the only reason the width was measured.
            let layout = self.text.line(text)?;
            dc.text(
                Point { x: x + box_w - pad - text.chars().count() as f32 * cw, y: row_y },
                &layout,
                &label,
            );
        }
        dc.pop_clip();
                Ok(())
    }

    /// The idle status line, assembled from whatever is switched on.
    ///
    /// Built as a list of parts rather than one format string, so turning a
    /// segment off cannot leave a stray separator behind.
    fn status_line(&self) -> String {
        let on = self.cfg.status;
        let mut parts: Vec<String> = Vec::new();

        // Vim first, where vim puts it: the mode, the keys typed so far of
        // an unfinished command, and the register a macro is going into. A
        // command line being typed takes the whole bar, as in vim, because
        // it is the one thing on it the user is looking at.
        if let Some(v) = self.focused_vim() {
            if let Some(line) = v.cmdline() {
                return line;
            }
            let mut mode = match v.mode() {
                kb_vim::Mode::Normal => "NORMAL".to_string(),
                m => format!("-- {} --", m.label()),
            };
            let pending = v.pending();
            if !pending.is_empty() {
                mode.push_str(&format!("  {pending}"));
            }
            if let Some(r) = self.vim.recording() {
                mode.push_str(&format!("  recording @{r}"));
            }
            parts.push(mode);
        }

        if on.cursor {
            if let Some(Content::Editor(e)) = self.content.get(&self.focus) {
                let c = e.buffer.cursor;
                let selected = if self.cfg.vim.enabled {
                    e.vim.selection_text(&e.buffer)
                } else {
                    e.buffer.selected_text()
                };
                let sel = selected.map(|s| format!("  ({} selected)", s.chars().count())).unwrap_or_default();
                parts.push(format!("Ln {}, Col {}{sel}", c.line + 1, c.col + 1));
            }
        }
        if on.font {
            parts.push(format!("{} {:.0}px", self.text.family(), self.text.size()));
        }
        if on.panes {
            parts.push(format!("{} panes", self.layout.panes.len()));
        }
        if on.frame_time {
            parts.push(format!("{:.2} ms", self.frame_ms));
        }
        if on.git {
            let git = self.git.snapshot();
            if let Some(b) = &git.branch {
                // A notdef box in front of the branch name is worse than no
                // symbol at all: the name already says it is a branch.
                let mark = if self.glyphs.branch { "\u{e0a0} " } else { "" };
                parts.push(if git.files.is_empty() {
                    format!("{mark}{b}")
                } else {
                    format!("{mark}{b} ~{}", git.files.len())
                });
            }
        }
        if on.pomodoro {
            // The clock is Font Awesome, so it stands or falls with the icons.
            // No substitute: "work 24:13" already says what it is.
            let mark = if self.glyphs.icons == kb_fs::Icons::Nerd { "\u{f017} " } else { "" };
            parts.push(format!("{mark}{}", self.timer.label()));
        }
        if on.clock {
            parts.push(clock(on.clock_24h));
        }
        parts.join("   \u{b7}   ")
    }

    fn draw_status(&mut self, dc: &Canvas, h: f32, chrome: &Chrome) -> Result<()> {
        let pad = self.cfg.window.padding;
        let lh = self.text.line_height();

        // The settings button, leftmost on the bar. Drawn with lines like
        // the caption icons — a gear glyph would stand or fall with the
        // font, and the way into the options must not depend on them.
        let btn = Rect::new(pad - 4.0, h - lh - 4.0, lh + 8.0, lh + 4.0);
        self.settings_btn = Some(btn);
        if self.settings_hover || self.settings_pressed {
            // Hover lights the ground, pressing dims it — the same
            // two-state grammar as the caption buttons above.
            let a = if self.settings_pressed { 0.20 } else { 0.10 };
            let bg = dc.solid(themed(self.cfg.theme.fg, a))?;
            dc.fill_rounded(
                &RoundedRect {
                    rect: Bounds {
                        left: btn.x,
                        top: btn.y,
                        right: btn.right(),
                        bottom: btn.bottom(),
                    },
                    radius_x: 6.0,
                    radius_y: 6.0,
                },
                &bg,
            );
        }
        // Three sliders: the oldest picture of "adjust things" there is.
        let ia = match (chrome.active, self.settings_hover || self.settings_pressed) {
            (_, true) => 0.95,
            (true, false) => 0.65,
            (false, false) => 0.40,
        };
        let icon = dc.solid(themed(self.cfg.theme.dim, ia))?;
        let cx = btn.x + btn.w * 0.5;
        let cy = btn.y + btn.h * 0.5;
        let half = 5.0;
        // Knobs offset left, right, centre — straight rows would read
        // as a menu icon, and this is not a menu.
        for (i, knob) in [-2.5f32, 2.5, 0.0].iter().enumerate() {
            let ly = cy + (i as f32 - 1.0) * 4.0;
            dc.line(
                Point { x: cx - half, y: ly },
                Point { x: cx + half, y: ly },
                &icon,
                1.0,
            );
            dc.fill_ellipse(Point { x: cx + knob, y: ly }, 1.8, 1.8, &icon);
        }
        
        // A config error takes over the status bar and uses the error color.
        // Anything less and a typo looks like the setting had no effect.
        // A refused close outranks the config error, which outranks the idle
        // readout: the most recent thing the user did comes first.
        // Vim's own message — "E486: Pattern not found", "3 fewer lines" —
        // sits below the notice: it is the answer to the last key, and the
        // next key clears it.
        let vim_message = self.focused_vim().and_then(|v| v.message().map(|(m, err)| (m.to_string(), err)));
        let (text, color) = match (&self.notice, &self.cfg_problem, vim_message) {
            (Some(n), _, _) => (n.clone(), self.cfg.theme.warning),
            (None, Some(p), _) => (format!("config: {p}"), self.cfg.theme.error),
            (None, None, Some((m, err))) => (m, if err { self.cfg.theme.error } else { self.cfg.theme.fg }),
            (None, None, None) => (self.status_line(), self.cfg.theme.dim),
        };

        let layout = self.text.volatile(&text)?;
        let brush = dc.solid(themed(color, if chrome.active { 1.0 } else { 0.55 }))?;
        dc.text(
            Point { x: btn.right() + 10.0, y: h - lh - 2.0 },
            &layout,
            &brush,
        );
                Ok(())
    }

    fn draw_caption(&self, dc: &Canvas, chrome: &Chrome) -> Result<()> {
        let c = self.cfg.theme.caption;
        let hot = dc.solid(themed(c.hover, 1.0))?;
        let hot_press = dc.solid(themed(c.press, 1.0))?;
        let danger = dc.solid(themed(c.close_hover, 1.0))?;
        let danger_press = dc.solid(themed(c.close_press, 1.0))?;
        let icon_a = if chrome.active { 0.80 } else { 0.45 };
        let icon = dc.solid(themed(c.icon, icon_a))?;
        let icon_hot = dc.solid(themed(c.icon, 0.98))?;

        for b in [CaptionButton::Minimize, CaptionButton::Maximize, CaptionButton::Close] {
            let r = chrome.button(b);
            let hovered = chrome.hovered == Some(b);
            let pressed = chrome.pressed == Some(b);
            let rect = Bounds { left: r.left, top: r.top, right: r.right, bottom: r.bottom };

            if hovered || pressed {
                let bg = match (b, pressed) {
                    (CaptionButton::Close, false) => &danger,
                    (CaptionButton::Close, true) => &danger_press,
                    (_, false) => &hot,
                    (_, true) => &hot_press,
                };
                dc.fill_rect(&rect, bg);
            }

            let fg = if hovered || pressed { &icon_hot } else { &icon };
            let cx = (r.left + r.right) * 0.5;
            let cy = (r.top + r.bottom) * 0.5;
            let s = 5.0;

            match b {
                CaptionButton::Minimize => dc.line(
                    Point { x: cx - s, y: cy },
                    Point { x: cx + s, y: cy },
                    fg,
                    1.0),
                CaptionButton::Maximize if chrome.maximized => {
                    dc.stroke_rect(
                        &Bounds {
                            left: cx - s,
                            top: cy - s + 2.0,
                            right: cx + s - 2.0,
                            bottom: cy + s,
                        },
                        fg,
                        1.0,
                    );
                    dc.line(
                        Point { x: cx - s + 2.0, y: cy - s },
                        Point { x: cx + s, y: cy - s },
                        fg,
                        1.0,
                    );
                    dc.line(
                        Point { x: cx + s, y: cy - s },
                        Point { x: cx + s, y: cy + s - 2.0 },
                        fg,
                        1.0,
                    );
                }
                CaptionButton::Maximize => dc.stroke_rect(
                    &Bounds { left: cx - s, top: cy - s, right: cx + s, bottom: cy + s },
                    fg,
                    1.0),
                CaptionButton::Close => {
                    dc.line(
                        Point { x: cx - s, y: cy - s },
                        Point { x: cx + s, y: cy + s },
                        fg,
                        1.0,
                    );
                    dc.line(
                        Point { x: cx + s, y: cy - s },
                        Point { x: cx - s, y: cy + s },
                        fg,
                        1.0,
                    );
                }
            }
        }
        Ok(())
            }
}
