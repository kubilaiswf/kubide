//! A language server, driven over a pipe.
//!
//! The editor colours text with tree-sitter and that is all it knows about
//! a language. Everything that needs to *understand* the code — what is
//! wrong with it, where a name is defined, what type a thing has, how it
//! should be formatted — belongs to the language's own server, which
//! already exists, is maintained by the people who know, and speaks one
//! protocol. This crate speaks that protocol and nothing more.
//!
//! The same two halves as `kb-agent`: [`Framer`] and [`decode`], pure
//! functions from bytes to events and tested as such; and [`Client`], the
//! process around them. The parser knows nothing about threads and the
//! process knows nothing about JSON-RPC's meaning.
//!
//! Documents are synced whole. Incremental sync saves bytes on a pipe to a
//! local process, at the price of keeping two copies of the text in step
//! edit by edit — and the failure when they drift is a server confidently
//! reporting errors on code that is not there. At the file sizes this editor
//! opens, the whole text per change is the cheaper bug to not have.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

pub use serde_json::{json, Value};

/// A place in a document, in the editor's own terms: a line and a count of
/// characters into it. The wire format counts UTF-16 units unless the server
/// agrees otherwise; the conversion happens in here and nowhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub start: Pos,
    pub end: Pos,
    pub severity: Severity,
    pub message: String,
    /// `rustc`, `clippy`, `pyright` — who is complaining.
    pub source: Option<String>,
}

/// One replacement in a document, as formatting returns them.
#[derive(Clone, Debug, PartialEq)]
pub struct TextEdit {
    pub start: Pos,
    pub end: Pos,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub label: String,
    /// What is typed in when it is taken. The label unless the server says
    /// otherwise; snippet placeholders are stripped to their default text.
    pub insert: String,
    /// The signature or type, for the right-hand column.
    pub detail: Option<String>,
    pub kind: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The handshake is done and requests will be answered.
    Ready,
    /// The whole current set for one file — an empty list clears it.
    Diagnostics { path: PathBuf, items: Vec<Diagnostic> },
    Definition { id: u64, at: Option<(PathBuf, Pos)> },
    Hover { id: u64, text: Option<String> },
    Formatting { id: u64, edits: Vec<TextEdit> },
    Completions { id: u64, items: Vec<Completion> },
    /// A request the server refused, with its reason.
    Failed { id: u64, message: String },
    /// `window/showMessage` and friends: the server wants a word.
    Message(String),
    /// The process is gone; the string is the last thing it said on stderr.
    Exited(String),
}

/// How the wire counts columns. Asked for as UTF-32 — one unit per
/// character, which is what the buffer counts — and honoured as whatever
/// the server answers, because most only speak UTF-16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
    Utf32,
}

impl Encoding {
    fn from_name(name: &str) -> Self {
        match name {
            "utf-8" => Encoding::Utf8,
            "utf-32" => Encoding::Utf32,
            _ => Encoding::Utf16,
        }
    }

    /// Wire column to character column on `line`.
    pub fn to_chars(self, line: &str, units: usize) -> usize {
        let mut seen = 0;
        for (i, c) in line.chars().enumerate() {
            if seen >= units {
                return i;
            }
            seen += match self {
                Encoding::Utf8 => c.len_utf8(),
                Encoding::Utf16 => c.len_utf16(),
                Encoding::Utf32 => 1,
            };
        }
        line.chars().count()
    }

    /// Character column to wire column on `line`.
    pub fn to_units(self, line: &str, chars: usize) -> usize {
        line.chars()
            .take(chars)
            .map(|c| match self {
                Encoding::Utf8 => c.len_utf8(),
                Encoding::Utf16 => c.len_utf16(),
                Encoding::Utf32 => 1,
            })
            .sum()
    }
}

// ---------------------------------------------------------------------------
// Framing

/// Splits a byte stream into JSON-RPC messages.
///
/// Each one is `Content-Length: N\r\n\r\n` and then N bytes. The length is
/// in bytes and a read can end anywhere — mid-header, mid-character — so
/// this buffers bytes and only ever decodes a complete body.
#[derive(Default)]
pub struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Value> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let Some(head_end) = find(&self.buf, b"\r\n\r\n") else { break };
            let header = String::from_utf8_lossy(&self.buf[..head_end]).into_owned();
            let length = header.lines().find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok())?
            });
            let Some(length) = length else {
                // A header with no length cannot be skipped by size; drop it
                // and resynchronise on the next one.
                self.buf.drain(..head_end + 4);
                continue;
            };
            let body_start = head_end + 4;
            if self.buf.len() < body_start + length {
                break;
            }
            if let Ok(v) = serde_json::from_slice(&self.buf[body_start..body_start + length]) {
                out.push(v);
            }
            self.buf.drain(..body_start + length);
        }
        out
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// One message, framed for the wire.
pub fn frame(message: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).unwrap_or_default();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

