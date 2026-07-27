//! The overlay: go to file, find in file, go to line.
//!
//! One overlay for all three because they are the same interaction — type,
//! see candidates, pick one — and three separate widgets would be three
//! separate sets of bugs about where the selection went.
//!
//! It captures every key while open. Anything less means a stray keystroke
//! reaching the editor underneath, which is how a find box ends up typing
//! into the file it was searching.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Fuzzy file open.
    File,
    /// Search within the focused buffer. Literal, unlike [`Mode::File`]: a
    /// file name is something you half-remember, a string in a document is
    /// something you know.
    Find,
    /// Matches from across the project, already found. A fixed list to narrow,
    /// not a search — which is why it is not [`Mode::Find`] despite reading
    /// like one.
    Results,
    /// Jump to a line number.
    Line,
    /// Run a command by name.
    Command,
    /// A question with a fixed set of answers, drawn as a bordered box with
    /// the answers side by side.
    ///
    /// Three ways out of one situation, which a press-again confirmation
    /// cannot express: it offers to go through with it or to do nothing, and
    /// the answer people usually want is the third one.
    Choice,
    /// Free text, with the caller deciding what it means.
    Prompt,
}

impl Mode {
    /// The label in front of the input, so it's never unclear what typing
    /// here will do.
    pub fn prompt(self) -> &'static str {
        match self {
            Mode::File => "open",
            Mode::Find => "find",
            Mode::Results => "results",
            Mode::Line => "line",
            Mode::Command => ">",
            // Replaced by the question itself.
            Mode::Choice => "",
            // Replaced by the caller's own label.
            Mode::Prompt => "",
        }
    }
}

/// A candidate row, already formatted for drawing.
pub struct Row {
    pub text: String,
    /// Character positions the fuzzy match landed on, highlighted so it's
    /// visible why a row is in the list.
    pub hits: Vec<usize>,
    /// The key chord, for the command list. Its own field rather than padded
    /// into `text`, so the renderer can right-align it into a column — a gap
    /// of spaces only lines up while every title is shorter than the gap.
    pub detail: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Target {
    Path(PathBuf),
    Pos(kb_edit::Pos),
    Run(kb_cfg::Action),
    /// Whatever was typed. What it means is the caller's business.
    Text(String),
    /// A place in a file that is not necessarily open yet.
    Location(PathBuf, kb_edit::Pos),
    /// One of a fixed set of answers, by position. What it means is the
    /// caller's business, the same way `Text` is.
    Answer(usize),
}

pub struct Palette {
    pub mode: Mode,
    /// Overrides the mode's label, for prompts that ask something specific.
    /// In [`Mode::Choice`] it is the box's title bar.
    pub label: Option<String>,
    /// The sentence a [`Mode::Choice`] box asks. Its own field because `note`
    /// is rewritten by filtering and this must survive.
    pub question: Option<String>,
    pub query: String,
    pub selected: usize,
    /// First visible match.
    ///
    /// The list is longer than the box on anything but a short query, and
    /// without this it always drew from the first match — so the selection
    /// walked off the bottom and everything past that row was unreachable
    /// while still being selectable.
    pub top: usize,
    /// Everything searchable, as displayed. Built once when the palette opens:
    /// re-listing files on every keystroke would stat the whole project.
    items: Vec<String>,
    /// Per-item chord, parallel to `items`. Command mode only; empty (and an
    /// empty string means unbound) everywhere else.
    details: Vec<String>,
    targets: Vec<Target>,
    /// Filtered indices into `items`, best first.
    matches: Vec<kb_find::Match>,
    /// The buffer's lines, for [`Mode::Find`] only.
    ///
    /// Find rebuilds its rows on every keystroke rather than filtering a fixed
    /// list, because its rows are occurrences and there is no such thing as an
    /// occurrence until there is something to look for. The other modes have a
    /// list that exists before you type and leave this empty.
    lines: Vec<String>,
    /// Message shown instead of results, e.g. why there are none.
    pub note: Option<String>,
}

/// Whether the overlay handles this key itself.
///
/// Anything not listed here must be reported as unhandled, even though the
/// overlay still owns the keyboard. Claiming a key makes the window layer
/// swallow the character that key would have produced, and typing is the
/// entire point of the box — a palette that consumes every key can never
/// receive a single letter.
pub fn consumes(vk: u8) -> bool {
    matches!(
        vk,
        0x1B // escape
        | 0x08 // backspace
        | 0x25 // left
        | 0x26 // up
        | 0x27 // right
        | 0x28 // down
        | 0x0D // enter
    )
}

/// More than this and the list is noise; the fuzzy pattern is the filter.
const MAX_ROWS: usize = 200;

impl Palette {
    pub fn files(paths: Vec<PathBuf>, root: &std::path::Path) -> Self {
        // Shown relative to the project: the absolute prefix is identical on
        // every row and would push the useful part off the edge.
        let items: Vec<String> = paths
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        let targets = paths.into_iter().map(Target::Path).collect();
        let note = items.is_empty().then(|| "no files found".to_string());
        let mut me = Self {
            mode: Mode::File,
            label: None,
            question: None,
            query: String::new(),
            selected: 0,
            top: 0,
            items,
            details: Vec::new(),
            targets,
            matches: Vec::new(),
            lines: Vec::new(),
            note,
        };
        me.refilter();
        me
    }

