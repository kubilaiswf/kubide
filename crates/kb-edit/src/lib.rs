//! Text buffer: editing, cursor movement, selection and undo.
//!
//! Lines are a `Vec<String>`, not a rope. A rope wins on multi-megabyte files
//! and single-line files of great length; for source code, a vector of lines is
//! simpler to reason about and every operation here is bounded by the length of
//! one line. The day that stops being true, this is the file to rewrite — the
//! API is line-and-column, which a rope can serve just as well.
//!
//! Columns are **character** counts, never byte offsets. Byte arithmetic looks
//! right until the first non-ASCII character, and then Home/End land inside a
//! multi-byte sequence and the text is corrupted.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub line: usize,
    /// Column in characters.
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// How the file ended its lines when we read it.
///
/// Preserved on save. Rewriting a CRLF file with LF endings turns a one-line
/// change into a diff that touches every line, which is how you accidentally
/// declare war on your own repository.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }
}

/// One undoable step: the text that was there, and the text that replaced it.
///
/// Storing both directions means undo and redo are the same operation with the
/// arguments swapped, and neither has to reconstruct anything.
#[derive(Clone, Debug)]
struct Edit {
    at: Pos,
    removed: String,
    inserted: String,
    /// Where the cursor was before the edit, so undo puts it back where the
    /// user was rather than where the change happened to end.
    cursor_before: Pos,
}

/// What kind of edit this was, for deciding whether to group with the previous
/// one. Typing a word should undo as a word, not as eight separate letters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Insert,
    Delete,
    Other,
}

pub struct Buffer {
    lines: Vec<String>,
    pub cursor: Pos,
    /// The other end of the selection. `None` means no selection.
    pub anchor: Option<Pos>,
    /// Column the cursor wants when moving vertically.
    ///
    /// Without it, moving down through a short line and back up loses the
    /// original column — which every editor gets right and is jarring to miss.
    goal_col: Option<usize>,
    path: Option<PathBuf>,
    line_ending: LineEnding,
    /// Whether the file ended with a newline. Adding or removing one silently
    /// is a diff nobody asked for.
    final_newline: bool,
    undo: Vec<Vec<Edit>>,
    redo: Vec<Vec<Edit>>,
    /// The group currently being appended to.
    open_group: Option<Kind>,
    /// Undo depth at the last save, for an honest modified flag. A counter, not
    /// a bool, so undoing back to the saved state clears the flag.
    saved_at: usize,
    /// What the file looked like on disk when we last read or wrote it.
    ///
    /// Size and modification time, not a hash: reading the whole file again
    /// just to compare it would defeat the point on anything large, and this
    /// catches every case that matters — another program rewrote the file
    /// while it was open here.
    disk_stamp: Option<(std::time::SystemTime, u64)>,
    /// Bumped on every change to the text.
    ///
    /// Lets callers cache anything derived from the buffer — syntax
    /// highlighting above all — without re-deriving it every frame. Cursor
    /// movement does not bump it, because moving the cursor changes no colours.
    revision: u64,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl Buffer {
    pub fn from_text(text: &str) -> Self {
        // First \r\n wins. Mixed endings exist; picking the first one seen and
        // leaving the rest alone is the least surprising thing to do.
        let line_ending = if text.contains("\r\n") { LineEnding::Crlf } else { LineEnding::Lf };
        let final_newline = text.ends_with('\n');
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            lines,
            cursor: Pos::default(),
            anchor: None,
            goal_col: None,
            path: None,
            line_ending,
            final_newline,
            undo: Vec::new(),
            redo: Vec::new(),
            open_group: None,
            saved_at: 0,
            revision: 0,
            disk_stamp: None,
        }
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        // Lossy: one bad byte should not stop you from opening a file. It does
        // mean saving would rewrite that byte, so this is a place to revisit
        // once there is a way to warn about it.
        let mut me = Self::from_text(&String::from_utf8_lossy(&bytes));
        me.path = Some(path.to_path_buf());
        me.disk_stamp = stamp(path);
        Ok(me)
    }

    /// Whether the file changed underneath us since we read or wrote it.
    ///
    /// Saving over someone else's change is not recoverable — there is no undo
    /// for the other program's work — so this is worth a question.
    pub fn changed_on_disk(&self) -> bool {
        let Some(path) = &self.path else { return false };
        match (stamp(path), self.disk_stamp) {
            (Some(now), Some(then)) => now != then,
            // Unreadable metadata is not evidence of a change; refusing to
            // save on it would be worse than the risk.
            _ => false,
        }
    }

