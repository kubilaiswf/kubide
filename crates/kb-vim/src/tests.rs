//! Behaviour tests, written as key sequences against small buffers.
//!
//! Keys are given in vim's own notation — `dw`, `ci(`, `3ihi<Esc>` — so a
//! test reads like the thing it checks, and a vim user can tell at a glance
//! whether the expectation is right.

use super::*;

#[derive(Default)]
struct FakeHost {
    clip: Option<String>,
}

impl Host for FakeHost {
    fn clipboard(&mut self) -> Option<String> {
        self.clip.clone()
    }
    fn set_clipboard(&mut self, text: &str) {
        self.clip = Some(text.to_string());
    }
}

struct T {
    vim: Vim,
    s: Session,
    buf: Buffer,
    host: FakeHost,
    fx: Vec<Effect>,
    ctx: Ctx,
}

impl T {
    fn new(text: &str) -> T {
        T {
            vim: Vim::new(),
            s: Session::default(),
            buf: Buffer::from_text(text),
            host: FakeHost::default(),
            fx: Vec::new(),
            ctx: Ctx { top: 0, visible: 20, auto_close: false },
        }
    }

    /// Feeds keys in vim notation.
    fn keys(&mut self, s: &str) -> &mut T {
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            let key = if c == '<' {
                let mut name = String::new();
                for n in chars.by_ref() {
                    if n == '>' {
                        break;
                    }
                    name.push(n);
                }
                match name.as_str() {
                    "Esc" => Key::Esc,
                    "CR" => Key::Enter,
                    "BS" => Key::Backspace,
                    "Del" => Key::Delete,
                    "Tab" => Key::Tab,
                    "Up" => Key::Up,
                    "Down" => Key::Down,
                    "Left" => Key::Left,
                    "Right" => Key::Right,
                    "Home" => Key::Home,
                    "End" => Key::End,
                    "lt" => Key::Char('<'),
                    n if n.starts_with("C-") => Key::Ctrl(n.chars().nth(2).unwrap()),
                    other => panic!("unknown key <{other}>"),
                }
            } else {
                Key::Char(c)
            };
            match self.vim.key(key, &mut self.buf, &mut self.s, &mut self.host, self.ctx) {
                Outcome::Handled(fx) => self.fx.extend(fx),
                Outcome::Pass => {
                    // What the editor would do with a key vim declined.
                    match key {
                        Key::Tab => self.buf.insert("    "),
                        Key::Left => self.buf.move_left(false),
                        Key::Right => self.buf.move_right(false),
                        Key::Up => self.buf.move_vertical(-1, false),
                        Key::Down => self.buf.move_vertical(1, false),
                        Key::Home => self.buf.move_line_start(false),
                        Key::End => self.buf.move_line_end(false),
                        _ => {}
                    }
                }
            }
        }
        self
    }

    fn text(&self) -> String {
        self.buf.to_text()
    }

    fn cursor(&self) -> (usize, usize) {
        (self.buf.cursor.line, self.buf.cursor.col)
    }

    fn reg(&self, c: char) -> String {
        self.s.registers.get(&c).map(|r| r.text.clone()).unwrap_or_default()
    }
}

// -- modes -----------------------------------------------------------------

#[test]
fn starts_in_normal_mode_and_i_enters_insert() {
    let mut t = T::new("abc");
    assert_eq!(t.vim.mode(), Mode::Normal);
    t.keys("ixy<Esc>");
    assert_eq!(t.text(), "xyabc");
    assert_eq!(t.vim.mode(), Mode::Normal);
    assert_eq!(t.cursor(), (0, 1), "Esc steps back onto the last typed character");
}

#[test]
fn normal_mode_caret_never_sits_past_the_last_character() {
    let mut t = T::new("abc");
    t.keys("$");
    assert_eq!(t.cursor(), (0, 2));
    t.keys("l");
    assert_eq!(t.cursor(), (0, 2), "l stops at the end");
    t.keys("A!<Esc>");
    assert_eq!(t.text(), "abc!");
    assert_eq!(t.cursor(), (0, 3));
}

#[test]
fn a_and_o_and_their_capitals() {
    let mut t = T::new("  hello\nworld");
    t.keys("a-<Esc>");
    assert_eq!(t.buf.line(0), " - hello");
    t.keys("A!<Esc>");
    assert_eq!(t.buf.line(0), " - hello!");
    t.keys("I#<Esc>");
    assert_eq!(t.buf.line(0), " #- hello!", "I goes to the first non-blank");
    t.keys("gI@<Esc>");
    assert_eq!(t.buf.line(0), "@ #- hello!");
    t.keys("onew<Esc>");
    assert_eq!(t.text(), "@ #- hello!\nnew\nworld");
    t.keys("Oabove<Esc>");
    assert_eq!(t.text(), "@ #- hello!\nabove\nnew\nworld");
}

#[test]
fn o_carries_the_indent() {
    let mut t = T::new("    x");
    t.keys("oy<Esc>");
    assert_eq!(t.text(), "    x\n    y");
}

#[test]
fn a_counted_insert_repeats_the_text() {
    let mut t = T::new("");
    t.keys("3ihi<Esc>");
    assert_eq!(t.text(), "hihihi");
    let mut t = T::new("a");
    t.keys("2oline<Esc>");
    assert_eq!(t.text(), "a\nline\nline");
}

#[test]
fn an_insert_session_is_one_undo_step() {
    let mut t = T::new("");
    t.keys("ione<CR>two<CR>three<Esc>");
    assert_eq!(t.text(), "one\ntwo\nthree");
    t.keys("u");
    assert_eq!(t.text(), "", "one u takes the whole session");
    t.keys("<C-r>");
    assert_eq!(t.text(), "one\ntwo\nthree");
}

#[test]
fn ctrl_w_and_ctrl_u_in_insert_mode() {
    let mut t = T::new("");
    t.keys("ifoo bar<C-w><Esc>");
    assert_eq!(t.text(), "foo ");
    let mut t = T::new("");
    t.keys("ifoo bar<C-u>x<Esc>");
    assert_eq!(t.text(), "x");
}

#[test]
fn ctrl_o_runs_one_normal_command_from_insert() {
    let mut t = T::new("abc def");
    t.keys("i<C-o>$!<Esc>");
    assert_eq!(t.text(), "abc def!");
    assert_eq!(t.vim.mode(), Mode::Normal);
}

#[test]
fn ctrl_r_inserts_a_register() {
    let mut t = T::new("word");
    t.keys("yiwA <C-r>\"<Esc>");
    assert_eq!(t.text(), "word word");
}

