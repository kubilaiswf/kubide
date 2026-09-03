//! The `:` command line.
//!
//! The commands people actually type: writing and quitting, `:s`, `:g`,
//! `:normal`, line ranges, `:set` for the few options that exist here, and
//! the window commands mapped onto panes. Anything else is refused with
//! vim's own error number, so someone who knows vim knows what happened.

use kb_edit::{Buffer, Pos};

use crate::ops::{delete_range, range_text, StoreKind};
use crate::regex::expand_replacement;
use crate::{first_non_blank, Ctx, Effect, Host, Key, Mode, Range, Register, Session, Vim};

/// The two halves of `s/pat/rep/flags`: text up to an unescaped delimiter,
/// and what follows it. `\/` stays in the pattern as `/`, other escapes are
/// kept for the pattern compiler to read.
fn split_delim(text: &str, delim: char) -> (String, Option<&str>) {
    let mut out = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some((_, e)) if e == delim => out.push(e),
                Some((_, e)) => {
                    out.push('\\');
                    out.push(e);
                }
                None => out.push('\\'),
            }
            continue;
        }
        if c == delim {
            return (out, Some(&text[i + c.len_utf8()..]));
        }
        out.push(c);
    }
    (out, None)
}

/// A resolved line range, zero-based and inclusive; `None` when the command
/// line gave none.
type Lines = Option<(usize, usize)>;

