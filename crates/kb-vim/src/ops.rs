//! Operators, registers and the one-key commands.

use kb_edit::{Buffer, Pos};

use crate::motion::{lines_of, Kind};
use crate::parse::{Act, Cmd, Motion, Op, ScrollAt, Target};
use crate::{first_non_blank, order, Ctx, Dir, Effect, Host, Mode, Range, Register, Session, Vim};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreKind {
    Yank,
    Delete,
}

/// A register's text as the clipboard should see it: linewise text gets its
/// final newline back.
fn clip_text(r: &Register) -> String {
    if r.linewise { format!("{}\n", r.text) } else { r.text.clone() }
}

fn from_clipboard(host: &mut dyn Host) -> Option<Register> {
    let text = host.clipboard()?.replace("\r\n", "\n").replace('\r', "\n");
    // Text ending in a newline was cut by line; putting it back by line is
    // what anyone pasting a copied line expects.
    match text.strip_suffix('\n') {
        Some(t) => Some(Register { text: t.to_string(), linewise: true }),
        None => Some(Register { text, linewise: false }),
    }
}

/// The text a range covers.
pub(crate) fn range_text(buf: &Buffer, r: &Range) -> String {
    if r.linewise {
        let (a, b) = lines_of(r);
        buf.lines()[a..=b.min(buf.len() - 1)].join("\n")
    } else {
        buf.text_in(r.start, r.end)
    }
}

/// Removes a range. Linewise ranges take a line break with them, whichever
/// side has one, so the file never grows a blank line from a delete.
pub(crate) fn delete_range(buf: &mut Buffer, r: &Range) {
    if !r.linewise {
        buf.edit(r.start, r.end, "");
        return;
    }
    let (first, last) = lines_of(r);
    let last = last.min(buf.len() - 1);
    if last + 1 < buf.len() {
        buf.edit(Pos::new(first, 0), Pos::new(last + 1, 0), "");
    } else if first > 0 {
        buf.edit(Pos::new(first - 1, buf.line_len(first - 1)), Pos::new(last, buf.line_len(last)), "");
    } else {
        buf.edit(Pos::new(0, 0), Pos::new(last, buf.line_len(last)), "");
    }
}

/// The range of an exclusive motion from `a` to `b`, with vim's rule for
/// one that ends at the start of a line (`:help exclusive-linewise`): it
/// stops at the end of the line before instead, and if it also began at or
/// before the first non-blank, it takes whole lines.
pub(crate) fn exclusive_range(buf: &Buffer, a: Pos, b: Pos) -> Range {
    if b.col == 0 && b.line > a.line {
        let prev_line = b.line - 1;
        if a.col <= first_non_blank(buf, a.line) {
            Range::lines(a.line, prev_line)
        } else {
            Range { start: a, end: Pos::new(prev_line, buf.line_len(prev_line)), linewise: false }
        }
    } else {
        Range { start: a, end: b, linewise: false }
    }
}

fn toggle_case(c: char) -> String {
    if c.is_uppercase() { c.to_lowercase().collect() } else { c.to_uppercase().collect() }
}