#[test]
fn replace_mode_overwrites_and_backspace_restores() {
    let mut t = T::new("abcdef");
    t.keys("lRXY<Esc>");
    assert_eq!(t.text(), "aXYdef");
    t.keys("lRxyz<BS><BS><Esc>");
    assert_eq!(t.text(), "aXYxef", "backspace puts the covered characters back");
    let mut t = T::new("ab");
    t.keys("Rwxyz<Esc>");
    assert_eq!(t.text(), "wxyz", "past the end it appends");
}

#[test]
fn escape_from_insert_at_column_zero_stays_there() {
    let mut t = T::new("abc");
    t.keys("i<Esc>");
    assert_eq!(t.cursor(), (0, 0));
}

// -- motions ---------------------------------------------------------------

#[test]
fn hjkl_and_counts() {
    let mut t = T::new("abcdef\nxy\nlonger line");
    t.keys("3l");
    assert_eq!(t.cursor(), (0, 3));
    t.keys("j");
    assert_eq!(t.cursor(), (1, 1), "clamped to the short line");
    t.keys("j");
    assert_eq!(t.cursor(), (2, 3), "and back to the wanted column");
    t.keys("k2h");
    assert_eq!(t.cursor(), (1, 0));
    t.keys("h");
    assert_eq!(t.cursor(), (1, 0), "h does not wrap");
}

#[test]
fn dollar_sticks_to_the_line_end_going_down() {
    let mut t = T::new("abc\nlonger\nxy");
    t.keys("$j");
    assert_eq!(t.cursor(), (1, 5));
    t.keys("j");
    assert_eq!(t.cursor(), (2, 1));
}

#[test]
fn word_motions() {
    let mut t = T::new("foo bar_baz, qux\n\n  next");
    t.keys("w");
    assert_eq!(t.cursor(), (0, 4));
    t.keys("w");
    assert_eq!(t.cursor(), (0, 11), "punctuation is its own word");
    t.keys("w");
    assert_eq!(t.cursor(), (0, 13));
    t.keys("w");
    assert_eq!(t.cursor(), (1, 0), "an empty line is a word");
    t.keys("w");
    assert_eq!(t.cursor(), (2, 2));
    t.keys("b");
    assert_eq!(t.cursor(), (1, 0));
    t.keys("b");
    assert_eq!(t.cursor(), (0, 13));
    t.keys("gg");
    t.keys("e");
    assert_eq!(t.cursor(), (0, 2));
    t.keys("e");
    assert_eq!(t.cursor(), (0, 10));
    t.keys("W");
    assert_eq!(t.cursor(), (0, 13), "W skips punctuation");
    t.keys("ge");
    assert_eq!(t.cursor(), (0, 11));
    t.keys("0E");
    assert_eq!(t.cursor(), (0, 2));
    t.keys("E");
    assert_eq!(t.cursor(), (0, 11));
}

#[test]
fn line_and_file_motions() {
    let mut t = T::new("  a\nb\nc\n  d");
    t.keys("$0");
    assert_eq!(t.cursor(), (0, 0));
    t.keys("^");
    assert_eq!(t.cursor(), (0, 2));
    t.keys("G");
    assert_eq!(t.cursor(), (3, 2), "G lands on the first non-blank");
    t.keys("gg");
    assert_eq!(t.cursor(), (0, 2));
    t.keys("3G");
    assert_eq!(t.cursor(), (2, 0));
    t.keys("2gg");
    assert_eq!(t.cursor(), (1, 0));
    t.keys("+");
    assert_eq!(t.cursor(), (2, 0));
    t.keys("-");
    assert_eq!(t.cursor(), (1, 0));
    t.keys("<CR>");
    assert_eq!(t.cursor(), (2, 0));
    t.keys("50%");
    assert_eq!(t.cursor(), (1, 0));
}

#[test]
fn find_and_till_and_their_repeats() {
    let mut t = T::new("a-b-c-d");
    t.keys("f-");
    assert_eq!(t.cursor(), (0, 1));
    t.keys(";");
    assert_eq!(t.cursor(), (0, 3));
    t.keys(",");
    assert_eq!(t.cursor(), (0, 1));
    t.keys("2f-");
    assert_eq!(t.cursor(), (0, 5));
    t.keys("0t-");
    assert_eq!(t.cursor(), (0, 0), "t stops before the target");
    t.keys("l;");
    assert_eq!(t.cursor(), (0, 2), "; after t skips the adjacent target");
    t.keys("$F-");
    assert_eq!(t.cursor(), (0, 5));
    t.keys("T-");
    assert_eq!(t.cursor(), (0, 4));
    t.keys("fz");
    assert_eq!(t.cursor(), (0, 4), "a missing character moves nothing");
}

#[test]
fn paragraph_motions() {
    let mut t = T::new("a\nb\n\nc\nd\n\ne");
    t.keys("}");
    assert_eq!(t.cursor(), (2, 0));
    t.keys("}");
    assert_eq!(t.cursor(), (5, 0));
    t.keys("}");
    assert_eq!(t.cursor(), (6, 0), "the end of the buffer");
    t.keys("{{");
    assert_eq!(t.cursor(), (2, 0));
}

#[test]
fn percent_jumps_between_brackets() {
    let mut t = T::new("f(a, [b]) {\n}");
    t.keys("%");
    assert_eq!(t.cursor(), (0, 8), "the first bracket on the line, then its match");
    t.keys("%");
    assert_eq!(t.cursor(), (0, 1));
    t.keys("$%");
    assert_eq!(t.cursor(), (1, 0));
}

#[test]
fn screen_motions_use_the_view() {
    let text: String = (0..50).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
    let mut t = T::new(&text);
    t.ctx.top = 10;
    t.ctx.visible = 10;
    t.keys("H");
    assert_eq!(t.cursor(), (10, 0));
    t.keys("L");
    assert_eq!(t.cursor(), (19, 0));
    t.keys("M");
    assert_eq!(t.cursor(), (14, 0));
    t.keys("3H");
    assert_eq!(t.cursor(), (12, 0));
}

#[test]
fn marks_and_jumps() {
    let mut t = T::new("  a\nb\nc\nd");
    t.keys("lmaG");
    assert_eq!(t.cursor(), (3, 0));
    t.keys("'a");
    assert_eq!(t.cursor(), (0, 2), "' goes to the first non-blank");
    t.keys("G`a");
    assert_eq!(t.cursor(), (0, 1), "` goes to the exact column");
    t.keys("<C-o>");
    assert_eq!(t.cursor(), (3, 0), "back where the jump came from");
    t.keys("<C-i>");
    assert_eq!(t.cursor(), (0, 1));
    t.keys("''");
    assert_eq!(t.cursor(), (3, 0));
    t.keys("'z");
    assert!(t.vim.message().is_some_and(|(m, e)| e && m.contains("Mark not set")));
}