impl Vim {
    /// Runs one command line. `line` is what was typed after the `:`.
    pub(crate) fn ex(&mut self, line: &str, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx) -> Vec<Effect> {
        let line = line.trim_start_matches([':', ' ']);
        if line.trim().is_empty() {
            return Vec::new();
        }
        let (range, rest) = match self.parse_range(buf, s, line) {
            Ok(r) => r,
            Err(e) => {
                self.error(&e);
                return Vec::new();
            }
        };
        let rest = rest.trim_start();
        if rest.trim().is_empty() {
            // A bare range goes there: `:42`.
            if let Some((_, last)) = range {
                let line = last.min(buf.len() - 1);
                self.push_jump(buf.cursor);
                buf.set_cursor(Pos::new(line, first_non_blank(buf, line)));
            }
            return Vec::new();
        }

        // The command name: letters, or one of the symbol commands.
        let name_len = rest.chars().take_while(|c| c.is_ascii_alphabetic()).count();
        let (name, args) = if name_len == 0 {
            let c = rest.chars().next().unwrap();
            (&rest[..c.len_utf8()], &rest[c.len_utf8()..])
        } else {
            rest.split_at(name_len)
        };
        let bang = args.starts_with('!');
        let args = if bang { &args[1..] } else { args };
        // `:normal` keeps its trailing spaces — `:normal A ` appends one —
        // so the raw text is kept beside the trimmed one.
        let raw_args = args.strip_prefix(' ').unwrap_or(args);
        let args = args.trim();
        let mut fx = Vec::new();
        let cur = buf.cursor.line;
        let lines = range.unwrap_or((cur, cur));
        let (first, last) = (lines.0.min(buf.len() - 1), lines.1.min(buf.len() - 1));

        match name {
            "w" | "write" | "up" | "update" => {
                if !args.is_empty() {
                    self.error("E: writing to another file is not supported here");
                } else {
                    fx.push(Effect::Save);
                }
            }
            "wq" | "x" | "xit" | "exi" | "exit" => fx.push(Effect::SaveClose),
            "wa" | "wall" => fx.push(Effect::SaveAll),
            "wqa" | "wqall" | "xa" | "xall" => {
                fx.push(Effect::SaveAll);
                fx.push(Effect::Quit);
            }
            "q" | "quit" | "clo" | "close" => fx.push(if bang { Effect::ClosePaneForce } else { Effect::ClosePane }),
            "qa" | "qall" | "quita" | "quitall" | "cq" => fx.push(if bang { Effect::QuitForce } else { Effect::Quit }),
            "e" | "edit" => {
                if !args.is_empty() {
                    fx.push(Effect::OpenFile(args.to_string()));
                } else if bang || !buf.modified() {
                    match buf.reload() {
                        Ok(()) => self.say("reloaded"),
                        Err(e) => self.error(&format!("E: {e}")),
                    }
                } else {
                    self.error("E37: No write since last change (add ! to override)");
                }
            }
            "s" | "su" | "substitute" | "&" | "&&" => {
                let repeat = name.starts_with('&') || args.is_empty() || !args.starts_with(|c: char| !c.is_alphanumeric() && !c.is_whitespace() && c != '"' && c != '|');
                let keep_flags = name == "&&";
                self.substitute(buf, s, range, args, repeat, keep_flags);
            }
            "g" | "global" | "v" | "vglobal" => {
                let invert = name.starts_with('v') || bang;
                fx.extend(self.global(buf, s, host, ctx, range, args, invert));
            }
            "d" | "de" | "del" | "delete" | "y" | "ya" | "yank" => {
                let delete = name.starts_with('d');
                let mut reg = None;
                let mut rest = args;
                if let Some(c) = rest.chars().next().filter(|c| !c.is_ascii_digit()) {
                    if crate::parse::is_register(c) {
                        reg = Some(c);
                        rest = rest[c.len_utf8()..].trim();
                    }
                }
                let (first, last) = match rest.parse::<usize>() {
                    Ok(n) if n > 0 => (last, (last + n - 1).min(buf.len() - 1)),
                    _ => (first, last),
                };
                let r = Range::lines(first, last);
                let text = range_text(buf, &r);
                if delete {
                    self.store(s, host, reg, text, true, StoreKind::Delete);
                    delete_range(buf, &r);
                    let line = first.min(buf.len() - 1);
                    buf.set_cursor(Pos::new(line, first_non_blank(buf, line)));
                    if last + 1 - first > 2 {
                        self.say(&format!("{} fewer lines", last + 1 - first));
                    }
                } else {
                    self.store(s, host, reg, text, true, StoreKind::Yank);
                    if last + 1 - first > 2 {
                        self.say(&format!("{} lines yanked", last + 1 - first));
                    }
                }
            }
            "pu" | "put" => {
                let reg = args.chars().next().filter(|c| crate::parse::is_register(*c));
                match self.fetch(s, host, reg) {
                    Some(r) => {
                        let r = Register { text: r.text, linewise: true };
                        let at = if bang { first } else { last };
                        let block = format!("{}\n", r.text);
                        if bang {
                            buf.edit(Pos::new(at, 0), Pos::new(at, 0), &block);
                            buf.set_cursor(Pos::new(at, first_non_blank(buf, at)));
                        } else {
                            let end = Pos::new(at, buf.line_len(at));
                            buf.edit(end, end, &format!("\n{}", r.text));
                            buf.set_cursor(Pos::new(at + 1, first_non_blank(buf, at + 1)));
                        }
                    }
                    None => self.error("E353: Nothing in register"),
                }
            }
            "j" | "join" => {
                let last = if range.is_some_and(|(a, b)| a != b) { last } else { last + 1 };
                let last = match args.parse::<usize>() {
                    Ok(n) if n > 1 => last.max(first + n - 1),
                    _ => last,
                };
                self.join_lines(buf, first, last.min(buf.len() - 1), !bang);
            }
            ">" | "<" => {
                // `:>>` shifts twice.
                let times = 1 + args.chars().take_while(|c| *c == name.chars().next().unwrap()).count();
                let r = Range::lines(first, last);
                let op = if name == ">" { crate::parse::Op::Indent } else { crate::parse::Op::Dedent };
                self.apply_op(op, r, None, times, buf, s, host, &ctx);
            }
            "m" | "mo" | "move" | "t" | "co" | "copy" => {
                let dest = match self.parse_range(buf, s, args) {
                    Ok((Some((_, d)), _)) => Some(d as i64),
                    Ok((None, rest)) if rest.trim() == "0" => Some(-1),
                    _ => None,
                };
                let Some(dest) = dest else {
                    self.error("E14: Invalid address");
                    return fx;
                };
                let block: Vec<String> = buf.lines()[first..=last].to_vec();
                let text = block.join("\n");
                let moving = name.starts_with('m');
                if moving && dest >= first as i64 - 1 && dest <= last as i64 {
                    if dest != first as i64 - 1 {
                        self.error("E134: Cannot move a range of lines into itself");
                    }
                    return fx;
                }
                // Insert after `dest` (`0` means before the first line).
                let insert_after = |buf: &mut Buffer, dest: i64, text: &str| -> usize {
                    if dest < 0 {
                        buf.edit(Pos::new(0, 0), Pos::new(0, 0), &format!("{text}\n"));
                        0
                    } else {
                        let d = (dest as usize).min(buf.len() - 1);
                        let end = Pos::new(d, buf.line_len(d));
                        buf.edit(end, end, &format!("\n{text}"));
                        d + 1
                    }
                };
                let landed = if moving && dest > last as i64 {
                    let at = insert_after(buf, dest, &text);
                    delete_range(buf, &Range::lines(first, last));
                    at - (last + 1 - first)
                } else {
                    let at = insert_after(buf, dest, &text);
                    if moving {
                        let shift = last + 1 - first;
                        delete_range(buf, &Range::lines(first + shift, last + shift));
                    }
                    at
                };
                let line = (landed + block.len() - 1).min(buf.len() - 1);
                buf.set_cursor(Pos::new(line, first_non_blank(buf, line)));
            }
            "p" | "print" | "P" | "nu" | "number" | "#" | "l" | "list" => {
                let shown: Vec<String> = (first..=last).map(|i| format!("{:>4} {}", i + 1, buf.line(i))).collect();
                self.say(&shown.join("  |  "));
                buf.set_cursor(Pos::new(last, first_non_blank(buf, last)));
            }
            "noh" | "nohl" | "nohlsearch" => s.hl = None,
            "se" | "set" | "setl" | "setlocal" => self.set(s, args),
            "sp" | "split" | "new" => {
                fx.push(Effect::SplitDown);
                if !args.is_empty() {
                    fx.push(Effect::OpenFile(args.to_string()));
                }
            }
            "vs" | "vsp" | "vsplit" | "vne" | "vnew" => {
                fx.push(Effect::SplitRight);
                if !args.is_empty() {
                    fx.push(Effect::OpenFile(args.to_string()));
                }
            }
            "on" | "only" => self.error("E: :only is not available; close the other panes"),
            "term" | "terminal" => fx.push(Effect::OpenTerminal),
            "reg" | "registers" | "di" | "display" => {
                let mut names: Vec<char> = s.registers.keys().copied().collect();
                names.sort();
                let shown: Vec<String> = names
                    .iter()
                    .filter(|c| args.is_empty() || args.contains(**c))
                    .filter_map(|c| s.registers.get(c).map(|r| (c, r)))
                    .map(|(c, r)| format!("\"{c}  {}", r.text.replace('\n', "^J").chars().take(40).collect::<String>()))
                    .collect();
                self.say(&if shown.is_empty() { "no registers".to_string() } else { shown.join("   ") });
            }
            "marks" => {
                let mut names: Vec<(&char, &Pos)> = self.marks.iter().collect();
                names.sort();
                let shown: Vec<String> = names.iter().map(|(c, p)| format!("{c} {}:{}", p.line + 1, p.col + 1)).collect();
                self.say(&if shown.is_empty() { "no marks".to_string() } else { shown.join("   ") });
            }
            "ju" | "jumps" => {
                let shown: Vec<String> = self.jumps.iter().map(|p| format!("{}:{}", p.line + 1, p.col + 1)).collect();
                self.say(&if shown.is_empty() { "no jumps".to_string() } else { shown.join("  ") });
            }
            "norm" | "normal" => fx.extend(self.normal_cmd(buf, s, host, ctx, range, raw_args)),
            "u" | "un" | "undo" => {
                if !buf.undo() {
                    self.error("Already at oldest change");
                }
                self.clamp_normal(buf);
            }
            "red" | "redo" => {
                if !buf.redo() {
                    self.error("Already at newest change");
                }
                self.clamp_normal(buf);
            }
            "sor" | "sort" => {
                let (first, last) = if range.is_some() { (first, last) } else { (0, buf.len() - 1) };
                let mut block: Vec<String> = buf.lines()[first..=last].to_vec();
                let ci = args.contains('i');
                let numeric = args.contains('n');
                let key = |l: &String| -> (i128, String) {
                    let num = if numeric {
                        let digits: String = l.chars().skip_while(|c| !c.is_ascii_digit() && *c != '-').take_while(|c| c.is_ascii_digit() || *c == '-').collect();
                        digits.parse().unwrap_or(0)
                    } else {
                        0
                    };
                    (num, if ci { l.to_lowercase() } else { l.clone() })
                };
                block.sort_by_cached_key(key);
                if bang {
                    block.reverse();
                }
                if args.contains('u') {
                    block.dedup_by_key(|l| if ci { l.to_lowercase() } else { l.clone() });
                }
                let text = block.join("\n");
                buf.edit(Pos::new(first, 0), Pos::new(last, buf.line_len(last)), &text);
                buf.set_cursor(Pos::new(first, 0));
            }
            "h" | "help" => self.say("no :help here — the vim section of config.example.toml lists what works"),
            "map" | "nmap" | "imap" | "vmap" | "noremap" | "nnoremap" | "inoremap" | "vnoremap" | "let" | "au" | "autocmd" | "so" | "source" => {
                self.error(&format!("E: :{name} is not supported; shortcuts live in the [keys] table of config.toml"))
            }
            "sy" | "syntax" | "colo" | "colorscheme" | "hi" | "highlight" => {
                self.error(&format!("E: :{name} is not supported; colours come from the theme files"))
            }
            _ => self.error(&format!("E492: Not an editor command: {rest}")),
        }
        fx
    }