    /// Search within a buffer.
    ///
    /// The lines are kept rather than turned into rows up front, because a row
    /// here is one occurrence and not one line. A line holding the query three
    /// times is three places you might want to go, and folding them into one
    /// row is exactly what made this feel like it had no next-match.
    pub fn find(lines: &[String]) -> Self {
        let mut me = Self {
            mode: Mode::Find,
            label: None,
            question: None,
            query: String::new(),
            selected: 0,
            top: 0,
            items: Vec::new(),
            details: Vec::new(),
            targets: Vec::new(),
            matches: Vec::new(),
            lines: lines.to_vec(),
            note: None,
        };
        me.refilter();
        me
    }

    /// Every action, with its binding shown alongside.
    ///
    /// The binding is the point as much as the command is: this is where
    /// someone finds out a shortcut exists. It rides in `details` rather than
    /// being padded into the row, so typing narrows on the command's name and
    /// the chords sit in a right-aligned column of their own.
    pub fn commands(keys: &kb_cfg::Keymap) -> Self {
        let actions: Vec<kb_cfg::Action> = kb_cfg::Action::ALL.to_vec();
        let items: Vec<String> = actions.iter().map(|a| a.title().to_string()).collect();
        let details: Vec<String> = actions
            .iter()
            .map(|a| {
                keys.binding_for(*a)
                    .map(|chord| chord.to_string())
                    .unwrap_or_default()
            })
            .collect();
        let targets = actions.into_iter().map(Target::Run).collect();
        let mut me = Self {
            mode: Mode::Command,
            label: None,
            question: None,
            query: String::new(),
            selected: 0,
            top: 0,
            items,
            details,
            targets,
            matches: Vec::new(),
            lines: Vec::new(),
            note: None,
        };
        me.refilter();
        me
    }

    /// A question with a fixed set of answers, listed in order.
    ///
    /// The wording is the interface: "Close without saving" says what will
    /// happen where a Yes button would only say that something will. Typing
    /// narrows them like any other list, so `sav` reaches the first one
    /// without arrowing to it.
    pub fn ask(title: &str, question: &str, answers: &[&str]) -> Self {
        let items: Vec<String> = answers.iter().map(|a| a.to_string()).collect();
        let targets = (0..items.len()).map(Target::Answer).collect();
        // Built once and never filtered. There is no text box on a question
        // like this, so there is nothing to narrow — and a stray keystroke
        // hiding one of three answers would be a way to lose work.
        let matches = (0..items.len())
            .map(|index| kb_find::Match { index, score: 0, positions: Vec::new() })
            .collect();
        Self {
            mode: Mode::Choice,
            label: Some(title.to_string()),
            question: Some(question.to_string()),
            query: String::new(),
            selected: 0,
            top: 0,
            items,
            details: Vec::new(),
            targets,
            matches,
            lines: Vec::new(),
            note: None,
        }
    }

    /// The answers, in order, for drawing them side by side.
    pub fn answers(&self) -> &[String] {
        &self.items
    }