// -- operators -------------------------------------------------------------

#[test]
fn dw_does_not_join_lines() {
    let mut t = T::new("one two\nthree");
    t.keys("wdw");
    assert_eq!(t.text(), "one \nthree", "the last word on the line, and only it");
    let mut t = T::new("one two\nthree");
    t.keys("dw");
    assert_eq!(t.text(), "two\nthree");
}

#[test]
fn de_and_db_and_counts() {
    let mut t = T::new("one two three four");
    t.keys("de");
    assert_eq!(t.text(), " two three four");
    t.keys("d2w");
    assert_eq!(t.text(), "three four");
    t.keys("$db");
    assert_eq!(t.text(), "three r");
    let mut t = T::new("a b c d");
    t.keys("2dw");
    assert_eq!(t.text(), "c d");
    let mut t = T::new("a b c d e f g");
    t.keys("2d3w");
    assert_eq!(t.text(), "g", "counts multiply");
}

#[test]
fn dd_and_friends() {
    let mut t = T::new("a\nb\nc\nd");
    t.keys("jdd");
    assert_eq!(t.text(), "a\nc\nd");
    assert_eq!(t.cursor(), (1, 0));
    t.keys("2dd");
    assert_eq!(t.text(), "a");
    t.keys("dd");
    assert_eq!(t.text(), "", "the last line goes too");
    let mut t = T::new("a\nb");
    t.keys("jdd");
    assert_eq!(t.text(), "a", "deleting the last line takes the newline before it");
    assert_eq!(t.cursor(), (0, 0));
}

#[test]
fn d_dollar_and_capital_d_and_c() {
    let mut t = T::new("hello world\nnext");
    t.keys("wD");
    assert_eq!(t.text(), "hello \nnext");
    assert_eq!(t.cursor(), (0, 5));
    t.keys("0Cbye<Esc>");
    assert_eq!(t.text(), "bye\nnext");
    let mut t = T::new("a b\nc\nd");
    t.keys("l2D");
    assert_eq!(t.text(), "a\nd");
}

#[test]
fn cw_keeps_the_space_after_the_word() {
    let mut t = T::new("one two");
    t.keys("cwuno<Esc>");
    assert_eq!(t.text(), "uno two");
    t.keys("wcwdos<Esc>");
    assert_eq!(t.text(), "uno dos");
    let mut t = T::new("a-b");
    t.keys("cwx<Esc>");
    assert_eq!(t.text(), "x-b", "on a one-character word cw changes just it");
    let mut t = T::new("one   two");
    t.keys("lllcw_<Esc>");
    assert_eq!(t.text(), "one_two", "on white space cw acts like dw");
}

#[test]
fn cc_keeps_the_indent() {
    let mut t = T::new("    old\nnext");
    t.keys("ccnew<Esc>");
    assert_eq!(t.text(), "    new\nnext");
    let mut t = T::new("a\n  b\nc");
    t.keys("2Sx<Esc>");
    assert_eq!(t.text(), "x\nc");
}

#[test]
fn x_and_capital_x_and_s() {
    let mut t = T::new("abcdef");
    t.keys("x");
    assert_eq!(t.text(), "bcdef");
    t.keys("$x");
    assert_eq!(t.text(), "bcde");
    assert_eq!(t.cursor(), (0, 3), "the caret steps back onto the new last character");
    t.keys("2X");
    assert_eq!(t.text(), "be");
    t.keys("sZ<Esc>");
    assert_eq!(t.text(), "bZ");
    let mut t = T::new("abc");
    t.keys("5x");
    assert_eq!(t.text(), "", "a count past the end takes what is there");
    let mut t = T::new("");
    t.keys("x");
    assert_eq!(t.text(), "", "nothing to delete on an empty line");
}

#[test]
fn yank_and_put() {
    let mut t = T::new("one two");
    t.keys("yw$p");
    assert_eq!(t.text(), "one twoone ");
    let mut t = T::new("abc");
    t.keys("ylP");
    assert_eq!(t.text(), "aabc");
    assert_eq!(t.cursor(), (0, 0));
    t.keys("yl3p");
    assert_eq!(t.text(), "aaaaabc", "a count repeats the text");
}

#[test]
fn linewise_put_goes_below_or_above() {
    let mut t = T::new("  a\nb");
    t.keys("yyjp");
    assert_eq!(t.text(), "  a\nb\n  a");
    assert_eq!(t.cursor(), (2, 2), "on the first non-blank of the new line");
    t.keys("P");
    assert_eq!(t.text(), "  a\nb\n  a\n  a");
    assert_eq!(t.cursor(), (2, 2));
    t.keys("ggyj");
    t.keys("Gp");
    assert_eq!(t.text(), "  a\nb\n  a\n  a\n  a\nb");
}

#[test]
fn gp_leaves_the_caret_after_the_text() {
    let mut t = T::new("a\nb");
    t.keys("yygp");
    assert_eq!(t.text(), "a\na\nb");
    assert_eq!(t.cursor(), (2, 0));
}

#[test]
fn capital_y_yanks_the_line_and_y_moves_to_the_start() {
    let mut t = T::new("abc def");
    t.keys("wY");
    assert_eq!(t.reg('"'), "abc def");
    assert_eq!(t.cursor(), (0, 4), "Y does not move");
    t.keys("yb");
    assert_eq!(t.cursor(), (0, 0), "a backward yank moves to the start");
    assert_eq!(t.reg('"'), "abc ");
}

#[test]
fn registers_named_numbered_and_appended() {
    let mut t = T::new("a\nb\nc");
    t.keys("yy");
    assert_eq!(t.reg('0'), "a");
    t.keys("\"xyy");
    assert_eq!(t.reg('x'), "a");
    assert_eq!(t.reg('0'), "a", "a yank into a named register leaves 0 alone");
    t.keys("j\"Xyy");
    assert_eq!(t.reg('x'), "a\nb", "uppercase appends");
    t.keys("dd");
    assert_eq!(t.reg('1'), "b");
    t.keys("dd");
    assert_eq!(t.reg('1'), "c");
    assert_eq!(t.reg('2'), "b", "deletes shift down the numbered registers");
    assert_eq!(t.reg('0'), "a", "the last yank stays in 0");
    let mut t = T::new("abc");
    t.keys("x");
    assert_eq!(t.reg('-'), "a", "a small delete goes to -");
    assert_eq!(t.reg('1'), "", "and not to 1");
    t.keys("\"_x");
    assert_eq!(t.reg('"'), "a", "the black hole leaves the unnamed register alone");
    t.keys("\"xp");
    assert_eq!(t.text(), "c", "an empty register puts nothing");
}

