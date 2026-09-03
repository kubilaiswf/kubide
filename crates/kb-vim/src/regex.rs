//! Regular expressions in vim's dialect.
//!
//! Vim's patterns are not POSIX and not Perl: `\(` groups and `\|` alternates
//! in the default "magic" mode, `\<` and `\>` are word edges, `\{n,m}` is a
//! counted repeat, and `\v` at the front switches to "very magic" where the
//! backslashes fall away. Someone typing `/\vfoo(bar|baz)` expects exactly
//! that, so this speaks the dialect rather than translating it.
//!
//! A backtracking matcher over characters. Lines are short and patterns are
//! typed by hand; the pathological cases that make backtracking famous need
//! both a hostile pattern and a hostile line, and a code editor has neither.
//! Repeats of a single character (`.*`, `\s\+`, `[a-z]*`) run as a loop rather
//! than a recursion so a long minified line cannot exhaust the stack.

/// Capture slots while matching.
type Caps = Vec<Option<(usize, usize)>>;
/// What the matcher calls with the end of a match; `false` asks it to keep
/// looking.
type Cont<'a> = &'a mut dyn FnMut(usize, &mut Caps) -> bool;

/// A compiled pattern.
#[derive(Clone, Debug)]
pub struct Regex {
    alts: Vec<Vec<Node>>,
    ignore_case: bool,
    groups: usize,
}

/// Where a match was found. Positions are character indices into the text
/// that was searched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
    /// Capture groups `\1`..`\9`, in order; `None` for a group that did not
    /// take part in the match.
    pub groups: Vec<Option<(usize, usize)>>,
}

#[derive(Clone, Debug)]
enum Node {
    Char(char),
    Any,
    Class(Vec<ClassItem>, bool),
    Bol,
    Eol,
    WordStart,
    WordEnd,
    /// `\(...\)`: alternatives, and the capture slot they fill.
    Group(Vec<Vec<Node>>, usize),
    /// `\%(...\)`: alternatives that capture nothing.
    Plain(Vec<Vec<Node>>),
    Repeat { node: Box<Node>, min: usize, max: Option<usize>, greedy: bool },
    Backref(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClassItem {
    Ch(char),
    Range(char, char),
    Word,
    NotWord,
    Digit,
    NotDigit,
    Space,
    NotSpace,
    Alpha,
    NotAlpha,
    Lower,
    Upper,
    Hex,
}

/// How much of the pattern is special without a backslash. `\v`, `\m`, `\M`
/// and `\V` move between them mid-pattern, the way vim allows.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Magic {
    VeryNo,
    No,
    Yes,
    Very,
}

/// One unit of the pattern after the magic level has been applied, so the
/// grammar below never has to ask which mode it is in.
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Lit(char),
    Any,
    Star,
    Plus,
    Quest,
    Brace(usize, Option<usize>, bool),
    Open(bool),
    Close,
    Bar,
    Bol,
    Eol,
    WordStart,
    WordEnd,
    Class(Vec<ClassItem>, bool),
    Backref(usize),
}

struct Parser {
    chars: Vec<char>,
    i: usize,
    magic: Magic,
    /// `\c` or `\C` seen anywhere in the pattern, which vim lets override the
    /// options wherever it appears.
    case: Option<bool>,
    groups: usize,
}

impl Regex {
    /// Compiles a pattern. `ignore_case` is the option; `\c` and `\C` in the
    /// pattern win over it.
    pub fn new(pattern: &str, ignore_case: bool) -> Result<Regex, String> {
        let mut p = Parser { chars: pattern.chars().collect(), i: 0, magic: Magic::Yes, case: None, groups: 0 };
        let alts = p.alternatives(0)?;
        if p.i < p.chars.len() {
            return Err("unmatched \\)".into());
        }
        Ok(Regex { alts, ignore_case: p.case.unwrap_or(ignore_case), groups: p.groups })
    }