    /// Search results from across the project.
    ///
    /// The path is shortened and the line number shown, because a list of
    /// matching lines with no idea which file they are in is unreadable.
    pub fn results(hits: Vec<kb_git::Hit>, root: &std::path::Path) -> Self {
        let items: Vec<String> = hits
            .iter()
            .map(|h| {
                let path = h
                    .path
                    .strip_prefix(root)
                    .unwrap_or(&h.path)
                    .to_string_lossy()
                    .replace('\\', "/");
                format!("{path}:{}  {}", h.line + 1, h.text.trim())
            })
            .collect();
        let note = items.is_empty().then(|| "no matches".to_string());
        let targets = hits
            .into_iter()
            .map(|h| Target::Location(h.path, kb_edit::Pos::new(h.line, 0)))
            .collect();
        let mut me = Self {
            mode: Mode::Results,
            label: None,
            question: None,
            query: String::new(),
            selected: 0,
            top: 0,
            items,
            details: Vec::new(),
            targets,
            matches: Vec::new(),
            lines: Vec::new(),
            note,
        };
        me.refilter();
        me
    }

    pub fn line() -> Self {
        Self {
            mode: Mode::Line,
            label: None,
            question: None,
            query: String::new(),
            selected: 0,
            top: 0,
            items: Vec::new(),
            details: Vec::new(),
            targets: Vec::new(),
            matches: Vec::new(),
            lines: Vec::new(),
            note: Some("type a line number".to_string()),
        }
    }

    /// A one-line question: new name, new file, and so on.
    pub fn prompt(label: &str, initial: &str) -> Self {
        Self {
            mode: Mode::Prompt,
            label: Some(label.to_string()),
            question: None,
            query: initial.to_string(),
            selected: 0,
            top: 0,
            items: Vec::new(),
            details: Vec::new(),
            targets: Vec::new(),
            matches: Vec::new(),
            lines: Vec::new(),
            note: None,
        }
    }

    pub fn push(&mut self, c: char) {
        self.query.push(c);
        self.refilter();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.refilter();
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() as i32 - 1;
        self.selected = (self.selected as i32 + delta).clamp(0, last) as usize;
    }

    /// Scrolls just enough to keep the selection in view.
    ///
    /// Called from drawing, because only the renderer knows how many rows fit
    /// in the box — the same arrangement the file tree and the editor use.
    pub fn ensure_visible(&mut self, visible: usize) {
        if visible == 0 {
            return;
        }
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + visible {
            self.top = self.selected + 1 - visible;
        }
        self.top = self.top.min(self.matches.len().saturating_sub(visible));
    }

    /// How many matches there are, for saying so when they do not all fit.
    pub fn len(&self) -> usize {
        self.matches.len()
    }


    /// Rows to draw, from the current scroll position.
    pub fn rows(&self, limit: usize) -> Vec<Row> {
        self.matches
            .iter()
            .skip(self.top)
            .take(limit)
            .filter_map(|m| {
                Some(Row {
                    text: self.items.get(m.index)?.clone(),
                    hits: m.positions.clone(),
                    detail: self
                        .details
                        .get(m.index)
                        .filter(|d| !d.is_empty())
                        .cloned(),
                })
            })
            .collect()
    }

    /// What pressing Enter should do.
    pub fn chosen(&self) -> Option<Target> {
        if self.mode == Mode::Prompt {
            let text = self.query.trim();
            // An empty answer is a cancelled prompt, not a file named "".
            return (!text.is_empty()).then(|| Target::Text(text.to_string()));
        }
        if self.mode == Mode::Line {
            // A line number is typed, not picked from a list. Humans count
            // from one; the buffer counts from zero.
            let n: usize = self.query.trim().parse().ok()?;
            return Some(Target::Pos(kb_edit::Pos::new(n.saturating_sub(1), 0)));
        }
        let m = self.matches.get(self.selected)?;
        self.targets.get(m.index).cloned()
    }

