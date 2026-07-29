//! Syntax highlighting, per line, in character columns.
//!
//! The renderer draws a monospace grid and positions text by column, so that
//! is what this hands back: for each line, a list of spans with a start column,
//! an end column and a role. Byte offsets never leave this crate — the moment
//! they do, some non-ASCII line renders with its colors shifted sideways.
//!
//! Roles are coarse on purpose. tree-sitter's capture names are open-ended
//! (`function.method.builtin`), and a theme with forty entries is a theme
//! nobody edits. Unknown names fall back along their dots, so a grammar that
//! adds `keyword.control.return` still lands on `keyword`.

use std::collections::HashMap;
use std::path::Path;

use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

/// A highlight role. One per color a theme has to define.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Keyword,
    Function,
    Type,
    String,
    Number,
    Comment,
    Constant,
    Operator,
    Punctuation,
    Variable,
    Property,
    Attribute,
}

/// A coloured run within one line, in character columns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

/// Capture names handed to tree-sitter, in the order their indices are
/// assigned. The order is the mapping, so this array and [`KINDS`] have to stay
/// aligned — a test checks that they do.
const NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "function",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.escape",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
    // Markdown and friends. Two naming families exist in the wild — the older
    // `text.*` and the newer `markup.*` — and which one a grammar uses is not
    // something to guess at, so both are listed.
    "text.title",
    "text.literal",
    "text.emphasis",
    "text.strong",
    "text.uri",
    "text.reference",
    "text.quote",
    "markup.heading",
    "markup.raw",
    "markup.italic",
    "markup.strong",
    "markup.link",
    "markup.link.url",
    "markup.quote",
    "markup.list",
    // Appended rather than sorted in: matching is by longest dotted prefix,
    // not by position, and appending keeps the diff honest about what the
    // newer grammars actually needed.
    "boolean",
    "character",
    "string.special.key",
];

const KINDS: &[Kind] = &[
    Kind::Attribute,
    Kind::Comment,
    Kind::Constant,
    Kind::Constant,
    Kind::Type,
    Kind::Function,
    Kind::Function,
    Kind::Function,
    Kind::Keyword,
    Kind::Keyword,
    Kind::Number,
    Kind::Operator,
    Kind::Property,
    Kind::Punctuation,
    Kind::Punctuation,
    Kind::Punctuation,
    Kind::Punctuation,
    Kind::String,
    Kind::String,
    Kind::String,
    Kind::Type,
    Kind::Type,
    Kind::Type,
    Kind::Variable,
    Kind::Variable,
    Kind::Variable,
    // text.*
    Kind::Function,
    Kind::String,
    Kind::Constant,
    Kind::Constant,
    Kind::Property,
    Kind::Property,
    Kind::Comment,
    // markup.*
    Kind::Function,
    Kind::String,
    Kind::Constant,
    Kind::Constant,
    Kind::Property,
    Kind::Property,
    Kind::Comment,
    Kind::Punctuation,
    // boolean, character, string.special.key
    Kind::Constant,
    Kind::String,
    Kind::Property,
];

/// Languages we have a grammar for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Lang {
    Rust,
    Json,
    Toml,
    Markdown,
    Javascript,
    Typescript,
    /// Its own grammar, not a flag on TypeScript: TSX changes the parse —
    /// `<T>` is a type argument in one and an element in the other.
    Tsx,
    Python,
    C,
    Cpp,
    Html,
    Css,
    Yaml,
    Bash,
    Go,
}

impl Lang {
    /// Guessed from the file name. Extension only: content sniffing guesses
    /// wrong on short files, and a wrongly coloured file is worse than a plain
    /// one.
    pub fn of(path: &Path) -> Option<Lang> {
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        Some(match ext {
            "rs" => Lang::Rust,
            "json" => Lang::Json,
            "toml" => Lang::Toml,
            "md" | "markdown" => Lang::Markdown,
            // .jsx is the JavaScript grammar too: it parses JSX natively.
            "js" | "mjs" | "cjs" | "jsx" => Lang::Javascript,
            "ts" | "mts" | "cts" => Lang::Typescript,
            "tsx" => Lang::Tsx,
            "py" | "pyi" => Lang::Python,
            // .h is C here. It is often C++, but the C grammar degrades
            // gracefully on C++ headers; the reverse guess colours plain C
            // with a parser expecting classes.
            "c" | "h" => Lang::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Lang::Cpp,
            "html" | "htm" => Lang::Html,
            "css" => Lang::Css,
            "yaml" | "yml" => Lang::Yaml,
            "sh" | "bash" | "zsh" => Lang::Bash,
            "go" => Lang::Go,
            _ => return None,
        })
    }