    /// Reads a line range off the front of a command line. Line numbers
    /// come back zero-based.
    fn parse_range<'a>(&mut self, buf: &Buffer, s: &Session, text: &'a str) -> Result<(Lines, &'a str), String> {
        let last = buf.len() - 1;
        let mut rest = text.trim_start();
        if let Some(r) = rest.strip_prefix('%') {
            return Ok((Some((0, last)), r));
        }
        let mut addrs: Vec<usize> = Vec::new();
        let mut base = buf.cursor.line;
        loop {
            let (addr, after) = self.parse_address(buf, s, rest, base)?;
            rest = after;
            let Some(a) = addr else { break };
            addrs.push(a);
            let after = rest.trim_start();
            if let Some(r) = after.strip_prefix(',') {
                rest = r;
            } else if let Some(r) = after.strip_prefix(';') {
                // `;` makes the next address relative to this one.
                base = a;
                rest = r;
            } else {
                break;
            }
        }
        Ok(match addrs.len() {
            0 => (None, rest),
            1 => (Some((addrs[0], addrs[0])), rest),
            _ => {
                let (a, b) = (addrs[addrs.len() - 2], addrs[addrs.len() - 1]);
                (Some(if a <= b { (a, b) } else { (b, a) }), rest)
            }
        })
    }

    /// One address: `.`, `$`, a number, `'m`, `/pat/`, `?pat?`, each with
    /// any number of `+N` / `-N` after it. A bare offset counts from the
    /// current line.
    fn parse_address<'a>(&mut self, buf: &Buffer, s: &Session, text: &'a str, base: usize) -> Result<(Option<usize>, &'a str), String> {
        let last = buf.len() as i64 - 1;
        let mut rest = text.trim_start();
        let mut line: Option<i64> = None;
        let first = rest.chars().next();
        match first {
            Some('.') => {
                line = Some(base as i64);
                rest = &rest[1..];
            }
            Some('$') => {
                line = Some(last);
                rest = &rest[1..];
            }
            Some(c) if c.is_ascii_digit() => {
                let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
                rest = &rest[n.len()..];
                // Line numbers are one-based; `0` is "before the first".
                line = Some(n.parse::<i64>().unwrap_or(0) - 1);
            }
            Some('\'') => {
                let m = rest.chars().nth(1).ok_or("E20: Mark not set")?;
                let p = match m {
                    '<' | '>' => self.marks.get(&m).copied(),
                    _ => self.marks.get(&m).copied(),
                }
                .ok_or("E20: Mark not set")?;
                line = Some(p.line as i64);
                rest = &rest[1 + m.len_utf8()..];
            }
            Some(d @ ('/' | '?')) => {
                let (pat, after) = split_delim(&rest[1..], d);
                rest = after.unwrap_or("");
                let pattern = if pat.is_empty() {
                    s.last_search.as_ref().map(|(p, _)| p.clone()).ok_or("E35: No previous regular expression")?
                } else {
                    pat
                };
                let re = s.compile(&pattern)?;
                let total = buf.len();
                let found = if d == '/' {
                    (1..=total).map(|k| (base + k) % total).find(|l| re.is_match(&buf.line(*l).chars().collect::<Vec<_>>()))
                } else {
                    (1..=total).map(|k| (base + total * 2 - k) % total).find(|l| re.is_match(&buf.line(*l).chars().collect::<Vec<_>>()))
                };
                line = Some(found.ok_or_else(|| format!("E486: Pattern not found: {pattern}"))? as i64);
            }
            _ => {}
        }
        // Offsets.
        loop {
            let r = rest.trim_start();
            let Some(sign) = r.chars().next().filter(|c| *c == '+' || *c == '-') else { break };
            let digits: String = r[1..].chars().take_while(char::is_ascii_digit).collect();
            let n: i64 = if digits.is_empty() { 1 } else { digits.parse().unwrap_or(1) };
            let l = line.get_or_insert(base as i64);
            *l += if sign == '+' { n } else { -n };
            rest = &r[1 + digits.len()..];
        }
        Ok(match line {
            None => (None, rest),
            Some(l) => {
                if l < -1 || l > last {
                    return Err("E16: Invalid range".into());
                }
                (Some(l.max(0) as usize), rest)
            }
        })
    }

    /// `:s/pat/rep/flags`.
    fn substitute(&mut self, buf: &mut Buffer, s: &mut Session, range: Option<(usize, usize)>, args: &str, repeat: bool, keep_flags: bool) {
        let (pattern, replacement, mut flags, count_text) = if repeat {
            let Some((p, r, f)) = s.last_sub.clone() else {
                self.error("E35: No previous regular expression");
                return;
            };
            let extra = args.trim_start_matches('&').trim();
            let (more_flags, count) = split_flags(extra);
            (p, r, if keep_flags || args.starts_with('&') { format!("{f}{more_flags}") } else { more_flags }, count)
        } else {
            let delim = args.chars().next().unwrap();
            let (pat, after) = split_delim(&args[delim.len_utf8()..], delim);
            let (rep, after) = match after {
                Some(a) => split_delim(a, delim),
                None => (String::new(), None),
            };
            let (flags, count) = split_flags(after.unwrap_or("").trim());
            let pattern = if pat.is_empty() {
                match &s.last_search {
                    Some((p, _)) => p.clone(),
                    None => {
                        self.error("E35: No previous regular expression");
                        return;
                    }
                }
            } else {
                pat
            };
            (pattern, rep, flags, count)
        };
        if flags.contains('c') {
            self.error("E: the c flag (confirm each) is not available; run without it, and undo if it went wrong");
            return;
        }
        flags.retain(|c| c != '&');
        s.last_sub = Some((pattern.clone(), replacement.clone(), flags.clone()));
        s.set_search(&pattern, true);

        let ignore = if flags.contains('I') {
            false
        } else if flags.contains('i') {
            true
        } else {
            s.options.ignorecase && !(s.options.smartcase && crate::Regex::pattern_has_upper(&pattern))
        };
        let re = match crate::Regex::new(&pattern, ignore) {
            Ok(r) => r,
            Err(e) => {
                self.error(&format!("E: {e}"));
                return;
            }
        };
        let cur = buf.cursor.line;
        let (mut first, mut last) = range.unwrap_or((cur, cur));
        if let Some(n) = count_text {
            first = last;
            last = (last + n - 1).min(buf.len() - 1);
        }
        let global = flags.contains('g');
        let count_only = flags.contains('n');

        let mut out: Vec<String> = Vec::with_capacity(last + 1 - first);
        let mut subs = 0usize;
        let mut lines_hit = 0usize;
        let mut last_hit = first;
        for l in first..=last {
            let chars: Vec<char> = buf.line(l).chars().collect();
            let matches = if global { re.find_all(&chars) } else { re.find_from(&chars, 0).into_iter().collect() };
            if matches.is_empty() {
                out.push(buf.line(l).to_string());
                continue;
            }
            lines_hit += 1;
            subs += matches.len();
            last_hit = l;
            if count_only {
                out.push(buf.line(l).to_string());
                continue;
            }
            let mut new = String::new();
            let mut at = 0;
            for m in &matches {
                new.extend(&chars[at..m.start]);
                new.push_str(&expand_replacement(&replacement, &chars, m));
                at = m.end;
            }
            new.extend(&chars[at..]);
            out.push(new);
        }
        if subs == 0 {
            if !flags.contains('e') {
                self.error(&format!("E486: Pattern not found: {pattern}"));
            }
            return;
        }
        if count_only {
            self.say(&format!("{subs} match{} on {lines_hit} line{}", if subs == 1 { "" } else { "es" }, if lines_hit == 1 { "" } else { "s" }));
            return;
        }
        let text = out.join("\n");
        let added: usize = out.iter().map(|l| l.matches('\n').count()).sum();
        buf.edit(Pos::new(first, 0), Pos::new(last, buf.line_len(last)), &text);
        // The caret lands on the last line that changed, counting the lines
        // the replacements themselves added, that one included.
        let extra_above: usize = out[..=last_hit - first].iter().map(|l| l.matches('\n').count()).sum();
        let line = (last_hit + extra_above).min(buf.len() - 1);
        let _ = added;
        buf.set_cursor(Pos::new(line, first_non_blank(buf, line)));
        if lines_hit > 2 {
            self.say(&format!("{subs} substitution{} on {lines_hit} lines", if subs == 1 { "" } else { "s" }));
        }
    }

    /// `:g/pat/cmd` and `:v/pat/cmd`.
    #[allow(clippy::too_many_arguments)]
    fn global(&mut self, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx, range: Option<(usize, usize)>, args: &str, invert: bool) -> Vec<Effect> {
        let Some(delim) = args.chars().next() else {
            self.error("E: :g needs a pattern");
            return Vec::new();
        };
        let (pat, after) = split_delim(&args[delim.len_utf8()..], delim);
        let cmd = after.unwrap_or("").trim();
        let cmd = if cmd.is_empty() { "p" } else { cmd };
        let pattern = if pat.is_empty() {
            match &s.last_search {
                Some((p, _)) => p.clone(),
                None => {
                    self.error("E35: No previous regular expression");
                    return Vec::new();
                }
            }
        } else {
            pat
        };
        s.set_search(&pattern, true);
        let re = match s.compile(&pattern) {
            Ok(r) => r,
            Err(e) => {
                self.error(&format!("E: {e}"));
                return Vec::new();
            }
        };
        let (first, last) = range.unwrap_or((0, buf.len() - 1));
        let hits: Vec<usize> = (first..=last.min(buf.len() - 1))
            .filter(|l| re.is_match(&buf.line(*l).chars().collect::<Vec<_>>()) != invert)
            .collect();
        if hits.is_empty() {
            self.error(&format!("E486: Pattern not found: {pattern}"));
            return Vec::new();
        }
        if cmd == "p" || cmd == "print" || cmd == "#" || cmd == "nu" {
            let shown: Vec<String> = hits.iter().take(20).map(|l| format!("{:>4} {}", l + 1, buf.line(*l))).collect();
            self.say(&shown.join("  |  "));
            let l = *hits.last().unwrap();
            buf.set_cursor(Pos::new(l, first_non_blank(buf, l)));
            return Vec::new();
        }
        // Every hit in turn, with line numbers corrected for what earlier
        // commands added or removed. Bottom-up would be simpler but wrong:
        // `:g/x/normal` commands are written expecting top-down order.
        let mut fx = Vec::new();
        let mut delta: i64 = 0;
        let mut messages = Vec::new();
        buf.begin_undo_group();
        for h in hits {
            let line = h as i64 + delta;
            if line < 0 || line as usize >= buf.len() {
                continue;
            }
            let before = buf.len() as i64;
            fx.extend(self.ex(&format!("{}{cmd}", line + 1), buf, s, host, ctx));
            if let Some((m, true)) = self.message.take() {
                messages.push(m);
            }
            delta += buf.len() as i64 - before;
        }
        buf.end_undo_group();
        if let Some(m) = messages.last() {
            self.error(m);
        }
        fx
    }

    /// `:normal keys`, once or on every line of a range.
    fn normal_cmd(&mut self, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, ctx: Ctx, range: Option<(usize, usize)>, keys: &str) -> Vec<Effect> {
        let keys = crate::text_to_keys(keys);
        let mut fx = Vec::new();
        let run = |vim: &mut Vim, buf: &mut Buffer, s: &mut Session, host: &mut dyn Host, fx: &mut Vec<Effect>| {
            vim.macro_depth += 1;
            fx.extend(vim.feed(&keys, buf, s, host, ctx));
            // An unfinished command is abandoned, as if Esc had been typed.
            if matches!(vim.mode, Mode::Insert | Mode::Replace) {
                fx.extend(vim.feed(&[Key::Esc], buf, s, host, ctx));
            }
            if vim.mode == Mode::Command {
                vim.cmdline = None;
                vim.mode = Mode::Normal;
            }
            vim.keys.clear();
            vim.macro_depth -= 1;
        };
        match range {
            None => run(self, buf, s, host, &mut fx),
            Some((first, last)) => {
                let mut delta: i64 = 0;
                for l in first..=last {
                    let line = l as i64 + delta;
                    if line < 0 || line as usize >= buf.len() {
                        break;
                    }
                    let before = buf.len() as i64;
                    buf.set_cursor(Pos::new(line as usize, 0));
                    run(self, buf, s, host, &mut fx);
                    delta += buf.len() as i64 - before;
                }
            }
        }
        fx
    }

    /// `:set`.
    fn set(&mut self, s: &mut Session, args: &str) {
        if args.is_empty() {
            let o = s.options;
            self.say(&format!(
                "{}ignorecase {}smartcase {}hlsearch clipboard={}",
                if o.ignorecase { "" } else { "no" },
                if o.smartcase { "" } else { "no" },
                if o.hlsearch { "" } else { "no" },
                if o.clipboard { "unnamedplus" } else { "" }
            ));
            return;
        }
        for item in args.split_whitespace() {
            let (name, value) = item.split_once('=').map(|(n, v)| (n, Some(v))).unwrap_or((item, None));
            let query = name.ends_with('?');
            let name = name.trim_end_matches('?');
            let (on, name) = match name.strip_prefix("no") {
                Some(n) if !n.is_empty() && n != "number" => (false, n),
                _ => (true, name),
            };
            let (name, on) = match name.strip_suffix('!') {
                Some(n) => (n, !self.option_value(s, n).unwrap_or(false)),
                None => (name, on),
            };
            match name {
                "hls" | "hlsearch" => {
                    if query {
                        self.say(if s.options.hlsearch { "  hlsearch" } else { "nohlsearch" });
                        continue;
                    }
                    s.options.hlsearch = on;
                    s.hl = if on { s.last_search.as_ref().and_then(|(p, _)| s.compile(p).ok()) } else { None };
                }
                "ic" | "ignorecase" => {
                    if query {
                        self.say(if s.options.ignorecase { "  ignorecase" } else { "noignorecase" });
                        continue;
                    }
                    s.options.ignorecase = on;
                }
                "scs" | "smartcase" => {
                    if query {
                        self.say(if s.options.smartcase { "  smartcase" } else { "nosmartcase" });
                        continue;
                    }
                    s.options.smartcase = on;
                }
                "cb" | "clipboard" => {
                    if query || value.is_none() {
                        self.say(&format!("  clipboard={}", if s.options.clipboard { "unnamedplus" } else { "" }));
                        continue;
                    }
                    s.options.clipboard = value.is_some_and(|v| v.contains("unnamed"));
                }
                "nu" | "number" | "rnu" | "relativenumber" | "wrap" | "ts" | "tabstop" | "sw" | "shiftwidth" | "et" | "expandtab" | "ai"
                | "autoindent" | "list" | "cursorline" | "cul" | "so" | "scrolloff" | "is" | "incsearch" | "paste" | "spell" | "ft"
                | "filetype" | "syntax" | "syn" | "encoding" | "enc" | "ff" | "fileformat" | "mouse" | "ru" | "ruler" | "sm"
                | "showmatch" | "wm" | "tw" | "textwidth" | "ls" | "laststatus" | "sc" | "showcmd" | "smd" | "showmode" | "nrformats" | "nf"
                | "bs" | "backspace" | "ww" | "whichwrap" | "vb" | "visualbell" | "eb" | "errorbells" => {
                    self.say(&format!("'{name}' is not an option here; the editor's own settings are in config.toml"));
                }
                _ => self.error(&format!("E518: Unknown option: {name}")),
            }
        }
    }

    fn option_value(&self, s: &Session, name: &str) -> Option<bool> {
        Some(match name {
            "hls" | "hlsearch" => s.options.hlsearch,
            "ic" | "ignorecase" => s.options.ignorecase,
            "scs" | "smartcase" => s.options.smartcase,
            _ => return None,
        })
    }
}

/// Flags and the optional count after the last delimiter of `:s`.
fn split_flags(text: &str) -> (String, Option<usize>) {
    let flags: String = text.chars().take_while(|c| "&cegiInp#lr".contains(*c)).collect();
    let rest = text[flags.len()..].trim();
    let count = rest.parse::<usize>().ok().filter(|n| *n > 0);
    (flags, count)
}
