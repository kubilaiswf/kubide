//! Claude Code, driven over a pipe.
//!
//! The agent is the `claude` CLI, spawned in print mode with one JSON object
//! per line in both directions (`--input-format stream-json` in,
//! `--output-format stream-json` out). That is the seam the official editor
//! extensions and the Agent SDK sit on, and it is why there is no tool loop,
//! no permission system and no context manager in here: the CLI owns every
//! one of those, and reimplementing any of them against the Messages API
//! would produce a second, worse Claude Code.
//!
//! No PTY. The protocol is lines of JSON and nothing else, so a plain pipe
//! is the right tool; ConPTY is for programs that draw.
//!
//! Two halves: [`Parser`], a pure function from a line to events, tested as
//! one; and [`Agent`], the process around it. The parser knows nothing about
//! threads and the process knows nothing about JSON.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

// Tool inputs are handed on as they came; the caller reads them with the
// same type rather than a second JSON dependency, and builds test fixtures
// with the same macro.
pub use serde_json::{json, Value};

/// How to start the CLI.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// The executable: `claude` on PATH, or a full path.
    pub command: String,
    /// The workspace. Claude Code reads CLAUDE.md and scopes its file
    /// tools to it.
    pub cwd: PathBuf,
    /// `None` leaves the CLI's own default.
    pub model: Option<String>,
    /// `acceptEdits`, `plan`, `bypassPermissions`, and the rest of the CLI's
    /// list — passed through, not interpreted.
    pub permission_mode: String,
    /// `--allowedTools` patterns, e.g. `Bash(cargo test *)`.
    pub allowed_tools: Vec<String>,
    /// A session to continue rather than a fresh one.
    pub resume: Option<String>,
}

/// One piece of an assistant message.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        /// The tool's arguments. Empty while the block is still streaming;
        /// complete once the `assistant` message arrives.
        input: Value,
    },
    /// Reasoning happened. Current models do not return the text, so this
    /// says only that much.
    Thinking,
}

/// What the CLI said, translated. One event per line at most, often none:
/// the stream carries plenty this layer has no use for.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The session is up. Arrives once, before anything else.
    Init { session_id: String, model: String },
    /// A content block began streaming inside message `message_id`.
    BlockStart { message_id: String, index: usize, block: Block },
    /// More text for a streaming block.
    TextDelta { message_id: String, index: usize, text: String },
    /// The finished blocks of an assistant message. Supersedes whatever was
    /// streamed for the same id — the CLI may send one of these per block
    /// or one per message, and either way the streamed copy is the draft.
    Assistant { message_id: String, blocks: Vec<Block> },
    /// What a tool call came back with.
    ToolResult { tool_use_id: String, is_error: bool, text: String },
    /// The turn is over.
    Result {
        is_error: bool,
        /// Cumulative for the session, as the CLI reports it.
        cost_usd: Option<f64>,
        turns: u64,
        duration_ms: u64,
        /// The final text on success; the failure kind otherwise.
        text: Option<String>,
    },
    /// Where the subscription's usage windows stand. Sent whenever the CLI
    /// learns something new from a response, so at least once a turn.
    RateLimit(Limits),
    /// The CLI is asking whether a tool may run. The turn waits on
    /// [`Agent::respond`]; nothing else moves until it gets one.
    Permission(Permission),
    /// The CLI answered one of ours — an interrupt, a mode change. `error`
    /// carries its complaint when it refused.
    Control { request_id: String, error: Option<String> },
    /// A line on stderr — where the CLI puts "not logged in" and friends.
    Stderr(String),
    /// The process is gone. `None` when it was killed.
    Exited(Option<i32>),
}

/// One usage window of a subscription: how much of it is spent, and when
/// it starts over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    /// 0.0 to 1.0.
    pub utilization: f64,
    /// Unix seconds.
    pub resets_at: u64,
}

/// A subscription's limits, as the CLI reports them. Only ever seen on a
/// claude.ai login: an API key has no windows, and its turns cost money
/// instead.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// `allowed`, `allowed_warning` or `rejected`.
    pub status: String,
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    /// Past the plan and into paid usage, in which case the cost is real
    /// again.
    pub using_overage: bool,
}

impl Limits {
    pub fn rejected(&self) -> bool {
        self.status == "rejected"
    }
}

