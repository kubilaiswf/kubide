//! The normal-mode grammar: keys in, one command out.
//!
//! Vim's command language is `["x] [count] operator [count] motion`, with a
//! few dozen commands that stand alone. Rather than a state machine that
//! remembers where it is, the keys pressed so far are kept in a list and the
//! whole list is re-read after every keystroke. That makes "what am I in the
//! middle of" a pure function of the keys, which is what lets `.` and macros
//! replay a recording and get the same answer.

use crate::Key;

/// What the keys so far amount to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Parse {
    /// More keys needed.
    Incomplete,
    /// Not a command; the keys are thrown away.
    Invalid,
    Complete(Cmd),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cmd {
    pub reg: Option<char>,
    pub count: Option<usize>,
    pub kind: CmdKind,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CmdKind {
    Move(Motion),
    Op { op: Op, target: Target },
    /// An operator pressed in visual mode: applies to the selection.
    VisualOp { op: Op, linewise: bool },
    /// A text object pressed in visual mode: extends the selection.
    Object(Obj),
    Act(Act),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Target {
    Motion { count: Option<usize>, motion: Motion },
    Object { count: Option<usize>, obj: Obj },
    /// The operator doubled: `dd`, `yy`, `>>`.
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Delete,
    Change,
    Yank,
    Indent,
    Dedent,
    ToggleCase,
    Lower,
    Upper,
    /// `=`: there is no formatter to hand the lines to, so it re-indents
    /// nothing and says so.
    Format,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Motion {
    Left,
    /// Backspace: `h` that crosses onto the previous line.
    LeftWrap,
    Right,
    /// Space: `l` that crosses onto the next line.
    RightWrap,
    Up,
    Down,
    LineStart,
    FirstNonBlank,
    LineEnd,
    LastNonBlank,
    /// `|`: to a column.
    Column,
    WordFwd { big: bool },
    WordBack { big: bool },
    WordEnd { big: bool },
    WordEndBack { big: bool },
    Find { c: char, back: bool, till: bool },
    RepeatFind { reverse: bool },
    /// `G`: to a line, the last by default.
    GotoLine,
    /// `gg`: to a line, the first by default.
    GotoLineTop,
    ParaFwd,
    ParaBack,
    SentenceFwd,
    SentenceBack,
    /// `%`: the matching bracket, or with a count a percentage of the file.
    Match,
    ScreenTop,
    ScreenMiddle,
    ScreenBottom,
    /// `+`, Enter: down to the first non-blank.
    LineDownFirst,
    /// `-`: up to the first non-blank.
    LineUpFirst,
    /// `_`: count-1 lines down, first non-blank.
    CurrentLineFirst,
    SearchNext { reverse: bool },
    /// `d/pat<CR>`: the operator waits for a search to be typed.
    SearchPrompt { forward: bool },
    /// `*`, `#`, `g*`, `g#`.
    WordSearch { forward: bool, whole: bool },
    Mark { c: char, exact: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Obj {
    Word { big: bool, around: bool },
    Sentence { around: bool },
    Paragraph { around: bool },
    /// `(`, `[`, `{`, `<` with their closers.
    Bracket { open: char, around: bool },
    Quote { q: char, around: bool },
    Tag { around: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    DeleteChar,
    DeleteCharBack,
    Substitute,
    SubstituteLine,
    DeleteToEnd,
    ChangeToEnd,
    YankLine,
    Paste { before: bool, cursor_after: bool },
    Join { spaces: bool },
    Undo,
    Redo,
    Repeat,
    Tilde,
    InsertBefore,
    InsertAfter,
    InsertLineStart,
    InsertLineEnd,
    /// `gI`: insert at column zero, indentation be damned.
    InsertColumnZero,
    /// `gi`: insert where insert mode was last left.
    InsertLast,
    OpenBelow,
    OpenAbove,
    Visual,
    VisualLine,
    /// `gv`.
    Reselect,
    /// `o` in visual mode: the other end.
    VisualSwap,
    ReplaceMode,
    ReplaceChar(char),
    SetMark(char),
    Record(char),
    StopRecord,
    PlayMacro(char),
    /// `ZZ`.
    SaveQuit,
    /// `ZQ`.
    QuitDiscard,
    ExPrompt,
    SearchPrompt { forward: bool },
    ScrollCursor { at: ScrollAt, first_non_blank: bool },
    ScrollHalf { down: bool },
    ScrollPage { down: bool },
    ScrollLine { down: bool },
    Increment { by: i64 },
    JumpOlder,
    JumpNewer,
    Window(char),
    FileInfo,
    /// `&`: repeat the last `:s` on the current line.
    RepeatSubstitute,
    Escape,
    /// `Ctrl+V`: no block mode here, and the key should say so rather than
    /// do nothing.
    VisualBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollAt {
    Top,
    Middle,
    Bottom,
}

struct Keys<'a> {
    keys: &'a [Key],
    i: usize,
}

impl Keys<'_> {
    fn next(&mut self) -> Option<Key> {
        let k = self.keys.get(self.i).copied();
        if k.is_some() {
            self.i += 1;
        }
        k
    }

    fn peek(&self) -> Option<Key> {
        self.keys.get(self.i).copied()
    }

    /// A count: digits, where a leading zero is not a digit but the `0`
    /// motion. `None` when there is none.
    fn count(&mut self) -> Option<usize> {
        let mut n: Option<usize> = None;
        while let Some(Key::Char(c)) = self.peek() {
            if !c.is_ascii_digit() || (c == '0' && n.is_none()) {
                break;
            }
            self.i += 1;
            n = Some(n.unwrap_or(0).saturating_mul(10).saturating_add(c as usize - '0' as usize));
        }
        n
    }

    /// The character a command like `f` or `r` is waiting for. `Esc`
    /// cancels; anything that is not a character is not an answer.
    fn char_arg(&mut self) -> Result<Option<char>, ()> {
        match self.next() {
            None => Ok(None),
            Some(Key::Char(c)) => Ok(Some(c)),
            // `r<Enter>` splits the line, so Enter is an answer here.
            Some(Key::Enter) => Ok(Some('\n')),
            Some(_) => Err(()),
        }
    }
}

/// Reads the keys pressed so far. `visual` changes what `i`, `a`, `o`,
/// `x`, and the operators mean; `recording` decides whether `q` starts or
/// stops.
pub(crate) fn parse(keys: &[Key], visual: bool, recording: bool) -> Parse {
    let mut k = Keys { keys, i: 0 };
    let mut reg = None;
    let mut count = None;

    // A register and a count, in either order — vim takes `"a3dd` and
    // `3"add` alike.
    loop {
        match k.peek() {
            Some(Key::Char('"')) => {
                k.next();
                match k.next() {
                    None => return Parse::Incomplete,
                    Some(Key::Char(c)) if is_register(c) => reg = Some(c),
                    Some(_) => return Parse::Invalid,
                }
            }
            Some(Key::Char(c)) if c.is_ascii_digit() && (c != '0' || count.is_some()) => {
                count = k.count().or(count);
            }
            _ => break,
        }
    }

    let done = |kind: CmdKind| Parse::Complete(Cmd { reg, count, kind });
    let Some(key) = k.next() else { return Parse::Incomplete };

    if let Some(m) = simple_motion(key) {
        return done(CmdKind::Move(m));
    }

    let kind = match key {
        Key::Char('f') | Key::Char('F') | Key::Char('t') | Key::Char('T') => match find_motion(key, &mut k) {
            Ok(Some(m)) => CmdKind::Move(m),
            Ok(None) => return Parse::Incomplete,
            Err(()) => return Parse::Invalid,
        },
        Key::Char('\'') | Key::Char('`') => match k.char_arg() {
            Ok(Some(c)) => CmdKind::Move(Motion::Mark { c, exact: key == Key::Char('`') }),
            Ok(None) => return Parse::Incomplete,
            Err(()) => return Parse::Invalid,
        },
        Key::Char('g') => match g_prefixed(&mut k, visual) {
            Ok(Some(kind)) => kind,
            Ok(None) => return Parse::Incomplete,
            Err(()) => return Parse::Invalid,
        },
        Key::Char('z') => match k.next() {
            None => return Parse::Incomplete,
            Some(Key::Char('z')) => CmdKind::Act(Act::ScrollCursor { at: ScrollAt::Middle, first_non_blank: false }),
            Some(Key::Char('.')) => CmdKind::Act(Act::ScrollCursor { at: ScrollAt::Middle, first_non_blank: true }),
            Some(Key::Char('t')) => CmdKind::Act(Act::ScrollCursor { at: ScrollAt::Top, first_non_blank: false }),
            Some(Key::Enter) => CmdKind::Act(Act::ScrollCursor { at: ScrollAt::Top, first_non_blank: true }),
            Some(Key::Char('b')) => CmdKind::Act(Act::ScrollCursor { at: ScrollAt::Bottom, first_non_blank: false }),
            Some(Key::Char('-')) => CmdKind::Act(Act::ScrollCursor { at: ScrollAt::Bottom, first_non_blank: true }),
            Some(_) => return Parse::Invalid,
        },
        Key::Char('Z') => match k.next() {
            None => return Parse::Incomplete,
            Some(Key::Char('Z')) => CmdKind::Act(Act::SaveQuit),
            Some(Key::Char('Q')) => CmdKind::Act(Act::QuitDiscard),
            Some(_) => return Parse::Invalid,
        },
        Key::Char('r') => match k.char_arg() {
            Ok(Some(c)) => CmdKind::Act(Act::ReplaceChar(c)),
            Ok(None) => return Parse::Incomplete,
            Err(()) => return Parse::Invalid,
        },
        Key::Char('m') => match k.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c)) if c.is_ascii_alphabetic() || matches!(c, '\'' | '`' | '<' | '>' | '[' | ']') => {
                CmdKind::Act(Act::SetMark(c))
            }
            Some(_) => return Parse::Invalid,
        },
        Key::Char('q') => {
            if recording {
                CmdKind::Act(Act::StopRecord)
            } else {
                match k.next() {
                    None => return Parse::Incomplete,
                    Some(Key::Char(c)) if is_register(c) && c != '_' => CmdKind::Act(Act::Record(c)),
                    Some(_) => return Parse::Invalid,
                }
            }
        }
        Key::Char('@') => match k.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c)) if is_register(c) || c == '@' => CmdKind::Act(Act::PlayMacro(c)),
            Some(_) => return Parse::Invalid,
        },
        Key::Ctrl('w') => match k.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c)) => CmdKind::Act(Act::Window(c)),
            Some(Key::Ctrl(c)) => CmdKind::Act(Act::Window(c)),
            Some(Key::Left) => CmdKind::Act(Act::Window('h')),
            Some(Key::Right) => CmdKind::Act(Act::Window('l')),
            Some(Key::Up) => CmdKind::Act(Act::Window('k')),
            Some(Key::Down) => CmdKind::Act(Act::Window('j')),
            Some(_) => return Parse::Invalid,
        },

        // Operators. In visual mode they act at once; otherwise they wait
        // for what to act on.
        Key::Char('d') | Key::Char('c') | Key::Char('y') | Key::Char('<') | Key::Char('>') | Key::Char('=') => {
            let op = match key {
                Key::Char('d') => Op::Delete,
                Key::Char('c') => Op::Change,
                Key::Char('y') => Op::Yank,
                Key::Char('<') => Op::Dedent,
                Key::Char('>') => Op::Indent,
                _ => Op::Format,
            };
            if visual {
                CmdKind::VisualOp { op, linewise: false }
            } else {
                match operator_target(&mut k, key) {
                    Ok(Some(target)) => CmdKind::Op { op, target },
                    Ok(None) => return Parse::Incomplete,
                    Err(()) => return Parse::Invalid,
                }
            }
        }

        // Text objects only exist after an operator or in visual mode; in
        // normal mode the same letters enter insert mode.
        Key::Char('i') | Key::Char('a') if visual => match k.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c)) => match object(c, key == Key::Char('a')) {
                Some(obj) => CmdKind::Object(obj),
                None => return Parse::Invalid,
            },
            Some(_) => return Parse::Invalid,
        },

        Key::Char(c) => match simple_act(c, visual) {
            Some(act) => act,
            None => return Parse::Invalid,
        },
        Key::Ctrl(c) => match ctrl_act(c) {
            Some(act) => CmdKind::Act(act),
            None => return Parse::Invalid,
        },
        Key::Esc => CmdKind::Act(Act::Escape),
        Key::Delete => CmdKind::Act(Act::DeleteChar),
        Key::PageDown => CmdKind::Act(Act::ScrollPage { down: true }),
        Key::PageUp => CmdKind::Act(Act::ScrollPage { down: false }),
        Key::Tab => CmdKind::Act(Act::JumpNewer),
        _ => return Parse::Invalid,
    };
    done(kind)
}