    /// Rows for [`Mode::Find`]: one per occurrence, rebuilt per keystroke.
    ///
    /// Literal, where the other lists are fuzzy. Fuzzy is right for a file you
    /// half-remember the name of and wrong for text you are looking at:
    /// searching this project's CONTRIBUTING for `kubi` used to return "the
    /// issue number, KB number, or doc link", because those four letters
    /// appear along it in order.
    fn refilter_find(&mut self) {
        // (line, column) for every place worth going.
        let mut rows: Vec<(usize, usize)> = Vec::new();
        if self.query.is_empty() {
            // The file itself, so the box says something before you type.
            rows.extend((0..self.lines.len()).map(|i| (i, 0)));
        } else {
            for (i, line) in self.lines.iter().enumerate() {
                rows.extend(
                    kb_find::occurrences(&self.query, line)
                        .into_iter()
                        .map(|at| (i, at)),
                );
                if rows.len() >= MAX_ROWS {
                    break;
                }
            }
        }
        rows.truncate(MAX_ROWS);

        // Right-aligned to the file's widest number, or the text starts in a
        // different column on line 9 than on line 10 and the list looks ragged.
        let width = self.lines.len().to_string().len();
        let needle = self.query.chars().count();
        self.items = Vec::with_capacity(rows.len());
        self.targets = Vec::with_capacity(rows.len());
        self.matches = Vec::with_capacity(rows.len());

        for (index, (line, col)) in rows.into_iter().enumerate() {
            let prefix = format!("{:>width$}: ", line + 1);
            // The number is part of the row, so the highlight sits past it.
            let shift = prefix.chars().count();
            self.items.push(format!("{prefix}{}", self.lines[line].trim_end()));
            // The column, not zero: landing at the start of a long line leaves
            // you hunting for what you just searched for.
            self.targets.push(Target::Pos(kb_edit::Pos::new(line, col)));
            self.matches.push(kb_find::Match {
                index,
                score: 0,
                positions: if self.query.is_empty() {
                    Vec::new()
                } else {
                    (shift + col..shift + col + needle).collect()
                },
            });
        }

        self.selected = 0;
        self.top = 0;
        self.note = if self.lines.is_empty() {
            Some("nothing to search".to_string())
        } else if self.items.is_empty() {
            Some(format!("no match for '{}'", self.query))
        } else {
            None
        };
    }