#[test]
fn the_clipboard_registers_reach_the_host() {
    let mut t = T::new("hello");
    t.keys("\"+yiw");
    assert_eq!(t.host.clip.as_deref(), Some("hello"));
    t.host.clip = Some("from outside".into());
    t.keys("$\"*p");
    assert_eq!(t.text(), "hellofrom outside");
    t.host.clip = Some("a line\n".into());
    t.keys("\"+p");
    assert_eq!(t.text(), "hellofrom outside\na line", "text ending in a newline is put linewise");
}

#[test]
fn clipboard_unnamedplus_makes_the_unnamed_register_the_clipboard() {
    let mut t = T::new("one\ntwo");
    t.s.options.clipboard = true;
    t.keys("yy");
    assert_eq!(t.host.clip.as_deref(), Some("one\n"));
    t.host.clip = Some("ext".into());
    t.keys("jp");
    assert_eq!(t.text(), "one\ntextwo");
}

#[test]
fn indent_and_dedent() {
    let mut t = T::new("a\nb\nc");
    t.keys(">j");
    assert_eq!(t.text(), "    a\n    b\nc");
    t.keys("3>>");
    assert_eq!(t.text(), "        a\n        b\n    c");
    t.keys("<lt>G");
    assert_eq!(t.text(), "    a\n    b\nc");
    t.keys("Vj>");
    assert_eq!(t.text(), "        a\n        b\nc");
    t.keys("gv2<lt>");
    assert_eq!(t.text(), "a\nb\nc", "a count in visual mode shifts that many times");
}

#[test]
fn case_operators() {
    let mut t = T::new("hello World");
    t.keys("~");
    assert_eq!(t.text(), "Hello World");
    assert_eq!(t.cursor(), (0, 1));
    t.keys("gUiw");
    assert_eq!(t.text(), "HELLO World");
    t.keys("guu");
    assert_eq!(t.text(), "hello world");
    t.keys("g~~");
    assert_eq!(t.text(), "HELLO WORLD");
    t.keys("wg~iw");
    assert_eq!(t.text(), "HELLO world");
    t.keys("0veU");
    assert_eq!(t.text(), "HELLO world");
    t.keys("0veu");
    assert_eq!(t.text(), "hello world");
    t.keys("3~");
    assert_eq!(t.text(), "HELlo world");
}

#[test]
fn join_lines() {
    let mut t = T::new("a\n    b\nc\n)d");
    t.keys("J");
    assert_eq!(t.text(), "a b\nc\n)d");
    assert_eq!(t.cursor(), (0, 1), "the caret lands on the joining space");
    t.keys("3J");
    assert_eq!(t.text(), "a b c)d", "no space before a )");
    let mut t = T::new("a\n  b");
    t.keys("gJ");
    assert_eq!(t.text(), "a  b", "gJ keeps the white space");
    let mut t = T::new("a\nb\nc\nd");
    t.keys("VjjJ");
    assert_eq!(t.text(), "a b c\nd");
}

#[test]
fn replace_char() {
    let mut t = T::new("abcd");
    t.keys("rx");
    assert_eq!(t.text(), "xbcd");
    t.keys("l2ry");
    assert_eq!(t.text(), "xyyd");
    assert_eq!(t.cursor(), (0, 2));
    t.keys("$5rz");
    assert_eq!(t.text(), "xyyd", "a count past the end changes nothing");
    t.keys("0lr<CR>");
    assert_eq!(t.text(), "x\nyd", "r Enter splits the line");
    let mut t = T::new("abc\ndef");
    t.keys("vjrx");
    assert_eq!(t.text(), "xxx\nxef");
}

#[test]
fn increment_and_decrement() {
    let mut t = T::new("x = 41;");
    t.keys("<C-a>");
    assert_eq!(t.text(), "x = 42;");
    assert_eq!(t.cursor(), (0, 5));
    t.keys("5<C-x>");
    assert_eq!(t.text(), "x = 37;");
    let mut t = T::new("v-1");
    t.keys("2<C-a>");
    assert_eq!(t.text(), "v1", "a leading minus counts");
    let mut t = T::new("0x0f");
    t.keys("<C-a>");
    assert_eq!(t.text(), "0x10");
    let mut t = T::new("id_1 007");
    t.keys("$<C-a>");
    assert_eq!(t.text(), "id_1 008", "leading zeros are kept");
}

// -- text objects ------------------------------------------------------------

#[test]
fn word_objects() {
    let mut t = T::new("one two three");
    t.keys("wdiw");
    assert_eq!(t.text(), "one  three");
    let mut t = T::new("one two three");
    t.keys("wdaw");
    assert_eq!(t.text(), "one three", "aw takes the space after");
    let mut t = T::new("one two");
    t.keys("$daw");
    assert_eq!(t.text(), "one", "or the space before, at the end of the line");
    let mut t = T::new("a b c d");
    t.keys("d2aw");
    assert_eq!(t.text(), "c d");
    let mut t = T::new("foo.bar baz");
    t.keys("ciWx<Esc>");
    assert_eq!(t.text(), "x baz");
}

#[test]
fn bracket_objects() {
    let mut t = T::new("f(a, (b), c)");
    t.keys("fbdi(");
    assert_eq!(t.text(), "f(a, (), c)", "the innermost pair");
    t.keys("da(");
    assert_eq!(t.text(), "f(a, , c)");
    t.keys("ci(x<Esc>");
    assert_eq!(t.text(), "f(x)");
    let mut t = T::new("f(a, (b), c)");
    t.keys("fbd2i(");
    assert_eq!(t.text(), "f()", "a count goes out a level");
    let mut t = T::new("[a] {b} <c>");
    t.keys("di[f{diBf<lt>dit");
    assert_eq!(t.text(), "[] {} <c>");
    t.keys("di<lt>");
    assert_eq!(t.text(), "[] {} <>");
    let mut t = T::new("x(y)");
    t.keys("$di(");
    assert_eq!(t.text(), "x()", "on the closer counts as inside");
    t.keys("hdi(");
    assert_eq!(t.text(), "x()", "on the opener too");
}

#[test]
fn a_block_object_takes_whole_lines() {
    let mut t = T::new("fn f() {\n    a();\n    b();\n}");
    t.keys("jdi{");
    assert_eq!(t.text(), "fn f() {\n}");
    let mut t = T::new("fn f() {\n    a();\n}");
    t.keys("jci{x<Esc>");
    assert_eq!(t.text(), "fn f() {\n    x\n}", "the indent is kept, as with cc");
}