/// The motions that are one key long.
fn simple_motion(key: Key) -> Option<Motion> {
    Some(match key {
        Key::Char('h') | Key::Left | Key::Ctrl('h') => Motion::Left,
        Key::Backspace => Motion::LeftWrap,
        Key::Char('l') | Key::Right => Motion::Right,
        Key::Char(' ') => Motion::RightWrap,
        Key::Char('j') | Key::Down | Key::Ctrl('j') | Key::Ctrl('n') => Motion::Down,
        Key::Char('k') | Key::Up | Key::Ctrl('p') => Motion::Up,
        Key::Char('0') | Key::Home => Motion::LineStart,
        Key::Char('^') => Motion::FirstNonBlank,
        Key::Char('$') | Key::End => Motion::LineEnd,
        Key::Char('|') => Motion::Column,
        Key::Char('w') => Motion::WordFwd { big: false },
        Key::Char('W') => Motion::WordFwd { big: true },
        Key::Char('b') => Motion::WordBack { big: false },
        Key::Char('B') => Motion::WordBack { big: true },
        Key::Char('e') => Motion::WordEnd { big: false },
        Key::Char('E') => Motion::WordEnd { big: true },
        Key::Char('G') => Motion::GotoLine,
        Key::Char('}') => Motion::ParaFwd,
        Key::Char('{') => Motion::ParaBack,
        Key::Char(')') => Motion::SentenceFwd,
        Key::Char('(') => Motion::SentenceBack,
        Key::Char('%') => Motion::Match,
        Key::Char('H') => Motion::ScreenTop,
        Key::Char('M') => Motion::ScreenMiddle,
        Key::Char('L') => Motion::ScreenBottom,
        Key::Char('+') | Key::Enter | Key::Ctrl('m') => Motion::LineDownFirst,
        Key::Char('-') => Motion::LineUpFirst,
        Key::Char('_') => Motion::CurrentLineFirst,
        Key::Char('n') => Motion::SearchNext { reverse: false },
        Key::Char('N') => Motion::SearchNext { reverse: true },
        Key::Char('*') => Motion::WordSearch { forward: true, whole: true },
        Key::Char('#') => Motion::WordSearch { forward: false, whole: true },
        Key::Char(';') => Motion::RepeatFind { reverse: false },
        Key::Char(',') => Motion::RepeatFind { reverse: true },
        _ => return None,
    })
}