    fn refilter(&mut self) {
        match self.mode {
            // A prompt has nothing to narrow, and no note worth keeping past
            // the keystroke that changed the text underneath it; Line keeps
            // its standing "type a line number".
            Mode::Prompt => {
                self.note = None;
                return;
            }
            Mode::Line | Mode::Choice => return,
            Mode::Find => return self.refilter_find(),
            Mode::File | Mode::Command | Mode::Results => {}
        }
        self.matches = kb_find::rank(&self.query, &self.items, MAX_ROWS);
        self.selected = 0;
        self.top = 0;
        self.note = if self.items.is_empty() {
            // Whatever the constructor said is more specific than anything
            // this function could invent — "no matches" beats "nothing to
            // search" when a search is exactly what just happened.
            self.note
                .clone()
                .or_else(|| Some("nothing to search".to_string()))
        } else if self.matches.is_empty() {
            // Saying so beats an empty box, which reads as a broken widget.
            Some(format!("no match for '{}'", self.query))
        } else {
            None
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(names: &[&str]) -> Palette {
        let root = std::path::Path::new("/repo");
        Palette::files(names.iter().map(|n| root.join(n)).collect(), root)
    }

    #[test]
    fn typing_keys_are_left_for_the_character_path() {
        // The bug this pins: claiming these made the window layer swallow the
        // WM_CHAR they generate, so the query stayed empty forever.
        for vk in [b'A', b'Z', b'0', b'9', 0xBF, 0xBE, 0x20] {
            assert!(!consumes(vk), "0x{vk:02X} must reach the character path");
        }
    }

    #[test]
    fn navigation_keys_are_handled_here() {
        for vk in [0x1B, 0x08, 0x26, 0x28, 0x0D] {
            assert!(consumes(vk), "0x{vk:02X}");
        }
        // Tab is not: nothing in the overlay uses it, and a consumed key is
        // a swallowed character.
        assert!(!consumes(0x09));
    }

    #[test]
    fn typing_into_a_prompt_clears_a_stale_note() {
        // A note describes the text it was written about; keeping it while
        // that text changes underneath would have it lie within a keystroke.
        let mut p = Palette::prompt("new file", "");
        p.note = Some("that name is taken".into());
        p.push('x');
        assert!(p.note.is_none());
    }

    #[test]
    fn everything_is_listed_before_you_type() {
        // An empty finder is useless; the list is the point of opening it.
        let p = files(&["a.rs", "b.rs"]);
        assert_eq!(p.rows(10).len(), 2);
    }

    #[test]
    fn typing_narrows_and_ranks() {
        let p = {
            let mut p = files(&["src/deep/other.rs", "main.rs"]);
            for c in "main".chars() {
                p.push(c);
            }
            p
        };
        assert_eq!(p.rows(10)[0].text, "main.rs");
    }

    #[test]
    fn paths_are_shown_relative_to_the_project() {
        // The absolute prefix is the same on every row and would push the
        // useful part off the edge.
        let p = files(&["src/main.rs"]);
        assert_eq!(p.rows(1)[0].text, "src/main.rs");
    }

    #[test]
    fn no_match_says_so() {
        let mut p = files(&["a.rs"]);
        for c in "zzzz".chars() {
            p.push(c);
        }
        assert!(p.rows(10).is_empty());
        assert!(p.note.as_deref().unwrap().contains("zzzz"));
    }

    #[test]
    fn backspace_brings_the_results_back() {
        let mut p = files(&["a.rs"]);
        for c in "zz".chars() {
            p.push(c);
        }
        p.backspace();
        p.backspace();
        assert_eq!(p.rows(10).len(), 1);
    }

    #[test]
    fn the_selection_resets_when_the_query_changes() {
        // Otherwise it points at whatever happens to be in that slot now.
        let mut p = files(&["a.rs", "b.rs", "c.rs"]);
        p.move_selection(2);
        assert_eq!(p.selected, 2);
        p.push('a');
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn the_selection_stays_inside_the_list() {
        let mut p = files(&["a.rs", "b.rs"]);
        p.move_selection(-5);
        assert_eq!(p.selected, 0);
        p.move_selection(50);
        assert_eq!(p.selected, 1);
    }

    #[test]
    fn commands_show_their_binding() {
        // Where someone finds out a shortcut exists at all. The chord rides
        // in `detail`, not in the row text: the renderer owns the column.
        let p = Palette::commands(&kb_cfg::Keymap::default());
        let save = p
            .rows(200)
            .into_iter()
            .find(|r| r.text.starts_with("Save file"))
            .expect("Save should be listed");
        assert_eq!(save.detail.as_deref(), Some("Ctrl+S"));
    }

    #[test]
    fn an_unbound_command_shows_no_chord() {
        // An empty string drawn right-aligned is invisible, but a Some("")
        // would still make the renderer measure and place it.
        let keys = kb_cfg::Keymap::default();
        let p = Palette::commands(&keys);
        for row in p.rows(200) {
            if let Some(d) = &row.detail {
                assert!(!d.is_empty(), "{}", row.text);
            }
        }
    }

    #[test]
    fn commands_are_searchable_by_name() {
        let mut p = Palette::commands(&kb_cfg::Keymap::default());
        for c in "termi".chars() {
            p.push(c);
        }
        assert!(p.rows(10)[0].text.starts_with("Open terminal"));
    }

    #[test]
    fn results_say_which_file_and_line() {
        // A list of matching lines with no file next to them is unreadable.
        let root = std::path::Path::new("/repo");
        let p = Palette::results(
            vec![kb_git::Hit {
                path: root.join("src/main.rs"),
                line: 41,
                text: "    let x = 1;".into(),
            }],
            root,
        );
        assert_eq!(p.rows(1)[0].text, "src/main.rs:42  let x = 1;");
    }

    #[test]
    fn empty_results_say_so() {
        let p = Palette::results(Vec::new(), std::path::Path::new("/repo"));
        assert_eq!(p.note.as_deref(), Some("no matches"));
    }

    #[test]
    fn a_prompt_returns_what_was_typed() {
        let mut p = Palette::prompt("new file", "");
        for c in "notes.md".chars() {
            p.push(c);
        }
        assert!(matches!(p.chosen(), Some(Target::Text(t)) if t == "notes.md"));
    }

    #[test]
    fn an_empty_prompt_is_a_cancelled_prompt() {
        // Not a file named "".
        assert!(Palette::prompt("new file", "").chosen().is_none());
        assert!(Palette::prompt("new file", "   ").chosen().is_none());
    }

    #[test]
    fn a_prompt_can_start_with_the_old_name() {
        // Renaming usually means changing part of a name, not retyping it.
        let p = Palette::prompt("rename", "main.rs");
        assert_eq!(p.query, "main.rs");
    }

    #[test]
    fn go_to_line_counts_from_one() {
        // Humans count from one, buffers from zero. Getting this backwards
        // sends you one line off, every time, quietly.
        let mut p = Palette::line();
        for c in "42".chars() {
            p.push(c);
        }
        match p.chosen() {
            Some(Target::Pos(pos)) => assert_eq!(pos.line, 41),
            other => panic!("{other:?} is not a position"),
        }
    }

    #[test]
    fn line_zero_and_junk_do_not_panic() {
        let mut p = Palette::line();
        p.push('0');
        assert!(matches!(p.chosen(), Some(Target::Pos(pos)) if pos.line == 0));

        let mut p = Palette::line();
        p.push('x');
        assert!(p.chosen().is_none());
    }

    fn lines(of: &[&str]) -> Vec<String> {
        of.iter().map(|s| s.to_string()).collect()
    }

    fn typed(p: &mut Palette, query: &str) {
        for c in query.chars() {
            p.push(c);
        }
    }

    #[test]
    fn find_rows_carry_their_line_number() {
        let mut p = Palette::find(&lines(&["alpha", "beta", "gamma"]));
        typed(&mut p, "beta");
        assert!(p.rows(10)[0].text.starts_with("2: "));
        match p.chosen() {
            Some(Target::Pos(pos)) => assert_eq!(pos.line, 1),
            other => panic!("{other:?} is not a position"),
        }
    }

    #[test]
    fn find_is_literal_not_fuzzy() {
        // The complaint that started this: searching a document for `kubi`
        // returned lines where those letters merely appeared in order.
        let mut p = Palette::find(&lines(&[
            "the issue number, KB number, or doc link",
            "kubide exists to be native",
        ]));
        typed(&mut p, "kubi");
        let rows = p.rows(10);
        assert_eq!(rows.len(), 1, "{:?}", rows.iter().map(|r| &r.text).collect::<Vec<_>>());
        assert!(rows[0].text.contains("kubide exists"));
    }

    #[test]
    fn the_caret_lands_on_the_match_not_the_line_start() {
        // Jumping to column zero of a long line leaves you hunting for the
        // thing you just searched for.
        let mut p = Palette::find(&lines(&["let needle = 1;"]));
        typed(&mut p, "needle");
        match p.chosen() {
            Some(Target::Pos(pos)) => assert_eq!((pos.line, pos.col), (0, 4)),
            other => panic!("{other:?} is not a position"),
        }
    }

    #[test]
    fn a_line_with_several_matches_gets_a_row_each() {
        // This is what "no next/previous within a line" was: three places to
        // go, folded into one row you could not step through.
        let mut p = Palette::find(&lines(&["x = x + x"]));
        typed(&mut p, "x");
        assert_eq!(p.rows(10).len(), 3);

        let columns: Vec<usize> = (0..3)
            .map(|i| {
                p.selected = i;
                match p.chosen() {
                    Some(Target::Pos(pos)) => pos.col,
                    other => panic!("{other:?}"),
                }
            })
            .collect();
        assert_eq!(columns, vec![0, 4, 8]);
    }

    #[test]
    fn the_highlight_clears_the_line_number() {
        // The row is "12: text", so a match at column 0 is not at position 0.
        let mut p = Palette::find(&lines(&["needle"]));
        typed(&mut p, "needle");
        let row = &p.rows(1)[0];
        let shift = row.text.find("needle").unwrap();
        assert_eq!(row.hits, (shift..shift + 6).collect::<Vec<_>>());
    }

    #[test]
    fn find_lists_the_file_before_you_type() {
        // An empty box reads as a broken widget.
        let p = Palette::find(&lines(&["a", "b", "c"]));
        assert_eq!(p.rows(10).len(), 3);
        assert!(p.note.is_none());
    }

    #[test]
    fn find_says_when_nothing_matches() {
        let mut p = Palette::find(&lines(&["alpha"]));
        typed(&mut p, "zzzz");
        assert!(p.rows(10).is_empty());
        assert!(p.note.as_deref().unwrap().contains("zzzz"));
    }

    #[test]
    fn line_numbers_line_up_in_a_long_file() {
        // Ragged numbers put the text in a different column on every row.
        let many: Vec<String> = (0..120).map(|i| format!("line {i}")).collect();
        let p = Palette::find(&many);
        let rows = p.rows(200);
        assert!(rows[0].text.starts_with("  1: "));
        assert!(rows[99].text.starts_with("100: "));
    }

    #[test]
    fn project_results_still_narrow_by_fuzzy_match() {
        // Results are a list to filter, not a document to search, which is
        // why they are their own mode rather than sharing Find's.
        let root = std::path::Path::new("/repo");
        let hit = |name: &str| kb_git::Hit {
            path: root.join(name),
            line: 0,
            text: "x".into(),
        };
        let mut p = Palette::results(vec![hit("src/main.rs"), hit("docs/notes.md")], root);
        typed(&mut p, "mn");
        assert!(p.rows(10)[0].text.starts_with("src/main.rs"), "{}", p.rows(10)[0].text);
    }
}