/// A tool call waiting on a yes or no.
///
/// What the CLI would have asked in its own terminal, handed over instead:
/// the same fields its prompt is built from, so the box drawn here can say
/// the same things.
#[derive(Clone, Debug, PartialEq)]
pub struct Permission {
    /// Goes back with the answer, so the CLI knows which question it was.
    pub request_id: String,
    pub tool_name: String,
    /// The call's arguments, exactly as the tool would receive them.
    pub input: Value,
    /// The CLI's own one-line summary of the call.
    pub description: String,
    /// Rules the CLI proposes for "don't ask again", ready to send back
    /// as `updatedPermissions`. Opaque here on purpose: their shape is the
    /// CLI's business, and echoing them is all a "yes, always" needs.
    pub suggestions: Vec<Value>,
    pub tool_use_id: String,
}

/// An answer to a [`Permission`].
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Run it. `remember` sends the CLI's suggested rules back with the
    /// answer, which is what its own "yes, and don't ask again" does.
    Allow { remember: bool },
    Deny,
}

/// The line that answers a permission request. Pure, so the tests can
/// read it without a process on the other end.
pub fn response_line(ask: &Permission, decision: &Decision) -> Value {
    let response = match decision {
        Decision::Allow { remember } => {
            let mut r = json!({ "behavior": "allow", "updatedInput": ask.input });
            if *remember && !ask.suggestions.is_empty() {
                r["updatedPermissions"] = Value::Array(ask.suggestions.clone());
            }
            r
        }
        Decision::Deny => json!({
            "behavior": "deny",
            "message": "The user declined this action in kubide.",
        }),
    };
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": ask.request_id,
            "response": response,
        },
    })
}

/// The wire format, one line at a time.
///
/// Stateful only for the message id: deltas do not repeat it, so it is
/// remembered from `message_start`.
#[derive(Default)]
pub struct Parser {
    current: Option<String>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Events for one line of stdout. Unparseable or irrelevant lines yield
    /// nothing rather than an error: the stream carries diagnostics and
    /// event kinds this layer has no use for, and none of them should stop
    /// the ones it does.
    pub fn feed(&mut self, line: &str) -> Vec<Event> {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        // A sub-agent's traffic carries the Task call it belongs to. Its
        // parent shows as one tool row; the chatter under it would drown
        // the transcript.
        if v.get("parent_tool_use_id").and_then(Value::as_str).is_some() {
            return Vec::new();
        }
        match v.get("type").and_then(Value::as_str) {
            Some("system") => self.system(&v),
            Some("stream_event") => self.stream(v.get("event").unwrap_or(&Value::Null)),
            Some("assistant") => self.assistant(&v),
            Some("user") => self.user(&v),
            Some("result") => self.result(&v),
            Some("rate_limit_event") => self.rate_limit(&v),
            Some("control_request") => self.control_request(&v),
            Some("control_response") => self.control_response(&v),
            _ => Vec::new(),
        }
    }

    fn control_request(&mut self, v: &Value) -> Vec<Event> {
        let Some(req) = v.get("request") else { return Vec::new() };
        // Only the question. Hook callbacks and MCP relays are for a client
        // that registered them, which this one never does.
        if req.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
            return Vec::new();
        }
        vec![Event::Permission(Permission {
            request_id: str_of(v, "request_id"),
            tool_name: str_of(req, "tool_name"),
            input: req.get("input").cloned().unwrap_or(Value::Null),
            description: str_of(req, "description"),
            suggestions: req
                .get("permission_suggestions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            tool_use_id: str_of(req, "tool_use_id"),
        })]
    }

    fn control_response(&mut self, v: &Value) -> Vec<Event> {
        let Some(r) = v.get("response") else { return Vec::new() };
        let error = (r.get("subtype").and_then(Value::as_str) == Some("error"))
            .then(|| str_of(r, "error"));
        vec![Event::Control { request_id: str_of(r, "request_id"), error }]
    }

    fn rate_limit(&mut self, v: &Value) -> Vec<Event> {
        let Some(info) = v.get("rate_limit_info") else { return Vec::new() };
        let window = |name: &str| -> Option<Window> {
            let w = info.get("unifiedWindows")?.get(name)?;
            Some(Window {
                utilization: w.get("utilization")?.as_f64()?,
                resets_at: w.get("resetsAt").and_then(Value::as_u64).unwrap_or(0),
            })
        };
        vec![Event::RateLimit(Limits {
            status: str_of(info, "status"),
            five_hour: window("five_hour"),
            seven_day: window("seven_day"),
            using_overage: info.get("isUsingOverage").and_then(Value::as_bool).unwrap_or(false),
        })]
    }

