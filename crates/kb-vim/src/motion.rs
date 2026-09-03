//! Motions, text objects and search: where a command's range ends.
//!
//! Every motion answers with a position and a kind — exclusive, inclusive or
//! linewise — because the operators need both. `dw` and `de` from the same
//! spot differ by exactly one character, and that one character is the kind.

use kb_edit::{Buffer, Pos};

use crate::parse::{Motion, Obj};
use crate::{first_non_blank, order, Ctx, Range, Session, Vim};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Dest {
    pub pos: Pos,
    pub kind: Kind,
}

/// Motions that go on the jump list, so Ctrl+O can come back from them.
pub(crate) fn is_jump(m: Motion) -> bool {
    use Motion::*;
    matches!(
        m,
        GotoLine
            | GotoLineTop
            | ParaFwd
            | ParaBack
            | SentenceFwd
            | SentenceBack
            | Match
            | ScreenTop
            | ScreenMiddle
            | ScreenBottom
            | SearchNext { .. }
            | WordSearch { .. }
            | Mark { .. }
    )
}

/// What a character is to word motions. An empty line is its own class:
/// `w` and `b` stop on one, where a line of only blanks is skipped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Blank,
    Punct,
    Word,
    Empty,
}

fn class_of(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn class(buf: &Buffer, p: Pos, big: bool) -> Class {
    let line = buf.line(p.line);
    if line.is_empty() {
        return Class::Empty;
    }
    match line.chars().nth(p.col) {
        // The position after the last character is the line break.
        None => Class::Blank,
        Some(c) => class_of(c, big),
    }
}

/// The next position, where the position after a line's last character
/// counts and steps onto the next line's first.
fn next(buf: &Buffer, p: Pos) -> Option<Pos> {
    if p.col < buf.line_len(p.line) {
        Some(Pos::new(p.line, p.col + 1))
    } else if p.line + 1 < buf.len() {
        Some(Pos::new(p.line + 1, 0))
    } else {
        None
    }
}

fn prev(buf: &Buffer, p: Pos) -> Option<Pos> {
    if p.col > 0 {
        Some(Pos::new(p.line, p.col - 1))
    } else if p.line > 0 {
        Some(Pos::new(p.line - 1, buf.line_len(p.line - 1)))
    } else {
        None
    }
}

fn chars_of(buf: &Buffer, line: usize) -> Vec<char> {
    buf.line(line).chars().collect()
}

/// `w`: the start of the next word. `stop_at_eol` is the operator rule: when
/// the word moved over was the last on its line, the range ends with the
/// line rather than reaching into the next one, so `dw` never joins lines.
fn word_fwd(buf: &Buffer, from: Pos, big: bool, stop_at_eol: bool) -> Pos {
    let mut q = from;
    let c0 = class(buf, q, big);
    match c0 {
        Class::Word | Class::Punct => loop {
            match next(buf, q) {
                Some(n) => {
                    q = n;
                    if class(buf, q, big) != c0 {
                        break;
                    }
                }
                None => return q,
            }
        },
        Class::Empty => match next(buf, q) {
            Some(n) => q = n,
            None => return q,
        },
        Class::Blank => {}
    }
    while class(buf, q, big) == Class::Blank {
        if stop_at_eol && q.col >= buf.line_len(q.line) {
            return q;
        }
        match next(buf, q) {
            Some(n) => q = n,
            None => return q,
        }
    }
    q
}

/// `e`: the end of the word ahead.
fn word_end(buf: &Buffer, from: Pos, big: bool) -> Option<Pos> {
    let mut q = next(buf, from)?;
    while matches!(class(buf, q, big), Class::Blank | Class::Empty) {
        q = next(buf, q)?;
    }
    let c = class(buf, q, big);
    while let Some(n) = next(buf, q) {
        if class(buf, n, big) != c {
            break;
        }
        q = n;
    }
    Some(q)
}

/// `b`: the start of the word behind.
fn word_back(buf: &Buffer, from: Pos, big: bool) -> Option<Pos> {
    let mut q = prev(buf, from)?;
    while class(buf, q, big) == Class::Blank {
        match prev(buf, q) {
            Some(p) => q = p,
            None => return Some(q),
        }
    }
    if class(buf, q, big) == Class::Empty {
        return Some(q);
    }
    let c = class(buf, q, big);
    while let Some(p) = prev(buf, q) {
        if class(buf, p, big) != c {
            break;
        }
        q = p;
    }
    Some(q)
}

/// `ge`: the end of the word behind.
fn word_end_back(buf: &Buffer, from: Pos, big: bool) -> Option<Pos> {
    let mut q = from;
    let c0 = class(buf, q, big);
    if matches!(c0, Class::Word | Class::Punct) {
        loop {
            q = prev(buf, q)?;
            if class(buf, q, big) != c0 {
                break;
            }
        }
    } else if c0 == Class::Empty {
        q = prev(buf, q)?;
    }
    while class(buf, q, big) == Class::Blank {
        q = prev(buf, q)?;
    }
    Some(q)
}

/// `f`, `t`, `F`, `T` on the caret's line. `skip_adjacent` is what `;`
/// after a `t` needs: the target one step away would leave `;` stuck.
fn find_char(buf: &Buffer, from: Pos, c: char, back: bool, till: bool, count: usize, skip_adjacent: bool) -> Option<Pos> {
    let chars = chars_of(buf, from.line);
    let mut col = from.col;
    if skip_adjacent && till {
        col = if back { col.checked_sub(1)? } else { col + 1 };
    }
    for _ in 0..count.max(1) {
        col = if back {
            (0..col).rev().find(|i| chars[*i] == c)?
        } else {
            ((col + 1)..chars.len()).find(|i| chars[*i] == c)?
        };
    }
    if till {
        col = if back { col + 1 } else { col.checked_sub(1)? };
    }
    Some(Pos::new(from.line, col))
}

fn blank_line(buf: &Buffer, line: usize) -> bool {
    buf.line(line).trim().is_empty()
}

/// `}`: the blank line after the paragraph, or the end of the buffer.
fn para_fwd(buf: &Buffer, from: usize, count: usize) -> Pos {
    let mut line = from;
    let last = buf.len() - 1;
    for _ in 0..count.max(1) {
        while line < last && blank_line(buf, line) {
            line += 1;
        }
        while line < last && !blank_line(buf, line) {
            line += 1;
        }
        if line == last {
            break;
        }
    }
    if line == last && !blank_line(buf, last) {
        return Pos::new(last, buf.line_len(last));
    }
    Pos::new(line, 0)
}

fn para_back(buf: &Buffer, from: usize, count: usize) -> Pos {
    let mut line = from;
    for _ in 0..count.max(1) {
        while line > 0 && blank_line(buf, line) {
            line -= 1;
        }
        while line > 0 && !blank_line(buf, line) {
            line -= 1;
        }
        if line == 0 {
            break;
        }
    }
    Pos::new(line, 0)
}

/// Where sentences begin: after `.`, `!` or `?` (closing quotes and
/// brackets allowed) followed by white space, and at the start of every
/// paragraph. Blank lines count as a sentence of their own, as in vim.
fn sentence_starts(buf: &Buffer) -> Vec<Pos> {
    let mut out = Vec::new();
    let mut at_start = true;
    for (li, line) in buf.lines().iter().enumerate() {
        if line.trim().is_empty() {
            out.push(Pos::new(li, 0));
            at_start = true;
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if at_start && !c.is_whitespace() {
                out.push(Pos::new(li, i));
                at_start = false;
            }
            if matches!(c, '.' | '!' | '?') {
                let mut j = i + 1;
                while j < chars.len() && matches!(chars[j], ')' | ']' | '"' | '\'') {
                    j += 1;
                }
                if j >= chars.len() || chars[j].is_whitespace() {
                    at_start = true;
                    i = j;
                    continue;
                }
            }
            i += 1;
        }
        // A line break after a sentence end is white space too.
    }
    out
}

fn match_bracket(buf: &Buffer, from: Pos) -> Option<Pos> {
    let chars = chars_of(buf, from.line);
    let col = (from.col..chars.len()).find(|i| matches!(chars[*i], '(' | ')' | '[' | ']' | '{' | '}'))?;
    let c = chars[col];
    let at = Pos::new(from.line, col);
    match c {
        '(' => scan_forward(buf, at, '(', ')'),
        '[' => scan_forward(buf, at, '[', ']'),
        '{' => scan_forward(buf, at, '{', '}'),
        ')' => scan_backward(buf, at, '(', ')'),
        ']' => scan_backward(buf, at, '[', ']'),
        _ => scan_backward(buf, at, '{', '}'),
    }
}

/// The closer matching the opener at `from`. Bounded, like the editor's own
/// bracket matching: an unmatched `{` at the top of a big file must not
/// cost a full walk on every `%`.
fn scan_forward(buf: &Buffer, from: Pos, open: char, close: char) -> Option<Pos> {
    let mut depth = 0usize;
    let mut budget = 200_000usize;
    for line in from.line..buf.len() {
        let skip = if line == from.line { from.col + 1 } else { 0 };
        for (col, c) in buf.line(line).chars().enumerate().skip(skip) {
            budget = budget.checked_sub(1)?;
            if c == open {
                depth += 1;
            } else if c == close {
                if depth == 0 {
                    return Some(Pos::new(line, col));
                }
                depth -= 1;
            }
        }
    }
    None
}

fn scan_backward(buf: &Buffer, from: Pos, open: char, close: char) -> Option<Pos> {
    let mut depth = 0usize;
    let mut budget = 200_000usize;
    for line in (0..=from.line).rev() {
        let chars = chars_of(buf, line);
        let end = if line == from.line { from.col } else { chars.len() };
        for col in (0..end).rev() {
            budget = budget.checked_sub(1)?;
            let c = chars[col];
            if c == close {
                depth += 1;
            } else if c == open {
                if depth == 0 {
                    return Some(Pos::new(line, col));
                }
                depth -= 1;
            }
        }
    }
    None
}

impl Vim {
    /// Where a motion lands from the caret. `None` when it cannot move at
    /// all, which cancels whatever was going to use it.
    pub(crate) fn motion(&mut self, buf: &Buffer, s: &mut Session, ctx: &Ctx, m: Motion, count: Option<usize>, for_op: bool) -> Option<Dest> {
        use Motion::*;
        let n = count.unwrap_or(1).max(1);
        let cur = buf.cursor;
        let last = buf.len() - 1;
        let len = buf.line_len(cur.line);
        let ex = |pos: Pos| Some(Dest { pos, kind: Kind::Exclusive });
        let inc = |pos: Pos| Some(Dest { pos, kind: Kind::Inclusive });
        let lw = |line: usize| Some(Dest { pos: Pos::new(line, first_non_blank(buf, line)), kind: Kind::Linewise });

        match m {
            Left => {
                if cur.col == 0 {
                    return None;
                }
                ex(Pos::new(cur.line, cur.col.saturating_sub(n)))
            }
            LeftWrap => {
                let mut p = cur;
                for _ in 0..n {
                    p = prev(buf, p).unwrap_or(p);
                    if p.col >= buf.line_len(p.line) && buf.line_len(p.line) > 0 && !for_op {
                        p.col = buf.line_len(p.line) - 1;
                    }
                }
                if p == cur {
                    return None;
                }
                ex(p)
            }
            Right => {
                // In normal mode the caret stops on the last character; an
                // operator may reach past it, which is what makes `dl` (and
                // so `x`) work on the last character of a line.
                let limit = if for_op || self.in_visual() || self.one_shot { len } else { len.saturating_sub(1) };
                if cur.col >= limit {
                    return None;
                }
                ex(Pos::new(cur.line, (cur.col + n).min(limit)))
            }
            RightWrap => {
                let mut p = cur;
                for _ in 0..n {
                    let np = next(buf, p).unwrap_or(p);
                    // Space skips the line break rather than resting on it.
                    p = if np.col >= buf.line_len(np.line) && buf.line_len(np.line) > 0 && !for_op {
                        next(buf, np).unwrap_or(np)
                    } else {
                        np
                    };
                }
                if p == cur {
                    return None;
                }
                ex(p)
            }
            Up | Down => {
                let target = if m == Up {
                    if cur.line == 0 {
                        return None;
                    }
                    cur.line.saturating_sub(n)
                } else {
                    if cur.line == last {
                        return None;
                    }
                    (cur.line + n).min(last)
                };
                let goal = self.want_col.unwrap_or(cur.col);
                self.want_col = Some(goal);
                let tlen = buf.line_len(target);
                let col = if goal == usize::MAX { tlen } else { goal.min(tlen) };
                Some(Dest { pos: Pos::new(target, col), kind: Kind::Linewise })
            }
            LineStart => ex(Pos::new(cur.line, 0)),
            FirstNonBlank => ex(Pos::new(cur.line, first_non_blank(buf, cur.line))),
            LineEnd => {
                let line = (cur.line + n - 1).min(last);
                inc(Pos::new(line, buf.line_len(line)))
            }
            LastNonBlank => {
                let line = (cur.line + n - 1).min(last);
                let col = buf.line(line).trim_end().chars().count().saturating_sub(1);
                inc(Pos::new(line, col))
            }
            Column => ex(Pos::new(cur.line, (n - 1).min(len.saturating_sub(1)))),
            WordFwd { big } => {
                let mut p = cur;
                for i in 0..n {
                    let q = word_fwd(buf, p, big, for_op && i + 1 == n);
                    if q == p {
                        break;
                    }
                    p = q;
                }
                if p == cur {
                    return None;
                }
                ex(p)
            }
            WordBack { big } => {
                let mut p = cur;
                for _ in 0..n {
                    p = word_back(buf, p, big)?;
                }
                ex(p)
            }
            WordEnd { big } => {
                let mut p = cur;
                for _ in 0..n {
                    p = word_end(buf, p, big)?;
                }
                inc(p)
            }
            WordEndBack { big } => {
                let mut p = cur;
                for _ in 0..n {
                    p = word_end_back(buf, p, big)?;
                }
                inc(p)
            }
            Find { c, back, till } => {
                s.last_find = Some((c, back, till));
                let p = find_char(buf, cur, c, back, till, n, false)?;
                if back { ex(p) } else { inc(p) }
            }
            RepeatFind { reverse } => {
                let (c, back, till) = s.last_find?;
                let back = back != reverse;
                let p = find_char(buf, cur, c, back, till, n, true)?;
                if back { ex(p) } else { inc(p) }
            }
            GotoLine => lw(count.map_or(last, |c| c.saturating_sub(1).min(last))),
            GotoLineTop => lw(count.map_or(0, |c| c.saturating_sub(1).min(last))),
            ParaFwd => ex(para_fwd(buf, cur.line, n)),
            ParaBack => ex(para_back(buf, cur.line, n)),
            SentenceFwd => {
                let starts = sentence_starts(buf);
                let mut p = cur;
                for _ in 0..n {
                    p = match starts.iter().find(|s| **s > p) {
                        Some(s) => *s,
                        None => Pos::new(last, buf.line_len(last)),
                    };
                }
                ex(p)
            }
            SentenceBack => {
                let starts = sentence_starts(buf);
                let mut p = cur;
                for _ in 0..n {
                    p = *starts.iter().rev().find(|s| **s < p)?;
                }
                ex(p)
            }
            Match => match count {
                // `50%` is halfway down the file.
                Some(c) => lw(((c.min(100) * buf.len()).div_ceil(100)).saturating_sub(1).min(last)),
                None => inc(match_bracket(buf, cur)?),
            },
            ScreenTop | ScreenMiddle | ScreenBottom => {
                let top = ctx.top.min(last);
                let bottom = (ctx.top + ctx.visible.max(1) - 1).min(last);
                let line = match m {
                    ScreenTop => (top + n - 1).min(bottom),
                    ScreenBottom => bottom.saturating_sub(n - 1).max(top),
                    _ => top + (bottom - top) / 2,
                };
                lw(line)
            }
            LineDownFirst => {
                if cur.line == last {
                    return None;
                }
                lw((cur.line + n).min(last))
            }
            LineUpFirst => {
                if cur.line == 0 {
                    return None;
                }
                lw(cur.line.saturating_sub(n))
            }
            CurrentLineFirst => lw((cur.line + n - 1).min(last)),
            // Answered by the command line, never evaluated here.
            SearchPrompt { .. } => None,
            SearchNext { reverse } => {
                let (pattern, forward) = s.last_search.clone()?;
                let forward = forward != reverse;
                let p = self.search(buf, s, &pattern, forward, cur, n)?;
                ex(p)
            }
            WordSearch { forward, whole } => {
                let chars = chars_of(buf, cur.line);
                // The word under the caret, or the next one on the line.
                let mut i = cur.col.min(chars.len());
                while i < chars.len() && !(chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                if i >= chars.len() {
                    self.error("E348: No string under cursor");
                    return None;
                }
                let mut a = i;
                while a > 0 && (chars[a - 1].is_alphanumeric() || chars[a - 1] == '_') {
                    a -= 1;
                }
                let mut b = i;
                while b < chars.len() && (chars[b].is_alphanumeric() || chars[b] == '_') {
                    b += 1;
                }
                let word: String = chars[a..b].iter().collect();
                let escaped: String = word.chars().flat_map(|c| if "\\/.*$^~[]".contains(c) { vec!['\\', c] } else { vec![c] }).collect();
                let pattern = if whole { format!("\\<{escaped}\\>") } else { escaped };
                s.set_search(&pattern, forward);
                s.search_history.retain(|h| *h != pattern);
                s.search_history.push(pattern.clone());
                // From the start of the word, so the match under the caret
                // is skipped rather than found again.
                let from = Pos::new(cur.line, a);
                let p = self.search(buf, s, &pattern, forward, from, n)?;
                ex(p)
            }
            Mark { c, exact } => {
                let p = match c {
                    '\'' | '`' => self.marks.get(&'\'').copied().unwrap_or(Pos::new(0, 0)),
                    _ => match self.marks.get(&c) {
                        Some(p) => *p,
                        None => {
                            self.error("E20: Mark not set");
                            return None;
                        }
                    },
                };
                let line = p.line.min(last);
                if exact {
                    ex(Pos::new(line, p.col.min(buf.line_len(line))))
                } else {
                    lw(line)
                }
            }
        }
    }

    /// Finds `pattern` from `from`, `count` times, wrapping at the ends of
    /// the buffer and saying so.
    pub(crate) fn search(&mut self, buf: &Buffer, s: &Session, pattern: &str, forward: bool, from: Pos, count: usize) -> Option<Pos> {
        let re = match s.compile(pattern) {
            Ok(r) => r,
            Err(e) => {
                self.error(&format!("E: {e}"));
                return None;
            }
        };
        let mut pos = from;
        let mut wrapped = false;
        for _ in 0..count.max(1) {
            match search_once(buf, &re, forward, pos) {
                Some((p, w)) => {
                    pos = p;
                    wrapped |= w;
                }
                None => {
                    self.error(&format!("E486: Pattern not found: {pattern}"));
                    return None;
                }
            }
        }
        if wrapped {
            self.say(if forward { "search hit BOTTOM, continuing at TOP" } else { "search hit TOP, continuing at BOTTOM" });
        }
        Some(pos)
    }

    /// The span of a text object at the caret.
    pub(crate) fn object_range(&self, buf: &Buffer, obj: Obj, count: usize) -> Option<Range> {
        let cur = buf.cursor;
        match obj {
            Obj::Word { big, around } => word_object(buf, cur, big, around, count),
            Obj::Paragraph { around } => paragraph_object(buf, cur.line, around, count),
            Obj::Sentence { around } => sentence_object(buf, cur, around),
            Obj::Bracket { open, around } => bracket_object(buf, cur, open, around, count),
            Obj::Quote { q, around } => quote_object(buf, cur, q, around),
            Obj::Tag { around } => tag_object(buf, cur, around, count),
        }
    }
}

/// One search step. Returns the position and whether it wrapped.
fn search_once(buf: &Buffer, re: &crate::Regex, forward: bool, from: Pos) -> Option<(Pos, bool)> {
    let total = buf.len();
    if forward {
        // The rest of this line, every line after, then around to the
        // start and back up to the caret.
        let chars = chars_of(buf, from.line);
        if let Some(m) = re.find_from(&chars, from.col + 1) {
            return Some((Pos::new(from.line, m.start), false));
        }
        for line in from.line + 1..total {
            if let Some(m) = re.find_from(&chars_of(buf, line), 0) {
                return Some((Pos::new(line, m.start), false));
            }
        }
        for line in 0..=from.line {
            if let Some(m) = re.find_from(&chars_of(buf, line), 0) {
                if line < from.line || m.start <= from.col {
                    return Some((Pos::new(line, m.start), true));
                }
            }
        }
        None
    } else {
        let chars = chars_of(buf, from.line);
        if let Some(m) = re.find_last_before(&chars, from.col) {
            return Some((Pos::new(from.line, m.start), false));
        }
        for line in (0..from.line).rev() {
            let chars = chars_of(buf, line);
            if let Some(m) = re.find_last_before(&chars, chars.len() + 1) {
                return Some((Pos::new(line, m.start), false));
            }
        }
        for line in (from.line..total).rev() {
            let chars = chars_of(buf, line);
            if let Some(m) = re.find_last_before(&chars, chars.len() + 1) {
                if line > from.line || m.start >= from.col {
                    return Some((Pos::new(line, m.start), true));
                }
            }
        }
        None
    }
}

fn word_object(buf: &Buffer, cur: Pos, big: bool, around: bool, count: usize) -> Option<Range> {
    let chars = chars_of(buf, cur.line);
    if chars.is_empty() {
        return None;
    }
    let col = cur.col.min(chars.len() - 1);
    let cls = |i: usize| class_of(chars[i], big);
    let c = cls(col);
    let mut start = col;
    while start > 0 && cls(start - 1) == c {
        start -= 1;
    }
    let mut end = col + 1;
    while end < chars.len() && cls(end) == c {
        end += 1;
    }
    // Each further count takes the next run — a word, or the space before
    // the next word — the way `2iw` and `2aw` do.
    let extend_run = |end: &mut usize| {
        if *end < chars.len() {
            let c = cls(*end);
            while *end < chars.len() && cls(*end) == c {
                *end += 1;
            }
        }
    };
    if around {
        if c != Class::Blank {
            if end < chars.len() && cls(end) == Class::Blank {
                extend_run(&mut end);
            } else {
                while start > 0 && cls(start - 1) == Class::Blank {
                    start -= 1;
                }
            }
        } else {
            extend_run(&mut end);
        }
        for _ in 1..count {
            extend_run(&mut end);
            extend_run(&mut end);
        }
    } else {
        for _ in 1..count {
            extend_run(&mut end);
        }
    }
    Some(Range { start: Pos::new(cur.line, start), end: Pos::new(cur.line, end), linewise: false })
}

fn paragraph_object(buf: &Buffer, line: usize, around: bool, count: usize) -> Option<Range> {
    let last = buf.len() - 1;
    let kind = blank_line(buf, line);
    let mut first = line;
    while first > 0 && blank_line(buf, first - 1) == kind {
        first -= 1;
    }
    let mut end = line;
    while end < last && blank_line(buf, end + 1) == kind {
        end += 1;
    }
    let extend = |end: &mut usize| {
        if *end < last {
            let k = blank_line(buf, *end + 1);
            while *end < last && blank_line(buf, *end + 1) == k {
                *end += 1;
            }
        }
    };
    if around {
        if end < last {
            extend(&mut end);
        } else if !kind {
            while first > 0 && blank_line(buf, first - 1) {
                first -= 1;
            }
        }
    }
    for _ in 1..count {
        extend(&mut end);
        if around {
            extend(&mut end);
        }
    }
    Some(Range::lines(first, end))
}

fn sentence_object(buf: &Buffer, cur: Pos, around: bool) -> Option<Range> {
    let starts = sentence_starts(buf);
    let start = *starts.iter().rev().find(|s| **s <= cur).unwrap_or(&Pos::new(0, 0));
    let last = buf.len() - 1;
    let mut end = starts.iter().find(|s| **s > cur).copied().unwrap_or(Pos::new(last, buf.line_len(last)));
    if !around {
        // Back off the white space before the next sentence.
        while let Some(p) = prev(buf, end) {
            if p <= start {
                break;
            }
            let c = buf.line(p.line).chars().nth(p.col);
            if c.is_some_and(|c| !c.is_whitespace()) {
                break;
            }
            end = p;
        }
    }
    Some(Range { start, end, linewise: false })
}

fn bracket_object(buf: &Buffer, cur: Pos, open: char, around: bool, count: usize) -> Option<Range> {
    let close = match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => '>',
    };
    let under = buf.line(cur.line).chars().nth(cur.col);
    let mut open_at = if under == Some(close) {
        scan_backward(buf, cur, open, close)?
    } else if under == Some(open) {
        cur
    } else {
        // Backwards from the caret, counting closers so a nested pair
        // behind the caret is stepped over.
        let mut depth = 0usize;
        let mut p = cur;
        loop {
            p = prev(buf, p)?;
            match buf.line(p.line).chars().nth(p.col) {
                Some(c) if c == close => depth += 1,
                Some(c) if c == open => {
                    if depth == 0 {
                        break p;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
    };
    for _ in 1..count {
        open_at = scan_backward(buf, open_at, open, close)?;
    }
    let close_at = scan_forward(buf, open_at, open, close)?;
    if around {
        return Some(Range { start: open_at, end: Pos::new(close_at.line, close_at.col + 1), linewise: false });
    }
    // A block whose opener ends its line and whose closer starts its own
    // is the lines between: `di{` on a function body leaves the braces on
    // their lines rather than gluing them together.
    let opener_ends_line = open_at.col + 1 >= buf.line_len(open_at.line);
    let closer_starts_line = first_non_blank(buf, close_at.line) >= close_at.col;
    if opener_ends_line && closer_starts_line && close_at.line > open_at.line + 1 {
        return Some(Range::lines(open_at.line + 1, close_at.line - 1));
    }
    let start = next(buf, open_at).unwrap_or(open_at);
    Some(Range { start, end: close_at, linewise: false })
}

fn quote_object(buf: &Buffer, cur: Pos, q: char, around: bool) -> Option<Range> {
    let chars = chars_of(buf, cur.line);
    let quotes: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter(|(i, c)| **c == q && (*i == 0 || chars[i - 1] != '\\'))
        .map(|(i, _)| i)
        .collect();
    let col = cur.col.min(chars.len().saturating_sub(1));
    let (open, close) = match quotes.iter().position(|i| *i == col) {
        // On a quote: even ones open, odd ones close.
        Some(k) if k % 2 == 0 => (quotes[k], *quotes.get(k + 1)?),
        Some(k) => (quotes[k - 1], quotes[k]),
        None => {
            let before = quotes.iter().filter(|i| **i < col).count();
            if before % 2 == 1 {
                // Inside a pair.
                (quotes[before - 1], *quotes.get(before)?)
            } else {
                // Between pairs, or before the first: the next pair.
                (*quotes.get(before)?, *quotes.get(before + 1)?)
            }
        }
    };
    if !around {
        return Some(Range { start: Pos::new(cur.line, open + 1), end: Pos::new(cur.line, close), linewise: false });
    }
    let mut start = open;
    let mut end = close + 1;
    if end < chars.len() && chars[end].is_whitespace() {
        while end < chars.len() && chars[end].is_whitespace() {
            end += 1;
        }
    } else {
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
    }
    Some(Range { start: Pos::new(cur.line, start), end: Pos::new(cur.line, end), linewise: false })
}

/// `it` / `at`: the innermost `<tag>…</tag>` around the caret. Textual, so
/// a `<` in a string literal will confuse it, the same way vim's does.
fn tag_object(buf: &Buffer, cur: Pos, around: bool, count: usize) -> Option<Range> {
    // Flatten the buffer so tags can span lines.
    let mut text: Vec<char> = Vec::new();
    let mut starts = Vec::with_capacity(buf.len());
    for line in buf.lines() {
        starts.push(text.len());
        text.extend(line.chars());
        text.push('\n');
    }
    let at = starts[cur.line] + cur.col;
    let to_pos = |i: usize| {
        let line = starts.iter().rposition(|s| *s <= i).unwrap_or(0);
        Pos::new(line, i - starts[line])
    };
    // Every tag, in order: (start, end after '>', name, closing?)
    let mut tags: Vec<(usize, usize, String, bool)> = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if text[i] == '<' {
            let closing = text.get(i + 1) == Some(&'/');
            let mut j = i + 1 + closing as usize;
            let name_start = j;
            while j < text.len() && (text[j].is_alphanumeric() || matches!(text[j], '-' | '_' | ':')) {
                j += 1;
            }
            let name: String = text[name_start..j].iter().collect();
            if name.is_empty() {
                i += 1;
                continue;
            }
            while j < text.len() && text[j] != '>' {
                j += 1;
            }
            if j >= text.len() {
                break;
            }
            let self_closing = text[j - 1] == '/';
            if !self_closing {
                tags.push((i, j + 1, name, closing));
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    // Pair them with a stack, keeping the pairs that enclose the caret.
    let mut stack: Vec<usize> = Vec::new();
    let mut enclosing: Vec<(usize, usize, usize, usize)> = Vec::new();
    for (k, (s, e, name, closing)) in tags.iter().enumerate() {
        if !closing {
            stack.push(k);
            continue;
        }
        if let Some(pos) = stack.iter().rposition(|o| tags[*o].2 == *name) {
            let o = stack[pos];
            stack.truncate(pos);
            let (os, oe, _, _) = &tags[o];
            if *os <= at && at < *e {
                enclosing.push((*os, *oe, *s, *e));
            }
        }
    }
    // Innermost first: the one opened last.
    enclosing.sort_by_key(|(os, ..)| std::cmp::Reverse(*os));
    let (os, oe, cs, ce) = *enclosing.get(count.max(1) - 1)?;
    Some(if around {
        Range { start: to_pos(os), end: to_pos(ce), linewise: false }
    } else {
        Range { start: to_pos(oe), end: to_pos(cs), linewise: false }
    })
}

/// The lines a range covers, for the operators that work by line whatever
/// the range's shape.
pub(crate) fn lines_of(r: &Range) -> (usize, usize) {
    if r.linewise {
        return (r.start.line, r.end.line);
    }
    let (a, b) = order(r.start, r.end);
    // An exclusive end at the start of a line does not include that line.
    let last = if b.col == 0 && b.line > a.line { b.line - 1 } else { b.line };
    (a.line, last)
}