// ---------------------------------------------------------------------------
// Paths

/// `file://` URI for a path. Percent-encodes what a URI cannot carry; a
/// Windows drive path gets the extra slash (`file:///C:/…`).
pub fn uri_of(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        out.push('/');
    }
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn path_of(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                bytes.push(b);
                i += 3;
                continue;
            }
        }
        bytes.push(raw[i]);
        i += 1;
    }
    let text = String::from_utf8(bytes).ok()?;
    // `/C:/dir` is a Windows path wearing a URI's leading slash.
    let windows = text.len() > 2 && text.as_bytes()[0] == b'/' && text.as_bytes()[2] == b':';
    Some(PathBuf::from(if windows { text[1..].to_string() } else { text }))
}

// ---------------------------------------------------------------------------
// Decoding

/// What a request was, kept until its answer arrives — the answer carries
/// only the id, so its meaning has to be remembered on this side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Initialize,
    Definition,
    Hover,
    Formatting,
    Completion,
    Shutdown,
}

/// Where column conversion gets a line's text from: the documents as last
/// sent, and the disk for anything else a location points into.
pub trait Lines {
    fn line(&self, path: &Path, line: usize) -> Option<String>;
}

/// Turns one incoming message into what it means for the editor.
///
/// `pending` says what each outstanding id was asking. Returns the event, if
/// the message is one the editor acts on, and a reply when the message was a
/// request from the server that must not be left hanging.
pub fn decode(
    message: &Value,
    pending: &mut HashMap<u64, Request>,
    encoding: Encoding,
    lines: &dyn Lines,
) -> (Option<Event>, Option<Value>) {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id");

    // A request from the server: it blocks on an answer. None of these are
    // things the editor does, so each gets the answer that means "no".
    if let (Some(method), Some(id)) = (method, id) {
        let result = match method {
            "workspace/configuration" => {
                let n = message["params"]["items"].as_array().map_or(0, Vec::len);
                json!(vec![Value::Null; n])
            }
            _ => Value::Null,
        };
        return (None, Some(json!({ "jsonrpc": "2.0", "id": id, "result": result })));
    }

    if let Some(method) = method {
        let params = &message["params"];
        let event = match method {
            "textDocument/publishDiagnostics" => {
                let path = params["uri"].as_str().and_then(path_of);
                path.map(|path| {
                    let items = params["diagnostics"]
                        .as_array()
                        .map(|list| {
                            list.iter().filter_map(|d| diagnostic(d, &path, encoding, lines)).collect()
                        })
                        .unwrap_or_default();
                    Event::Diagnostics { path, items }
                })
            }
            "window/showMessage" => params["message"].as_str().map(|m| Event::Message(m.to_string())),
            _ => None,
        };
        return (event, None);
    }

    // A response to one of ours.
    let Some(id) = id.and_then(Value::as_u64) else { return (None, None) };
    let Some(request) = pending.remove(&id) else { return (None, None) };
    if let Some(error) = message.get("error") {
        let text = error["message"].as_str().unwrap_or("request failed").to_string();
        return (Some(Event::Failed { id, message: text }), None);
    }
    let result = &message["result"];
    let event = match request {
        Request::Initialize | Request::Shutdown => None,
        Request::Definition => Some(Event::Definition { id, at: location(result, encoding, lines) }),
        Request::Hover => Some(Event::Hover { id, text: hover_text(&result["contents"]) }),
        Request::Formatting => Some(Event::Formatting {
            id,
            // Edits are against the document as sent, whose lines the
            // caller owns; the path is only needed for the conversion.
            edits: Vec::new(),
        }),
        Request::Completion => Some(Event::Completions { id, items: completions(result) }),
    };
    (event, None)
}

fn position(v: &Value, path: &Path, encoding: Encoding, lines: &dyn Lines) -> Option<Pos> {
    let line = v["line"].as_u64()? as usize;
    let units = v["character"].as_u64()? as usize;
    let col = match lines.line(path, line) {
        Some(text) => encoding.to_chars(&text, units),
        None => units,
    };
    Some(Pos { line, col })
}