    fn system(&mut self, v: &Value) -> Vec<Event> {
        if v.get("subtype").and_then(Value::as_str) != Some("init") {
            return Vec::new();
        }
        vec![Event::Init {
            session_id: str_of(v, "session_id"),
            model: str_of(v, "model"),
        }]
    }

    fn stream(&mut self, e: &Value) -> Vec<Event> {
        match e.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                self.current = e
                    .get("message")
                    .and_then(|m| m.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                Vec::new()
            }
            Some("content_block_start") => {
                let (Some(message_id), Some(block)) = (
                    self.current.clone(),
                    e.get("content_block").and_then(block_of),
                ) else {
                    return Vec::new();
                };
                vec![Event::BlockStart { message_id, index: index_of(e), block }]
            }
            Some("content_block_delta") => {
                let Some(message_id) = self.current.clone() else { return Vec::new() };
                let delta = e.get("delta").unwrap_or(&Value::Null);
                if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
                    // Tool arguments stream as JSON fragments; the complete
                    // input comes with the assistant message, so partial
                    // JSON is not worth showing.
                    return Vec::new();
                }
                vec![Event::TextDelta { message_id, index: index_of(e), text: str_of(delta, "text") }]
            }
            _ => Vec::new(),
        }
    }

    fn assistant(&mut self, v: &Value) -> Vec<Event> {
        let Some(m) = v.get("message") else { return Vec::new() };
        let message_id = str_of(m, "id");
        let blocks = m
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| blocks.iter().filter_map(block_of).collect())
            .unwrap_or_default();
        vec![Event::Assistant { message_id, blocks }]
    }

    fn user(&mut self, v: &Value) -> Vec<Event> {
        let Some(content) = v.get("message").and_then(|m| m.get("content")).and_then(Value::as_array)
        else {
            return Vec::new();
        };
        content
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .map(|b| Event::ToolResult {
                tool_use_id: str_of(b, "tool_use_id"),
                is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                text: result_text(b.get("content").unwrap_or(&Value::Null)),
            })
            .collect()
    }

    fn result(&mut self, v: &Value) -> Vec<Event> {
        let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        // The result text when there is one, else the failure kind. A
        // failed turn can still carry `subtype: success` — a usage limit
        // does — so the kind is the fallback, not the rule.
        let text = v
            .get("result")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .or_else(|| {
                is_error
                    .then(|| v.get("subtype").and_then(Value::as_str))
                    .flatten()
                    .filter(|s| *s != "success")
                    .map(str::to_string)
            });
        vec![Event::Result {
            is_error,
            cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
            turns: v.get("num_turns").and_then(Value::as_u64).unwrap_or(0),
            duration_ms: v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
            text,
        }]
    }
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn index_of(e: &Value) -> usize {
    e.get("index").and_then(Value::as_u64).unwrap_or(0) as usize
}

fn block_of(b: &Value) -> Option<Block> {
    Some(match b.get("type").and_then(Value::as_str)? {
        "text" => Block::Text(str_of(b, "text")),
        "tool_use" => Block::ToolUse {
            id: str_of(b, "id"),
            name: str_of(b, "name"),
            input: b.get("input").cloned().unwrap_or(Value::Null),
        },
        "thinking" | "redacted_thinking" => Block::Thinking,
        _ => return None,
    })
}

/// A tool result's content is a string or a list of blocks; either way,
/// the text of it.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A slash command the CLI knows for this workspace: a skill or a custom
/// command, from the user's own folder or the project's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// Without the slash.
    pub name: String,
    /// The `description:` line of its frontmatter, or the first line of
    /// its text; empty when neither says anything.
    pub description: String,
}

/// The slash commands available in `cwd`, sorted by name.
///
/// Read from disk the way the CLI reads them — `.claude/commands/*.md`
/// and `.claude/skills/*/SKILL.md`, in the home folder and the project —
/// because the CLI has no way to be asked, and a list typed by hand
/// would be stale by the next `claude` update. Plugins are not walked:
/// their layout is the CLI's business and changes with it.
pub fn skills(cwd: &std::path::Path) -> Vec<Skill> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".claude"));
    }
    roots.push(cwd.join(".claude"));

    let mut out: Vec<Skill> = Vec::new();
    for root in roots {
        if let Ok(entries) = std::fs::read_dir(root.join("commands")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|s| s.to_str()) else { continue };
                out.push(Skill { name: name.to_string(), description: describe(&path) });
            }
        }
        if let Ok(entries) = std::fs::read_dir(root.join("skills")) {
            for entry in entries.flatten() {
                let path = entry.path().join("SKILL.md");
                if !path.is_file() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
                out.push(Skill { name, description: describe(&path) });
            }
        }
    }
    // The project's copy of a name wins, the way the CLI resolves it.
    out.reverse();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// What a command file says about itself, in one line.