    /// The canonical name, which is also the key an injection resolves: a
    /// markdown fence saying ```rust and an HTML `<script>` both reach their
    /// grammar through this string.
    fn name(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Json => "json",
            Lang::Toml => "toml",
            Lang::Markdown => "markdown",
            Lang::Javascript => "javascript",
            Lang::Typescript => "typescript",
            Lang::Tsx => "tsx",
            Lang::Python => "python",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Html => "html",
            Lang::Css => "css",
            Lang::Yaml => "yaml",
            Lang::Bash => "bash",
            Lang::Go => "go",
        }
    }
}

/// What a free-text injection name means. Fence info strings are whatever the
/// author typed — `js`, `shell`, `c++` — and refusing the common spellings
/// would colour fences only for people who write like our `Lang::name`.
fn canonical(name: &str) -> &str {
    match name {
        "js" | "mjs" | "cjs" | "jsx" | "node" => "javascript",
        "ts" => "typescript",
        "py" | "python3" => "python",
        "c++" => "cpp",
        "yml" => "yaml",
        "sh" | "shell" | "zsh" | "console" => "bash",
        "golang" => "go",
        "rs" => "rust",
        _ => name,
    }
}

pub struct Syntax {
    /// Every loaded grammar, keyed by canonical name. One map rather than a
    /// per-language one plus an "injected" one: an injection is just a name,
    /// and a fence saying ```python deserves the same colours a .py file
    /// gets. `markdown_inline` sits here too, reachable only by injection.
    configs: HashMap<&'static str, HighlightConfiguration>,
}

impl Default for Syntax {
    fn default() -> Self {
        Self::new()
    }
}

impl Syntax {
    pub fn new() -> Self {
        let mut configs = HashMap::new();
        // A grammar that fails to load is skipped rather than fatal: losing
        // colours for one language must not stop the editor from opening.
        let mut add = |name: &'static str, cfg: Result<HighlightConfiguration, _>| {
            if let Ok(mut cfg) = cfg {
                cfg.configure(NAMES);
                configs.insert(name, cfg);
            }
        };