#[test]
fn quote_objects() {
    let mut t = T::new("say \"hello there\" now");
    t.keys("fhdi\"");
    assert_eq!(t.text(), "say \"\" now");
    let mut t = T::new("say \"hello\" now");
    t.keys("fhda\"");
    assert_eq!(t.text(), "say now", "a\" takes the trailing space");
    let mut t = T::new("x = 'a' + 'b'");
    t.keys("di'");
    assert_eq!(t.text(), "x = '' + 'b'", "from before the first quote, the first pair");
    t.keys("f+di'");
    assert_eq!(t.text(), "x = '' + ''", "between pairs, the next pair");
    let mut t = T::new("a \"b\\\"c\" d");
    t.keys("fbdi\"");
    assert_eq!(t.text(), "a \"\" d", "an escaped quote does not close");
}

#[test]
fn paragraph_objects() {
    let mut t = T::new("a\nb\n\nc\n\nd");
    t.keys("dip");
    assert_eq!(t.text(), "\nc\n\nd");
    let mut t = T::new("a\nb\n\nc\n\nd");
    t.keys("dap");
    assert_eq!(t.text(), "c\n\nd", "ap takes the blank line after");
    let mut t = T::new("a\n\nb");
    t.keys("Gdap");
    assert_eq!(t.text(), "a", "or before, at the end");
    let mut t = T::new("a\nb\n\nc");
    t.keys("yipGp");
    assert_eq!(t.text(), "a\nb\n\nc\na\nb");
}

#[test]
fn sentence_objects_and_motions() {
    let mut t = T::new("One here. Two there!  Three.");
    t.keys(")");
    assert_eq!(t.cursor(), (0, 10));
    t.keys(")");
    assert_eq!(t.cursor(), (0, 22));
    t.keys("(");
    assert_eq!(t.cursor(), (0, 10));
    t.keys("dis");
    assert_eq!(t.text(), "One here.   Three.");
    let mut t = T::new("One here. Two there!  Three.");
    t.keys("fTdas");
    assert_eq!(t.text(), "One here. Three.");
}

#[test]
fn tag_objects() {
    let mut t = T::new("<div><p class=\"x\">hi <b>there</b></p></div>");
    t.keys("fhdit");
    assert_eq!(t.text(), "<div><p class=\"x\"></p></div>");
    let mut t = T::new("<div><p>hi <b>there</b></p></div>");
    t.keys("fedat");
    assert_eq!(t.text(), "<div><p>hi </p></div>");
    t.keys("d2it");
    assert_eq!(t.text(), "<div></div>");
}

// -- visual mode -------------------------------------------------------------

#[test]
fn visual_selection_is_inclusive() {
    let mut t = T::new("abcdef");
    t.keys("lvl");
    assert_eq!(t.vim.selection(&t.buf), Some((Pos::new(0, 1), Pos::new(0, 3))));
    t.keys("d");
    assert_eq!(t.text(), "adef");
    assert_eq!(t.vim.mode(), Mode::Normal);
    let mut t = T::new("abc");
    t.keys("vy");
    assert_eq!(t.reg('"'), "a", "a one-character selection is one character");
}

#[test]
fn visual_line_mode() {
    let mut t = T::new("a\nb\nc\nd");
    t.keys("jVjd");
    assert_eq!(t.text(), "a\nd");
    t.keys("Vy");
    assert!(t.s.registers[&'"'].linewise);
    t.keys("p");
    assert_eq!(t.text(), "a\nd\nd");
}

#[test]
fn visual_o_swaps_the_ends_and_gv_reselects() {
    let mut t = T::new("abcdef");
    t.keys("lvllo");
    assert_eq!(t.cursor(), (0, 1));
    t.keys("<Esc>");
    assert_eq!(t.vim.mode(), Mode::Normal);
    t.keys("$gv");
    assert_eq!(t.vim.mode(), Mode::Visual);
    assert_eq!(t.vim.selection(&t.buf), Some((Pos::new(0, 1), Pos::new(0, 4))));
    t.keys("<Esc>`<lt>");
    assert_eq!(t.cursor(), (0, 1));
    t.keys("`>");
    assert_eq!(t.cursor(), (0, 3));
}

#[test]
fn visual_objects_and_change() {
    let mut t = T::new("f(one two)");
    t.keys("fovi(");
    assert_eq!(t.vim.selection(&t.buf), Some((Pos::new(0, 2), Pos::new(0, 9))));
    t.keys("cx<Esc>");
    assert_eq!(t.text(), "f(x)");
    let mut t = T::new("one two three");
    t.keys("wviwiw");
    assert_eq!(t.vim.selection(&t.buf), Some((Pos::new(0, 4), Pos::new(0, 8))), "a second iw extends");
}

#[test]
fn visual_paste_swaps_the_registers() {
    let mut t = T::new("aaa bbb");
    t.keys("yiwwviwp");
    assert_eq!(t.text(), "aaa aaa");
    assert_eq!(t.reg('"'), "bbb", "the replaced text is what the unnamed register holds now");
}

#[test]
fn visual_capital_i_and_a_and_switching_kinds() {
    let mut t = T::new("abc");
    t.keys("lvlA!<Esc>");
    assert_eq!(t.text(), "abc!");
    t.keys("0vlI#<Esc>");
    assert_eq!(t.text(), "#abc!");
    t.keys("vV");
    assert_eq!(t.vim.mode(), Mode::VisualLine);
    t.keys("v");
    assert_eq!(t.vim.mode(), Mode::Visual);
    t.keys("v");
    assert_eq!(t.vim.mode(), Mode::Normal);
}

#[test]
fn visual_x_and_capital_d_and_search_extend() {
    let mut t = T::new("one two\nthree");
    t.keys("vex");
    assert_eq!(t.text(), " two\nthree");
    let mut t = T::new("one two\nthree");
    t.keys("lvD");
    assert_eq!(t.text(), "three", "D in visual mode is linewise");
    let mut t = T::new("one two three");
    t.keys("v/thr<CR>d");
    assert_eq!(t.text(), "hree", "a search extends the selection to the match");
}

// -- repeat and macros -------------------------------------------------------

#[test]
fn dot_repeats_a_delete() {
    let mut t = T::new("a b c d e");
    t.keys("dw..");
    assert_eq!(t.text(), "d e");
    t.keys("2.");
    assert_eq!(t.text(), "", "a count replaces the original count");
}

#[test]
fn dot_repeats_an_insert() {
    let mut t = T::new("x\ny\nz");
    t.keys("A;<Esc>j.j.");
    assert_eq!(t.text(), "x;\ny;\nz;");
    let mut t = T::new("");
    t.keys("ihi<Esc>3.");
    assert_eq!(t.text(), "hhihihii", "repeated at the caret, which Esc left on the i");
}