fn describe(path: &std::path::Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else { return String::new() };
    let mut lines = text.lines();
    if lines.next().map(str::trim) == Some("---") {
        for line in lines.by_ref() {
            let line = line.trim();
            if line == "---" {
                break;
            }
            if let Some(rest) = line.strip_prefix("description:") {
                return clip_line(rest.trim().trim_matches('"'));
            }
        }
    }
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("---"))
        .map(|l| clip_line(l.trim_start_matches('#').trim()))
        .unwrap_or_default()
}

fn clip_line(s: &str) -> String {
    const MAX: usize = 100;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX - 1).collect();
    out.push('\u{2026}');
    out
}

/// A running `claude` process.
///
/// Dropping it kills the process. There is nothing to hand back: a session
/// lives on disk under the CLI's own directory, and `Options::resume`
/// brings it up again.
pub struct Agent {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<Event>,
    /// Numbers our own control requests, so their acknowledgements can be
    /// told apart.
    next_request: u64,
}

impl Agent {
    pub fn spawn(opts: &Options) -> Result<Self, String> {
        let mut cmd = Command::new(&opts.command);
        cmd.current_dir(&opts.cwd)
            .arg("-p")
            // Print mode refuses stream-json output without it.
            .arg("--verbose")
            .args(["--output-format", "stream-json"])
            .args(["--input-format", "stream-json"])
            .arg("--include-partial-messages")
            // Questions come down the pipe as `control_request` lines
            // instead of being answered "no" on the CLI's own.
            .args(["--permission-prompt-tool", "stdio"])
            .args(["--permission-mode", &opts.permission_mode]);
        if let Some(model) = &opts.model {
            cmd.args(["--model", model]);
        }
        if let Some(id) = &opts.resume {
            cmd.args(["--resume", id]);
        }
        // Variadic, so it goes last: anything after it would be read as
        // another pattern.
        if !opts.allowed_tools.is_empty() {
            cmd.arg("--allowedTools").args(&opts.allowed_tools);
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        no_window(&mut cmd);

        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                format!("`{}` is not on PATH — install Claude Code first", opts.command)
            }
            _ => e.to_string(),
        })?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;

        let (tx, events) = channel();
        let err_tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if !line.trim().is_empty() && err_tx.send(Event::Stderr(line)).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || read_loop(stdout, tx));

        Ok(Self { child, stdin, events, next_request: 1 })
    }

    /// Sends one user turn. Fails when the process is gone, which is the
    /// caller's cue to start another one with `resume`.
    pub fn send(&mut self, text: &str) -> Result<(), String> {
        self.write(&json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{ "type": "text", "text": text }],
            },
        }))
    }

    /// Answers a [`Permission`]. The turn is stuck until this happens.
    pub fn respond(&mut self, ask: &Permission, decision: &Decision) -> Result<(), String> {
        self.write(&response_line(ask, decision))
    }

    /// Stops the current turn without stopping the process. The CLI ends
    /// the turn with an error-kind result and a "[Request interrupted by
    /// user]" note; the session and the process both live on.
    pub fn interrupt(&mut self) -> Result<String, String> {
        self.control(json!({ "subtype": "interrupt" }))
    }

    /// Switches how the rest of the session asks about tools: `plan`,
    /// `default`, `acceptEdits`, and the rest of the CLI's list.
    pub fn set_permission_mode(&mut self, mode: &str) -> Result<String, String> {
        self.control(json!({ "subtype": "set_permission_mode", "mode": mode }))
    }

    /// Switches models from the next turn on.
    pub fn set_model(&mut self, model: &str) -> Result<String, String> {
        self.control(json!({ "subtype": "set_model", "model": model }))
    }

    /// One of our requests to the CLI. Returns its id, which the matching
    /// [`Event::Control`] carries back.
    fn control(&mut self, request: Value) -> Result<String, String> {
        let id = format!("kb-{}", self.next_request);
        self.next_request += 1;
        self.write(&json!({
            "type": "control_request",
            "request_id": id,
            "request": request,
        }))?;
        Ok(id)
    }

    fn write(&mut self, line: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(line).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("could not reach claude: {e}"))
    }

    /// Everything that arrived since the last call.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(e) => out.push(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // Both reader threads are gone, which only happens after
                    // stdout closed — and that path already said `Exited`.
                    break;
                }
            }
        }
        out
    }

    /// Stops the process. Nothing softer is on offer: the CLI has no
    /// documented way to interrupt a turn from this side, and the session
    /// survives on disk for `resume`.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.kill();
    }
}