impl Vim {
    /// Puts text into a register, and into the unnamed register and the
    /// history registers the way vim does: `"0` for yanks, `"1`-`"9` for
    /// deletes of a line or more, `"-` for smaller deletes.
    pub(crate) fn store(&mut self, s: &mut Session, host: &mut dyn Host, reg: Option<char>, text: String, linewise: bool, kind: StoreKind) {
        if reg == Some('_') {
            return;
        }
        let r = Register { text: text.clone(), linewise };
        match reg {
            Some(c) if c.is_ascii_uppercase() => {
                let e = s.registers.entry(c.to_ascii_lowercase()).or_default();
                if e.linewise || linewise {
                    if !e.text.is_empty() {
                        e.text.push('\n');
                    }
                    e.linewise = true;
                }
                e.text.push_str(&text);
            }
            Some('+') | Some('*') => host.set_clipboard(&clip_text(&r)),
            Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' => {
                s.registers.insert(c, r.clone());
            }
            _ => {}
        }
        match kind {
            StoreKind::Yank => {
                if reg.is_none() || reg == Some('"') {
                    s.registers.insert('0', r.clone());
                }
            }
            StoreKind::Delete => {
                if linewise || text.contains('\n') {
                    for i in (1..9u8).rev() {
                        if let Some(older) = s.registers.get(&((b'0' + i) as char)).cloned() {
                            s.registers.insert((b'0' + i + 1) as char, older);
                        }
                    }
                    s.registers.insert('1', r.clone());
                } else if reg.is_none() {
                    s.registers.insert('-', r.clone());
                }
            }
        }
        s.registers.insert('"', r.clone());
        if s.options.clipboard && reg.is_none() {
            host.set_clipboard(&clip_text(&r));
        }
    }

    pub(crate) fn fetch(&self, s: &Session, host: &mut dyn Host, reg: Option<char>) -> Option<Register> {
        let r = match reg {
            Some('+') | Some('*') => from_clipboard(host),
            None | Some('"') => {
                if s.options.clipboard {
                    from_clipboard(host)
                } else {
                    s.registers.get(&'"').cloned()
                }
            }
            Some('/') => s.last_search.as_ref().map(|(p, _)| Register { text: p.clone(), linewise: false }),
            Some(':') => s.cmd_history.last().map(|c| Register { text: c.clone(), linewise: false }),
            Some('.') => Some(Register { text: s.last_insert_text.clone(), linewise: false }),
            Some('_') => None,
            Some(c) => s.registers.get(&c.to_ascii_lowercase()).cloned(),
        };
        r.filter(|r| !r.text.is_empty())
    }

    /// The range an operator's target covers from the caret.
    pub(crate) fn op_range(&mut self, buf: &Buffer, s: &mut Session, ctx: &Ctx, op: Op, target: &Target, count: Option<usize>) -> Option<Range> {
        let cur = buf.cursor;
        let last = buf.len() - 1;
        match target {
            Target::Line => {
                let n = count.unwrap_or(1).max(1);
                Some(Range::lines(cur.line, (cur.line + n - 1).min(last)))
            }
            Target::Object { count: c2, obj } => {
                let n = count.unwrap_or(1).max(1) * c2.unwrap_or(1).max(1);
                self.object_range(buf, *obj, n)
            }
            Target::Motion { count: c2, motion } => {
                let total = match (count, c2) {
                    (None, None) => None,
                    (a, b) => Some(a.unwrap_or(1).max(1) * b.unwrap_or(1).max(1)),
                };
                let mut motion = *motion;
                // `cw` on a word is `ce`: the space after the word survives,
                // which is what everybody means by "change this word".
                if op == Op::Change {
                    if let Motion::WordFwd { big } = motion {
                        let under = buf.line(cur.line).chars().nth(cur.col);
                        if under.is_some_and(|c| !c.is_whitespace()) {
                            let n = total.unwrap_or(1);
                            // Already on the last character of a word: `cw`
                            // changes just that character.
                            let after = buf.line(cur.line).chars().nth(cur.col + 1);
                            let same_word = |a: char, b: char| {
                                if big {
                                    !a.is_whitespace() && !b.is_whitespace()
                                } else {
                                    (a.is_alphanumeric() || a == '_') == (b.is_alphanumeric() || b == '_') && !b.is_whitespace()
                                }
                            };
                            if n == 1 && !after.is_some_and(|b| same_word(under.unwrap(), b)) {
                                return Some(Range { start: cur, end: Pos::new(cur.line, cur.col + 1), linewise: false });
                            }
                            motion = Motion::WordEnd { big };
                        }
                    }
                }
                let dest = self.motion(buf, s, ctx, motion, total, true)?;
                let (a, mut b) = order(cur, dest.pos);
                Some(match dest.kind {
                    Kind::Linewise => Range::lines(a.line, b.line),
                    Kind::Inclusive => {
                        let len = buf.line_len(b.line);
                        if b.col < len {
                            b.col += 1;
                        } else if b.line + 1 < buf.len() && dest.pos == b && a != b && buf.line_len(b.line) == 0 {
                            b = Pos::new(b.line + 1, 0);
                        }
                        Range { start: a, end: b, linewise: false }
                    }
                    Kind::Exclusive => {
                        if matches!(motion, Motion::LeftWrap | Motion::RightWrap) {
                            Range { start: a, end: b, linewise: false }
                        } else {
                            exclusive_range(buf, a, b)
                        }
                    }
                })
            }
        }
    }

    /// Runs an operator over a range.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_op(&mut self, op: Op, r: Range, reg: Option<char>, n: usize, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: &Ctx) -> Vec<Effect> {
        let _ = ctx;
        let (first, last) = lines_of(&r);
        let last = last.min(buf.len() - 1);
        let cur = buf.cursor;
        match op {
            Op::Delete => {
                let text = range_text(buf, &r);
                self.store(s, host, reg, text, r.linewise, StoreKind::Delete);
                delete_range(buf, &r);
                if r.linewise {
                    let line = first.min(buf.len() - 1);
                    buf.set_cursor(Pos::new(line, first_non_blank(buf, line)));
                    if last + 1 - first > 2 {
                        self.say(&format!("{} fewer lines", last + 1 - first));
                    }
                } else {
                    buf.set_cursor(r.start);
                }
                self.clamp_normal(buf);
            }
            Op::Change => {
                let text = range_text(buf, &r);
                self.store(s, host, reg, text, r.linewise, StoreKind::Delete);
                if r.linewise {
                    // The first line keeps its indent — autoindent, the
                    // setting everyone has on.
                    let indent = first_non_blank(buf, first);
                    buf.edit(Pos::new(first, indent), Pos::new(last, buf.line_len(last)), "");
                    buf.set_cursor(Pos::new(first, indent));
                } else {
                    buf.edit(r.start, r.end, "");
                    buf.set_cursor(r.start);
                }
                self.enter_insert(buf, 1, None, false);
            }
            Op::Yank => {
                let text = range_text(buf, &r);
                self.store(s, host, reg, text, r.linewise, StoreKind::Yank);
                if r.linewise {
                    let col = cur.col.min(buf.line_len(first).saturating_sub(1));
                    buf.set_cursor(Pos::new(first, col));
                    if last + 1 - first > 2 {
                        self.say(&format!("{} lines yanked", last + 1 - first));
                    }
                } else {
                    buf.set_cursor(r.start);
                }
            }
            Op::Indent | Op::Dedent => {
                // The buffer shifts whatever is selected, so select the lines.
                buf.anchor = Some(Pos::new(first, 0));
                buf.cursor = Pos::new(last, buf.line_len(last));
                for _ in 0..n.max(1) {
                    if op == Op::Indent {
                        buf.indent(4);
                    } else {
                        buf.dedent(4);
                    }
                }
                buf.clear_selection();
                buf.set_cursor(Pos::new(first, first_non_blank(buf, first)));
                if last + 1 - first > 2 {
                    self.say(&format!("{} lines {}ed", last + 1 - first, if op == Op::Indent { ">" } else { "<" }));
                }
            }
            Op::ToggleCase | Op::Lower | Op::Upper => {
                let (start, end) = if r.linewise {
                    (Pos::new(first, 0), Pos::new(last, buf.line_len(last)))
                } else {
                    (r.start, r.end)
                };
                let text = buf.text_in(start, end);
                let changed: String = text
                    .chars()
                    .map(|c| match op {
                        Op::Lower => c.to_lowercase().collect::<String>(),
                        Op::Upper => c.to_uppercase().collect(),
                        _ => toggle_case(c),
                    })
                    .collect();
                if changed != text {
                    buf.edit(start, end, &changed);
                }
                buf.set_cursor(if r.linewise { cur } else { start });
                self.clamp_normal(buf);
            }
            Op::Format => {
                self.say("= does nothing here: there is no formatter to hand the lines to");
                buf.set_cursor(Pos::new(first, first_non_blank(buf, first)));
            }
        }
        Vec::new()
    }

    /// Puts a register's text at the caret.
    fn put(&mut self, buf: &mut Buffer, r: &Register, before: bool, n: usize, cursor_after: bool) {
        let cur = buf.cursor;
        let n = n.max(1);
        if r.linewise {
            let block = vec![r.text.as_str(); n].join("\n");
            let lines = block.matches('\n').count() + 1;
            if before {
                let at = Pos::new(cur.line, 0);
                buf.edit(at, at, &format!("{block}\n"));
                buf.set_cursor(if cursor_after {
                    Pos::new((cur.line + lines).min(buf.len() - 1), 0)
                } else {
                    Pos::new(cur.line, first_non_blank(buf, cur.line))
                });
            } else {
                let at = Pos::new(cur.line, buf.line_len(cur.line));
                buf.edit(at, at, &format!("\n{block}"));
                let line = cur.line + 1;
                buf.set_cursor(if cursor_after {
                    Pos::new((line + lines).min(buf.len() - 1), 0)
                } else {
                    Pos::new(line, first_non_blank(buf, line))
                });
            }
            if lines > 2 {
                self.say(&format!("{lines} more lines"));
            }
        } else {
            let text = r.text.repeat(n);
            let len = buf.line_len(cur.line);
            let at = if before || len == 0 { cur } else { Pos::new(cur.line, (cur.col + 1).min(len)) };
            buf.edit(at, at, &text);
            let end = buf.cursor;
            buf.set_cursor(if cursor_after {
                end
            } else if text.contains('\n') {
                at
            } else {
                Pos::new(end.line, end.col.saturating_sub(1))
            });
        }
    }

    /// Joins lines `first..=last` into one. With `spaces`, leading white
    /// space goes and one space comes between, except before a `)`.
    pub(crate) fn join_lines(&mut self, buf: &mut Buffer, first: usize, last: usize, spaces: bool) {
        let last = last.min(buf.len() - 1);
        if last <= first {
            return;
        }
        let mut out = buf.line(first).to_string();
        let mut col = 0;
        for l in first + 1..=last {
            let next = buf.line(l);
            if spaces {
                let trimmed = next.trim_start();
                out = out.trim_end().to_string();
                col = out.chars().count();
                if !trimmed.is_empty() && !out.is_empty() && !trimmed.starts_with(')') {
                    out.push(' ');
                }
                out.push_str(trimmed);
            } else {
                col = out.chars().count();
                out.push_str(next);
            }
        }
        buf.edit(Pos::new(first, 0), Pos::new(last, buf.line_len(last)), &out);
        buf.set_cursor(Pos::new(first, col));
        self.clamp_normal(buf);
    }

    /// Ctrl+A / Ctrl+X: the number at or after the caret, plus `by`.
    fn increment(&mut self, buf: &mut Buffer, by: i64) {
        let cur = buf.cursor;
        let chars: Vec<char> = buf.line(cur.line).chars().collect();
        let mut i = cur.col.min(chars.len());
        let is_hex_body = |i: usize| i >= 2 && chars[i - 2] == '0' && matches!(chars[i - 1], 'x' | 'X');
        if i < chars.len() && chars[i].is_ascii_hexdigit() {
            // Inside a number already: back up to where it starts.
            while i > 0 && chars[i - 1].is_ascii_hexdigit() {
                i -= 1;
            }
            if !is_hex_body(i) {
                // Not hex after all: only decimal digits count.
                i = cur.col;
                if !chars[i].is_ascii_digit() {
                    i = (cur.col..chars.len()).find(|k| chars[*k].is_ascii_digit()).unwrap_or(chars.len());
                } else {
                    while i > 0 && chars[i - 1].is_ascii_digit() {
                        i -= 1;
                    }
                }
            }
        } else {
            while i < chars.len() && !chars[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i >= chars.len() {
            return;
        }
        let hex = is_hex_body(i) || (chars[i] == '0' && chars.get(i + 1).is_some_and(|c| matches!(c, 'x' | 'X')) && chars.get(i + 2).is_some_and(char::is_ascii_hexdigit));
        let (start, end, new_text) = if hex {
            let body = if is_hex_body(i) { i } else { i + 2 };
            let mut end = body;
            while end < chars.len() && chars[end].is_ascii_hexdigit() {
                end += 1;
            }
            let digits: String = chars[body..end].iter().collect();
            let value = u64::from_str_radix(&digits, 16).unwrap_or(0);
            let value = (value as i128 + by as i128).rem_euclid(1i128 << 64) as u64;
            let upper = digits.chars().any(|c| c.is_ascii_uppercase());
            let width = digits.len();
            let text = if upper { format!("{value:0width$X}") } else { format!("{value:0width$x}") };
            (body - 2, end, format!("0{}{}", chars[body - 1], text))
        } else {
            let mut end = i;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            // A minus right before the digits is a sign, whatever precedes
            // it — the same reading vim makes, surprising as it is in `x-1`.
            let negative = i > 0 && chars[i - 1] == '-';
            let start = if negative { i - 1 } else { i };
            let digits: String = chars[i..end].iter().collect();
            let value: i128 = digits.parse::<i128>().unwrap_or(0) * if negative { -1 } else { 1 };
            let value = value + by as i128;
            // Leading zeros are kept, as vim does with 'nrformats' lacking
            // octal: 007 becomes 008.
            let width = if digits.starts_with('0') && digits.len() > 1 { digits.len() } else { 0 };
            let text = format!("{:0width$}", value.unsigned_abs());
            (start, end, if value < 0 { format!("-{text}") } else { text })
        };
        buf.edit(Pos::new(cur.line, start), Pos::new(cur.line, end), &new_text);
        buf.set_cursor(Pos::new(cur.line, start + new_text.chars().count() - 1));
    }

    /// The one-key commands.
    pub(crate) fn act(&mut self, act: Act, cmd: &Cmd, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx) -> Vec<Effect> {
        use Act::*;
        let mut fx = Vec::new();
        let count = cmd.count;
        let n = count.unwrap_or(1).max(1);
        let reg = cmd.reg;
        let cur = buf.cursor;
        let last = buf.len() - 1;
        let len = buf.line_len(cur.line);
        let visual = self.in_visual();
        let changes = matches!(
            act,
            DeleteChar | DeleteCharBack | Substitute | SubstituteLine | DeleteToEnd | ChangeToEnd | Paste { .. } | Join { .. }
                | Tilde | InsertBefore | InsertAfter | InsertLineStart | InsertLineEnd | InsertColumnZero | InsertLast
                | OpenBelow | OpenAbove | ReplaceMode | ReplaceChar(_) | Increment { .. } | RepeatSubstitute
        );
        if changes {
            let extent = visual.then(|| self.extent(buf));
            self.note_change(cmd, extent);
        }

        match act {
            DeleteChar => {
                if visual {
                    let r = self.visual_range(buf, false);
                    self.exit_visual(buf);
                    return self.apply_op(Op::Delete, r, reg, n, buf, s, host, &ctx);
                }
                if len == 0 {
                    return fx;
                }
                let r = Range { start: cur, end: Pos::new(cur.line, (cur.col + n).min(len)), linewise: false };
                return self.apply_op(Op::Delete, r, reg, n, buf, s, host, &ctx);
            }
            DeleteCharBack => {
                if cur.col == 0 {
                    return fx;
                }
                let r = Range { start: Pos::new(cur.line, cur.col.saturating_sub(n)), end: cur, linewise: false };
                return self.apply_op(Op::Delete, r, reg, n, buf, s, host, &ctx);
            }
            Substitute => {
                let end = Pos::new(cur.line, (cur.col + n).min(len));
                return self.apply_op(Op::Change, Range { start: cur, end, linewise: false }, reg, n, buf, s, host, &ctx);
            }
            SubstituteLine => {
                let r = Range::lines(cur.line, (cur.line + n - 1).min(last));
                return self.apply_op(Op::Change, r, reg, n, buf, s, host, &ctx);
            }
            DeleteToEnd | ChangeToEnd => {
                let line = (cur.line + n - 1).min(last);
                let r = Range { start: cur, end: Pos::new(line, buf.line_len(line)), linewise: false };
                let op = if act == DeleteToEnd { Op::Delete } else { Op::Change };
                return self.apply_op(op, r, reg, n, buf, s, host, &ctx);
            }
            YankLine => {
                let r = Range::lines(cur.line, (cur.line + n - 1).min(last));
                return self.apply_op(Op::Yank, r, reg, n, buf, s, host, &ctx);
            }
            Paste { before, cursor_after } => {
                let Some(r) = self.fetch(s, host, reg) else {
                    self.error(&format!("E353: Nothing in register {}", reg.unwrap_or('"')));
                    return fx;
                };
                if visual {
                    // What the selection held goes to the unnamed register,
                    // so `p` twice pastes the previous text — vim's quirk,
                    // kept because people rely on it.
                    let range = self.visual_range(buf, false);
                    self.exit_visual(buf);
                    let removed = range_text(buf, &range);
                    delete_range(buf, &range);
                    self.store(s, host, None, removed, range.linewise, StoreKind::Delete);
                    let start = if range.linewise { Pos::new(range.start.line.min(buf.len() - 1), 0) } else { range.start };
                    buf.set_cursor(start);
                    let r = if range.linewise {
                        Register { text: r.text.clone(), linewise: true }
                    } else {
                        r
                    };
                    let before = if range.linewise { range.start.line < buf.len() || buf.len() == 1 } else { true };
                    if range.linewise && range.start.line >= buf.len() {
                        buf.set_cursor(Pos::new(buf.len() - 1, 0));
                        self.put(buf, &r, false, n, cursor_after);
                    } else if range.linewise && buf.len() == 1 && buf.is_empty() {
                        buf.edit(Pos::new(0, 0), Pos::new(0, 0), &vec![r.text.as_str(); n].join("\n"));
                        buf.set_cursor(Pos::new(0, first_non_blank(buf, 0)));
                    } else {
                        self.put(buf, &r, before, n, cursor_after);
                    }
                    return fx;
                }
                self.put(buf, &r, before, n, cursor_after);
            }
            Join { spaces } => {
                if visual {
                    let r = self.visual_range(buf, true);
                    self.exit_visual(buf);
                    let (a, b) = lines_of(&r);
                    self.join_lines(buf, a, b.max(a + 1), spaces);
                } else {
                    if cur.line == last {
                        return fx;
                    }
                    self.join_lines(buf, cur.line, cur.line + n.max(2) - 1, spaces);
                }
            }
            Undo => {
                for _ in 0..n {
                    if !buf.undo() {
                        self.error("Already at oldest change");
                        break;
                    }
                }
                self.clamp_normal(buf);
            }
            Redo => {
                for _ in 0..n {
                    if !buf.redo() {
                        self.error("Already at newest change");
                        break;
                    }
                }
                self.clamp_normal(buf);
            }
            Repeat => return self.repeat_last(count, buf, s, host, ctx),
            Tilde => {
                if visual {
                    let r = self.visual_range(buf, false);
                    self.exit_visual(buf);
                    return self.apply_op(Op::ToggleCase, r, reg, n, buf, s, host, &ctx);
                }
                if len == 0 {
                    return fx;
                }
                let end = Pos::new(cur.line, (cur.col + n).min(len));
                let text = buf.text_in(cur, end);
                let changed: String = text.chars().map(toggle_case).collect();
                buf.edit(cur, end, &changed);
                buf.set_cursor(end);
                self.clamp_normal(buf);
            }
            InsertBefore => {
                if visual {
                    let start = self.visual_range(buf, false).start;
                    self.exit_visual(buf);
                    buf.set_cursor(start);
                }
                self.enter_insert(buf, n, None, false);
            }
            InsertAfter => {
                if visual {
                    let r = self.visual_range(buf, false);
                    self.exit_visual(buf);
                    buf.set_cursor(r.end);
                } else if len > 0 {
                    buf.set_cursor(Pos::new(cur.line, (cur.col + 1).min(len)));
                }
                self.enter_insert(buf, n, None, false);
            }
            InsertLineStart => {
                buf.set_cursor(Pos::new(cur.line, first_non_blank(buf, cur.line)));
                self.enter_insert(buf, n, None, false);
            }
            InsertColumnZero => {
                buf.set_cursor(Pos::new(cur.line, 0));
                self.enter_insert(buf, n, None, false);
            }
            InsertLineEnd => {
                buf.set_cursor(Pos::new(cur.line, len));
                self.enter_insert(buf, n, None, false);
            }
            InsertLast => {
                if let Some(p) = self.marks.get(&'^').copied() {
                    buf.set_cursor(p);
                }
                self.enter_insert(buf, n, None, false);
            }
            OpenBelow | OpenAbove => {
                let below = act == OpenBelow;
                self.open_line(buf, below);
                self.enter_insert(buf, n, Some(below), false);
            }
            Visual | VisualLine => {
                let want = if act == Visual { Mode::Visual } else { Mode::VisualLine };
                if self.mode == want {
                    self.exit_visual(buf);
                } else if visual {
                    self.mode = want;
                } else {
                    self.enter_visual(buf, want == Mode::VisualLine);
                    if let Some(c) = count {
                        // `3v` selects three characters.
                        let end = Pos::new(cur.line, (cur.col + c - 1).min(len.saturating_sub(1)));
                        buf.set_cursor(end);
                    }
                }
            }
            Reselect => {
                if let Some((a, c, linewise)) = self.last_visual {
                    let current = visual.then(|| (self.visual_anchor, buf.cursor, self.mode == Mode::VisualLine));
                    let last_line = buf.len() - 1;
                    let clamp = |p: Pos| {
                        let line = p.line.min(last_line);
                        Pos::new(line, p.col.min(buf.line_len(line).saturating_sub(1)))
                    };
                    self.visual_anchor = clamp(a);
                    buf.set_cursor(clamp(c));
                    self.mode = if linewise { Mode::VisualLine } else { Mode::Visual };
                    // Swapping means `gv` twice comes back.
                    self.last_visual = current.or(self.last_visual);
                }
            }
            VisualSwap => {
                if visual {
                    let a = self.visual_anchor;
                    self.visual_anchor = buf.cursor;
                    buf.set_cursor(a);
                }
            }
            ReplaceMode => self.enter_insert(buf, n, None, true),
            ReplaceChar(c) => {
                if visual {
                    let r = self.visual_range(buf, false);
                    self.exit_visual(buf);
                    let (start, end) = if r.linewise {
                        (Pos::new(r.start.line, 0), Pos::new(r.end.line, buf.line_len(r.end.line)))
                    } else {
                        (r.start, r.end)
                    };
                    let text = buf.text_in(start, end);
                    let changed: String = text.chars().map(|x| if x == '\n' { '\n' } else { c }).collect();
                    buf.edit(start, end, &changed);
                    buf.set_cursor(start);
                    return fx;
                }
                if c == '\n' {
                    // `r<Enter>` replaces the character with a line break;
                    // the count is ignored there, as in vim.
                    if len == 0 {
                        return fx;
                    }
                    buf.edit(cur, Pos::new(cur.line, cur.col + 1), "\n");
                    return fx;
                }
                if cur.col + n > len {
                    return fx;
                }
                let end = Pos::new(cur.line, cur.col + n);
                buf.edit(cur, end, &c.to_string().repeat(n));
                buf.set_cursor(Pos::new(cur.line, cur.col + n - 1));
            }
            SetMark(c) => {
                self.marks.insert(c, cur);
            }
            Record(c) => {
                s.recording = Some((c, Vec::new()));
            }
            StopRecord => {
                if let Some((c, mut keys)) = s.recording.take() {
                    // The `q` that stopped it is not part of the macro.
                    keys.pop();
                    let text = crate::keys_to_text(&keys);
                    if c.is_ascii_uppercase() {
                        s.registers.entry(c.to_ascii_lowercase()).or_default().text.push_str(&text);
                    } else {
                        s.registers.insert(c, Register { text, linewise: false });
                    }
                }
            }
            PlayMacro(c) => {
                let name = if c == '@' {
                    match s.last_macro {
                        Some(m) => m,
                        None => {
                            self.error("E748: No previously used register");
                            return fx;
                        }
                    }
                } else {
                    c
                };
                let Some(r) = self.fetch(s, host, Some(name)) else {
                    return fx;
                };
                if self.macro_depth > 50 {
                    self.error("E132: macro nested too deep");
                    return fx;
                }
                s.last_macro = Some(name);
                let keys = crate::text_to_keys(&r.text);
                self.macro_depth += 1;
                for _ in 0..n {
                    fx.extend(self.feed(&keys, buf, s, host, ctx));
                    if self.failed {
                        break;
                    }
                }
                self.macro_depth -= 1;
            }
            SaveQuit => fx.push(Effect::SaveClose),
            QuitDiscard => fx.push(Effect::ClosePaneForce),
            ExPrompt => {
                let prefill = if visual {
                    "'<,'>".to_string()
                } else if let Some(c) = count {
                    if c > 1 { format!(".,.+{}", c - 1) } else { ".".to_string() }
                } else {
                    String::new()
                };
                self.open_cmdline(':', &prefill, buf);
            }
            SearchPrompt { forward } => self.open_cmdline(if forward { '/' } else { '?' }, "", buf),
            ScrollCursor { at, first_non_blank: fnb } => {
                let line = count.map_or(cur.line, |c| c.saturating_sub(1).min(last));
                let vis = ctx.visible.max(1);
                let top = match at {
                    ScrollAt::Top => line,
                    ScrollAt::Middle => line.saturating_sub(vis / 2),
                    ScrollAt::Bottom => (line + 1).saturating_sub(vis),
                };
                fx.push(Effect::ScrollTo(top));
                let col = if fnb { first_non_blank(buf, line) } else { cur.col };
                buf.set_cursor(Pos::new(line, col));
                self.clamp_normal(buf);
            }
            ScrollHalf { down } => {
                let amount = count.unwrap_or(ctx.visible / 2).max(1);
                let line = if down { (cur.line + amount).min(last) } else { cur.line.saturating_sub(amount) };
                let top = if down { (ctx.top + amount).min(last) } else { ctx.top.saturating_sub(amount) };
                fx.push(Effect::ScrollTo(top));
                self.move_to_line(buf, line);
            }
            ScrollPage { down } => {
                let page = ctx.visible.saturating_sub(2).max(1);
                let (top, line) = if down {
                    let top = (ctx.top + page * n).min(last);
                    (top, top)
                } else {
                    let top = ctx.top.saturating_sub(page * n);
                    (top, (top + ctx.visible.max(1) - 1).min(last))
                };
                fx.push(Effect::ScrollTo(top));
                self.move_to_line(buf, line);
            }
            ScrollLine { down } => {
                let top = if down { (ctx.top + n).min(last) } else { ctx.top.saturating_sub(n) };
                fx.push(Effect::ScrollTo(top));
                let vis = ctx.visible.max(1);
                if cur.line < top {
                    self.move_to_line(buf, top);
                } else if cur.line >= top + vis {
                    self.move_to_line(buf, top + vis - 1);
                }
            }
            Increment { by } => self.increment(buf, by * n as i64),
            JumpOlder => {
                if self.jump_at > 0 {
                    if self.jump_at == self.jumps.len() {
                        self.jumps.push(cur);
                    }
                    self.jump_at -= 1;
                    let p = self.jumps[self.jump_at];
                    buf.set_cursor(Pos::new(p.line.min(last), p.col));
                    self.clamp_normal(buf);
                }
            }
            JumpNewer => {
                if self.jump_at + 1 < self.jumps.len() {
                    self.jump_at += 1;
                    let p = self.jumps[self.jump_at];
                    buf.set_cursor(Pos::new(p.line.min(last), p.col));
                    self.clamp_normal(buf);
                }
            }
            Window(c) => match c {
                'v' => fx.push(Effect::SplitRight),
                's' | 'n' => fx.push(Effect::SplitDown),
                'h' => fx.push(Effect::Focus(Dir::Left)),
                'l' | 'w' => fx.push(Effect::Focus(Dir::Right)),
                'k' | 'p' => fx.push(Effect::Focus(Dir::Up)),
                'j' => fx.push(Effect::Focus(Dir::Down)),
                'q' | 'c' => fx.push(Effect::ClosePane),
                _ => self.error(&format!("Ctrl+W {c} is not available here")),
            },
            FileInfo => {
                let name = buf.path().map(|p| p.display().to_string()).unwrap_or_else(|| "[No Name]".into());
                let pct = (cur.line + 1) * 100 / buf.len();
                self.say(&format!(
                    "\"{name}\"{} {} lines --{pct}%--",
                    if buf.modified() { " [Modified]" } else { "" },
                    buf.len()
                ));
            }
            RepeatSubstitute => return self.ex("s", buf, s, host, ctx),
            Escape => {
                if visual {
                    self.exit_visual(buf);
                }
                self.clamp_normal(buf);
            }
            VisualBlock => self.error("visual block mode is not available here; V selects lines"),
        }
        fx
    }

    /// Moves to a line keeping the wanted column, for the scroll commands.
    fn move_to_line(&mut self, buf: &mut Buffer, line: usize) {
        let goal = self.want_col.unwrap_or(buf.cursor.col);
        let len = buf.line_len(line);
        let col = if goal == usize::MAX { len } else { goal.min(len) };
        buf.set_cursor(Pos::new(line, col));
        self.want_col = Some(goal);
        self.clamp_normal(buf);
    }
}