#[test]
fn dot_repeats_a_change_with_its_text() {
    let mut t = T::new("foo foo foo");
    t.keys("cwbar<Esc>w.w.");
    assert_eq!(t.text(), "bar bar bar");
    let mut t = T::new("a\nb\nc");
    t.keys("ccX<Esc>j.j.");
    assert_eq!(t.text(), "X\nX\nX");
}

#[test]
fn dot_repeats_a_visual_operation_on_the_same_extent() {
    let mut t = T::new("abcdefgh");
    t.keys("vlld");
    assert_eq!(t.text(), "defgh");
    t.keys(".");
    assert_eq!(t.text(), "gh");
    let mut t = T::new("a\nb\nc\nd\ne");
    t.keys("Vjd.");
    assert_eq!(t.text(), "e");
}

#[test]
fn dot_does_not_repeat_a_yank_or_a_motion() {
    let mut t = T::new("a b c");
    t.keys("xywj.");
    assert_eq!(t.text(), " b c");
    let mut t = T::new("ab");
    t.keys("xl.");
    assert_eq!(t.text(), "");
}

#[test]
fn undo_after_dot_undoes_only_the_repeat() {
    let mut t = T::new("a b c");
    t.keys("dw.u");
    assert_eq!(t.text(), "b c");
}

#[test]
fn macros_record_and_replay() {
    let mut t = T::new("1\n2\n3\n4");
    t.keys("qaA.<Esc>jq");
    assert_eq!(t.text(), "1.\n2\n3\n4");
    assert_eq!(t.s.recording(), None);
    t.keys("@a");
    assert_eq!(t.text(), "1.\n2.\n3\n4");
    t.keys("2@@");
    assert_eq!(t.text(), "1.\n2.\n3.\n4.");
    assert_eq!(t.reg('a'), "A.\u{1b}j", "the macro is text in its register");
}

#[test]
fn a_macro_stops_when_a_step_fails() {
    let mut t = T::new("a\nb\nc");
    t.keys("qqA!<Esc>jq");
    t.keys("100@q");
    assert_eq!(t.text(), "a!\nb!\nc!");
}

#[test]
fn a_macro_can_be_edited_as_text_and_run_again() {
    let mut t = T::new("x");
    t.s.registers.insert('m', Register { text: "A-\u{1b}".into(), linewise: false });
    t.keys("@m");
    assert_eq!(t.text(), "x-");
}

// -- search ------------------------------------------------------------------

#[test]
fn search_forward_backward_and_wrap() {
    let mut t = T::new("foo\nbar foo\nbaz");
    t.keys("/foo<CR>");
    assert_eq!(t.cursor(), (1, 4));
    t.keys("n");
    assert_eq!(t.cursor(), (0, 0));
    assert!(t.vim.message().is_some_and(|(m, _)| m.contains("BOTTOM")));
    t.keys("N");
    assert_eq!(t.cursor(), (1, 4));
    t.keys("?ba<CR>");
    assert_eq!(t.cursor(), (1, 0));
    t.keys("n");
    assert_eq!(t.cursor(), (2, 0), "n after ? keeps going backwards, wrapping");
    t.keys("/nothing<CR>");
    assert!(t.vim.message().is_some_and(|(m, e)| e && m.contains("E486")));
    assert_eq!(t.cursor(), (2, 0), "a failed search does not move");
}

#[test]
fn search_uses_vim_patterns_and_case_options() {
    let mut t = T::new("Foo foo\nfo0");
    t.keys("/\\vfo+$<CR>");
    assert_eq!(t.cursor(), (0, 4));
    t.keys("/Foo<CR>");
    assert_eq!(t.cursor(), (0, 0));
    t.keys(":set ic<CR>");
    t.keys("/FOO<CR>");
    assert_eq!(t.cursor(), (0, 4));
    t.keys(":set scs<CR>/Foo<CR>");
    assert_eq!(t.cursor(), (0, 0), "smartcase: an uppercase pattern is exact");
}

#[test]
fn star_and_hash_search_the_word_under_the_caret() {
    let mut t = T::new("cat concat cat");
    t.keys("*");
    assert_eq!(t.cursor(), (0, 11), "whole words only");
    t.keys("#");
    assert_eq!(t.cursor(), (0, 0));
    t.keys("g*");
    assert_eq!(t.cursor(), (0, 7), "g* matches inside words");
}

#[test]
fn d_slash_deletes_up_to_the_match() {
    let mut t = T::new("one two three");
    t.keys("d/thr<CR>");
    assert_eq!(t.text(), "three");
}

#[test]
fn hlsearch_follows_the_last_search_and_noh_clears_it() {
    let mut t = T::new("a b");
    t.keys("/b<CR>");
    assert!(t.s.highlight().is_some());
    t.keys(":noh<CR>");
    assert!(t.s.highlight().is_none());
    t.keys("n");
    assert!(t.s.highlight().is_none(), "n does not bring it back");
    t.keys("/a<CR>");
    assert!(t.s.highlight().is_some());
}

// -- the command line --------------------------------------------------------

#[test]
fn colon_commands_produce_effects() {
    let mut t = T::new("a");
    t.keys(":w<CR>");
    assert_eq!(t.fx, [Effect::Save]);
    t.fx.clear();
    t.keys(":wq<CR>");
    assert_eq!(t.fx, [Effect::SaveClose]);
    t.fx.clear();
    t.keys(":q!<CR>");
    assert_eq!(t.fx, [Effect::ClosePaneForce]);
    t.fx.clear();
    t.keys(":qa<CR>:vs<CR>:sp<CR>:e other.rs<CR>ZZ");
    assert_eq!(
        t.fx,
        [Effect::Quit, Effect::SplitRight, Effect::SplitDown, Effect::OpenFile("other.rs".into()), Effect::SaveClose]
    );
    t.fx.clear();
    t.keys("<C-w>v<C-w>l<C-w>q");
    assert_eq!(t.fx, [Effect::SplitRight, Effect::Focus(Dir::Right), Effect::ClosePane]);
}

#[test]
fn unknown_commands_say_so() {
    let mut t = T::new("a");
    t.keys(":frobnicate<CR>");
    assert!(t.vim.message().is_some_and(|(m, e)| e && m.contains("E492") && m.contains("frobnicate")));
    assert_eq!(t.vim.mode(), Mode::Normal);
}

#[test]
fn a_bare_number_goes_to_the_line() {
    let mut t = T::new("a\nb\n  c");
    t.keys(":3<CR>");
    assert_eq!(t.cursor(), (2, 2));
    t.keys(":$-1<CR>");
    assert_eq!(t.cursor(), (1, 0));
}