fn find_motion(key: Key, k: &mut Keys) -> Result<Option<Motion>, ()> {
    let Some(c) = k.char_arg()? else { return Ok(None) };
    let back = matches!(key, Key::Char('F') | Key::Char('T'));
    let till = matches!(key, Key::Char('t') | Key::Char('T'));
    Ok(Some(Motion::Find { c, back, till }))
}

/// What follows a `g`.
fn g_prefixed(k: &mut Keys, visual: bool) -> Result<Option<CmdKind>, ()> {
    let Some(key) = k.next() else { return Ok(None) };
    Ok(Some(match key {
        Key::Char('g') => CmdKind::Move(Motion::GotoLineTop),
        Key::Char('e') => CmdKind::Move(Motion::WordEndBack { big: false }),
        Key::Char('E') => CmdKind::Move(Motion::WordEndBack { big: true }),
        Key::Char('j') | Key::Down => CmdKind::Move(Motion::Down),
        Key::Char('k') | Key::Up => CmdKind::Move(Motion::Up),
        Key::Char('0') | Key::Home => CmdKind::Move(Motion::LineStart),
        Key::Char('^') => CmdKind::Move(Motion::FirstNonBlank),
        Key::Char('$') | Key::End => CmdKind::Move(Motion::LineEnd),
        Key::Char('_') => CmdKind::Move(Motion::LastNonBlank),
        Key::Char('*') => CmdKind::Move(Motion::WordSearch { forward: true, whole: false }),
        Key::Char('#') => CmdKind::Move(Motion::WordSearch { forward: false, whole: false }),
        Key::Char('v') => CmdKind::Act(Act::Reselect),
        Key::Char('i') => CmdKind::Act(Act::InsertLast),
        Key::Char('I') => CmdKind::Act(Act::InsertColumnZero),
        Key::Char('J') => CmdKind::Act(Act::Join { spaces: false }),
        Key::Char('p') => CmdKind::Act(Act::Paste { before: false, cursor_after: true }),
        Key::Char('P') => CmdKind::Act(Act::Paste { before: true, cursor_after: true }),
        Key::Char('u') | Key::Char('U') | Key::Char('~') => {
            let op = match key {
                Key::Char('u') => Op::Lower,
                Key::Char('U') => Op::Upper,
                _ => Op::ToggleCase,
            };
            if visual {
                CmdKind::VisualOp { op, linewise: false }
            } else {
                match operator_target(k, key)? {
                    Some(target) => CmdKind::Op { op, target },
                    None => return Ok(None),
                }
            }
        }
        _ => return Err(()),
    }))
}