    /// Rereads the file, throwing away unsaved changes.
    pub fn reload(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "buffer has no path",
            ));
        };
        let fresh = Self::open(&path)?;
        let cursor = self.cursor;
        *self = fresh;
        // Keep the caret roughly where the reader was looking.
        self.cursor = self.clamp(cursor);
        Ok(())
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "buffer has no path",
            ));
        };
        std::fs::write(&path, self.to_text())?;
        self.saved_at = self.undo.len();
        self.disk_stamp = stamp(&path);
        Ok(())
    }

    pub fn to_text(&self) -> String {
        let mut out = self.lines.join(self.line_ending.as_str());
        if self.final_newline {
            out.push_str(self.line_ending.as_str());
        }
        out
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map(String::as_str).unwrap_or("")
    }
    pub fn len(&self) -> usize {
        self.lines.len()
    }
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }
    pub fn modified(&self) -> bool {
        self.undo.len() != self.saved_at
    }
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn line_chars(&self, i: usize) -> usize {
        self.line(i).chars().count()
    }

    /// Character column to byte offset within a line.
    fn byte_of(&self, line: usize, col: usize) -> usize {
        self.line(line)
            .char_indices()
            .nth(col)
            .map(|(b, _)| b)
            .unwrap_or_else(|| self.line(line).len())
    }

    fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len().saturating_sub(1));
        Pos::new(line, p.col.min(self.line_chars(line)))
    }

    // -- selection ---------------------------------------------------------

    /// The selection as an ordered pair, if there is one.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            return None;
        }
        Some(if anchor < self.cursor { (anchor, self.cursor) } else { (self.cursor, anchor) })
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        Some(self.text_between(start, end))
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    fn text_between(&self, start: Pos, end: Pos) -> String {
        if start.line == end.line {
            let s = self.byte_of(start.line, start.col);
            let e = self.byte_of(start.line, end.col);
            return self.line(start.line)[s..e].to_owned();
        }
        let mut out = String::new();
        let s = self.byte_of(start.line, start.col);
        out.push_str(&self.line(start.line)[s..]);
        for i in start.line + 1..end.line {
            out.push('\n');
            out.push_str(self.line(i));
        }
        out.push('\n');
        let e = self.byte_of(end.line, end.col);
        out.push_str(&self.line(end.line)[..e]);
        out
    }

    // -- editing -----------------------------------------------------------

    /// Replaces a range with text. Every edit goes through here, so undo only
    /// has one shape to record.
    fn replace(&mut self, start: Pos, end: Pos, text: &str, kind: Kind) {
        let removed = self.text_between(start, end);
        let cursor_before = self.cursor;

        let head = self.line(start.line)[..self.byte_of(start.line, start.col)].to_owned();
        let tail = self.line(end.line)[self.byte_of(end.line, end.col)..].to_owned();

        let mut new_lines: Vec<String> = Vec::new();
        let inserted: Vec<&str> = text.split('\n').collect();
        for (i, piece) in inserted.iter().enumerate() {
            let mut line = String::new();
            if i == 0 {
                line.push_str(&head);
            }
            line.push_str(piece);
            if i == inserted.len() - 1 {
                line.push_str(&tail);
            }
            new_lines.push(line);
        }

        self.lines.splice(start.line..=end.line, new_lines);
        self.revision += 1;

        // The cursor lands at the end of what was inserted.
        self.cursor = if inserted.len() == 1 {
            Pos::new(start.line, start.col + inserted[0].chars().count())
        } else {
            Pos::new(
                start.line + inserted.len() - 1,
                inserted.last().unwrap().chars().count(),
            )
        };
        self.anchor = None;
        self.goal_col = None;

        let edit = Edit {
            at: start,
            removed,
            inserted: text.to_owned(),
            cursor_before,
        };

        // Group with the previous step when it was the same kind of change,
        // so a typed word undoes as a word.
        if self.open_group == Some(kind) && !self.undo.is_empty() {
            self.undo.last_mut().unwrap().push(edit);
        } else {
            self.undo.push(vec![edit]);
            self.open_group = Some(kind);
        }
        self.redo.clear();
    }

    /// Ends the current undo group, so the next edit starts a new one.
    ///
    /// Called when the cursor moves or the buffer is saved: an edit here and
    /// another one somewhere else are not one action, however close in time.
    pub fn break_undo_group(&mut self) {
        self.open_group = None;
    }

    pub fn insert(&mut self, text: &str) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let kind = if text.contains('\n') { Kind::Other } else { Kind::Insert };
        self.replace(start, end, text, kind);
        if kind == Kind::Other {
            self.break_undo_group();
        }
    }

    pub fn insert_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.insert(c.encode_utf8(&mut buf));
    }

    /// Types one character with bracket-and-quote pairing.
    ///
    /// Three promises, in the order they are checked: a selection plus an
    /// opener wraps the selection — nobody selects a word and presses `(`
    /// hoping the word dies. Typing a closer that is already under the
    /// cursor steps over it — the pair went in together, and this is the
    /// other half of that. And typing an opener inserts its closer with the
    /// caret between, but only where that helps: not gluing a pair onto the
    /// start of a word, and not treating an apostrophe inside one as an
    /// unterminated string.
    pub fn type_char(&mut self, c: char) {
        if let (Some((start, end)), Some(close)) = (self.selection(), closer_of(c)) {
            let inner = self.text_between(start, end);
            self.replace(start, end, &format!("{c}{inner}{close}"), Kind::Other);
            self.break_undo_group();
            return;
        }
        if matches!(c, ')' | ']' | '}' | '"' | '\'' | '`') && self.char_at_cursor() == Some(c) {
            self.move_right(false);
            return;
        }
        if let Some(close) = closer_of(c) {
            let is_quote = matches!(c, '"' | '\'' | '`');
            let in_word = self
                .char_before_cursor()
                .is_some_and(|p| p.is_alphanumeric() || p == '_');
            let next_leaves_room = matches!(
                self.char_at_cursor(),
                None | Some(')' | ']' | '}' | ' ' | '\t' | ',' | ';' | ':')
            );
            if next_leaves_room && !(is_quote && in_word) {
                let mut pair = String::new();
                pair.push(c);
                pair.push(close);
                self.insert(&pair);
                self.move_left(false);
                return;
            }
        }
        self.insert_char(c);
    }

    /// Backspace that takes the other half of a pair it sits between:
    /// `(|)` dies whole, because it was born whole.
    pub fn backspace_pair(&mut self) {
        if self.selection().is_none() {
            if let (Some(prev), Some(next)) = (self.char_before_cursor(), self.char_at_cursor()) {
                if closer_of(prev) == Some(next) {
                    let start = Pos::new(self.cursor.line, self.cursor.col - 1);
                    let end = Pos::new(self.cursor.line, self.cursor.col + 1);
                    self.replace(start, end, "", Kind::Delete);
                    return;
                }
            }
        }
        self.backspace();
    }

    /// The identifier ending at the cursor — what a snippet trigger can be.
    pub fn word_before_cursor(&self) -> String {
        let head: Vec<char> = self
            .line(self.cursor.line)
            .chars()
            .take(self.cursor.col)
            .collect();
        let n = head
            .iter()
            .rev()
            .take_while(|c| c.is_alphanumeric() || **c == '_')
            .count();
        head[head.len() - n..].iter().collect()
    }

    /// Replaces the `trigger` characters before the cursor with `body`, as
    /// one undo step. `$0` marks where the caret lands; without one it lands
    /// at the end. Continuation lines pick up the trigger line's indent,
    /// because a snippet pasted flat against the margin inside a nested
    /// block is worse than typing it out.
    pub fn expand_snippet(&mut self, trigger: usize, body: &str) {
        let end = self.cursor;
        let start = Pos::new(end.line, end.col.saturating_sub(trigger));
        let indent: String = self
            .line(end.line)
            .chars()
            .take_while(|c| *c == ' ')
            .collect();
        let body = body.replace('\n', &format!("\n{indent}"));
        let (before, after) = match body.split_once("$0") {
            Some((b, a)) => (b.to_string(), a.to_string()),
            None => (body.clone(), String::new()),
        };
        self.break_undo_group();
        self.replace(start, end, &format!("{before}{after}"), Kind::Other);
        self.break_undo_group();
        self.cursor = self.clamp(advance(start, &before));
        self.goal_col = None;
    }

    fn char_at_cursor(&self) -> Option<char> {
        self.line(self.cursor.line).chars().nth(self.cursor.col)
    }

    fn char_before_cursor(&self) -> Option<char> {
        if self.cursor.col == 0 {
            return None;
        }
        self.line(self.cursor.line).chars().nth(self.cursor.col - 1)
    }

    /// Enter, carrying the current line's indentation onto the new line.
    ///
    /// Without this, writing anything nested means retyping the indent on
    /// every line — the single most noticeable thing missing from an editor
    /// that otherwise works.
    pub fn insert_newline(&mut self) {
        let indent: String = self
            .line(self.cursor.line)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            // Only up to the cursor: pressing Enter inside the leading
            // whitespace should not copy the part after it.
            .take(self.cursor.col)
            .collect();
        self.insert(&format!("\n{indent}"));
    }

    /// Indents every line the selection touches.
    ///
    /// Line-wise on purpose: Tab with a selection means "shift this block".
    /// Replacing the selection with a tab character instead is the mistake
    /// people only make once before they stop trusting the editor.
    pub fn indent(&mut self, width: usize) {
        let pad = " ".repeat(width);
        self.shift_lines(|line| Some(format!("{pad}{line}")));
    }

    /// Removes up to `width` leading spaces from every line the selection
    /// touches. Lines with no indentation are left alone rather than losing
    /// the first characters of code.
    pub fn dedent(&mut self, width: usize) {
        self.shift_lines(move |line| {
            let strip = line.chars().take(width).take_while(|c| *c == ' ').count();
            (strip > 0).then(|| line.chars().skip(strip).collect())
        });
    }

    fn shift_lines(&mut self, f: impl Fn(&str) -> Option<String>) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let (first, last) = (start.line, end.line);

        let cursor = self.cursor;
        let anchor = self.anchor;
        let mut moved_cursor = 0i64;
        let mut moved_anchor = 0i64;

        for line in first..=last {
            let Some(new) = f(self.line(line)) else { continue };
            let old_len = self.line_chars(line);
            let new_len = new.chars().count();
            self.replace(Pos::new(line, 0), Pos::new(line, old_len), &new, Kind::Other);

            let delta = new_len as i64 - old_len as i64;
            if cursor.line == line {
                moved_cursor = delta;
            }
            if anchor.map(|a| a.line) == Some(line) {
                moved_anchor = delta;
            }
        }
        self.break_undo_group();

        // Put the selection back, shifted. `replace` collapses it, and losing
        // the selection after every Tab makes re-indenting a block impossible.
        let shift = |p: Pos, d: i64| Pos::new(p.line, (p.col as i64 + d).max(0) as usize);
        self.anchor = anchor.map(|a| self.clamp(shift(a, moved_anchor)));
        self.cursor = self.clamp(shift(cursor, moved_cursor));
    }

    /// Comments or uncomments every line the selection touches.
    ///
    /// One decision for the whole block: if any line is uncommented, the block
    /// gets commented; only when all of them already are does it uncomment.
    /// Deciding per line would leave a mixed block flipping half in and half
    /// out on every press.
    ///
    /// The marker is inserted at each line's own indentation rather than at
    /// column zero, so an indented block stays readable.
    pub fn toggle_comment(&mut self, marker: &str) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let range = start.line..=end.line;

        let all_commented = range
            .clone()
            .filter(|i| !self.line(*i).trim().is_empty())
            .all(|i| self.line(i).trim_start().starts_with(marker));

        let marker = marker.to_string();
        if all_commented {
            self.shift_lines(move |line| {
                let indent = line.len() - line.trim_start().len();
                let rest = line[indent..].strip_prefix(&marker)?;
                // Also take the conventional single space after the marker,
                // which is what we put there.
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                Some(format!("{}{}", &line[..indent], rest))
            });
        } else {
            self.shift_lines(move |line| {
                if line.trim().is_empty() {
                    // Commenting blank lines adds trailing whitespace nobody
                    // asked for and shows up as noise in a diff.
                    return None;
                }
                let indent = line.len() - line.trim_start().len();
                Some(format!("{}{} {}", &line[..indent], marker, &line[indent..]))
            });
        }
    }

    /// Moves the lines the selection touches up or down by one.
    pub fn move_lines(&mut self, down: bool) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let (first, last) = (start.line, end.line);
        if (down && last + 1 >= self.lines.len()) || (!down && first == 0) {
            return;
        }

        let cursor = self.cursor;
        let anchor = self.anchor;
        // Rewriting the whole span, neighbour included, keeps this a single
        // undo step instead of two edits that undo out of order.
        let (from, to) = if down { (first, last + 1) } else { (first - 1, last) };
        let mut block: Vec<String> = self.lines[from..=to].to_vec();
        if down {
            block.rotate_right(1);
        } else {
            block.rotate_left(1);
        }
        let text = block.join("
");
        let end_col = self.line_chars(to);
        self.replace(Pos::new(from, 0), Pos::new(to, end_col), &text, Kind::Other);
        self.break_undo_group();

        let shift = |p: Pos| {
            let line = if down { p.line + 1 } else { p.line - 1 };
            Pos::new(line, p.col)
        };
        self.anchor = anchor.map(|a| self.clamp(shift(a)));
        self.cursor = self.clamp(shift(cursor));
    }

    /// Copies the lines the selection touches below themselves.
    pub fn duplicate_lines(&mut self) {
        let cursor = self.cursor;
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let block: Vec<String> = self.lines[start.line..=end.line].to_vec();
        let text = format!("{}
{}", block.join("
"), block.join("
"));
        let end_col = self.line_chars(end.line);
        self.replace(
            Pos::new(start.line, 0),
            Pos::new(end.line, end_col),
            &text,
            Kind::Other,
        );
        self.break_undo_group();

        // The caret follows the copy, which is the one you are about to edit.
        let span = end.line - start.line + 1;
        self.anchor = None;
        self.cursor = self.clamp(Pos::new(cursor.line + span, cursor.col));
    }

    /// Selects the word under a position, for double-click.
    pub fn select_word_at(&mut self, at: Pos) {
        let at = self.clamp(at);
        let chars: Vec<char> = self.line(at.line).chars().collect();
        if chars.is_empty() {
            return;
        }
        // A click just past the end of a word still takes that word.
        let probe = at.col.min(chars.len() - 1);
        if !is_word(chars[probe]) {
            self.move_to(at, false);
            return;
        }
        let mut start = probe;
        while start > 0 && is_word(chars[start - 1]) {
            start -= 1;
        }
        let mut end = probe;
        while end < chars.len() && is_word(chars[end]) {
            end += 1;
        }
        self.anchor = Some(Pos::new(at.line, start));
        self.cursor = Pos::new(at.line, end);
        self.break_undo_group();
    }

    /// Backspace. Deletes the selection when there is one.
    pub fn backspace(&mut self) {
        if let Some((start, end)) = self.selection() {
            self.replace(start, end, "", Kind::Other);
            self.break_undo_group();
            return;
        }
        let end = self.cursor;
        let start = if end.col > 0 {
            Pos::new(end.line, end.col - 1)
        } else if end.line > 0 {
            Pos::new(end.line - 1, self.line_chars(end.line - 1))
        } else {
            return;
        };
        self.replace(start, end, "", Kind::Delete);
    }

    /// Forward delete.
    pub fn delete(&mut self) {
        if let Some((start, end)) = self.selection() {
            self.replace(start, end, "", Kind::Other);
            self.break_undo_group();
            return;
        }
        let start = self.cursor;
        let end = if start.col < self.line_chars(start.line) {
            Pos::new(start.line, start.col + 1)
        } else if start.line + 1 < self.lines.len() {
            Pos::new(start.line + 1, 0)
        } else {
            return;
        };
        self.replace(start, end, "", Kind::Delete);
    }

    // -- undo --------------------------------------------------------------

    pub fn undo(&mut self) -> bool {
        let Some(group) = self.undo.pop() else { return false };
        // Backwards: later edits in a group sit on top of earlier ones.
        for edit in group.iter().rev() {
            let end = advance(edit.at, &edit.inserted);
            self.apply_raw(edit.at, end, &edit.removed);
            self.cursor = self.clamp(edit.cursor_before);
        }
        self.redo.push(group);
        self.open_group = None;
        self.anchor = None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(group) = self.redo.pop() else { return false };
        for edit in &group {
            let end = advance(edit.at, &edit.removed);
            self.apply_raw(edit.at, end, &edit.inserted);
        }
        self.undo.push(group);
        self.open_group = None;
        self.anchor = None;
        true
    }

    /// Replaces every given range with `text`, as one undo step.
    ///
    /// Ranges are `(start, characters)` — single-line spans in document
    /// order, non-overlapping, which is the shape a search hands over. The
    /// buffer does not search itself: what counts as a match is the finder's
    /// decision, and this must replace exactly what the finder highlighted.
    ///
    /// Applied back to front, so each replacement leaves the positions of
    /// everything before it untouched. Returns how many were replaced.
    pub fn replace_ranges(&mut self, ranges: &[(Pos, usize)], text: &str) -> usize {
        // Its own undo group on both sides: "replace all" is one action, and
        // undoing it must not also pull back the edit typed before it.
        self.break_undo_group();
        for (start, chars) in ranges.iter().rev() {
            let end = Pos::new(start.line, start.col + chars);
            self.replace(*start, end, text, Kind::Other);
        }
        self.break_undo_group();
        ranges.len()
    }

    /// Splices text in without touching the undo stacks.
    fn apply_raw(&mut self, start: Pos, end: Pos, text: &str) {
        let start = self.clamp(start);
        let end = self.clamp(end);
        let head = self.line(start.line)[..self.byte_of(start.line, start.col)].to_owned();
        let tail = self.line(end.line)[self.byte_of(end.line, end.col)..].to_owned();

        let pieces: Vec<&str> = text.split('\n').collect();
        let mut new_lines = Vec::with_capacity(pieces.len());
        for (i, piece) in pieces.iter().enumerate() {
            let mut line = String::new();
            if i == 0 {
                line.push_str(&head);
            }
            line.push_str(piece);
            if i == pieces.len() - 1 {
                line.push_str(&tail);
            }
            new_lines.push(line);
        }
        self.lines.splice(start.line..=end.line, new_lines);
        self.revision += 1;
        self.cursor = advance(start, text);
    }

    // -- movement ----------------------------------------------------------

    /// Moves the cursor. `extend` keeps the anchor, growing the selection.
    pub fn move_to(&mut self, to: Pos, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = self.clamp(to);
        self.break_undo_group();
    }

    pub fn move_left(&mut self, extend: bool) {
        let c = self.cursor;
        let to = if c.col > 0 {
            Pos::new(c.line, c.col - 1)
        } else if c.line > 0 {
            Pos::new(c.line - 1, self.line_chars(c.line - 1))
        } else {
            c
        };
        self.move_to(to, extend);
        self.goal_col = None;
    }

    pub fn move_right(&mut self, extend: bool) {
        let c = self.cursor;
        let to = if c.col < self.line_chars(c.line) {
            Pos::new(c.line, c.col + 1)
        } else if c.line + 1 < self.lines.len() {
            Pos::new(c.line + 1, 0)
        } else {
            c
        };
        self.move_to(to, extend);
        self.goal_col = None;
    }

    /// Vertical movement, remembering the column it started from.
    pub fn move_vertical(&mut self, delta: isize, extend: bool) {
        let goal = self.goal_col.unwrap_or(self.cursor.col);
        let line = (self.cursor.line as isize + delta)
            .clamp(0, self.lines.len() as isize - 1) as usize;
        self.move_to(Pos::new(line, goal.min(self.line_chars(line))), extend);
        // move_to clears it, so restore after: this is the one movement that
        // has to survive passing through a short line.
        self.goal_col = Some(goal);
    }

    pub fn move_line_start(&mut self, extend: bool) {
        // First press goes to the first non-blank, second to column zero. This
        // is what indented code actually needs.
        let line = self.line(self.cursor.line);
        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        let to = if self.cursor.col == indent { 0 } else { indent };
        self.move_to(Pos::new(self.cursor.line, to), extend);
        self.goal_col = None;
    }

    pub fn move_line_end(&mut self, extend: bool) {
        let end = self.line_chars(self.cursor.line);
        self.move_to(Pos::new(self.cursor.line, end), extend);
        self.goal_col = None;
    }

    pub fn move_word_left(&mut self, extend: bool) {
        let chars: Vec<char> = self.line(self.cursor.line).chars().collect();
        let mut col = self.cursor.col;
        if col == 0 {
            self.move_left(extend);
            return;
        }
        col -= 1;
        while col > 0 && chars[col].is_whitespace() {
            col -= 1;
        }
        let word = is_word(chars[col]);
        while col > 0 && is_word(chars[col - 1]) == word && !chars[col - 1].is_whitespace() {
            col -= 1;
        }
        self.move_to(Pos::new(self.cursor.line, col), extend);
        self.goal_col = None;
    }

    pub fn move_word_right(&mut self, extend: bool) {
        let chars: Vec<char> = self.line(self.cursor.line).chars().collect();
        let mut col = self.cursor.col;
        if col >= chars.len() {
            self.move_right(extend);
            return;
        }
        let word = is_word(chars[col]);
        while col < chars.len() && is_word(chars[col]) == word && !chars[col].is_whitespace() {
            col += 1;
        }
        while col < chars.len() && chars[col].is_whitespace() {
            col += 1;
        }
        self.move_to(Pos::new(self.cursor.line, col), extend);
        self.goal_col = None;
    }

    pub fn select_all(&mut self) {
        let last = self.lines.len() - 1;
        self.anchor = Some(Pos::new(0, 0));
        self.cursor = Pos::new(last, self.line_chars(last));
    }
}