        add(
            "rust",
            HighlightConfiguration::new(
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                tree_sitter_rust::INJECTIONS_QUERY,
                "",
            ),
        );
        add(
            "json",
            HighlightConfiguration::new(
                tree_sitter_json::LANGUAGE.into(),
                "json",
                tree_sitter_json::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        );
        add(
            "toml",
            HighlightConfiguration::new(
                tree_sitter_toml_ng::LANGUAGE.into(),
                "toml",
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        );
        add(
            "markdown",
            HighlightConfiguration::new(
                tree_sitter_md::LANGUAGE.into(),
                "markdown",
                tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
                tree_sitter_md::INJECTION_QUERY_BLOCK,
                "",
            ),
        );
        // The JSX query rides along for plain JavaScript too: the grammar
        // always knows the JSX nodes, and a .js file full of React is more
        // common than one that would mind.
        let js_highlights = format!(
            "{}{}",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
        );
        add(
            "javascript",
            HighlightConfiguration::new(
                tree_sitter_javascript::LANGUAGE.into(),
                "javascript",
                &js_highlights,
                tree_sitter_javascript::INJECTIONS_QUERY,
                tree_sitter_javascript::LOCALS_QUERY,
            ),
        );
        // TypeScript's query only adds what TypeScript adds; the JavaScript
        // query underneath it is how the whole language gets coloured. The
        // upstream queries are written to be concatenated this way.
        let ts_highlights = format!(
            "{}{}",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY
        );
        let ts_locals = format!(
            "{}{}",
            tree_sitter_javascript::LOCALS_QUERY,
            tree_sitter_typescript::LOCALS_QUERY
        );
        add(
            "typescript",
            HighlightConfiguration::new(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                "typescript",
                &ts_highlights,
                "",
                &ts_locals,
            ),
        );
        let tsx_highlights = format!(
            "{}{}{}",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY
        );
        add(
            "tsx",
            HighlightConfiguration::new(
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                "tsx",
                &tsx_highlights,
                "",
                &ts_locals,
            ),
        );
        add(
            "python",
            HighlightConfiguration::new(
                tree_sitter_python::LANGUAGE.into(),
                "python",
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        );
        add(
            "c",
            HighlightConfiguration::new(
                tree_sitter_c::LANGUAGE.into(),
                "c",
                tree_sitter_c::HIGHLIGHT_QUERY,
                "",
                "",
            ),
        );
        // Same concatenation trick as TypeScript: C++'s query inherits C's.
        let cpp_highlights =
            format!("{}{}", tree_sitter_c::HIGHLIGHT_QUERY, tree_sitter_cpp::HIGHLIGHT_QUERY);
        add(
            "cpp",
            HighlightConfiguration::new(
                tree_sitter_cpp::LANGUAGE.into(),
                "cpp",
                &cpp_highlights,
                "",
                "",
            ),
        );
        // HTML's injection query is what colours <script> and <style>: it
        // asks for "javascript" and "css" by name, and both live in this map.
        add(
            "html",
            HighlightConfiguration::new(
                tree_sitter_html::LANGUAGE.into(),
                "html",
                tree_sitter_html::HIGHLIGHTS_QUERY,
                tree_sitter_html::INJECTIONS_QUERY,
                "",
            ),
        );
        add(
            "css",
            HighlightConfiguration::new(
                tree_sitter_css::LANGUAGE.into(),
                "css",
                tree_sitter_css::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        );
        add(
            "yaml",
            HighlightConfiguration::new(
                tree_sitter_yaml::LANGUAGE.into(),
                "yaml",
                tree_sitter_yaml::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        );
        add(
            "bash",
            HighlightConfiguration::new(
                tree_sitter_bash::LANGUAGE.into(),
                "bash",
                tree_sitter_bash::HIGHLIGHT_QUERY,
                "",
                "",
            ),
        );
        add(
            "go",
            HighlightConfiguration::new(
                tree_sitter_go::LANGUAGE.into(),
                "go",
                tree_sitter_go::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        );

        // Markdown needs a second pass. Its block grammar only sees headings,
        // lists and fences; emphasis, links and code spans live in a separate
        // inline grammar, and the block grammar's injection query does not
        // reach it — it only injects fenced code blocks. Without running the
        // inline parser as well, markdown looks supported and colours almost
        // nothing.
        add(
            "markdown_inline",
            HighlightConfiguration::new(
                tree_sitter_md::INLINE_LANGUAGE.into(),
                "markdown_inline",
                tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
                "",
                "",
            ),
        );

        Self { configs }
    }

    pub fn supports(&self, lang: Lang) -> bool {
        self.configs.contains_key(lang.name())
    }

    /// Highlights a whole file into per-line spans.
    ///
    /// Returns an empty vector when the language is unknown or the source
    /// fails to parse, which the caller draws as plain text. A partially
    /// coloured file after a syntax error is normal and fine; tree-sitter
    /// recovers and keeps going.
    pub fn highlight(&self, lang: Lang, source: &str) -> Vec<Vec<Span>> {
        let Some(config) = self.configs.get(lang.name()) else {
            return Vec::new();
        };
        let mut highlighter = Highlighter::new();
        let configs = &self.configs;
        let Ok(events) = highlighter.highlight(config, source.as_bytes(), None, |name| {
            configs.get(canonical(name))
        }) else {
            return Vec::new();
        };

        let index = LineIndex::new(source);
        let mut lines: Vec<Vec<Span>> = vec![Vec::new(); index.count()];
        // Grammars that need a second parser over the same text. The extra
        // spans are merged only where the first pass left a gap, so a fenced
        // code block stays code rather than being reinterpreted as prose.
        let extra = match lang {
            Lang::Markdown => self.configs.get("markdown_inline"),
            _ => None,
        };
        // tree-sitter nests highlights, so this is a stack: the innermost one
        // is what a byte should be painted with.
        let mut stack: Vec<Highlight> = Vec::new();

        for event in events.flatten() {
            match event {
                HighlightEvent::HighlightStart(h) => stack.push(h),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    let Some(kind) = stack.last().and_then(|h| KINDS.get(h.0).copied()) else {
                        continue;
                    };
                    index.split_lines(start, end, source, |line, from, to| {
                        if from < to {
                            lines[line].push(Span { start: from, end: to, kind });
                        }
                    });
                }
            }
        }

        // Declared outside the `if` so it outlives the event iterator that
        // borrows from it.
        let mut second = Highlighter::new();
        if let Some(extra) = extra {
            if let Ok(events) = second.highlight(extra, source.as_bytes(), None, |_| None) {
                let mut stack: Vec<Highlight> = Vec::new();
                for event in events.flatten() {
                    match event {
                        HighlightEvent::HighlightStart(h) => stack.push(h),
                        HighlightEvent::HighlightEnd => {
                            stack.pop();
                        }
                        HighlightEvent::Source { start, end } => {
                            let Some(kind) = stack.last().and_then(|h| KINDS.get(h.0).copied())
                            else {
                                continue;
                            };
                            index.split_lines(start, end, source, |line, from, to| {
                                if from < to && !covered(&lines[line], from, to) {
                                    lines[line].push(Span { start: from, end: to, kind });
                                }
                            });
                        }
                    }
                }
            }
        }

        // Drawing walks spans left to right and would skip anything out of
        // order, so the merged result has to be sorted.
        for line in &mut lines {
            line.sort_by_key(|s| s.start);
        }
        lines
    }
}

/// Whether an existing span already covers any of this range.
fn covered(spans: &[Span], from: usize, to: usize) -> bool {
    spans.iter().any(|s| s.start < to && from < s.end)
}

/// Byte offsets to (line, character column).
struct LineIndex {
    /// Byte offset where each line starts.
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut starts = vec![0usize];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { starts }
    }

    fn count(&self) -> usize {
        self.starts.len()
    }

    fn line_of(&self, byte: usize) -> usize {
        match self.starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    /// Calls `f(line, start_col, end_col)` for each line a byte range touches.
    ///
    /// Ranges cross lines constantly — a block comment, a multi-line string —
    /// and the renderer draws one line at a time, so they have to be cut here.
    fn split_lines(&self, start: usize, end: usize, source: &str, mut f: impl FnMut(usize, usize, usize)) {
        let first = self.line_of(start);
        let last = self.line_of(end.saturating_sub(1).max(start));
        for line in first..=last.min(self.starts.len() - 1) {
            let line_start = self.starts[line];
            let line_end = self
                .starts
                .get(line + 1)
                .map(|n| n - 1) // drop the newline itself
                .unwrap_or(source.len());
            let from = start.max(line_start);
            let to = end.min(line_end);
            if from > to {
                continue;
            }
            f(
                line,
                char_col(source, line_start, from),
                char_col(source, line_start, to),
            );
        }
    }
}

/// Characters between two byte offsets on the same line.
fn char_col(source: &str, line_start: usize, byte: usize) -> usize {
    source
        .get(line_start..byte)
        .map(|s| s.chars().count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(lang: Lang, src: &str) -> Vec<Vec<Span>> {
        Syntax::new().highlight(lang, src)
    }

    fn kinds_on(lines: &[Vec<Span>], line: usize) -> Vec<Kind> {
        lines[line].iter().map(|s| s.kind).collect()
    }

    #[test]
    fn the_name_and_kind_tables_line_up() {
        // The index into NAMES is the index into KINDS. Adding a name without
        // a kind would silently colour it as whatever came next.
        assert_eq!(NAMES.len(), KINDS.len());
    }

    #[test]
    fn every_grammar_loads() {
        // A grammar that fails to load is skipped silently by design, so
        // without this test a broken one would just mean "no colours".
        let s = Syntax::new();
        for lang in [
            Lang::Rust,
            Lang::Json,
            Lang::Toml,
            Lang::Markdown,
            Lang::Javascript,
            Lang::Typescript,
            Lang::Tsx,
            Lang::Python,
            Lang::C,
            Lang::Cpp,
            Lang::Html,
            Lang::Css,
            Lang::Yaml,
            Lang::Bash,
            Lang::Go,
        ] {
            assert!(s.supports(lang), "{lang:?} grammar did not load");
        }
    }

    #[test]
    fn rust_keywords_and_strings_are_found() {
        let lines = spans(Lang::Rust, "fn main() {\n    let s = \"hi\";\n}\n");
        assert!(kinds_on(&lines, 0).contains(&Kind::Keyword), "fn");
        assert!(kinds_on(&lines, 1).contains(&Kind::String), "\"hi\"");
    }

    #[test]
    fn a_keyword_inside_a_string_is_not_a_keyword() {
        // The exact case regex highlighting gets wrong.
        let lines = spans(Lang::Rust, "let s = \"fn struct impl\";\n");
        let strings: Vec<_> = lines[0].iter().filter(|s| s.kind == Kind::String).collect();
        assert!(!strings.is_empty());
        // Everything inside the quotes must be string-coloured.
        let quote = 8;
        for span in &lines[0] {
            if span.start > quote && span.end < 24 {
                assert_eq!(span.kind, Kind::String, "{span:?}");
            }
        }
    }

    #[test]
    fn columns_are_characters_not_bytes() {
        // Two-byte characters before the comment would shift it right by two
        // columns if this used byte offsets.
        let src = "let s = \"ğüş\"; // yorum\n";
        let lines = spans(Lang::Rust, src);
        let comment = lines[0]
            .iter()
            .find(|s| s.kind == Kind::Comment)
            .expect("the comment should be found");
        let expected = src.chars().position(|c| c == '/').unwrap();
        assert_eq!(comment.start, expected, "comment starts at the wrong column");
    }

    #[test]
    fn a_block_comment_is_split_across_lines() {
        let lines = spans(Lang::Rust, "/* one\ntwo */\nfn x() {}\n");
        assert!(kinds_on(&lines, 0).contains(&Kind::Comment));
        assert!(kinds_on(&lines, 1).contains(&Kind::Comment));
        assert!(kinds_on(&lines, 2).contains(&Kind::Keyword), "code after it still parses");
    }

    #[test]
    fn spans_stay_inside_their_line() {
        let src = "fn a() {}\nfn b() {}\n";
        let lines = spans(Lang::Rust, src);
        let line_lens: Vec<usize> = src.lines().map(|l| l.chars().count()).collect();
        for (i, line) in lines.iter().enumerate() {
            for span in line {
                assert!(span.start <= span.end, "reversed span on line {i}");
                if let Some(len) = line_lens.get(i) {
                    assert!(span.end <= *len, "span past the end of line {i}: {span:?}");
                }
            }
        }
    }

    #[test]
    fn broken_syntax_still_highlights_what_it_can() {
        // tree-sitter recovers from errors; half a file of colour beats none.
        let lines = spans(Lang::Rust, "fn ok() {}\nfn broken( { { {\n");
        assert!(kinds_on(&lines, 0).contains(&Kind::Keyword));
    }

    #[test]
    fn language_is_detected_from_the_extension() {
        assert_eq!(Lang::of(Path::new("a/b/main.rs")), Some(Lang::Rust));
        assert_eq!(Lang::of(Path::new("Cargo.toml")), Some(Lang::Toml));
        assert_eq!(Lang::of(Path::new("README.MD")), Some(Lang::Markdown));
        assert_eq!(Lang::of(Path::new("app.jsx")), Some(Lang::Javascript));
        assert_eq!(Lang::of(Path::new("app.ts")), Some(Lang::Typescript));
        assert_eq!(Lang::of(Path::new("app.tsx")), Some(Lang::Tsx));
        assert_eq!(Lang::of(Path::new("tool.py")), Some(Lang::Python));
        assert_eq!(Lang::of(Path::new("lib.h")), Some(Lang::C));
        assert_eq!(Lang::of(Path::new("lib.hpp")), Some(Lang::Cpp));
        assert_eq!(Lang::of(Path::new("index.html")), Some(Lang::Html));
        assert_eq!(Lang::of(Path::new("style.css")), Some(Lang::Css));
        assert_eq!(Lang::of(Path::new("ci.yml")), Some(Lang::Yaml));
        assert_eq!(Lang::of(Path::new("run.sh")), Some(Lang::Bash));
        assert_eq!(Lang::of(Path::new("main.go")), Some(Lang::Go));
        assert_eq!(Lang::of(Path::new("notes.txt")), None);
        assert_eq!(Lang::of(Path::new("Makefile")), None);
    }

    #[test]
    fn every_language_colours_its_hello_world() {
        // One representative snippet per grammar. Not asserting specific
        // colours — queries differ on those — just that each grammar wired up
        // here actually produces spans, because a grammar that loads and
        // colours nothing looks exactly like a supported language.
        let cases: &[(Lang, &str)] = &[
            (Lang::Javascript, "function f(x) { return `hi ${x}`; } // c\n"),
            (Lang::Typescript, "const n: number = 1;\ninterface A { b: string }\n"),
            (Lang::Tsx, "const el = <div className={x}>hi</div>;\n"),
            (Lang::Python, "def f(x):\n    return f\"hi {x}\"  # c\n"),
            (Lang::C, "int main(void) { return 0; } /* c */\n"),
            (Lang::Cpp, "class A { public: int f() const; };\n"),
            (Lang::Html, "<html><body class=\"x\"><p>hi</p></body></html>\n"),
            (Lang::Css, ".a { color: #fff; margin: 0 auto; }\n"),
            (Lang::Yaml, "key: value\nlist:\n  - true\n"),
            (Lang::Bash, "for f in *.rs; do echo \"$f\"; done # c\n"),
            (Lang::Go, "func main() {\n\tfmt.Println(\"hi\")\n}\n"),
        ];
        let s = Syntax::new();
        for (lang, src) in cases {
            let total: usize = s.highlight(*lang, src).iter().map(Vec::len).sum();
            assert!(total > 0, "{lang:?} produced no spans for {src:?}");
        }
    }

    #[test]
    fn keywords_come_out_where_it_matters() {
        let s = Syntax::new();
        let js = s.highlight(Lang::Javascript, "function f() { return 1; }\n");
        assert!(js[0].iter().any(|x| x.kind == Kind::Keyword), "function/return");
        let py = s.highlight(Lang::Python, "def f():\n    return 'x'\n");
        assert!(py[0].iter().any(|x| x.kind == Kind::Keyword), "def");
        assert!(py[1].iter().any(|x| x.kind == Kind::String), "'x'");
    }

    #[test]
    fn a_markdown_fence_reaches_the_real_grammar() {
        // The fence's info string is an injection name, and the injection map
        // is the same map the languages live in — ```rust colours like a .rs
        // file, aliases included.
        let s = Syntax::new();
        for fence in ["rust", "rs", "python", "py", "js"] {
            let src = format!("# T\n\n```{fence}\n# not a heading\nfn f() {{ return 0 }}\n```\n");
            let lines = s.highlight(Lang::Markdown, &src);
            let inside: Vec<Kind> = lines[3..5].iter().flatten().map(|x| x.kind).collect();
            assert!(
                inside.contains(&Kind::Keyword) || inside.contains(&Kind::Comment),
                "```{fence} was not injected: {inside:?}"
            );
        }
    }

    #[test]
    fn html_style_and_script_blocks_get_their_own_colours() {
        let s = Syntax::new();
        let src = "<style>.a { color: red; }</style>\n<script>const x = 1;</script>\n";
        let lines = s.highlight(Lang::Html, src);
        let all: Vec<Kind> = lines.iter().flatten().map(|x| x.kind).collect();
        assert!(all.contains(&Kind::Keyword), "const inside <script>");
        assert!(all.contains(&Kind::Property), "color inside <style>");
    }

    #[test]
    fn markdown_actually_colours_something() {
        let lines = spans(Lang::Markdown, "# Heading

Some **bold** and `code`.
");
        let total: usize = lines.iter().map(|l| l.len()).sum();
        assert!(total > 0, "markdown produced no spans at all");
    }

    #[test]
    fn markdown_colours_inline_content_too() {
        // The whole reason for the second pass. The block grammar sees the
        // heading and stops; emphasis and code spans belong to a separate
        // inline parser that its injection query never reaches.
        let lines = spans(
            Lang::Markdown,
            "# Title\n\nSome **bold** text and `code` here.\n\n```rust\nfn x() {}\n```\n",
        );
        let all: Vec<Kind> = lines.iter().flatten().map(|s| s.kind).collect();
        assert!(all.contains(&Kind::Function), "the heading");
        assert!(all.contains(&Kind::Constant), "bold");
        assert!(all.contains(&Kind::String), "the code span");
    }

    #[test]
    fn merged_spans_stay_sorted_and_disjoint() {
        // Drawing walks spans left to right, so an unsorted or overlapping
        // merge would silently drop colour from the middle of a line.
        let lines = spans(
            Lang::Markdown,
            "# A `b` **c**\n\nplain `x` **y** [link](http://a.b)\n",
        );
        for (i, line) in lines.iter().enumerate() {
            let mut end = 0;
            for span in line {
                assert!(span.start >= end, "line {i} out of order or overlapping: {span:?}");
                end = span.end;
            }
        }
    }

    #[test]
    fn an_empty_file_produces_one_empty_line() {
        assert_eq!(spans(Lang::Rust, "").len(), 1);
    }

    #[test]
    fn toml_and_json_produce_something() {
        assert!(spans(Lang::Toml, "[a]\nb = 1\n").iter().any(|l| !l.is_empty()));
        assert!(spans(Lang::Json, "{\"a\": 1}\n").iter().any(|l| !l.is_empty()));
    }
}