fn diagnostic(d: &Value, path: &Path, encoding: Encoding, lines: &dyn Lines) -> Option<Diagnostic> {
    Some(Diagnostic {
        start: position(&d["range"]["start"], path, encoding, lines)?,
        end: position(&d["range"]["end"], path, encoding, lines)?,
        severity: match d["severity"].as_u64() {
            Some(2) => Severity::Warning,
            Some(3) => Severity::Info,
            Some(4) => Severity::Hint,
            // Unset means the client decides; an unlabelled complaint is
            // treated as the serious kind rather than hidden.
            _ => Severity::Error,
        },
        message: d["message"].as_str()?.to_string(),
        source: d["source"].as_str().map(str::to_string),
    })
}

/// Text edits for `path`, converted. Apart from [`decode`] because a
/// formatting answer does not name its document — the caller knows which one
/// it asked about.
pub fn text_edits(result: &Value, path: &Path, encoding: Encoding, lines: &dyn Lines) -> Vec<TextEdit> {
    result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|e| {
                    Some(TextEdit {
                        start: position(&e["range"]["start"], path, encoding, lines)?,
                        end: position(&e["range"]["end"], path, encoding, lines)?,
                        text: e["newText"].as_str()?.replace("\r\n", "\n"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The first place a definition answer names. Servers answer with one
/// location, a list of them, or a list of links, depending on mood.
fn location(result: &Value, encoding: Encoding, lines: &dyn Lines) -> Option<(PathBuf, Pos)> {
    let first = match result {
        Value::Array(list) => list.first()?,
        Value::Null => return None,
        one => one,
    };
    let (uri, range) = match first.get("targetUri") {
        Some(uri) => (uri, first.get("targetSelectionRange").unwrap_or(&first["targetRange"])),
        None => (&first["uri"], &first["range"]),
    };
    let path = path_of(uri.as_str()?)?;
    let at = position(&range["start"], &path, encoding, lines)?;
    Some((path, at))
}

/// Hover contents as plain lines. The three shapes the protocol has grown:
/// a string, a `{language, value}` pair, a `{kind, value}` markup block — or
/// a list of the first two.
fn hover_text(contents: &Value) -> Option<String> {
    fn one(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(_) => v["value"].as_str().map(str::to_string),
            _ => None,
        }
    }
    let text = match contents {
        Value::Array(list) => list.iter().filter_map(one).collect::<Vec<_>>().join("\n"),
        other => one(other)?,
    };
    // Markdown fences and rules are noise in a plain-text box.
    let cleaned: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("```") && l.trim() != "---")
        .collect();
    let cleaned = cleaned.join("\n").trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn completions(result: &Value) -> Vec<Completion> {
    let list = match result {
        Value::Array(list) => list.as_slice(),
        other => other["items"].as_array().map(Vec::as_slice).unwrap_or(&[]),
    };
    let mut items: Vec<(String, Completion)> = list
        .iter()
        .filter_map(|c| {
            let label = c["label"].as_str()?.to_string();
            let raw = c["textEdit"]["newText"]
                .as_str()
                .or_else(|| c["insertText"].as_str())
                .unwrap_or(&label);
            let insert = if c["insertTextFormat"].as_u64() == Some(2) { plain_snippet(raw) } else { raw.to_string() };
            let sort = c["sortText"].as_str().unwrap_or(&label).to_string();
            Some((
                sort,
                Completion {
                    label,
                    insert,
                    detail: c["detail"].as_str().map(str::to_string),
                    kind: c["kind"].as_u64().and_then(kind_name),
                },
            ))
        })
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    items.into_iter().map(|(_, c)| c).collect()
}

/// A snippet with its tab stops taken out: `push(${1:value})$0` becomes
/// `push(value)`. The editor's snippets have one caret and no tab-through,
/// so the placeholder's default text is the useful part.
fn plain_snippet(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '$' if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() => {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            '$' if i + 1 < chars.len() && chars[i + 1] == '{' => {
                // ${1:default} keeps `default`; ${1} keeps nothing.
                let mut j = i + 2;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                let mut depth = 1;
                let mut body = String::new();
                let has_default = j < chars.len() && chars[j] == ':';
                if has_default {
                    j += 1;
                }
                while j < chars.len() && depth > 0 {
                    match chars[j] {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    if depth > 0 {
                        body.push(chars[j]);
                    }
                    j += 1;
                }
                if has_default {
                    out.push_str(&plain_snippet(&body));
                }
                i = j;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn kind_name(kind: u64) -> Option<&'static str> {
    Some(match kind {
        2 => "method",
        3 => "fn",
        4 => "ctor",
        5 => "field",
        6 => "var",
        7 => "class",
        8 => "iface",
        9 => "mod",
        10 => "prop",
        13 => "enum",
        14 => "kw",
        15 => "snip",
        20 => "variant",
        21 => "const",
        22 => "struct",
        25 => "type",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// The process

/// How to start a server.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub command: String,
    pub args: Vec<String>,
    /// The workspace, sent as the root and used as the working directory.
    pub root: PathBuf,
    /// `rust`, `python`: the `languageId` documents are opened with.
    pub language: String,
}

/// The documents as the server last heard them, which is also what its
/// positions are counted against.
#[derive(Default)]
struct Documents {
    open: HashMap<PathBuf, (i64, Vec<String>)>,
}

impl Lines for Documents {
    fn line(&self, path: &Path, line: usize) -> Option<String> {
        if let Some((_, lines)) = self.open.get(path) {
            return lines.get(line).cloned();
        }
        // A definition in a file nobody has open: the disk is the text.
        std::fs::read_to_string(path).ok()?.lines().nth(line).map(str::to_string)
    }
}

enum Incoming {
    Message(Value),
    Stderr(String),
    Closed,
}

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    incoming: Receiver<Incoming>,
    pending: HashMap<u64, Request>,
    /// Which document each formatting request was about; the answer does
    /// not say.
    formatting: HashMap<u64, PathBuf>,
    docs: Documents,
    next_id: u64,
    encoding: Encoding,
    ready: bool,
    /// Sent before the handshake finished, flushed once it has.
    queued: Vec<Value>,
    last_stderr: String,
    language: String,
    exited: bool,
}

impl Client {
    pub fn spawn(opts: &Options) -> Result<Self, String> {
        let mut cmd = Command::new(&opts.command);
        cmd.args(&opts.args)
            .current_dir(&opts.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("`{}` is not on PATH", opts.command),
            _ => e.to_string(),
        })?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;

        let (tx, incoming) = channel();
        let err_tx = tx.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if !line.trim().is_empty() && err_tx.send(Incoming::Stderr(line)).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || read_loop(stdout, tx));

        let mut me = Self {
            child,
            stdin,
            incoming,
            pending: HashMap::new(),
            formatting: HashMap::new(),
            docs: Documents::default(),
            next_id: 1,
            encoding: Encoding::Utf16,
            ready: false,
            queued: Vec::new(),
            last_stderr: String::new(),
            language: opts.language.clone(),
            exited: false,
        };
        let root_uri = uri_of(&opts.root);
        let name = opts.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let id = me.take_id(Request::Initialize);
        me.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "clientInfo": { "name": "kubide" },
                "rootUri": root_uri,
                "workspaceFolders": [{ "uri": root_uri, "name": name }],
                "capabilities": {
                    "general": { "positionEncodings": ["utf-32", "utf-16"] },
                    "textDocument": {
                        "synchronization": { "didSave": true },
                        "publishDiagnostics": {},
                        "definition": { "linkSupport": true },
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        "formatting": {},
                        "completion": {
                            "completionItem": { "snippetSupport": true },
                        },
                    },
                },
            },
        }))?;
        Ok(me)
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn has_exited(&self) -> bool {
        self.exited
    }

    fn take_id(&mut self, request: Request) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, request);
        id
    }

    fn write(&mut self, message: &Value) -> Result<(), String> {
        self.stdin
            .write_all(&frame(message))
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("could not reach the language server: {e}"))
    }

    /// Sends now, or once the handshake is done: the protocol forbids
    /// anything but `initialize` before its answer.
    fn send(&mut self, message: Value) {
        if self.ready {
            let _ = self.write(&message);
        } else {
            self.queued.push(message);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, kind: Request, method: &str, params: Value) -> u64 {
        let id = self.take_id(kind);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    /// Tells the server what a document holds: opens it the first time,
    /// replaces its text after. A no-op when `revision` is the one already
    /// sent, so it is safe to call every frame.
    pub fn sync(&mut self, path: &Path, revision: u64, text: impl FnOnce() -> String) {
        let version = revision as i64;
        match self.docs.open.get(path) {
            Some((sent, _)) if *sent == version => {}
            Some(_) => {
                let text = text();
                self.docs.open.insert(path.to_path_buf(), (version, split(&text)));
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri_of(path), "version": version },
                        "contentChanges": [{ "text": text }],
                    }),
                );
            }
            None => {
                let text = text();
                self.docs.open.insert(path.to_path_buf(), (version, split(&text)));
                self.notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri_of(path),
                            "languageId": self.language,
                            "version": version,
                            "text": text,
                        },
                    }),
                );
            }
        }
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.docs.open.contains_key(path)
    }

    pub fn saved(&mut self, path: &Path) {
        if self.is_open(path) {
            self.notify("textDocument/didSave", json!({ "textDocument": { "uri": uri_of(path) } }));
        }
    }

    pub fn close(&mut self, path: &Path) {
        if self.docs.open.remove(path).is_some() {
            self.notify("textDocument/didClose", json!({ "textDocument": { "uri": uri_of(path) } }));
        }
    }

    fn at(&self, path: &Path, pos: Pos) -> Value {
        let line = self.docs.line(path, pos.line).unwrap_or_default();
        json!({
            "textDocument": { "uri": uri_of(path) },
            "position": { "line": pos.line, "character": self.encoding.to_units(&line, pos.col) },
        })
    }

    pub fn definition(&mut self, path: &Path, pos: Pos) -> u64 {
        let params = self.at(path, pos);
        self.request(Request::Definition, "textDocument/definition", params)
    }

    pub fn hover(&mut self, path: &Path, pos: Pos) -> u64 {
        let params = self.at(path, pos);
        self.request(Request::Hover, "textDocument/hover", params)
    }

    pub fn completion(&mut self, path: &Path, pos: Pos) -> u64 {
        let params = self.at(path, pos);
        self.request(Request::Completion, "textDocument/completion", params)
    }

    pub fn format(&mut self, path: &Path, tab_size: usize) -> u64 {
        let id = self.request(
            Request::Formatting,
            "textDocument/formatting",
            json!({
                "textDocument": { "uri": uri_of(path) },
                "options": { "tabSize": tab_size, "insertSpaces": true },
            }),
        );
        self.formatting.insert(id, path.to_path_buf());
        id
    }

    /// Everything that arrived since the last call.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            match self.incoming.try_recv() {
                Ok(Incoming::Message(message)) => self.handle(message, &mut out),
                Ok(Incoming::Stderr(line)) => self.last_stderr = line,
                Ok(Incoming::Closed) => {
                    if !self.exited {
                        self.exited = true;
                        out.push(Event::Exited(self.last_stderr.clone()));
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    fn handle(&mut self, message: Value, out: &mut Vec<Event>) {
        // The handshake's answer: learn the encoding, say `initialized`,
        // then let everything that queued behind it go.
        let id = message.get("id").and_then(Value::as_u64);
        if let Some(id) = id.filter(|id| self.pending.get(id) == Some(&Request::Initialize)) {
            self.pending.remove(&id);
            if let Some(name) = message["result"]["capabilities"]["positionEncoding"].as_str() {
                self.encoding = Encoding::from_name(name);
            }
            self.ready = true;
            let _ = self.write(&json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
            for queued in std::mem::take(&mut self.queued) {
                let _ = self.write(&queued);
            }
            out.push(Event::Ready);
            return;
        }

        let formatting = id.and_then(|id| self.formatting.remove(&id));
        let (event, reply) = decode(&message, &mut self.pending, self.encoding, &self.docs);
        if let Some(reply) = reply {
            let _ = self.write(&reply);
        }
        match (event, formatting) {
            (Some(Event::Formatting { id, .. }), Some(path)) => out.push(Event::Formatting {
                id,
                edits: text_edits(&message["result"], &path, self.encoding, &self.docs),
            }),
            (Some(event), _) => out.push(event),
            (None, _) => {}
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Asked nicely first: rust-analyzer flushes its caches on a clean
        // exit, and the kill that follows is for the ones that do not leave.
        if self.ready && !self.exited {
            let id = self.take_id(Request::Shutdown);
            let _ = self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": "shutdown" }));
            let _ = self.write(&json!({ "jsonrpc": "2.0", "method": "exit" }));
        }
        self.kill();
    }
}

fn split(text: &str) -> Vec<String> {
    text.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect()
}

fn read_loop(mut stdout: std::process::ChildStdout, tx: Sender<Incoming>) {
    let mut framer = Framer::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match stdout.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                for message in framer.feed(&chunk[..n]) {
                    if tx.send(Incoming::Message(message)).is_err() {
                        return;
                    }
                }
            }
        }
    }
    let _ = tx.send(Incoming::Closed);
}

/// A console window per server would flash up on Windows otherwise.
#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_: &mut Command) {}

#[cfg(test)]
mod tests;