fn read_loop(stdout: std::process::ChildStdout, tx: Sender<Event>) {
    let mut parser = Parser::new();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        for event in parser.feed(&line) {
            if tx.send(event).is_err() {
                return;
            }
        }
    }
    // Exit status is not readable from here — the child is owned by the
    // `Agent` — and a code nobody looks at is not worth a shared handle.
    let _ = tx.send(Event::Exited(None));
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: the editor is a windows-subsystem process with no
    // console, and without this one flashes up for the child.
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[&str]) -> Vec<Event> {
        let mut p = Parser::new();
        lines.iter().flat_map(|l| p.feed(l)).collect()
    }

    #[test]
    fn init_names_the_session_and_model() {
        let events = feed(&[
            r#"{"type":"system","subtype":"init","session_id":"abc","model":"claude-opus-5","tools":["Read"]}"#,
        ]);
        assert_eq!(
            events,
            vec![Event::Init { session_id: "abc".into(), model: "claude-opus-5".into() }]
        );
    }

    #[test]
    fn text_streams_under_the_message_that_started_it() {
        let events = feed(&[
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}}"#,
        ]);
        assert_eq!(
            events,
            vec![
                Event::BlockStart { message_id: "m1".into(), index: 0, block: Block::Text(String::new()) },
                Event::TextDelta { message_id: "m1".into(), index: 0, text: "Hel".into() },
                Event::TextDelta { message_id: "m1".into(), index: 0, text: "lo".into() },
            ]
        );
    }

    #[test]
    fn tool_argument_fragments_are_not_events() {
        let events = feed(&[
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"file"}}}"#,
        ]);
        assert!(events.is_empty());
    }

    #[test]
    fn a_complete_assistant_message_carries_its_blocks() {
        let events = feed(&[
            r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"Reading."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"C:\\x.rs"}}]}}"#,
        ]);
        assert_eq!(
            events,
            vec![Event::Assistant {
                message_id: "m1".into(),
                blocks: vec![
                    Block::Text("Reading.".into()),
                    Block::ToolUse {
                        id: "t1".into(),
                        name: "Read".into(),
                        input: json!({"file_path": "C:\\x.rs"}),
                    },
                ],
            }]
        );
    }

    #[test]
    fn tool_results_come_as_text_whichever_shape_they_take() {
        let events = feed(&[
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"},{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":[{"type":"text","text":"no such file"}]}]}}"#,
        ]);
        assert_eq!(
            events,
            vec![
                Event::ToolResult { tool_use_id: "t1".into(), is_error: false, text: "ok".into() },
                Event::ToolResult { tool_use_id: "t2".into(), is_error: true, text: "no such file".into() },
            ]
        );
    }

    #[test]
    fn the_result_line_closes_the_turn() {
        let events = feed(&[
            r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1200,"num_turns":3,"result":"Done.","total_cost_usd":0.0123}"#,
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"duration_ms":5,"num_turns":10}"#,
            r#"{"type":"result","subtype":"success","is_error":true,"duration_ms":5,"num_turns":1,"result":""}"#,
        ]);
        assert_eq!(
            events,
            vec![
                Event::Result {
                    is_error: false,
                    cost_usd: Some(0.0123),
                    turns: 3,
                    duration_ms: 1200,
                    text: Some("Done.".into()),
                },
                Event::Result {
                    is_error: true,
                    cost_usd: None,
                    turns: 10,
                    duration_ms: 5,
                    text: Some("error_max_turns".into()),
                },
                // A failed turn that still says "success" — a usage limit
                // does this — carries no text rather than a lie.
                Event::Result { is_error: true, cost_usd: None, turns: 1, duration_ms: 5, text: None },
            ]
        );
    }

    #[test]
    fn a_rate_limit_event_reads_both_windows() {
        // As captured from a real headless turn on a subscription login.
        let events = feed(&[
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1788397800,"rateLimitType":"five_hour","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.64,"resetsAt":1788397800},"seven_day":{"utilization":0.1,"resetsAt":1788620400}}},"uuid":"x","session_id":"s"}"#,
        ]);
        assert_eq!(
            events,
            vec![Event::RateLimit(Limits {
                status: "allowed".into(),
                five_hour: Some(Window { utilization: 0.64, resets_at: 1788397800 }),
                seven_day: Some(Window { utilization: 0.1, resets_at: 1788620400 }),
                using_overage: false,
            })]
        );
    }

    #[test]
    fn a_permission_question_arrives_with_what_the_cli_would_have_asked() {
        // As captured from a real headless turn with --permission-prompt-tool stdio.
        let events = feed(&[
            r#"{"type":"control_request","request_id":"cffba465","request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash","input":{"command":"powershell -Command Write-Output hi"},"description":"powershell -Command Write-Output hi","permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"powershell -Command Write-Output hi"}],"behavior":"allow","destination":"localSettings"}],"decision_reason":"This command requires approval","tool_use_id":"toolu_01"}}"#,
        ]);
        let [Event::Permission(ask)] = events.as_slice() else {
            panic!("expected one permission event, got {events:?}");
        };
        assert_eq!(ask.request_id, "cffba465");
        assert_eq!(ask.tool_name, "Bash");
        assert_eq!(ask.input["command"], "powershell -Command Write-Output hi");
        assert_eq!(ask.suggestions.len(), 1);
        assert_eq!(ask.tool_use_id, "toolu_01");
    }

    #[test]
    fn the_answer_echoes_the_input_and_only_remembers_when_asked() {
        let ask = Permission {
            request_id: "r1".into(),
            tool_name: "Bash".into(),
            input: json!({"command": "cargo test"}),
            description: "cargo test".into(),
            suggestions: vec![json!({"type": "addRules", "rules": []})],
            tool_use_id: "t".into(),
        };
        let once = response_line(&ask, &Decision::Allow { remember: false });
        assert_eq!(once["type"], "control_response");
        assert_eq!(once["response"]["request_id"], "r1");
        assert_eq!(once["response"]["response"]["behavior"], "allow");
        assert_eq!(once["response"]["response"]["updatedInput"]["command"], "cargo test");
        assert!(once["response"]["response"].get("updatedPermissions").is_none());

        let always = response_line(&ask, &Decision::Allow { remember: true });
        assert_eq!(always["response"]["response"]["updatedPermissions"], json!(ask.suggestions));

        let no = response_line(&ask, &Decision::Deny);
        assert_eq!(no["response"]["response"]["behavior"], "deny");
        assert!(no["response"]["response"]["message"].as_str().unwrap().contains("declined"));
    }

    #[test]
    fn other_control_traffic_is_reduced_to_acknowledgements() {
        let events = feed(&[
            r#"{"type":"control_request","request_id":"h1","request":{"subtype":"hook_callback","callback_id":"x"}}"#,
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"kb-1","response":{"still_queued":[]}}}"#,
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"kb-2","error":"no such model"}}"#,
        ]);
        assert_eq!(
            events,
            vec![
                Event::Control { request_id: "kb-1".into(), error: None },
                Event::Control { request_id: "kb-2".into(), error: Some("no such model".into()) },
            ]
        );
    }

    #[test]
    fn skills_are_read_from_the_project_with_their_descriptions() {
        let root = std::env::temp_dir().join("kb-agent-skills");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".claude/commands")).unwrap();
        std::fs::create_dir_all(root.join(".claude/skills/deploy")).unwrap();
        std::fs::write(
            root.join(".claude/commands/review.md"),
            "---\nname: review\ndescription: \"Review the diff\"\n---\nbody",
        )
        .unwrap();
        std::fs::write(root.join(".claude/commands/notes.txt"), "not a command").unwrap();
        std::fs::write(root.join(".claude/skills/deploy/SKILL.md"), "# Deploy it\n\nlonger text").unwrap();

        let found = skills(&root);
        let mine: Vec<&Skill> = found.iter().filter(|s| s.name == "review" || s.name == "deploy").collect();
        assert_eq!(
            mine,
            vec![
                &Skill { name: "deploy".into(), description: "Deploy it".into() },
                &Skill { name: "review".into(), description: "Review the diff".into() },
            ]
        );
        assert!(!found.iter().any(|s| s.name == "notes"));
    }

    #[test]
    fn subagent_traffic_and_noise_are_dropped() {
        let events = feed(&[
            "not json at all",
            r#"{"type":"assistant","parent_tool_use_id":"t9","message":{"id":"m2","content":[{"type":"text","text":"inner"}]}}"#,
            r#"{"type":"system","subtype":"api_retry"}"#,
        ]);
        assert!(events.is_empty());
    }
}