    /// Whether the pattern has an uppercase letter that was typed as a
    /// literal — the question `smartcase` asks. Escapes like `\S` are not
    /// letters the user meant to match.
    pub fn pattern_has_upper(pattern: &str) -> bool {
        let mut chars = pattern.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                chars.next();
                continue;
            }
            if c.is_uppercase() {
                return true;
            }
        }
        false
    }

    /// The first match starting at or after `from`.
    pub fn find_from(&self, text: &[char], from: usize) -> Option<Match> {
        (from..=text.len()).find_map(|s| self.match_at(text, s))
    }

    /// The last match starting strictly before `before`.
    pub fn find_last_before(&self, text: &[char], before: usize) -> Option<Match> {
        (0..before.min(text.len() + 1)).rev().find_map(|s| self.match_at(text, s))
    }

    /// Every non-overlapping match, left to right. An empty match still
    /// advances, so `x*` on "abc" terminates.
    pub fn find_all(&self, text: &[char]) -> Vec<Match> {
        let mut out = Vec::new();
        let mut at = 0;
        while at <= text.len() {
            let Some(m) = self.find_from(text, at) else { break };
            at = if m.end > m.start { m.end } else { m.start + 1 };
            out.push(m);
        }
        out
    }

    pub fn is_match(&self, text: &[char]) -> bool {
        self.find_from(text, 0).is_some()
    }

    /// A match that begins exactly at `start`.
    pub fn match_at(&self, text: &[char], start: usize) -> Option<Match> {
        let mut caps: Vec<Option<(usize, usize)>> = vec![None; self.groups];
        let mut end = None;
        let ok = self.m_alts(&self.alts, text, start, &mut caps, &mut |e, _| {
            end = Some(e);
            true
        });
        ok.then(|| Match { start, end: end.unwrap_or(start), groups: caps })
    }

    fn eq(&self, a: char, b: char) -> bool {
        if a == b {
            return true;
        }
        if !self.ignore_case {
            return false;
        }
        // Whole-string lowercasing would be wrong here: a single character
        // can lowercase into two, and then nothing lines up.
        a.to_lowercase().eq(b.to_lowercase())
    }

    fn in_class(&self, c: char, items: &[ClassItem], negated: bool) -> bool {
        let hit = items.iter().any(|item| match *item {
            ClassItem::Ch(x) => self.eq(c, x),
            ClassItem::Range(lo, hi) => {
                (lo..=hi).contains(&c)
                    || (self.ignore_case
                        && (c.to_lowercase().any(|l| (lo..=hi).contains(&l))
                            || c.to_uppercase().any(|u| (lo..=hi).contains(&u))))
            }
            ClassItem::Word => is_word(c),
            ClassItem::NotWord => !is_word(c),
            ClassItem::Digit => c.is_ascii_digit(),
            ClassItem::NotDigit => !c.is_ascii_digit(),
            ClassItem::Space => c == ' ' || c == '\t',
            ClassItem::NotSpace => !(c == ' ' || c == '\t'),
            ClassItem::Alpha => c.is_alphabetic(),
            ClassItem::NotAlpha => !c.is_alphabetic(),
            ClassItem::Lower => c.is_lowercase(),
            ClassItem::Upper => c.is_uppercase(),
            ClassItem::Hex => c.is_ascii_hexdigit(),
        });
        hit != negated
    }

    /// Whether a single-character node matches at `pos`. `None` for nodes
    /// that are not single characters, which take the general path.
    fn single(&self, node: &Node, text: &[char], pos: usize) -> Option<bool> {
        let Some(&c) = text.get(pos) else {
            return matches!(node, Node::Char(_) | Node::Any | Node::Class(..)).then_some(false);
        };
        Some(match node {
            Node::Char(x) => self.eq(c, *x),
            Node::Any => true,
            Node::Class(items, neg) => self.in_class(c, items, *neg),
            _ => return None,
        })
    }

    fn m_alts(
        &self,
        alts: &[Vec<Node>],
        text: &[char],
        pos: usize,
        caps: &mut Caps,
        k: Cont,
    ) -> bool {
        for seq in alts {
            let saved = caps.clone();
            if self.m_seq(seq, text, pos, caps, k) {
                return true;
            }
            *caps = saved;
        }
        false
    }

    fn m_seq(
        &self,
        seq: &[Node],
        text: &[char],
        pos: usize,
        caps: &mut Caps,
        k: Cont,
    ) -> bool {
        let Some((first, rest)) = seq.split_first() else {
            return k(pos, caps);
        };
        if let Some(hit) = self.single(first, text, pos) {
            return hit && self.m_seq(rest, text, pos + 1, caps, k);
        }
        match first {
            Node::Bol => pos == 0 && self.m_seq(rest, text, pos, caps, k),
            Node::Eol => pos == text.len() && self.m_seq(rest, text, pos, caps, k),
            Node::WordStart => {
                pos < text.len()
                    && is_word(text[pos])
                    && (pos == 0 || !is_word(text[pos - 1]))
                    && self.m_seq(rest, text, pos, caps, k)
            }
            Node::WordEnd => {
                pos > 0
                    && is_word(text[pos - 1])
                    && (pos == text.len() || !is_word(text[pos]))
                    && self.m_seq(rest, text, pos, caps, k)
            }
            Node::Group(alts, idx) => {
                let idx = *idx;
                self.m_alts(alts, text, pos, caps, &mut |end, caps| {
                    let old = caps[idx];
                    caps[idx] = Some((pos, end));
                    if self.m_seq(rest, text, end, caps, k) {
                        return true;
                    }
                    caps[idx] = old;
                    false
                })
            }
            Node::Plain(alts) => {
                self.m_alts(alts, text, pos, caps, &mut |end, caps| self.m_seq(rest, text, end, caps, k))
            }
            Node::Backref(n) => {
                let Some(Some((s, e))) = caps.get(*n).copied() else {
                    // A group that never matched compares equal to nothing,
                    // the same as vim.
                    return self.m_seq(rest, text, pos, caps, k);
                };
                let len = e - s;
                if pos + len > text.len() {
                    return false;
                }
                if !(0..len).all(|i| self.eq(text[pos + i], text[s + i])) {
                    return false;
                }
                self.m_seq(rest, text, pos + len, caps, k)
            }
            Node::Repeat { node, min, max, greedy } => {
                if self.single(node, text, pos).is_some() || matches!(**node, Node::Char(_) | Node::Any | Node::Class(..)) {
                    // Count how far the character can run, then hand the
                    // rest each length in preference order. A loop, so a
                    // `.*` over ten thousand characters is ten thousand
                    // iterations rather than ten thousand stack frames.
                    let mut n = 0;
                    while max.is_none_or(|m| n < m) && self.single(node, text, pos + n) == Some(true) {
                        n += 1;
                    }
                    if n < *min {
                        return false;
                    }
                    if *greedy {
                        (*min..=n).rev().any(|len| self.m_seq(rest, text, pos + len, caps, k))
                    } else {
                        (*min..=n).any(|len| self.m_seq(rest, text, pos + len, caps, k))
                    }
                } else {
                    self.m_rep(node, *min, *max, *greedy, rest, text, pos, 0, caps, k)
                }
            }
            // Single-character nodes were answered above.
            Node::Char(_) | Node::Any | Node::Class(..) => false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn m_rep(
        &self,
        node: &Node,
        min: usize,
        max: Option<usize>,
        greedy: bool,
        rest: &[Node],
        text: &[char],
        pos: usize,
        done: usize,
        caps: &mut Caps,
        k: Cont,
    ) -> bool {
        let can_more = max.is_none_or(|m| done < m);
        let one = std::slice::from_ref(node);
        let more = |caps: &mut Caps, k: Cont| {
            self.m_seq(one, text, pos, caps, &mut |p2, caps| {
                // An empty iteration would repeat forever; vim stops there too.
                p2 != pos && self.m_rep(node, min, max, greedy, rest, text, p2, done + 1, caps, k)
            })
        };
        if greedy {
            if can_more && more(caps, k) {
                return true;
            }
            done >= min && self.m_seq(rest, text, pos, caps, k)
        } else {
            if done >= min && self.m_seq(rest, text, pos, caps, k) {
                return true;
            }
            can_more && more(caps, k)
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    /// Whether `$` here ends a branch, which is the only place it is special.
    /// Looks at the raw characters after it, since the token that follows
    /// has not been read yet.
    fn dollar_ends_branch(&self) -> bool {
        let rest = &self.chars[self.i..];
        if rest.is_empty() {
            return true;
        }
        match self.magic {
            Magic::Very => matches!(rest[0], '|' | ')'),
            _ => rest.len() >= 2 && rest[0] == '\\' && matches!(rest[1], '|' | ')'),
        }
    }

    /// The next token, with the magic level applied. `None` at the end.
    fn token(&mut self, seq_start: bool) -> Result<Option<Tok>, String> {
        loop {
            let Some(c) = self.peek() else { return Ok(None) };
            self.i += 1;
            if c == '\\' {
                let Some(e) = self.peek() else { return Err("trailing backslash".into()) };
                self.i += 1;
                match e {
                    'v' => self.magic = Magic::Very,
                    'm' => self.magic = Magic::Yes,
                    'M' => self.magic = Magic::No,
                    'V' => self.magic = Magic::VeryNo,
                    'c' => self.case = Some(true),
                    'C' => self.case = Some(false),
                    // Match start / match end markers: accepted so a pattern
                    // that carries them still compiles, but not honoured.
                    'z' => {
                        self.i += 1;
                    }
                    _ => return Ok(Some(self.escaped(e)?)),
                }
                continue;
            }
            return Ok(Some(self.plain(c, seq_start)?));
        }
    }

    /// A backslashed character. Most of these mean the same at every magic
    /// level; the ones that are special *unescaped* in very magic mode fall
    /// back to their literal selves when escaped there.
    fn escaped(&mut self, e: char) -> Result<Tok, String> {
        let very = self.magic == Magic::Very;
        Ok(match e {
            '(' if !very => Tok::Open(true),
            ')' if !very => Tok::Close,
            '|' if !very => Tok::Bar,
            '+' if !very => Tok::Plus,
            '?' | '=' if !very => Tok::Quest,
            '{' if !very => self.brace()?,
            '<' if !very => Tok::WordStart,
            '>' if !very => Tok::WordEnd,
            '%' => {
                if self.peek() == Some('(') {
                    self.i += 1;
                    Tok::Open(false)
                } else {
                    return Err("unsupported \\% item".into());
                }
            }
            // `\.`, `\*`, `\[`, `\~`, `\^`, `\$`, `\/`, `\\` and, in very
            // magic mode, the operators: the literal character.
            '.' | '*' | '[' | ']' | '~' | '^' | '$' | '/' | '\\' | '(' | ')' | '|' | '+' | '?' | '='
            | '{' | '}' | '<' | '>' | '@' | '&' => Tok::Lit(e),
            's' => Tok::Class(vec![ClassItem::Space], false),
            'S' => Tok::Class(vec![ClassItem::NotSpace], false),
            'd' => Tok::Class(vec![ClassItem::Digit], false),
            'D' => Tok::Class(vec![ClassItem::NotDigit], false),
            'w' => Tok::Class(vec![ClassItem::Word], false),
            'W' => Tok::Class(vec![ClassItem::NotWord], false),
            'a' => Tok::Class(vec![ClassItem::Alpha], false),
            'A' => Tok::Class(vec![ClassItem::NotAlpha], false),
            'l' => Tok::Class(vec![ClassItem::Lower], false),
            'L' => Tok::Class(vec![ClassItem::Lower], true),
            'u' => Tok::Class(vec![ClassItem::Upper], false),
            'U' => Tok::Class(vec![ClassItem::Upper], true),
            'x' => Tok::Class(vec![ClassItem::Hex], false),
            'X' => Tok::Class(vec![ClassItem::Hex], true),
            // Identifier, keyword, filename and printable classes: word
            // characters are the honest approximation, and the difference
            // has never mattered to anyone searching source code.
            'i' | 'k' | 'f' | 'p' | 'h' => Tok::Class(vec![ClassItem::Word], false),
            'I' | 'K' | 'F' | 'P' | 'H' => Tok::Class(vec![ClassItem::Word, ClassItem::Digit], false),
            't' => Tok::Lit('\t'),
            'e' => Tok::Lit('\u{1b}'),
            'r' => Tok::Lit('\r'),
            // Lines are searched one at a time, so a newline never matches;
            // the escape is accepted rather than rejected mid-typing.
            'n' => Tok::Lit('\n'),
            '1'..='9' => Tok::Backref(e as usize - '0' as usize),
            _ => return Err(format!("unknown escape \\{e}")),
        })
    }

    /// An unescaped character.
    fn plain(&mut self, c: char, seq_start: bool) -> Result<Tok, String> {
        let m = self.magic;
        Ok(match c {
            '^' if seq_start => Tok::Bol,
            '$' if self.dollar_ends_branch() => Tok::Eol,
            '.' if m >= Magic::Yes => Tok::Any,
            '*' if m >= Magic::Yes => Tok::Star,
            '[' if m >= Magic::Yes => match self.class()? {
                Some(t) => t,
                // vim: a `[` with no closing `]` is a literal bracket.
                None => Tok::Lit('['),
            },
            '~' if m >= Magic::Yes => return Err("\\~ (last substitute string) is not supported".into()),
            '(' if m == Magic::Very => Tok::Open(true),
            ')' if m == Magic::Very => Tok::Close,
            '|' if m == Magic::Very => Tok::Bar,
            '+' if m == Magic::Very => Tok::Plus,
            '?' | '=' if m == Magic::Very => Tok::Quest,
            '{' if m == Magic::Very => self.brace()?,
            '<' if m == Magic::Very => Tok::WordStart,
            '>' if m == Magic::Very => Tok::WordEnd,
            '@' if m == Magic::Very => return Err("\\@ (look-around) is not supported".into()),
            _ => Tok::Lit(c),
        })
    }

    /// `{n,m}` after the opening brace was read. `{-n,m}` is vim's
    /// non-greedy spelling.
    fn brace(&mut self) -> Result<Tok, String> {
        let greedy = if self.peek() == Some('-') {
            self.i += 1;
            false
        } else {
            true
        };
        let mut lo = String::new();
        while let Some(d) = self.peek().filter(char::is_ascii_digit) {
            lo.push(d);
            self.i += 1;
        }
        let mut hi: Option<String> = None;
        if self.peek() == Some(',') {
            self.i += 1;
            let mut h = String::new();
            while let Some(d) = self.peek().filter(char::is_ascii_digit) {
                h.push(d);
                self.i += 1;
            }
            hi = Some(h);
        }
        // Either `}` or `\}` closes it; vim accepts both.
        if self.peek() == Some('\\') {
            self.i += 1;
        }
        if self.peek() != Some('}') {
            return Err("unclosed \\{".into());
        }
        self.i += 1;
        let min = lo.parse().unwrap_or(0);
        let max = match hi {
            None if lo.is_empty() => None,
            None => Some(min),
            Some(h) if h.is_empty() => None,
            Some(h) => Some(h.parse().map_err(|_| "bad \\{} count")?),
        };
        Ok(Tok::Brace(min, max, greedy))
    }

    /// `[...]` after the opening bracket was read. `None` when there is no
    /// closing bracket, in which case the caller treats `[` as a literal.
    fn class(&mut self) -> Result<Option<Tok>, String> {
        let start = self.i;
        let negated = if self.peek() == Some('^') {
            self.i += 1;
            true
        } else {
            false
        };
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                self.i = start;
                return Ok(None);
            };
            self.i += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            let item = if c == '\\' {
                let Some(e) = self.peek() else {
                    self.i = start;
                    return Ok(None);
                };
                self.i += 1;
                match e {
                    's' => ClassItem::Space,
                    'S' => ClassItem::NotSpace,
                    'd' => ClassItem::Digit,
                    'D' => ClassItem::NotDigit,
                    'w' => ClassItem::Word,
                    'W' => ClassItem::NotWord,
                    'a' => ClassItem::Alpha,
                    'l' => ClassItem::Lower,
                    'u' => ClassItem::Upper,
                    't' => ClassItem::Ch('\t'),
                    'e' => ClassItem::Ch('\u{1b}'),
                    'n' => ClassItem::Ch('\n'),
                    'r' => ClassItem::Ch('\r'),
                    other => ClassItem::Ch(other),
                }
            } else {
                ClassItem::Ch(c)
            };
            // A range: `a-z`. A `-` first or last is itself.
            if let ClassItem::Ch(lo) = item {
                if self.peek() == Some('-') && self.chars.get(self.i + 1).is_some_and(|c| *c != ']') {
                    self.i += 1;
                    let hi = self.chars[self.i];
                    self.i += 1;
                    if hi < lo {
                        return Err(format!("reverse range {lo}-{hi} in []"));
                    }
                    items.push(ClassItem::Range(lo, hi));
                    continue;
                }
            }
            items.push(item);
        }
        Ok(Some(Tok::Class(items, negated)))
    }

    fn alternatives(&mut self, depth: usize) -> Result<Vec<Vec<Node>>, String> {
        let mut alts = vec![Vec::new()];
        loop {
            let seq_start = alts.last().is_some_and(Vec::is_empty);
            let save = self.i;
            let Some(tok) = self.token(seq_start)? else {
                if depth > 0 {
                    return Err("unmatched \\(".into());
                }
                return Ok(alts);
            };
            match tok {
                Tok::Bar => alts.push(Vec::new()),
                Tok::Close => {
                    if depth == 0 {
                        // vim treats a stray close as an error too.
                        return Err("unmatched \\)".into());
                    }
                    return Ok(alts);
                }
                Tok::Open(capturing) => {
                    let idx = self.groups;
                    if capturing {
                        self.groups += 1;
                    }
                    let inner = self.alternatives(depth + 1)?;
                    let node = if capturing { Node::Group(inner, idx) } else { Node::Plain(inner) };
                    alts.last_mut().unwrap().push(node);
                }
                Tok::Star | Tok::Plus | Tok::Quest | Tok::Brace(..) => {
                    let seq = alts.last_mut().unwrap();
                    let Some(prev) = seq.pop() else {
                        // A leading `*` is a literal star in vim.
                        if tok == Tok::Star {
                            seq.push(Node::Char('*'));
                            continue;
                        }
                        self.i = save;
                        return Err("nothing to repeat".into());
                    };
                    if matches!(prev, Node::Bol | Node::Eol | Node::WordStart | Node::WordEnd) {
                        return Err("nothing to repeat".into());
                    }
                    let (min, max, greedy) = match tok {
                        Tok::Star => (0, None, true),
                        Tok::Plus => (1, None, true),
                        Tok::Quest => (0, Some(1), true),
                        Tok::Brace(a, b, g) => (a, b, g),
                        _ => unreachable!(),
                    };
                    seq.push(Node::Repeat { node: Box::new(prev), min, max, greedy });
                }
                Tok::Lit(c) => alts.last_mut().unwrap().push(Node::Char(c)),
                Tok::Any => alts.last_mut().unwrap().push(Node::Any),
                Tok::Bol => alts.last_mut().unwrap().push(Node::Bol),
                Tok::Eol => alts.last_mut().unwrap().push(Node::Eol),
                Tok::WordStart => alts.last_mut().unwrap().push(Node::WordStart),
                Tok::WordEnd => alts.last_mut().unwrap().push(Node::WordEnd),
                Tok::Class(items, neg) => alts.last_mut().unwrap().push(Node::Class(items, neg)),
                Tok::Backref(n) => {
                    if n > self.groups {
                        return Err(format!("\\{n} refers to a group that does not exist"));
                    }
                    alts.last_mut().unwrap().push(Node::Backref(n - 1));
                }
            }
        }
    }
}

/// Expands a `:s` replacement for one match: `&` and `\0` are the whole
/// match, `\1`..`\9` the groups, `\r` and `\n` a line break, `\t` a tab,
/// `\u` `\l` `\U` `\L` `\e` `\E` the case changers.
pub fn expand_replacement(rep: &str, text: &[char], m: &Match) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Case {
        None,
        Upper,
        Lower,
    }
    let mut out = String::new();
    let mut one: Case = Case::None;
    let mut all: Case = Case::None;
    let push = |out: &mut String, s: &str, one: &mut Case, all: Case| {
        for c in s.chars() {
            let cased = match (*one, all) {
                (Case::Upper, _) => c.to_uppercase().collect::<String>(),
                (Case::Lower, _) => c.to_lowercase().collect(),
                (Case::None, Case::Upper) => c.to_uppercase().collect(),
                (Case::None, Case::Lower) => c.to_lowercase().collect(),
                (Case::None, Case::None) => c.to_string(),
            };
            *one = Case::None;
            out.push_str(&cased);
        }
    };
    let group = |n: usize| -> String {
        if n == 0 {
            return text[m.start..m.end].iter().collect();
        }
        match m.groups.get(n - 1).copied().flatten() {
            Some((s, e)) => text[s..e].iter().collect(),
            None => String::new(),
        }
    };
    let mut chars = rep.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '&' => push(&mut out, &group(0), &mut one, all),
            '\\' => match chars.next() {
                Some(d @ '0'..='9') => push(&mut out, &group(d as usize - '0' as usize), &mut one, all),
                Some('r') | Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('u') => one = Case::Upper,
                Some('l') => one = Case::Lower,
                Some('U') => all = Case::Upper,
                Some('L') => all = Case::Lower,
                Some('e') | Some('E') => {
                    all = Case::None;
                    one = Case::None;
                }
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            _ => push(&mut out, &c.to_string(), &mut one, all),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(pat: &str, text: &str) -> Option<(usize, usize)> {
        let chars: Vec<char> = text.chars().collect();
        Regex::new(pat, false).unwrap().find_from(&chars, 0).map(|m| (m.start, m.end))
    }

    fn found(pat: &str, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let m = Regex::new(pat, false).unwrap().find_from(&chars, 0).expect("no match");
        chars[m.start..m.end].iter().collect()
    }

    #[test]
    fn literals_and_dot() {
        assert_eq!(find("b.d", "abcde"), Some((1, 4)));
        assert_eq!(find("xyz", "abcde"), None);
    }

    #[test]
    fn star_is_greedy_and_backtracks() {
        assert_eq!(found("a.*c", "abcabc"), "abcabc");
        assert_eq!(found("a.*b", "aXXbYYb"), "aXXbYYb");
        assert_eq!(found("a.\\{-}b", "aXXbYYb"), "aXXb");
    }

    #[test]
    fn magic_mode_needs_backslashes_for_groups_and_plus() {
        // `\+` repeats; a bare `+` is a plus sign.
        assert_eq!(found("a\\+", "caaat"), "aaa");
        assert_eq!(found("a+", "a+b"), "a+");
        assert_eq!(found("\\(ab\\)\\+", "xababab"), "ababab");
        assert_eq!(found("foo\\|bar", "xbar"), "bar");
    }

    #[test]
    fn very_magic_drops_the_backslashes() {
        assert_eq!(found("\\v(ab)+", "xababab"), "ababab");
        assert_eq!(found("\\vfoo|bar", "xbar"), "bar");
        assert_eq!(found("\\va{2,3}", "aaaa"), "aaa");
        assert_eq!(found("\\v<is>", "this is"), "is");
        assert_eq!(find("\\v<is>", "this"), None);
    }

    #[test]
    fn very_nomagic_is_literal() {
        assert_eq!(found("\\Va.*b", "a.*b and aXb"), "a.*b");
    }

    #[test]
    fn word_boundaries() {
        assert_eq!(find("\\<cat\\>", "concatenate cat"), Some((12, 15)));
        assert_eq!(find("\\<cat\\>", "concatenate"), None);
    }

    #[test]
    fn anchors_only_at_the_edges() {
        assert_eq!(find("^ab", "abab"), Some((0, 2)));
        assert_eq!(find("ab$", "abab"), Some((2, 4)));
        // Mid-pattern they are literal, as in vim.
        assert_eq!(find("a^b", "xa^b"), Some((1, 4)));
        assert_eq!(find("a$b", "xa$b"), Some((1, 4)));
        // But before an alternation bar they still anchor.
        assert_eq!(find("^x\\|y$", "aby"), Some((2, 3)));
    }

    #[test]
    fn classes_and_ranges() {
        assert_eq!(found("[a-c]\\+", "xxabcabd"), "abcab");
        assert_eq!(found("[^a-c]\\+", "abcxyzabc"), "xyz");
        assert_eq!(found("\\d\\+", "abc 123 def"), "123");
        assert_eq!(found("\\s\\+", "a   b"), "   ");
        assert_eq!(found("\\w\\+", "  foo_bar1 "), "foo_bar1");
        assert_eq!(found("[]a]\\+", "x]a]"), "]a]");
        // An unclosed bracket is a literal one.
        assert_eq!(found("[x", "a[x"), "[x");
    }

    #[test]
    fn case_flags() {
        let chars: Vec<char> = "Hello".chars().collect();
        assert!(Regex::new("hello", true).unwrap().is_match(&chars));
        assert!(!Regex::new("hello", false).unwrap().is_match(&chars));
        assert!(Regex::new("hello\\c", false).unwrap().is_match(&chars));
        assert!(!Regex::new("hello\\C", true).unwrap().is_match(&chars));
        assert!(Regex::pattern_has_upper("Hello"));
        assert!(!Regex::pattern_has_upper("\\Shello"));
    }

    #[test]
    fn groups_capture_and_backreference() {
        let chars: Vec<char> = "say abab".chars().collect();
        let m = Regex::new("\\(ab\\)\\1", false).unwrap().find_from(&chars, 0).unwrap();
        assert_eq!((m.start, m.end), (4, 8));
        assert_eq!(m.groups[0], Some((4, 6)));
    }

    #[test]
    fn replacement_expands_groups_and_case() {
        let chars: Vec<char> = "john smith".chars().collect();
        let re = Regex::new("\\(\\w\\+\\) \\(\\w\\+\\)", false).unwrap();
        let m = re.find_from(&chars, 0).unwrap();
        assert_eq!(expand_replacement("\\2, \\u\\1", &chars, &m), "smith, John");
        assert_eq!(expand_replacement("[&]", &chars, &m), "[john smith]");
        assert_eq!(expand_replacement("\\U\\1\\E \\2", &chars, &m), "JOHN smith");
        assert_eq!(expand_replacement("a\\rb", &chars, &m), "a\nb");
    }

    #[test]
    fn find_all_does_not_loop_on_empty_matches() {
        let chars: Vec<char> = "abc".chars().collect();
        let all = Regex::new("x*", false).unwrap().find_all(&chars);
        assert_eq!(all.len(), 4);
        let all = Regex::new("b*", false).unwrap().find_all(&chars);
        assert_eq!(all.iter().map(|m| (m.start, m.end)).collect::<Vec<_>>(), [(0, 0), (1, 2), (2, 2), (3, 3)]);
    }

    #[test]
    fn a_long_line_does_not_overflow_the_stack() {
        let text: Vec<char> = std::iter::repeat_n('a', 200_000).collect();
        let re = Regex::new("a*b", false).unwrap();
        assert!(re.find_from(&text, 0).is_none());
        let re = Regex::new("^.*$", false).unwrap();
        assert_eq!(re.find_from(&text, 0).unwrap().end, 200_000);
    }

    #[test]
    fn errors_are_reported_not_panicked() {
        assert!(Regex::new("\\(ab", false).is_err());
        assert!(Regex::new("ab\\)", false).is_err());
        assert!(Regex::new("\\2", false).is_err());
        assert!(Regex::new("ab\\", false).is_err());
    }

    #[test]
    fn find_last_before_walks_backwards() {
        let chars: Vec<char> = "ab ab ab".chars().collect();
        let re = Regex::new("ab", false).unwrap();
        assert_eq!(re.find_last_before(&chars, 6).unwrap().start, 3);
        assert_eq!(re.find_last_before(&chars, 7).unwrap().start, 6);
        assert!(re.find_last_before(&chars, 0).is_none());
    }
}