/// Size and modification time, or `None` when the file can't be stat'd.
fn stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Where a position ends up after inserting `text` at it.
fn advance(at: Pos, text: &str) -> Pos {
    let pieces: Vec<&str> = text.split('\n').collect();
    if pieces.len() == 1 {
        Pos::new(at.line, at.col + pieces[0].chars().count())
    } else {
        Pos::new(at.line + pieces.len() - 1, pieces.last().unwrap().chars().count())
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The closing half of an auto-typed pair, if `c` opens one.
fn closer_of(c: char) -> Option<char> {
    Some(match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        Buffer::from_text(text)
    }

    #[test]
    fn an_empty_buffer_still_has_one_line() {
        // Zero lines would make every cursor operation a special case.
        let b = Buffer::default();
        assert_eq!(b.len(), 1);
        assert!(b.is_empty());
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut b = buf("hello");
        b.move_line_end(false);
        b.insert_char('!');
        assert_eq!(b.line(0), "hello!");
    }

    #[test]
    fn columns_are_characters_not_bytes() {
        // The bug this prevents: End on a Turkish line landing mid-codepoint.
        let mut b = buf("ğüşiöç");
        b.move_line_end(false);
        assert_eq!(b.cursor.col, 6, "six characters, twelve bytes");
        b.insert_char('!');
        assert_eq!(b.line(0), "ğüşiöç!");
    }

    #[test]
    fn backspace_across_a_line_start_joins_lines() {
        let mut b = buf("ab\ncd");
        b.move_to(Pos::new(1, 0), false);
        b.backspace();
        assert_eq!(b.lines(), ["abcd"]);
        assert_eq!(b.cursor, Pos::new(0, 2));
    }

    #[test]
    fn enter_splits_the_line_at_the_cursor() {
        let mut b = buf("abcd");
        b.move_to(Pos::new(0, 2), false);
        b.insert_newline();
        assert_eq!(b.lines(), ["ab", "cd"]);
        assert_eq!(b.cursor, Pos::new(1, 0));
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut b = buf("hello world");
        b.move_to(Pos::new(0, 0), false);
        b.move_to(Pos::new(0, 5), true);
        b.insert("goodbye");
        assert_eq!(b.line(0), "goodbye world");
    }

    #[test]
    fn a_multi_line_selection_reads_back_whole() {
        let mut b = buf("one\ntwo\nthree");
        b.move_to(Pos::new(0, 1), false);
        b.move_to(Pos::new(2, 2), true);
        assert_eq!(b.selected_text().unwrap(), "ne\ntwo\nth");
    }

    #[test]
    fn a_typed_word_undoes_as_a_word() {
        // Undoing one letter at a time is technically correct and useless.
        let mut b = buf("");
        for c in "hello".chars() {
            b.insert_char(c);
        }
        assert!(b.undo());
        assert_eq!(b.line(0), "");
    }

    #[test]
    fn moving_the_cursor_starts_a_new_undo_group() {
        let mut b = buf("");
        b.insert("ab");
        b.move_line_start(false);
        b.insert("X");
        b.undo();
        assert_eq!(b.line(0), "ab", "only the second edit should be undone");
    }

    #[test]
    fn undo_then_redo_returns_the_same_text() {
        let mut b = buf("start");
        b.move_line_end(false);
        b.insert(" more");
        let after = b.to_text();
        assert!(b.undo());
        assert_eq!(b.to_text(), "start");
        assert!(b.redo());
        assert_eq!(b.to_text(), after);
    }

    #[test]
    fn undo_puts_the_cursor_back_where_the_user_was() {
        let mut b = buf("abc");
        b.move_to(Pos::new(0, 1), false);
        b.insert("XY");
        b.undo();
        assert_eq!(b.cursor, Pos::new(0, 1));
    }

    #[test]
    fn a_new_edit_drops_the_redo_stack() {
        // Otherwise redo would replay a change onto text it was never made on.
        let mut b = buf("a");
        b.move_line_end(false);
        b.insert("b");
        b.undo();
        b.insert("c");
        assert!(!b.redo());
    }

    #[test]
    fn undo_across_a_line_split_restores_both_lines() {
        let mut b = buf("abcd");
        b.move_to(Pos::new(0, 2), false);
        b.insert_newline();
        assert_eq!(b.len(), 2);
        b.undo();
        assert_eq!(b.lines(), ["abcd"]);
    }

    #[test]
    fn vertical_movement_remembers_the_column() {
        // Down through a short line and back up must return to column 8.
        let mut b = buf("long line here\nx\nanother long one");
        b.move_to(Pos::new(0, 8), false);
        b.move_vertical(1, false);
        assert_eq!(b.cursor.col, 1, "clamped to the short line");
        b.move_vertical(1, false);
        assert_eq!(b.cursor.col, 8, "and back to where it started");
    }

    #[test]
    fn home_toggles_between_indent_and_column_zero() {
        let mut b = buf("    indented");
        b.move_line_end(false);
        b.move_line_start(false);
        assert_eq!(b.cursor.col, 4, "first press: first non-blank");
        b.move_line_start(false);
        assert_eq!(b.cursor.col, 0, "second press: column zero");
    }

    #[test]
    fn word_movement_stops_at_word_boundaries() {
        let mut b = buf("foo bar_baz qux");
        b.move_to(Pos::new(0, 0), false);
        b.move_word_right(false);
        assert_eq!(b.cursor.col, 4, "past 'foo' and its space");
        b.move_word_right(false);
        assert_eq!(b.cursor.col, 12, "'bar_baz' is one word");
    }

    #[test]
    fn crlf_files_stay_crlf() {
        // Rewriting every line ending turns a one-line change into a diff that
        // touches the whole file.
        let mut b = buf("a\r\nb\r\n");
        assert_eq!(b.line_ending(), LineEnding::Crlf);
        b.move_line_end(false);
        b.insert("!");
        assert_eq!(b.to_text(), "a!\r\nb\r\n");
    }

    #[test]
    fn a_missing_final_newline_stays_missing() {
        let b = buf("a\nb");
        assert_eq!(b.to_text(), "a\nb");
        assert_eq!(buf("a\nb\n").to_text(), "a\nb\n");
    }

    #[test]
    fn modified_clears_when_you_undo_back_to_the_saved_state() {
        let mut b = buf("x");
        assert!(!b.modified());
        b.insert("y");
        assert!(b.modified());
        b.undo();
        assert!(!b.modified(), "undoing back to saved is not modified");
    }

    #[test]
    fn the_revision_tracks_text_changes_only() {
        // Anything cached off the buffer — syntax highlighting — keys on this.
        // Bumping it on cursor movement would re-highlight on every arrow key;
        // not bumping it on an edit would leave stale colours on screen.
        let mut b = buf("abc");
        let r0 = b.revision();
        b.move_right(false);
        assert_eq!(b.revision(), r0, "moving the cursor changes no text");
        b.insert("x");
        assert!(b.revision() > r0, "an edit must bump it");
        let r1 = b.revision();
        b.undo();
        assert!(b.revision() > r1, "undo changes the text too");
    }

    #[test]
    fn enter_carries_the_indentation() {
        let mut b = buf("    let x = 1;");
        b.move_line_end(false);
        b.insert_newline();
        assert_eq!(b.lines(), ["    let x = 1;", "    "]);
        assert_eq!(b.cursor, Pos::new(1, 4));
    }

    #[test]
    fn enter_inside_the_indent_keeps_the_text_where_it_was() {
        // Splitting inside leading whitespace: the tail already carries the
        // rest of the indent, and we add only the part before the cursor, so
        // the code ends up at the same column it started at.
        let mut b = buf("        deep");
        b.move_to(Pos::new(0, 4), false);
        b.insert_newline();
        assert_eq!(b.lines(), ["    ", "        deep"]);
    }

    #[test]
    fn tab_shifts_every_selected_line() {
        // Replacing the selection with a tab character instead is the mistake
        // people only make once before they stop trusting the editor.
        let mut b = buf("a\nb\nc");
        b.move_to(Pos::new(0, 0), false);
        b.move_to(Pos::new(2, 1), true);
        b.indent(4);
        assert_eq!(b.lines(), ["    a", "    b", "    c"]);
    }

    #[test]
    fn the_selection_survives_indenting() {
        // Otherwise pressing Tab twice on a block is impossible.
        let mut b = buf("a\nb");
        b.move_to(Pos::new(0, 0), false);
        b.move_to(Pos::new(1, 1), true);
        b.indent(4);
        assert!(b.selection().is_some(), "the selection was lost");
        b.indent(4);
        assert_eq!(b.lines(), ["        a", "        b"]);
    }

    #[test]
    fn dedent_leaves_unindented_lines_alone() {
        // Eating the first characters of code would be a silent corruption.
        let mut b = buf("    a\nb");
        b.move_to(Pos::new(0, 0), false);
        b.move_to(Pos::new(1, 1), true);
        b.dedent(4);
        assert_eq!(b.lines(), ["a", "b"]);
    }

    #[test]
    fn dedent_removes_at_most_one_level() {
        let mut b = buf("        deep");
        b.dedent(4);
        assert_eq!(b.line(0), "    deep");
    }

    #[test]
    fn a_double_click_takes_the_whole_word() {
        let mut b = buf("foo bar_baz qux");
        b.select_word_at(Pos::new(0, 6));
        assert_eq!(b.selected_text().unwrap(), "bar_baz");
    }

    #[test]
    fn double_clicking_a_space_selects_nothing() {
        let mut b = buf("foo bar");
        b.select_word_at(Pos::new(0, 3));
        assert!(b.selection().is_none());
    }

    #[test]
    fn indent_undoes_in_one_step() {
        // Three lines shifted is one action, not three.
        let mut b = buf("a\nb\nc");
        b.move_to(Pos::new(0, 0), false);
        b.move_to(Pos::new(2, 1), true);
        b.indent(4);
        b.undo();
        assert_eq!(b.lines(), ["a", "b", "c"]);
    }

    #[test]
    fn a_file_changed_underneath_us_is_noticed() {
        // Saving over another program's change is not recoverable — there is
        // no undo for the other program's work.
        let p = std::env::temp_dir().join("kubide-edit-external.txt");
        std::fs::write(&p, "one\n").unwrap();
        let mut b = Buffer::open(&p).unwrap();
        assert!(!b.changed_on_disk());

        // A different length is enough; timestamps have coarse resolution and
        // a same-size rewrite within one tick would otherwise slip through.
        std::fs::write(&p, "one two three\n").unwrap();
        assert!(b.changed_on_disk(), "an external write went unnoticed");

        b.reload().unwrap();
        assert_eq!(b.line(0), "one two three");
        assert!(!b.changed_on_disk(), "reloading clears it");
        assert!(!b.modified());
    }

    #[test]
    fn saving_updates_what_we_think_is_on_disk() {
        // Otherwise every save after the first would claim the file changed
        // underneath us — our own write.
        let p = std::env::temp_dir().join("kubide-edit-stamp.txt");
        std::fs::write(&p, "x\n").unwrap();
        let mut b = Buffer::open(&p).unwrap();
        b.move_line_end(false);
        b.insert("y");
        b.save().unwrap();
        assert!(!b.changed_on_disk());
    }

    #[test]
    fn a_buffer_with_no_path_never_claims_to_be_stale() {
        assert!(!buf("scratch").changed_on_disk());
    }

    #[test]
    fn comment_toggles_the_whole_block_one_way() {
        // Deciding per line would leave a mixed block flipping half in and
        // half out on every press.
        let mut b = buf("a
// b
c");
        b.move_to(Pos::new(0, 0), false);
        b.move_to(Pos::new(2, 1), true);
        b.toggle_comment("//");
        assert_eq!(b.lines(), ["// a", "// // b", "// c"]);
    }

    #[test]
    fn uncommenting_needs_every_line_commented() {
        let mut b = buf("// a
// b");
        b.move_to(Pos::new(0, 0), false);
        b.move_to(Pos::new(1, 1), true);
        b.toggle_comment("//");
        assert_eq!(b.lines(), ["a", "b"]);
    }

    #[test]
    fn comments_go_after_the_indentation() {
        let mut b = buf("    indented");
        b.toggle_comment("//");
        assert_eq!(b.line(0), "    // indented");
        b.toggle_comment("//");
        assert_eq!(b.line(0), "    indented", "and come back off cleanly");
    }

    #[test]
    fn blank_lines_are_not_commented() {
        // It would add trailing whitespace nobody asked for.
        let mut b = buf("a

b");
        b.move_to(Pos::new(0, 0), false);
        b.move_to(Pos::new(2, 1), true);
        b.toggle_comment("//");
        assert_eq!(b.line(1), "");
    }

    #[test]
    fn moving_a_line_swaps_it_with_its_neighbour() {
        let mut b = buf("a
b
c");
        b.move_to(Pos::new(1, 0), false);
        b.move_lines(true);
        assert_eq!(b.lines(), ["a", "c", "b"]);
        assert_eq!(b.cursor.line, 2, "the caret follows the line");
        b.move_lines(false);
        assert_eq!(b.lines(), ["a", "b", "c"]);
    }

    #[test]
    fn moving_stops_at_the_edges() {
        let mut b = buf("a
b");
        b.move_to(Pos::new(0, 0), false);
        b.move_lines(false);
        assert_eq!(b.lines(), ["a", "b"]);
        b.move_to(Pos::new(1, 0), false);
        b.move_lines(true);
        assert_eq!(b.lines(), ["a", "b"]);
    }

    #[test]
    fn moving_a_line_undoes_in_one_step() {
        let mut b = buf("a
b
c");
        b.move_to(Pos::new(0, 0), false);
        b.move_lines(true);
        b.undo();
        assert_eq!(b.lines(), ["a", "b", "c"]);
    }

    #[test]
    fn duplicate_copies_below_and_follows_the_caret() {
        let mut b = buf("a
b");
        b.move_to(Pos::new(0, 1), false);
        b.duplicate_lines();
        assert_eq!(b.lines(), ["a", "a", "b"]);
        assert_eq!(b.cursor.line, 1, "the caret sits on the copy");
    }

    #[test]
    fn duplicate_takes_the_whole_selected_block() {
        let mut b = buf("a
b
c");
        b.move_to(Pos::new(0, 0), false);
        b.move_to(Pos::new(1, 1), true);
        b.duplicate_lines();
        assert_eq!(b.lines(), ["a", "b", "a", "b", "c"]);
    }

    #[test]
    fn select_all_covers_the_whole_buffer() {
        let mut b = buf("one\ntwo");
        b.select_all();
        assert_eq!(b.selected_text().unwrap(), "one\ntwo");
    }

    #[test]
    fn deleting_at_the_end_of_the_buffer_does_nothing() {
        let mut b = buf("a");
        b.move_line_end(false);
        b.delete();
        assert_eq!(b.to_text(), "a");
        b.move_to(Pos::new(0, 0), false);
        b.backspace();
        assert_eq!(b.to_text(), "a");
    }

    #[test]
    fn pasting_multiple_lines_splits_correctly() {
        let mut b = buf("ac");
        b.move_to(Pos::new(0, 1), false);
        b.insert("X\nY");
        assert_eq!(b.lines(), ["aX", "Yc"]);
        assert_eq!(b.cursor, Pos::new(1, 1));
    }

    #[test]
    fn replace_ranges_survives_a_length_change() {
        // Three matches on one line, replaced by something longer. Applied
        // front to back this would replace the wrong columns; back to front
        // is the entire trick.
        let mut b = buf("x = x + x");
        let ranges = [
            (Pos::new(0, 0), 1),
            (Pos::new(0, 4), 1),
            (Pos::new(0, 8), 1),
        ];
        assert_eq!(b.replace_ranges(&ranges, "long"), 3);
        assert_eq!(b.to_text(), "long = long + long");
    }

    #[test]
    fn replace_all_undoes_in_one_step() {
        let mut b = buf("a\na\na");
        let ranges = [
            (Pos::new(0, 0), 1),
            (Pos::new(1, 0), 1),
            (Pos::new(2, 0), 1),
        ];
        b.replace_ranges(&ranges, "b");
        assert_eq!(b.to_text(), "b\nb\nb");
        assert!(b.undo(), "there is something to undo");
        assert_eq!(b.to_text(), "a\na\na", "one undo reverts the whole pass");
        assert!(b.redo());
        assert_eq!(b.to_text(), "b\nb\nb");
    }

    #[test]
    fn replace_ranges_does_not_join_the_edit_before_it() {
        // "Replace all" is one action of its own: undoing it must not also
        // pull back whatever was typed just before.
        let mut b = buf("a");
        b.move_line_end(false);
        b.insert("x");
        b.replace_ranges(&[(Pos::new(0, 0), 1)], "b");
        assert_eq!(b.to_text(), "bx");
        b.undo();
        assert_eq!(b.to_text(), "ax", "the typed x survives the first undo");
    }

    #[test]
    fn replacing_nothing_changes_nothing() {
        let mut b = buf("a");
        assert_eq!(b.replace_ranges(&[], "b"), 0);
        assert_eq!(b.to_text(), "a");
        assert!(!b.undo(), "no phantom undo step was recorded");
    }

    #[test]
    fn an_opener_brings_its_closer_and_the_caret_sits_between() {
        let mut b = buf("");
        b.type_char('(');
        assert_eq!(b.to_text(), "()");
        assert_eq!(b.cursor, Pos::new(0, 1));
    }

    #[test]
    fn typing_the_closer_steps_over_instead_of_doubling() {
        let mut b = buf("");
        b.type_char('(');
        b.type_char(')');
        assert_eq!(b.to_text(), "()", "not (())");
        assert_eq!(b.cursor, Pos::new(0, 2));
    }

    #[test]
    fn a_pair_undoes_in_one_step() {
        let mut b = buf("");
        b.type_char('(');
        b.undo();
        assert_eq!(b.to_text(), "");
    }

    #[test]
    fn an_opener_does_not_glue_onto_a_word() {
        // `(` before "word" means "I am wrapping this by hand" — a closer
        // jammed in as `(|w` would have to be deleted every single time.
        let mut b = buf("word");
        b.move_to(Pos::new(0, 0), false);
        b.type_char('(');
        assert_eq!(b.to_text(), "(word");
    }

    #[test]
    fn an_apostrophe_inside_a_word_stays_single() {
        let mut b = buf("dont");
        b.move_line_end(false);
        b.type_char('\'');
        assert_eq!(b.to_text(), "dont'");
    }

    #[test]
    fn a_selection_is_wrapped_not_replaced() {
        let mut b = buf("word");
        b.select_all();
        b.type_char('(');
        assert_eq!(b.to_text(), "(word)");
    }

    #[test]
    fn backspace_between_a_pair_takes_both() {
        let mut b = buf("");
        b.type_char('(');
        b.backspace_pair();
        assert_eq!(b.to_text(), "");
    }

    #[test]
    fn backspace_next_to_unrelated_characters_takes_one() {
        let mut b = buf("ab");
        b.move_to(Pos::new(0, 1), false);
        b.backspace_pair();
        assert_eq!(b.to_text(), "b");
    }

    #[test]
    fn the_word_before_the_cursor_is_the_trigger() {
        let mut b = buf("    print");
        b.move_line_end(false);
        assert_eq!(b.word_before_cursor(), "print");
        b.move_to(Pos::new(0, 0), false);
        assert_eq!(b.word_before_cursor(), "");
    }

    #[test]
    fn a_snippet_replaces_its_trigger_and_lands_on_the_marker() {
        let mut b = buf("print");
        b.move_line_end(false);
        b.expand_snippet(5, "println!(\"$0\");");
        assert_eq!(b.to_text(), "println!(\"\");");
        assert_eq!(b.cursor, Pos::new(0, 10), "inside the quotes");
    }

    #[test]
    fn a_snippet_with_no_marker_lands_at_the_end() {
        let mut b = buf("dbg");
        b.move_line_end(false);
        b.expand_snippet(3, "dbg!();");
        assert_eq!(b.cursor, Pos::new(0, 7));
    }

    #[test]
    fn a_multiline_snippet_keeps_the_indent() {
        // Pasted flat against the margin inside a nested block, a snippet
        // is worse than typing it out.
        let mut b = buf("    main");
        b.move_line_end(false);
        b.expand_snippet(4, "fn main() {\n    $0\n}");
        assert_eq!(b.lines(), ["    fn main() {", "        ", "    }"]);
        assert_eq!(b.cursor, Pos::new(1, 8));
    }

    #[test]
    fn a_snippet_undoes_in_one_step_back_to_the_trigger() {
        let mut b = buf("print");
        b.move_line_end(false);
        b.expand_snippet(5, "println!(\"$0\");");
        b.undo();
        assert_eq!(b.to_text(), "print");
    }
}