/// What an operator acts on: a doubled key for the line, `i`/`a` plus a
/// character for a text object, or a motion with its own count.
fn operator_target(k: &mut Keys, op_key: Key) -> Result<Option<Target>, ()> {
    let count = k.count();
    let Some(key) = k.next() else { return Ok(None) };
    if key == op_key && count.is_none() {
        return Ok(Some(Target::Line));
    }
    // `gugu`, `gUgU`, `g~g~` and the short forms `guu`, `gUU`, `g~~`.
    if let Key::Char(c @ ('u' | 'U' | '~')) = op_key {
        if key == Key::Char(c) || key == Key::Char('g') && k.peek() == Some(Key::Char(c)) {
            if key == Key::Char('g') {
                k.next();
            }
            return Ok(Some(Target::Line));
        }
    }
    if let Key::Char('i') | Key::Char('a') = key {
        return match k.next() {
            None => Ok(None),
            Some(Key::Char(c)) => match object(c, key == Key::Char('a')) {
                Some(obj) => Ok(Some(Target::Object { count, obj })),
                None => Err(()),
            },
            Some(_) => Err(()),
        };
    }
    let motion = if let Some(m) = simple_motion(key) {
        m
    } else {
        match key {
            Key::Char('f') | Key::Char('F') | Key::Char('t') | Key::Char('T') => match find_motion(key, k)? {
                Some(m) => m,
                None => return Ok(None),
            },
            Key::Char('\'') | Key::Char('`') => match k.char_arg()? {
                Some(c) => Motion::Mark { c, exact: key == Key::Char('`') },
                None => return Ok(None),
            },
            Key::Char('g') => match g_prefixed(k, false)? {
                Some(CmdKind::Move(m)) => m,
                Some(_) => return Err(()),
                None => return Ok(None),
            },
            Key::Char('/') => Motion::SearchPrompt { forward: true },
            Key::Char('?') => Motion::SearchPrompt { forward: false },
            Key::Esc => return Err(()),
            _ => return Err(()),
        }
    };
    Ok(Some(Target::Motion { count, motion }))
}