#[test]
fn cmdline_editing_and_escape() {
    let mut t = T::new("abc");
    t.keys(":dx<BS><BS>");
    assert_eq!(t.vim.cmdline().as_deref(), Some(":"));
    t.keys("<BS>");
    assert_eq!(t.vim.mode(), Mode::Normal, "backspace on an empty line leaves");
    t.keys(":junk<Esc>");
    assert_eq!(t.vim.mode(), Mode::Normal);
    assert_eq!(t.text(), "abc");
    t.keys(":foo bar<C-w>");
    assert_eq!(t.vim.cmdline().as_deref(), Some(":foo "));
    t.keys("<C-u>");
    assert_eq!(t.vim.cmdline().as_deref(), Some(":"));
    t.keys("<Esc>");
}

#[test]
fn cmdline_history_walks_with_up() {
    let mut t = T::new("a");
    t.keys(":set ic<CR>:noh<CR>");
    t.keys(":<Up>");
    assert_eq!(t.vim.cmdline().as_deref(), Some(":noh"));
    t.keys("<Up>");
    assert_eq!(t.vim.cmdline().as_deref(), Some(":set ic"));
    t.keys("<Down>");
    assert_eq!(t.vim.cmdline().as_deref(), Some(":noh"));
    t.keys("<Esc>:s<Up>");
    assert_eq!(t.vim.cmdline().as_deref(), Some(":set ic"), "Up matches the typed prefix");
    t.keys("<Esc>");
}

#[test]
fn substitute_on_a_line_and_the_whole_file() {
    let mut t = T::new("a a\na a");
    t.keys(":s/a/b/<CR>");
    assert_eq!(t.text(), "b a\na a");
    t.keys(":%s/a/c/g<CR>");
    assert_eq!(t.text(), "b c\nc c");
    t.keys("u");
    assert_eq!(t.text(), "b a\na a", ":%s is one undo step");
    t.keys(":%s/zzz/y/<CR>");
    assert!(t.vim.message().is_some_and(|(m, e)| e && m.contains("E486")));
}

#[test]
fn substitute_with_groups_ranges_and_special_replacements() {
    let mut t = T::new("john smith\nx\njane doe");
    t.keys(":1,3s/\\(\\w\\+\\) \\(\\w\\+\\)/\\2, \\u\\1/<CR>");
    assert_eq!(t.text(), "smith, John\nx\ndoe, Jane");
    let mut t = T::new("a,b,c");
    t.keys(":s/,/\\r/g<CR>");
    assert_eq!(t.text(), "a\nb\nc", "\\r splits the line");
    assert_eq!(t.cursor(), (2, 0), "the caret is on the last changed line");
    let mut t = T::new("path/to/x");
    t.keys(":s#/#\\\\#g<CR>");
    assert_eq!(t.text(), "path\\to\\x", "any delimiter works");
    let mut t = T::new("aXa");
    t.keys(":s/x/y/gi<CR>");
    assert_eq!(t.text(), "aya");
}

#[test]
fn substitute_repeats_and_uses_the_last_search() {
    let mut t = T::new("a a\na a\na a");
    t.keys(":s/a/b/<CR>j:s<CR>j&");
    assert_eq!(t.text(), "b a\nb a\nb a");
    t.keys("gg:s//c/g<CR>");
    assert_eq!(t.text(), "b c\nb a\nb a", "an empty pattern is the last search");
    t.keys("/b<CR>:s//d/<CR>");
    assert_eq!(t.text(), "b c\nd a\nb a");
    t.keys(":%s/a/x/gn<CR>");
    assert_eq!(t.text(), "b c\nd a\nb a", "n counts without changing");
    assert!(t.vim.message().is_some_and(|(m, _)| m.starts_with("2 match")), "{:?}", t.vim.message());
}

#[test]
fn visual_colon_prefills_the_range() {
    let mut t = T::new("a\na\na");
    t.keys("Vj:");
    assert_eq!(t.vim.cmdline().as_deref(), Some(":'<,'>"));
    t.keys("s/a/b/<CR>");
    assert_eq!(t.text(), "b\nb\na");
}

#[test]
fn global_deletes_and_runs_normal() {
    let mut t = T::new("keep\ndrop 1\nkeep\ndrop 2\ndrop 3");
    t.keys(":g/drop/d<CR>");
    assert_eq!(t.text(), "keep\nkeep");
    let mut t = T::new("a\nb\na");
    t.keys(":g/a/normal A!<CR>");
    assert_eq!(t.text(), "a!\nb\na!");
    t.keys(":v/!/d<CR>");
    assert_eq!(t.text(), "a!\na!");
    t.keys(":g/a/s/!/?/<CR>");
    assert_eq!(t.text(), "a?\na?");
    t.keys("u");
    assert_eq!(t.text(), "a!\na!", ":g is one undo step");
}

#[test]
fn normal_command_on_a_range() {
    let mut t = T::new("a\nb\nc");
    t.keys(":%normal I- <CR>");
    assert_eq!(t.text(), "- a\n- b\n- c");
    t.keys(":2,3norm $x<CR>");
    assert_eq!(t.text(), "- a\n- \n- ");
    assert_eq!(t.vim.mode(), Mode::Normal);
}

#[test]
fn delete_yank_put_move_copy_join_by_range() {
    let mut t = T::new("a\nb\nc\nd");
    t.keys(":2,3d<CR>");
    assert_eq!(t.text(), "a\nd");
    t.keys(":1y<CR>:$pu<CR>");
    assert_eq!(t.text(), "a\nd\na");
    t.keys(":1m$<CR>");
    assert_eq!(t.text(), "d\na\na");
    t.keys(":1t0<CR>");
    assert_eq!(t.text(), "d\nd\na\na");
    t.keys(":1,2j<CR>");
    assert_eq!(t.text(), "d d\na\na");
    t.keys(":2,3>>.<CR>");
    assert_eq!(t.text(), "d d\n        a\n        a");
    t.keys(":%<lt><CR>");
    assert_eq!(t.text(), "d d\n    a\n    a");
    t.keys(":$d x<CR>");
    assert_eq!(t.reg('x'), "    a");
}

#[test]
fn ranges_with_patterns_marks_and_offsets() {
    let mut t = T::new("one\ntwo\nthree\nfour\nfive");
    t.keys("jma");
    t.keys(":/four/d<CR>");
    assert_eq!(t.text(), "one\ntwo\nthree\nfive");
    t.keys(":'a,'a+1d<CR>");
    assert_eq!(t.text(), "one\nfive");
    t.keys(":.,$d<CR>");
    assert_eq!(t.text(), "one");
    let mut t = T::new("a\nb\nc\nd");
    t.keys(":2;+1d<CR>");
    assert_eq!(t.text(), "a\nd");
    t.keys(":9d<CR>");
    assert!(t.vim.message().is_some_and(|(m, e)| e && m.contains("E16")));
}