fn object(c: char, around: bool) -> Option<Obj> {
    Some(match c {
        'w' => Obj::Word { big: false, around },
        'W' => Obj::Word { big: true, around },
        's' => Obj::Sentence { around },
        'p' => Obj::Paragraph { around },
        '(' | ')' | 'b' => Obj::Bracket { open: '(', around },
        '[' | ']' => Obj::Bracket { open: '[', around },
        '{' | '}' | 'B' => Obj::Bracket { open: '{', around },
        '<' | '>' => Obj::Bracket { open: '<', around },
        '"' | '\'' | '`' => Obj::Quote { q: c, around },
        't' => Obj::Tag { around },
        _ => return None,
    })
}

/// One-key commands. Visual mode reads several of them differently.
fn simple_act(c: char, visual: bool) -> Option<CmdKind> {
    use Act::*;
    if visual {
        let kind = match c {
            'x' => CmdKind::VisualOp { op: Op::Delete, linewise: false },
            'X' | 'D' => CmdKind::VisualOp { op: Op::Delete, linewise: true },
            's' => CmdKind::VisualOp { op: Op::Change, linewise: false },
            'S' | 'C' | 'R' => CmdKind::VisualOp { op: Op::Change, linewise: true },
            'Y' => CmdKind::VisualOp { op: Op::Yank, linewise: true },
            'u' => CmdKind::VisualOp { op: Op::Lower, linewise: false },
            'U' => CmdKind::VisualOp { op: Op::Upper, linewise: false },
            '~' => CmdKind::VisualOp { op: Op::ToggleCase, linewise: false },
            'J' => CmdKind::Act(Join { spaces: true }),
            'o' | 'O' => CmdKind::Act(VisualSwap),
            'p' => CmdKind::Act(Paste { before: false, cursor_after: false }),
            'P' => CmdKind::Act(Paste { before: true, cursor_after: false }),
            'v' => CmdKind::Act(Visual),
            'V' => CmdKind::Act(VisualLine),
            'I' => CmdKind::Act(InsertBefore),
            'A' => CmdKind::Act(InsertAfter),
            ':' => CmdKind::Act(ExPrompt),
            '/' => CmdKind::Act(SearchPrompt { forward: true }),
            '?' => CmdKind::Act(SearchPrompt { forward: false }),
            _ => return None,
        };
        return Some(kind);
    }
    Some(CmdKind::Act(match c {
        'x' => DeleteChar,
        'X' => DeleteCharBack,
        's' => Substitute,
        'S' => SubstituteLine,
        'D' => DeleteToEnd,
        'C' => ChangeToEnd,
        'Y' => YankLine,
        'p' => Paste { before: false, cursor_after: false },
        'P' => Paste { before: true, cursor_after: false },
        'J' => Join { spaces: true },
        'u' => Undo,
        'U' => Undo,
        '.' => Repeat,
        '~' => Tilde,
        'i' => InsertBefore,
        'a' => InsertAfter,
        'I' => InsertLineStart,
        'A' => InsertLineEnd,
        'o' => OpenBelow,
        'O' => OpenAbove,
        'v' => Visual,
        'V' => VisualLine,
        'R' => ReplaceMode,
        ':' => ExPrompt,
        '/' => SearchPrompt { forward: true },
        '?' => SearchPrompt { forward: false },
        '&' => RepeatSubstitute,
        _ => return None,
    }))
}

fn ctrl_act(c: char) -> Option<Act> {
    use Act::*;
    Some(match c {
        'r' => Redo,
        'd' => ScrollHalf { down: true },
        'u' => ScrollHalf { down: false },
        'f' => ScrollPage { down: true },
        'b' => ScrollPage { down: false },
        'e' => ScrollLine { down: true },
        'y' => ScrollLine { down: false },
        'a' => Increment { by: 1 },
        'x' => Increment { by: -1 },
        'o' => JumpOlder,
        'i' => JumpNewer,
        'g' => FileInfo,
        'c' | '[' => Escape,
        'v' | 'q' => VisualBlock,
        // Redraw: nothing to do, but the key must not be an error either.
        'l' => Escape,
        _ => return None,
    })
}

/// Names a register may have. `"` is the unnamed one, `+`/`*` the clipboard,
/// `_` the hole, `0`-`9` the yank and delete history, `-` the small delete,
/// `/` the last search, `:` the last command line, `.` the last insert.
pub(crate) fn is_register(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '"' | '+' | '*' | '_' | '-' | '/' | ':' | '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(s: &str) -> Vec<Key> {
        s.chars().map(Key::Char).collect()
    }

    fn complete(s: &str) -> Cmd {
        match parse(&keys(s), false, false) {
            Parse::Complete(c) => c,
            other => panic!("{s:?} parsed as {other:?}"),
        }
    }

    #[test]
    fn a_bare_motion_moves() {
        assert_eq!(complete("w").kind, CmdKind::Move(Motion::WordFwd { big: false }));
        assert_eq!(complete("5j").count, Some(5));
    }

    #[test]
    fn zero_is_a_motion_unless_a_count_has_started() {
        assert_eq!(complete("0").kind, CmdKind::Move(Motion::LineStart));
        assert_eq!(complete("10j").count, Some(10));
    }

    #[test]
    fn operators_wait_for_a_target() {
        assert_eq!(parse(&keys("d"), false, false), Parse::Incomplete);
        assert_eq!(parse(&keys("d2"), false, false), Parse::Incomplete);
        assert_eq!(parse(&keys("di"), false, false), Parse::Incomplete);
        assert_eq!(
            complete("d2i(").kind,
            CmdKind::Op { op: Op::Delete, target: Target::Object { count: Some(2), obj: Obj::Bracket { open: '(', around: false } } }
        );
        assert_eq!(complete("d/").kind, CmdKind::Op {
            op: Op::Delete,
            target: Target::Motion { count: None, motion: Motion::SearchPrompt { forward: true } }
        });
        let c = complete("d2w");
        assert_eq!(
            c.kind,
            CmdKind::Op { op: Op::Delete, target: Target::Motion { count: Some(2), motion: Motion::WordFwd { big: false } } }
        );
        assert_eq!(complete("dd").kind, CmdKind::Op { op: Op::Delete, target: Target::Line });
        assert_eq!(
            complete("ci(").kind,
            CmdKind::Op { op: Op::Change, target: Target::Object { count: None, obj: Obj::Bracket { open: '(', around: false } } }
        );
        assert_eq!(
            complete("yaw").kind,
            CmdKind::Op { op: Op::Yank, target: Target::Object { count: None, obj: Obj::Word { big: false, around: true } } }
        );
        assert_eq!(complete("dfx").kind, CmdKind::Op {
            op: Op::Delete,
            target: Target::Motion { count: None, motion: Motion::Find { c: 'x', back: false, till: false } }
        });
    }

    #[test]
    fn registers_can_come_before_or_after_the_count() {
        let a = complete("\"a3dd");
        let b = complete("3\"add");
        assert_eq!(a.reg, Some('a'));
        assert_eq!(a.count, Some(3));
        assert_eq!(a, b);
    }

    #[test]
    fn nonsense_is_invalid_not_stuck() {
        assert_eq!(parse(&keys("dq"), false, false), Parse::Invalid);
        assert_eq!(parse(&keys("\"!"), false, false), Parse::Invalid);
        assert_eq!(parse(&[Key::Char('d'), Key::Esc], false, false), Parse::Invalid);
    }

    #[test]
    fn g_and_z_prefixes() {
        assert_eq!(complete("gg").kind, CmdKind::Move(Motion::GotoLineTop));
        assert_eq!(complete("dgg").kind, CmdKind::Op {
            op: Op::Delete,
            target: Target::Motion { count: None, motion: Motion::GotoLineTop }
        });
        assert_eq!(complete("gUiw").kind, CmdKind::Op {
            op: Op::Upper,
            target: Target::Object { count: None, obj: Obj::Word { big: false, around: false } }
        });
        assert_eq!(complete("guu").kind, CmdKind::Op { op: Op::Lower, target: Target::Line });
        assert_eq!(complete("g~g~").kind, CmdKind::Op { op: Op::ToggleCase, target: Target::Line });
        assert_eq!(complete("zz").kind, CmdKind::Act(Act::ScrollCursor { at: ScrollAt::Middle, first_non_blank: false }));
        assert_eq!(parse(&keys("g"), false, false), Parse::Incomplete);
    }

    #[test]
    fn visual_mode_reads_the_operators_as_immediate() {
        match parse(&keys("d"), true, false) {
            Parse::Complete(c) => assert_eq!(c.kind, CmdKind::VisualOp { op: Op::Delete, linewise: false }),
            other => panic!("{other:?}"),
        }
        match parse(&keys("iw"), true, false) {
            Parse::Complete(c) => assert_eq!(c.kind, CmdKind::Object(Obj::Word { big: false, around: false })),
            other => panic!("{other:?}"),
        }
        match parse(&keys("x"), true, false) {
            Parse::Complete(c) => assert_eq!(c.kind, CmdKind::VisualOp { op: Op::Delete, linewise: false }),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn recording_changes_what_q_means() {
        assert_eq!(parse(&keys("q"), false, false), Parse::Incomplete);
        assert_eq!(complete("qa").kind, CmdKind::Act(Act::Record('a')));
        match parse(&keys("q"), false, true) {
            Parse::Complete(c) => assert_eq!(c.kind, CmdKind::Act(Act::StopRecord)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn r_takes_enter_as_a_line_break() {
        assert_eq!(
            parse(&[Key::Char('r'), Key::Enter], false, false),
            Parse::Complete(Cmd { reg: None, count: None, kind: CmdKind::Act(Act::ReplaceChar('\n')) })
        );
    }

    #[test]
    fn window_commands_take_a_second_key() {
        assert_eq!(parse(&[Key::Ctrl('w')], false, false), Parse::Incomplete);
        assert_eq!(
            parse(&[Key::Ctrl('w'), Key::Char('v')], false, false),
            Parse::Complete(Cmd { reg: None, count: None, kind: CmdKind::Act(Act::Window('v')) })
        );
        assert_eq!(
            parse(&[Key::Ctrl('w'), Key::Ctrl('w')], false, false),
            Parse::Complete(Cmd { reg: None, count: None, kind: CmdKind::Act(Act::Window('w')) })
        );
    }
}