#[test]
fn sort_and_undo_commands() {
    let mut t = T::new("c\na\nB");
    t.keys(":sort<CR>");
    assert_eq!(t.text(), "B\na\nc");
    t.keys(":sort i<CR>");
    assert_eq!(t.text(), "a\nB\nc");
    t.keys(":sort!<CR>");
    assert_eq!(t.text(), "c\na\nB");
    t.keys(":undo<CR>");
    assert_eq!(t.text(), "a\nB\nc");
    t.keys(":redo<CR>");
    assert_eq!(t.text(), "c\na\nB");
    let mut t = T::new("x10\nx9\nx10");
    t.keys(":sort nu<CR>");
    assert_eq!(t.text(), "x9\nx10");
}

#[test]
fn set_reports_and_rejects() {
    let mut t = T::new("a");
    t.keys(":set nohls<CR>");
    assert!(!t.s.options.hlsearch);
    t.keys(":set hls?<CR>");
    assert!(t.vim.message().is_some_and(|(m, _)| m.contains("nohlsearch")));
    t.keys(":set clipboard=unnamedplus<CR>");
    assert!(t.s.options.clipboard);
    t.keys(":set nu<CR>");
    assert!(t.vim.message().is_some_and(|(m, e)| !e && m.contains("config.toml")));
    t.keys(":set bogus<CR>");
    assert!(t.vim.message().is_some_and(|(m, e)| e && m.contains("E518")));
    t.keys(":set ic!<CR>");
    assert!(t.s.options.ignorecase);
}

#[test]
fn edit_reloads_only_when_safe() {
    let mut t = T::new("a");
    t.keys("x:e<CR>");
    assert!(t.vim.message().is_some_and(|(m, e)| e && m.contains("E37")));
}

// -- scrolling ---------------------------------------------------------------

#[test]
fn scroll_commands_move_the_view_and_the_caret() {
    let text: String = (0..100).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
    let mut t = T::new(&text);
    t.ctx.visible = 20;
    t.keys("<C-d>");
    assert_eq!(t.cursor(), (10, 0));
    assert_eq!(t.fx.last(), Some(&Effect::ScrollTo(10)));
    t.keys("<C-f>");
    assert_eq!(t.fx.last(), Some(&Effect::ScrollTo(18)));
    assert_eq!(t.cursor(), (18, 0));
    t.keys("<C-u>");
    assert_eq!(t.cursor(), (8, 0));
    t.keys("50Gzz");
    assert_eq!(t.fx.last(), Some(&Effect::ScrollTo(39)));
    t.keys("zt");
    assert_eq!(t.fx.last(), Some(&Effect::ScrollTo(49)));
    t.keys("zb");
    assert_eq!(t.fx.last(), Some(&Effect::ScrollTo(30)));
    t.ctx.top = 49;
    t.keys("<C-e>");
    assert_eq!(t.fx.last(), Some(&Effect::ScrollTo(50)));
    assert_eq!(t.cursor(), (50, 0), "the caret is pushed along");
}

// -- status and misc ---------------------------------------------------------

#[test]
fn pending_keys_show_and_escape_clears_them() {
    let mut t = T::new("a");
    t.keys("2d");
    assert_eq!(t.vim.pending(), "2d");
    t.keys("<Esc>");
    assert_eq!(t.vim.pending(), "");
    assert_eq!(t.text(), "a");
}

#[test]
fn cursor_shapes_follow_the_mode() {
    let mut t = T::new("a");
    assert_eq!(t.vim.cursor_shape(), CursorShape::Block);
    t.keys("i");
    assert_eq!(t.vim.cursor_shape(), CursorShape::Bar);
    t.keys("<Esc>R");
    assert_eq!(t.vim.cursor_shape(), CursorShape::Underline);
    t.keys("<Esc>v");
    assert_eq!(t.vim.cursor_shape(), CursorShape::Block);
}

#[test]
fn the_mouse_can_start_a_selection_and_end_one() {
    let mut t = T::new("abcdef");
    t.buf.move_to(Pos::new(0, 1), false);
    t.buf.move_to(Pos::new(0, 4), true);
    t.vim.mouse_sync(&mut t.buf);
    assert_eq!(t.vim.mode(), Mode::Visual);
    assert_eq!(t.vim.selection(&t.buf), Some((Pos::new(0, 1), Pos::new(0, 4))));
    t.keys("d");
    assert_eq!(t.text(), "aef");
    t.buf.move_to(Pos::new(0, 3), false);
    t.vim.mouse_sync(&mut t.buf);
    assert_eq!(t.cursor(), (0, 2), "a click past the end clamps in normal mode");
}

#[test]
fn tab_passes_to_the_owner_in_insert_mode_and_is_repeated_as_spaces() {
    let mut t = T::new("");
    t.keys("i<Tab>x<Esc>");
    assert_eq!(t.text(), "    x");
    t.keys(".");
    assert_eq!(t.text(), "        xx");
}

#[test]
fn arrows_in_insert_mode_start_a_new_undo_step() {
    let mut t = T::new("");
    t.keys("iab<Left>c<Esc>");
    assert_eq!(t.text(), "acb");
    t.keys("u");
    assert_eq!(t.text(), "ab");
}

#[test]
fn ctrl_keys_are_declared_per_mode() {
    let mut t = T::new("a");
    assert!(t.vim.wants_ctrl('r'));
    assert!(!t.vim.wants_ctrl('s'));
    t.keys("i");
    assert!(t.vim.wants_ctrl('w'));
    assert!(!t.vim.wants_ctrl('s'));
    t.keys("<Esc>:");
    assert!(t.vim.wants_ctrl('u'));
    assert!(!t.vim.wants_ctrl('d'));
}

#[test]
fn file_info_and_messages() {
    let mut t = T::new("a\nb");
    t.keys("<C-g>");
    assert!(t.vim.message().is_some_and(|(m, _)| m.contains("2 lines")));
    t.keys("l");
    assert!(t.vim.message().is_none(), "the next key clears it");
}

#[test]
fn deleting_many_lines_reports_it() {
    let mut t = T::new("a\nb\nc\nd\ne");
    t.keys("4dd");
    assert!(t.vim.message().is_some_and(|(m, _)| m == "4 fewer lines"));
}

#[test]
fn escape_in_normal_mode_is_harmless() {
    let mut t = T::new("ab");
    t.keys("<Esc><Esc>x");
    assert_eq!(t.text(), "b");
}
