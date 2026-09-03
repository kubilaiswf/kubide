//! The agent pane: a conversation with Claude Code.
//!
//! A pane rather than an overlay, like the terminal: it is a live process
//! you glance at while working next to it. The transcript is the CLI's
//! stream turned into rows — what was asked, what Claude said, which tools
//! it reached for and what they returned — and a one-line box at the
//! bottom to type the next turn into.
//!
//! The pane never touches files itself. The CLI edits the working tree, and
//! the owner is told when it did so open editors can re-read from disk.

use std::collections::VecDeque;

use kb_agent::{Block, Decision, Event, Permission};

/// One entry of the transcript, before wrapping.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// What was typed.
    User(String),
    /// Typed while a turn was in flight; goes out when that one ends.
    Queued(String),
    /// What Claude said.
    Text(String),
    /// A tool call, with what it came back with once it has.
    Tool { id: String, name: String, summary: String, result: Option<(bool, String)> },
    /// Reasoning happened; the text is not returned.
    Thinking,
    /// Bookkeeping: the turn's cost, an interruption, a restart.
    Note(String),
    /// Something went wrong — the CLI's stderr, a refused start.
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub kind: Kind,
    /// The assistant message this came from, for replacing a streamed
    /// draft with the finished blocks. `None` for anything not Claude's.
    message_id: Option<String>,
    /// Still streaming. Drawn through the uncached path: a row that changes
    /// on every frame must not be shaped into a cache that never forgets.
    provisional: bool,
    /// How many characters of a text are on screen so far, while the rest
    /// is still being let out. Text arrives in bursts — a clause, a
    /// paragraph — and shown as it lands it jumps; paid out a few
    /// characters a frame it reads as typing, which is what a person
    /// watching an answer form expects to see. `None` once all of it is
    /// out, or for anything that never streamed.
    revealed: Option<usize>,
}

impl Item {
    /// Not settled yet: streaming, or still being let out.
    fn unsettled(&self) -> bool {
        self.provisional || self.revealed.is_some()
    }

    /// The text as far as it has been let out.
    fn shown(&self, text: &str) -> String {
        match self.revealed {
            Some(n) if n < text.chars().count() => text.chars().take(n).collect(),
            _ => text.to_string(),
        }
    }
}

/// Fewest characters let out per frame, and the share of the backlog let
/// out on top of that. At sixty frames a second the floor is a brisk
/// typist; the share means a paragraph that lands whole is out in a
/// fraction of a second rather than typed at leisure.
const REVEAL_FLOOR: usize = 2;
const REVEAL_SHARE: usize = 8;

/// How a wrapped row is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    User,
    Text,
    Tool,
    ToolResult,
    ToolError,
    Note,
    Error,
}

/// One drawn line of the transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub text: String,
    pub style: Style,
    /// Shape without caching — see [`Item::provisional`].
    pub volatile: bool,
}

/// What a poll found, for the owner to act on.
#[derive(Default)]
pub struct Poll {
    /// Something on screen changed.
    pub changed: bool,
    /// A file was written. Editors should look at the disk.
    pub edited: bool,
}

pub struct AgentPane {
    agent: Option<kb_agent::Agent>,
    /// How to start it, kept for a restart after Esc: the same options
    /// plus `resume`, so the conversation carries on where it stopped.
    opts: kb_agent::Options,
    items: Vec<Item>,
    /// Bumped when a finished item changes — one is added, one gets its
    /// tool result, drafts are replaced — so the settled part of the
    /// transcript is rewrapped only then. Streaming bumps `tail_gen`
    /// instead: a token a frame must not rewrap everything above it.
    generation: u64,
    tail_gen: u64,
    /// Slash commands the CLI knows for this workspace, read once.
    skills: Vec<kb_agent::Skill>,
    /// Which completion is under the highlight while `/` is being typed.
    pick_cmd: usize,
    /// When the turn in flight went out, for the running count in the hint.
    turn_started: Option<std::time::Instant>,
    input: String,
    /// A turn is in flight: sent, no result yet.
    busy: bool,
    pub session_id: Option<String>,
    pub model: Option<String>,
    /// Cumulative, as the CLI reports it. An estimate at API prices —
    /// only shown when there is nobody else paying, see [`Self::hint`].
    pub cost_usd: Option<f64>,
    /// The subscription's windows, once a turn has reported them.
    pub limits: Option<kb_agent::Limits>,
    pub turns: u64,
    /// First visible row.
    pub top: usize,
    /// Stick to the bottom as rows arrive. Off once the user scrolls up,
    /// back on when they scroll to the end or send a turn — the terminal's
    /// contract, which everyone already knows.
    follow: bool,
    /// The wrapped transcript: the settled rows, then the streaming tail.
    /// `final_len` is where the tail starts; the two are relaid on their
    /// own clocks, see `generation`.
    rows: Vec<Row>,
    final_len: usize,
    laid_for: Option<(u64, usize)>,
    tail_laid: Option<(u64, u64, usize)>,
    /// Questions from the CLI, oldest first. Parallel tool calls can ask
    /// several at once; each waits its turn in the one box.
    asks: VecDeque<Permission>,
    /// Which answer is under the highlight while a question is up.
    pick: usize,
    /// Turns typed while one was in flight, sent one per result.
    queue: VecDeque<String>,
    /// An interrupt has gone out and the result that ends the turn has
    /// not come back yet. A second Esc in this state kills the process.
    stopping: bool,
}

/// Longest a tool row or result gets before an ellipsis. A full Bash
/// command or a whole file's contents is what the terminal is for.
const SUMMARY_CHARS: usize = 120;

/// How many transcript rows fit a pane of height `h`: below the header,
/// above the input box. One formula, asked by drawing and by scrolling,
/// so a page always moves by what a page shows.
pub fn visible_rows(h: f32, line_h: f32) -> usize {
    let header = line_h * 1.6;
    let input = line_h * 1.6;
    (((h - crate::metrics::INSET * 2.0 - header - input) / line_h).floor()).max(1.0) as usize
}

impl AgentPane {
    /// Starts the CLI straight away, so the header can say which model
    /// answered before anything is typed — and so "claude is not
    /// installed" is the first thing on screen rather than a surprise on
    /// the first Enter.
    pub fn new(opts: kb_agent::Options) -> Self {
        let skills = kb_agent::skills(&opts.cwd);
        let mut me = Self {
            agent: None,
            opts,
            items: Vec::new(),
            generation: 0,
            tail_gen: 0,
            skills,
            pick_cmd: 0,
            turn_started: None,
            input: String::new(),
            busy: false,
            session_id: None,
            model: None,
            cost_usd: None,
            limits: None,
            turns: 0,
            top: 0,
            follow: true,
            rows: Vec::new(),
            final_len: 0,
            laid_for: None,
            tail_laid: None,
            asks: VecDeque::new(),
            pick: 0,
            queue: VecDeque::new(),
            stopping: false,
        };
        me.start();
        me
    }

    fn start(&mut self) {
        match kb_agent::Agent::spawn(&self.opts) {
            Ok(agent) => self.agent = Some(agent),
            Err(e) => self.push_item(Kind::Error(e), None, false),
        }
    }

    /// A process is up.
    pub fn running(&self) -> bool {
        self.agent.is_some()
    }

    /// A turn is in flight.
    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn push(&mut self, c: char) {
        self.input.push(c);
        self.pick_cmd = 0;
    }

    /// Backspace, or Ctrl+Backspace for the word before the caret.
    pub fn backspace(&mut self, word: bool) {
        self.pick_cmd = 0;
        if !word {
            self.input.pop();
            return;
        }
        let trimmed = self.input.trim_end();
        let cut = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
        self.input.truncate(cut);
    }

    /// Pasted text. One line: the box is one row, and a newline in it
    /// would be a row nobody can see.
    pub fn paste(&mut self, text: &str) {
        let flat = text.replace("\r\n", " ").replace(['\r', '\n'], " ");
        self.input.push_str(&flat);
        self.pick_cmd = 0;
    }

    /// What `/` so far could mean: the pane's own commands, then the
    /// CLI's skills for this workspace, narrowed by what is typed. Empty
    /// once a space follows the name — from there the rest is arguments.
    pub fn completions(&self) -> Vec<Completion> {
        let Some(rest) = self.input.strip_prefix('/') else { return Vec::new() };
        if rest.contains(char::is_whitespace) {
            return Vec::new();
        }
        let needle = rest.to_lowercase();
        LOCAL_COMMANDS
            .iter()
            .map(|(name, what)| Completion { name: name.to_string(), description: what.to_string() })
            .chain(self.skills.iter().map(|s| Completion {
                name: s.name.clone(),
                description: s.description.clone(),
            }))
            .filter(|c| c.name.to_lowercase().starts_with(&needle))
            .collect()
    }

    pub fn pick_cmd(&self) -> usize {
        self.pick_cmd
    }

    /// Up and down through the completions.
    pub fn complete_move(&mut self, delta: i32) {
        let last = self.completions().len().saturating_sub(1) as i32;
        self.pick_cmd = (self.pick_cmd as i32 + delta).clamp(0, last) as usize;
    }

    /// Takes the highlighted completion into the box, with a space after
    /// it for what comes next. Says whether the box changed: an exact
    /// match already typed is not a completion, it is a command.
    pub fn complete(&mut self) -> bool {
        let list = self.completions();
        let Some(chosen) = list.get(self.pick_cmd) else { return false };
        let typed = self.input.trim_start_matches('/');
        if typed == chosen.name {
            return false;
        }
        self.input = format!("/{} ", chosen.name);
        self.pick_cmd = 0;
        true
    }

    /// Enter. A turn goes out, or waits its turn behind the one in
    /// flight; a leading slash may be one of the pane's own commands.
    pub fn send(&mut self) {
        // With the list up, Enter picks from it first — the way every
        // completion list works — and sends on the next press.
        if self.complete() {
            return;
        }
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.input.clear();
        self.follow = true;
        if let Some(rest) = text.strip_prefix('/') {
            if self.command(rest) {
                return;
            }
            // Not ours — `/compact`, a skill — so the CLI gets it as typed.
        }
        if self.busy {
            self.queue.push_back(text.clone());
            self.push_item(Kind::Queued(text), None, false);
            return;
        }
        self.send_now(text, false);
    }

    /// Sends one turn. `shown` says the transcript already has it as a
    /// queued row, which becomes the real one rather than a second copy.
    fn send_now(&mut self, text: String, shown: bool) {
        if self.agent.is_none() {
            // Stopped, or never started. The session id, when there is
            // one, brings the conversation back.
            self.opts.resume = self.session_id.clone();
            self.start();
        }
        let Some(agent) = &mut self.agent else { return };
        match agent.send(&text) {
            Ok(()) => {
                self.busy = true;
                self.turn_started = Some(std::time::Instant::now());
                if shown {
                    if let Some(item) = self.items.iter_mut().find(|i| matches!(i.kind, Kind::Queued(_))) {
                        item.kind = Kind::User(text);
                    }
                    self.generation += 1;
                } else {
                    self.push_item(Kind::User(text), None, false);
                }
            }
            Err(e) => {
                self.agent = None;
                self.push_item(Kind::Error(e), None, false);
            }
        }
    }

    /// The next queued turn, once the current one is over.
    fn send_queued(&mut self) {
        if let Some(text) = self.queue.pop_front() {
            self.send_now(text, true);
        }
    }

    /// The pane's own slash commands. Anything else is the CLI's.
    fn command(&mut self, rest: &str) -> bool {
        let mut words = rest.split_whitespace();
        let (name, arg) = (words.next().unwrap_or(""), words.next());
        match (name, arg) {
            ("mode", Some(mode)) => {
                let sent = self.agent.as_mut().map(|a| a.set_permission_mode(mode));
                self.note_control(sent, format!("permission mode \u{2192} {mode}"));
            }
            ("model", Some(model)) => {
                let sent = self.agent.as_mut().map(|a| a.set_model(model));
                if self.note_control(sent, format!("model \u{2192} {model}")) {
                    self.model = Some(model.to_string());
                }
            }
            ("stop", None) => self.cancel(),
            _ => return false,
        }
        true
    }

    /// Reports a control request's fate: a note when it went out, an
    /// error when there was nobody to send it to.
    fn note_control(&mut self, sent: Option<Result<String, String>>, said: String) -> bool {
        match sent {
            Some(Ok(_)) => {
                self.push_item(Kind::Note(said), None, false);
                true
            }
            Some(Err(e)) => {
                self.push_item(Kind::Error(e), None, false);
                false
            }
            None => {
                self.push_item(Kind::Error("claude is not running — send a message first".into()), None, false);
                false
            }
        }
    }

    /// Esc: stop a turn in flight, else clear the box.
    ///
    /// The first press asks the CLI to stop, which ends the turn cleanly
    /// and keeps the process and everything it has read. A second press
    /// while that is still pending kills the process instead; the next
    /// Enter starts another one on the same session.
    pub fn cancel(&mut self) {
        if !self.busy {
            self.input.clear();
            return;
        }
        if !self.stopping {
            if let Some(agent) = &mut self.agent {
                if agent.interrupt().is_ok() {
                    self.stopping = true;
                    self.push_item(Kind::Note("stopping\u{2026} Esc again to kill".into()), None, false);
                    return;
                }
            }
        }
        if let Some(mut agent) = self.agent.take() {
            agent.kill();
        }
        self.turn_over(true);
        self.push_item(Kind::Note("stopped".into()), None, false);
    }

    /// The turn is over, however it ended. `dead` means the process went
    /// with it: questions can no longer be answered and queued turns
    /// would go nowhere.
    fn turn_over(&mut self, dead: bool) {
        self.busy = false;
        self.stopping = false;
        self.turn_started = None;
        self.finish_drafts();
        if dead {
            self.asks.clear();
            self.pick = 0;
            if !self.queue.is_empty() {
                self.queue.clear();
                self.items.retain(|i| !matches!(i.kind, Kind::Queued(_)));
                self.push_item(Kind::Note("queued messages dropped".into()), None, false);
            }
        }
    }

    /// A question is waiting on the person.
    pub fn needs_answer(&self) -> bool {
        !self.asks.is_empty()
    }

    /// The answers on offer for the question that is up. "Always" only
    /// when the CLI proposed a rule to remember; the rule sent back is
    /// its own, not one made up here.
    pub fn answers(&self) -> &'static [&'static str] {
        match self.asks.front() {
            Some(a) if !a.suggestions.is_empty() => &["Allow", "Always allow", "Deny"],
            _ => &["Allow", "Deny"],
        }
    }

    pub fn pick(&self) -> usize {
        self.pick
    }

    /// Left and right between the answers.
    pub fn ask_move(&mut self, delta: i32) {
        let last = self.answers().len() as i32 - 1;
        self.pick = (self.pick as i32 + delta).clamp(0, last) as usize;
    }

    /// The question as the box words it: what kind of thing is being
    /// asked, and the one argument a person decides on — the command,
    /// the file, the address. `max` is the room the box has for that
    /// line; a whole script gets cut, and says so.
    pub fn ask_text(&self, max: usize) -> Option<(String, String)> {
        let ask = self.asks.front()?;
        let field = |key: &str| ask.input.get(key).and_then(kb_agent::Value::as_str).map(str::to_string);
        let relative = |p: String| {
            std::path::Path::new(&p)
                .strip_prefix(&self.opts.cwd)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or(p)
        };
        let (title, what) = match ask.tool_name.as_str() {
            "Bash" => ("Run a command?", field("command")),
            "Edit" | "MultiEdit" | "NotebookEdit" => ("Change a file?", field("file_path").map(relative)),
            "Write" => ("Write a file?", field("file_path").map(relative)),
            "WebFetch" => ("Fetch a page?", field("url")),
            "WebSearch" => ("Search the web?", field("query")),
            _ => ("Use a tool?", None),
        };
        let what = what
            .filter(|w| !w.trim().is_empty())
            .unwrap_or_else(|| format!("{}  {}", ask.tool_name, ask.description).trim().to_string());
        let whole = what.trim();
        let mut line: String = whole.lines().next().unwrap_or("").trim().chars().take(max.max(8) - 1).collect();
        if line.chars().count() < whole.chars().count() {
            line.push('\u{2026}');
        }
        Some((format!("Claude \u{b7} {title}"), line))
    }

    /// Enter on the box: the highlighted answer.
    pub fn ask_answer(&mut self) {
        let always = self.answers().len() == 3;
        let decision = match self.pick {
            0 => Decision::Allow { remember: false },
            1 if always => Decision::Allow { remember: true },
            _ => Decision::Deny,
        };
        self.decide(decision);
    }

    /// Esc on the box. A question cannot just be dropped — the turn
    /// behind it waits until it hears something — so walking away is a no.
    pub fn ask_deny(&mut self) {
        self.decide(Decision::Deny);
    }

    fn decide(&mut self, decision: Decision) {
        self.pick = 0;
        let Some(ask) = self.asks.pop_front() else { return };
        let said = match &decision {
            Decision::Allow { remember: true } => "allowed \u{b7} rule saved",
            Decision::Allow { remember: false } => "allowed",
            Decision::Deny => "denied",
        };
        self.push_item(Kind::Note(said.into()), None, false);
        if let Some(agent) = &mut self.agent {
            if let Err(e) = agent.respond(&ask, &decision) {
                self.push_item(Kind::Error(e), None, false);
            }
        }
    }

    /// Drains the process. Called from the tick.
    pub fn poll(&mut self) -> Poll {
        let mut out = Poll::default();
        let Some(agent) = &mut self.agent else { return out };
        let events = agent.poll();
        for event in events {
            out.changed = true;
            self.apply(event, &mut out);
        }
        out
    }

    /// One event into the transcript. Public so the tests can feed the
    /// parser's output through without a process.
    pub fn apply(&mut self, event: Event, out: &mut Poll) {
        match event {
            Event::Init { session_id, model } => {
                self.session_id = Some(session_id);
                self.model = Some(model);
            }
            Event::BlockStart { message_id, block, .. } => {
                let kind = match block {
                    Block::Text(t) => Kind::Text(t),
                    Block::ToolUse { id, name, input } => Kind::Tool {
                        summary: summarize(&name, &input),
                        id,
                        name,
                        result: None,
                    },
                    Block::Thinking => Kind::Thinking,
                };
                self.push_draft(kind, message_id);
            }
            Event::TextDelta { message_id, text, .. } => {
                // The last provisional text of that message. Falling back
                // to a fresh item covers a delta whose start was missed.
                let found = self.items.iter_mut().rev().find(|i| {
                    i.provisional
                        && i.message_id.as_deref() == Some(message_id.as_str())
                        && matches!(i.kind, Kind::Text(_))
                });
                match found {
                    Some(Item { kind: Kind::Text(t), .. }) => {
                        t.push_str(&text);
                        self.tail_gen += 1;
                    }
                    _ => self.push_draft(Kind::Text(text), message_id),
                }
            }
            Event::Assistant { message_id, blocks } => {
                // The finished message replaces its draft. Tool inputs are
                // complete here where the draft's were empty, which is
                // what the summary and the file path need.
                // What the drafts had let out so far carries over to the
                // finished texts, in order, or the last words of every
                // answer would land in one jump.
                let mut let_out: VecDeque<usize> = self
                    .items
                    .iter()
                    .filter(|i| i.provisional && i.message_id.as_deref() == Some(message_id.as_str()))
                    .filter_map(|i| match i.kind {
                        Kind::Text(_) => Some(i.revealed.unwrap_or(0)),
                        _ => None,
                    })
                    .collect();
                self.items
                    .retain(|i| !(i.provisional && i.message_id.as_deref() == Some(message_id.as_str())));
                for block in blocks {
                    let (kind, revealed) = match block {
                        Block::Text(t) if t.trim().is_empty() => continue,
                        Block::Text(t) => {
                            let seen = let_out.pop_front().unwrap_or(0).min(t.chars().count());
                            (Kind::Text(t), Some(seen))
                        }
                        Block::ToolUse { id, name, input } => (
                            Kind::Tool {
                                summary: summarize(&name, &input),
                                id,
                                name,
                                result: None,
                            },
                            None,
                        ),
                        Block::Thinking => (Kind::Thinking, None),
                    };
                    self.items.push(Item { kind, message_id: Some(message_id.clone()), provisional: false, revealed });
                    self.generation += 1;
                }
            }
            Event::ToolResult { tool_use_id, is_error, text } => {
                let first = text
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
                    .to_string();
                let mut edited = false;
                for item in self.items.iter_mut().rev() {
                    if let Kind::Tool { id, name, result, .. } = &mut item.kind {
                        if *id == tool_use_id {
                            *result = Some((is_error, clip(&first)));
                            edited = !is_error && writes_files(name);
                            break;
                        }
                    }
                }
                out.edited |= edited;
                self.generation += 1;
            }
            Event::Result { is_error, cost_usd, turns, duration_ms, text } => {
                let stopped = self.stopping;
                self.turn_over(false);
                if cost_usd.is_some() {
                    self.cost_usd = cost_usd;
                }
                self.turns = turns;
                if stopped {
                    self.push_item(Kind::Note("stopped".into()), None, false);
                } else if is_error {
                    // The wall was already announced when the limit event
                    // came in; the CLI's own sentence about it is the
                    // last assistant text, so this row only says why.
                    let what = if self.limits.as_ref().is_some_and(|l| l.rejected()) {
                        "usage limit".to_string()
                    } else {
                        text.unwrap_or_else(|| "failed".into())
                    };
                    self.push_item(Kind::Error(format!("turn ended: {what}")), None, false);
                } else {
                    let secs = duration_ms as f64 / 1000.0;
                    self.push_item(Kind::Note(format!("{secs:.1}s")), None, false);
                }
                self.send_queued();
            }
            Event::Permission(ask) => self.asks.push_back(ask),
            Event::Control { error: Some(e), .. } => {
                self.push_item(Kind::Error(format!("claude refused: {e}")), None, false);
            }
            Event::Control { error: None, .. } => {}
            Event::RateLimit(limits) => {
                // Said once in the transcript when the wall is hit, so the
                // turn that dies right after has a visible reason.
                if limits.rejected() && !self.limits.as_ref().is_some_and(|l| l.rejected()) {
                    let when = limits
                        .five_hour
                        .map(|w| format!(" \u{b7} resets in {}", in_time(w.resets_at, now())))
                        .unwrap_or_default();
                    self.push_item(Kind::Error(format!("usage limit reached{when}")), None, false);
                }
                self.limits = Some(limits);
            }
            Event::Stderr(line) => self.push_item(Kind::Error(line), None, false),
            Event::Exited(_) => {
                self.agent = None;
                let mid_turn = self.busy && !self.stopping;
                self.turn_over(true);
                if mid_turn {
                    self.push_item(Kind::Error("claude exited mid-turn".into()), None, false);
                }
            }
        }
    }

    fn push_item(&mut self, kind: Kind, message_id: Option<String>, provisional: bool) {
        self.items.push(Item { kind, message_id, provisional, revealed: None });
        self.generation += 1;
    }

    /// A streaming item. Drafts only ever sit at the end, so adding one
    /// touches the tail and nothing settled. Text starts with nothing let
    /// out; the frames let it out from there.
    fn push_draft(&mut self, kind: Kind, message_id: String) {
        let revealed = matches!(kind, Kind::Text(_)).then_some(0);
        self.items.push(Item { kind, message_id: Some(message_id), provisional: true, revealed });
        self.tail_gen += 1;
    }

    /// One frame of letting streamed text out. Says whether anything
    /// moved, which is whether a repaint is owed.
    pub fn animate(&mut self) -> bool {
        let mut moved = false;
        let mut settled = false;
        for item in &mut self.items {
            let (Some(n), Kind::Text(t)) = (item.revealed, &item.kind) else { continue };
            let total = t.chars().count();
            if n >= total {
                // All out. A draft stays a draft until its message lands;
                // a finished one is settled from here, which moves it out
                // of the tail and into the rows laid out once.
                if !item.provisional {
                    item.revealed = None;
                    settled = true;
                }
                continue;
            }
            let step = REVEAL_FLOOR.max((total - n) / REVEAL_SHARE);
            item.revealed = Some((n + step).min(total));
            moved = true;
        }
        if settled {
            self.generation += 1;
        }
        if moved {
            self.tail_gen += 1;
        }
        moved || settled
    }

    /// Nothing is streaming any more; what is there is what there is.
    fn finish_drafts(&mut self) {
        for item in &mut self.items {
            item.provisional = false;
        }
        self.generation += 1;
    }

    /// The transcript wrapped to `cols`. The settled items are rewrapped
    /// when one of them changes or the width does; the streaming tail on
    /// every token, which is a line or two of work rather than the whole
    /// conversation's.
    pub fn rows(&mut self, cols: usize) -> &[Row] {
        let cols = cols.max(8);
        let n_final = self.items.iter().position(Item::unsettled).unwrap_or(self.items.len());
        if self.laid_for != Some((self.generation, cols)) {
            self.rows = layout(&self.items[..n_final], cols, false);
            self.final_len = self.rows.len();
            self.laid_for = Some((self.generation, cols));
            self.tail_laid = None;
        }
        if self.tail_laid != Some((self.generation, self.tail_gen, cols)) {
            self.rows.truncate(self.final_len);
            let tail = layout(&self.items[n_final..], cols, self.final_len > 0);
            self.rows.extend(tail);
            self.tail_laid = Some((self.generation, self.tail_gen, cols));
        }
        &self.rows
    }

    /// Seconds the turn in flight has been running.
    pub fn busy_secs(&self) -> Option<u64> {
        self.turn_started.filter(|_| self.busy).map(|t| t.elapsed().as_secs())
    }

    /// Follows the bottom while that is wanted; clamps otherwise. Called
    /// from drawing, which knows how many rows fit.
    pub fn ensure_visible(&mut self, visible: usize) {
        let max_top = self.rows.len().saturating_sub(visible.max(1));
        if self.follow {
            self.top = max_top;
        } else {
            self.top = self.top.min(max_top);
        }
    }

    /// Wheel and keys. Positive is up, like every other pane.
    pub fn scroll(&mut self, delta: i32, visible: usize) {
        let max_top = self.rows.len().saturating_sub(visible.max(1));
        let next = (self.top as i32 - delta).clamp(0, max_top as i32) as usize;
        self.top = next;
        // Back at the end means back to following it.
        self.follow = next >= max_top;
    }

    /// The pane's title. The model is known only once the CLI has
    /// answered something — the session starts on the first message, not
    /// when the process does — so before that the title is just the name.
    pub fn header(&self) -> String {
        match (&self.model, self.running()) {
            (Some(m), true) => format!("CLAUDE \u{b7} {m}"),
            (Some(m), false) => format!("CLAUDE \u{b7} {m} \u{b7} stopped"),
            (None, _) => "CLAUDE".to_string(),
        }
    }

    /// The right-hand hint: what is happening, then what to press.
    ///
    /// On a subscription the turns come out of the plan's windows, so the
    /// hint shows how much of each is spent and when it starts over. The
    /// dollar figure the CLI reports is an estimate at API prices and
    /// would be a bill nobody gets — it shows only with no windows in
    /// sight (an API key) or once usage has gone into paid overage.
    pub fn hint(&self) -> String {
        self.hint_at(now())
    }

    fn hint_at(&self, now: u64) -> String {
        let mut parts = Vec::new();
        match &self.limits {
            Some(l) => {
                if let Some(w) = l.five_hour {
                    parts.push(format!("5h {:.0}%", w.utilization * 100.0));
                }
                if let Some(w) = l.seven_day {
                    parts.push(format!("7d {:.0}%", w.utilization * 100.0));
                }
                if let Some(w) = l.five_hour.filter(|w| w.resets_at > now) {
                    parts.push(format!("resets in {}", in_time(w.resets_at, now)));
                }
                if l.using_overage {
                    if let Some(cost) = self.cost_usd {
                        parts.push(format!("overage ${cost:.2}"));
                    }
                }
            }
            None => {
                if let Some(cost) = self.cost_usd {
                    parts.push(format!("${cost:.3}"));
                }
            }
        }
        if self.turns > 0 {
            let plural = if self.turns == 1 { "turn" } else { "turns" };
            parts.push(format!("{} {plural}", self.turns));
        }
        let state = if self.needs_answer() {
            "waiting for your answer".to_string()
        } else if self.stopping {
            "stopping\u{2026}".to_string()
        } else if self.busy {
            // A count that moves says the turn is alive while the model
            // thinks; a still "working" reads as a hang.
            match self.busy_secs() {
                Some(s) => format!("working {s}s \u{b7} Esc stops"),
                None => "working\u{2026} Esc stops".to_string(),
            }
        } else {
            "Enter sends".to_string()
        };
        parts.push(state);
        parts.join(" \u{b7} ")
    }
}

/// Unix seconds now.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "2h13m", "48m", "under a minute" — how long until `at`.
fn in_time(at: u64, now: u64) -> String {
    let secs = at.saturating_sub(now);
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    match (h, m) {
        (0, 0) => "under a minute".to_string(),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h{m:02}m"),
    }
}

/// Tools that change the working tree, whose success means open editors
/// may be stale.
fn writes_files(name: &str) -> bool {
    matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

/// One line for a tool call: the argument a person would want to see.
fn summarize(name: &str, input: &kb_agent::Value) -> String {
    let field = |key: &str| input.get(key).and_then(kb_agent::Value::as_str);
    let arg = match name {
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => field("file_path"),
        "Bash" => field("command"),
        "Grep" | "Glob" => field("pattern"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        "Task" | "Agent" => field("description"),
        _ => None,
    };
    let arg = match arg {
        Some(a) => a.to_string(),
        None if input.is_object() && !input.as_object().is_some_and(|o| o.is_empty()) => {
            input.to_string()
        }
        None => String::new(),
    };
    // First line only: a multi-line command is a script, and the row is
    // a label.
    let arg = arg.lines().next().unwrap_or("").trim();
    if arg.is_empty() {
        name.to_string()
    } else {
        clip(&format!("{name}  {arg}"))
    }
}

fn clip(s: &str) -> String {
    if s.chars().count() <= SUMMARY_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(SUMMARY_CHARS - 1).collect();
    out.push('\u{2026}');
    out
}

/// Greedy word wrap to `cols` characters. Breaks after the last space that
/// fits; a word longer than a row is split, because a row that runs off
/// the pane is a row nobody can read.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut start = 0;
        while start < chars.len() {
            let end = (start + cols).min(chars.len());
            if end == chars.len() {
                out.push(chars[start..end].iter().collect());
                break;
            }
            // A space at `end` means the row fits exactly; otherwise look
            // back for one.
            let cut = if chars[end] == ' ' {
                Some(end)
            } else {
                chars[start..end].iter().rposition(|c| *c == ' ').map(|i| start + i)
            };
            match cut {
                Some(sp) if sp > start => {
                    out.push(chars[start..sp].iter().collect());
                    start = sp + 1;
                }
                _ => {
                    out.push(chars[start..end].iter().collect());
                    start = end;
                }
            }
            // Leading spaces on the continuation are the break, not text.
            while start < chars.len() && chars[start] == ' ' {
                start += 1;
            }
        }
    }
    out
}

/// A slash command on offer while `/` is being typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub name: String,
    pub description: String,
}

/// The pane's own slash commands, never sent to the CLI.
const LOCAL_COMMANDS: &[(&str, &str)] = &[
    ("mode", "permission mode for this session: default, acceptEdits, plan, bypassPermissions"),
    ("model", "the model for the turns from here on"),
    ("stop", "stop the turn in flight"),
];

/// The transcript as rows: a blank line between entries, a tool's result
/// tucked under it. `gap_first` puts a blank line before the first entry
/// too, for a tail that continues settled rows above it.
fn layout(items: &[Item], cols: usize, gap_first: bool) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut first = !gap_first;
    let push = |rows: &mut Vec<Row>, text: &str, prefix: &str, style: Style, volatile: bool| {
        let width = cols.saturating_sub(prefix.chars().count()).max(1);
        for (i, line) in wrap(text, width).into_iter().enumerate() {
            let lead = if i == 0 { prefix.to_string() } else { " ".repeat(prefix.chars().count()) };
            rows.push(Row { text: format!("{lead}{line}"), style, volatile });
        }
    };
    for item in items {
        // Space between entries, except a result under its tool call.
        if !first {
            rows.push(Row { text: String::new(), style: Style::Note, volatile: false });
        }
        first = false;
        let v = item.unsettled();
        match &item.kind {
            Kind::User(t) => push(&mut rows, t, "\u{203a} ", Style::User, v),
            // Faint until it goes out, so a message that has not been
            // heard yet does not read as one that has.
            Kind::Queued(t) => push(&mut rows, t, "\u{203a} ", Style::Note, v),
            Kind::Text(t) => push(&mut rows, &item.shown(t), "", Style::Text, v),
            Kind::Tool { summary, result, .. } => {
                push(&mut rows, summary, "\u{2022} ", Style::Tool, v);
                if let Some((is_error, text)) = result {
                    if !text.is_empty() {
                        let style = if *is_error { Style::ToolError } else { Style::ToolResult };
                        push(&mut rows, text, "  \u{b7} ", style, false);
                    }
                }
            }
            Kind::Thinking => push(&mut rows, "thinking", "\u{2022} ", Style::Note, v),
            Kind::Note(t) => push(&mut rows, t, "", Style::Note, v),
            Kind::Error(t) => push(&mut rows, t, "", Style::Error, v),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_agent::json;

    fn pane() -> AgentPane {
        // No process: the tests feed events by hand.
        AgentPane {
            agent: None,
            opts: kb_agent::Options {
                command: String::new(),
                cwd: std::path::PathBuf::from("C:\\proj"),
                model: None,
                permission_mode: String::new(),
                allowed_tools: Vec::new(),
                resume: None,
            },
            items: Vec::new(),
            generation: 0,
            tail_gen: 0,
            skills: vec![kb_agent::Skill { name: "review".into(), description: "Review the diff".into() }],
            pick_cmd: 0,
            turn_started: None,
            input: String::new(),
            busy: true,
            session_id: None,
            model: None,
            cost_usd: None,
            limits: None,
            turns: 0,
            top: 0,
            follow: true,
            rows: Vec::new(),
            final_len: 0,
            laid_for: None,
            tail_laid: None,
            asks: VecDeque::new(),
            pick: 0,
            queue: VecDeque::new(),
            stopping: false,
        }
    }

    #[test]
    fn streamed_text_is_let_out_a_few_characters_a_frame() {
        let mut p = pane();
        feed(
            &mut p,
            vec![
                Event::BlockStart { message_id: "m".into(), index: 0, block: Block::Text(String::new()) },
                Event::TextDelta { message_id: "m".into(), index: 0, text: "a burst of text".into() },
            ],
        );
        let shown = |p: &mut AgentPane| p.rows(80).last().map(|r| r.text.clone()).unwrap_or_default();
        assert_eq!(shown(&mut p), "", "nothing is out before the first frame");
        assert!(p.animate());
        let first = shown(&mut p);
        assert!(!first.is_empty() && first.len() < "a burst of text".len(), "{first:?}");

        // The finished message keeps the count rather than jumping to the end.
        feed(&mut p, vec![Event::Assistant { message_id: "m".into(), blocks: vec![Block::Text("a burst of text".into())] }]);
        assert_eq!(shown(&mut p), first);
        let mut frames = 0;
        while p.animate() {
            frames += 1;
            assert!(frames < 100, "must finish letting the text out");
        }
        assert_eq!(shown(&mut p), "a burst of text");
        assert!(p.items[0].revealed.is_none() && !p.items[0].provisional, "settled once it is all out");
    }

    #[test]
    fn streaming_rewraps_only_the_tail() {
        let mut p = pane();
        p.push_item(Kind::User("first".into()), None, false);
        p.rows(40);
        let settled = p.laid_for;
        feed(
            &mut p,
            vec![
                Event::BlockStart { message_id: "m".into(), index: 0, block: Block::Text(String::new()) },
                Event::TextDelta { message_id: "m".into(), index: 0, text: "hello".into() },
            ],
        );
        while p.animate() {}
        let rows: Vec<String> = p.rows(40).iter().map(|r| r.text.clone()).collect();
        assert_eq!(rows, vec!["\u{203a} first", "", "hello"]);
        assert_eq!(p.laid_for, settled, "the settled rows were not laid out again");

        feed(&mut p, vec![Event::TextDelta { message_id: "m".into(), index: 0, text: " there".into() }]);
        while p.animate() {}
        let rows: Vec<String> = p.rows(40).iter().map(|r| r.text.clone()).collect();
        assert_eq!(rows, vec!["\u{203a} first", "", "hello there"]);
    }

    #[test]
    fn slash_lists_the_panes_commands_and_the_skills() {
        let mut p = pane();
        p.busy = false;
        for c in "/".chars() {
            p.push(c);
        }
        let names: Vec<String> = p.completions().into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["mode", "model", "stop", "review"]);

        p.push('m');
        p.push('o');
        assert_eq!(p.completions().len(), 2);
        p.complete_move(1);
        assert!(p.complete());
        assert_eq!(p.input(), "/model ");
        assert!(p.completions().is_empty(), "arguments are not completed");

        // Enter on a partial name completes rather than sending.
        p.input = "/rev".into();
        p.send();
        assert_eq!(p.input(), "/review ");
        assert!(p.queue.is_empty());
    }

    fn ask(id: &str) -> Permission {
        Permission {
            request_id: id.into(),
            tool_name: "Bash".into(),
            input: json!({"command": "cargo test"}),
            description: "cargo test".into(),
            suggestions: Vec::new(),
            tool_use_id: "t".into(),
        }
    }

    #[test]
    fn questions_are_answered_in_order_with_the_highlighted_answer() {
        let mut p = pane();
        let mut b = ask("b");
        b.suggestions.push(json!({"type": "addRules"}));
        feed(&mut p, vec![Event::Permission(ask("a")), Event::Permission(b)]);
        assert!(p.needs_answer());
        assert_eq!(p.answers(), &["Allow", "Deny"], "no rule offered, no always");
        assert_eq!(p.ask_text(80).map(|(t, _)| t), Some("Claude \u{b7} Run a command?".into()));

        p.ask_move(5);
        assert_eq!(p.pick(), 1, "clamped to the last answer");
        p.ask_answer();
        assert_eq!(p.items.last().map(|i| &i.kind), Some(&Kind::Note("denied".into())));

        assert_eq!(p.answers().len(), 3, "a rule was offered for the second");
        assert_eq!(p.pick(), 0, "the highlight starts over per question");
        p.ask_move(1);
        p.ask_answer();
        assert_eq!(p.items.last().map(|i| &i.kind), Some(&Kind::Note("allowed \u{b7} rule saved".into())));
        assert!(!p.needs_answer());
    }

    #[test]
    fn the_question_line_names_the_argument_and_cuts_a_script() {
        let mut p = pane();
        let mut a = ask("a");
        a.input = json!({"command": "cargo test --all\ncargo clippy"});
        feed(&mut p, vec![Event::Permission(a)]);
        assert_eq!(p.ask_text(80).map(|(_, l)| l), Some("cargo test --all\u{2026}".into()));
        assert_eq!(p.ask_text(10).map(|(_, l)| l), Some("cargo tes\u{2026}".into()));
        p.ask_deny();

        let mut e = ask("e");
        e.tool_name = "Edit".into();
        e.input = json!({"file_path": p.opts.cwd.join("src").join("main.rs").to_string_lossy()});
        feed(&mut p, vec![Event::Permission(e)]);
        assert_eq!(p.ask_text(80), Some(("Claude \u{b7} Change a file?".into(), "src/main.rs".into())));
    }

    #[test]
    fn a_message_typed_mid_turn_waits_and_then_goes_out() {
        let mut p = pane();
        p.input = "next thing".into();
        p.send();
        assert!(p.input.is_empty());
        assert_eq!(p.queue.len(), 1);
        assert!(matches!(p.items.last().map(|i| &i.kind), Some(Kind::Queued(_))));

        // The turn ends; with no process to send to, the queued row is
        // still the one that would have gone out — no copy of it.
        feed(&mut p, vec![Event::Result { is_error: false, cost_usd: None, turns: 1, duration_ms: 1, text: None }]);
        assert!(p.queue.is_empty());
        assert_eq!(p.items.iter().filter(|i| matches!(i.kind, Kind::Queued(_) | Kind::User(_))).count(), 1);
    }

    #[test]
    fn a_stop_that_the_cli_confirms_reads_as_stopped_not_failed() {
        let mut p = pane();
        p.stopping = true;
        feed(
            &mut p,
            vec![Event::Result { is_error: true, cost_usd: None, turns: 1, duration_ms: 1, text: Some("error_during_execution".into()) }],
        );
        assert!(!p.busy() && !p.stopping);
        assert_eq!(p.items.last().map(|i| &i.kind), Some(&Kind::Note("stopped".into())));
    }

    #[test]
    fn the_panes_own_commands_do_not_reach_the_cli() {
        let mut p = pane();
        p.busy = false;
        p.input = "/mode plan".into();
        p.send();
        // No process: the command is reported as unsendable rather than
        // queued or typed into the transcript as a turn.
        assert!(matches!(p.items.last().map(|i| &i.kind), Some(Kind::Error(_))));
        assert!(!p.items.iter().any(|i| matches!(i.kind, Kind::User(_) | Kind::Queued(_))));

        p.busy = true;
        p.input = "/compact".into();
        p.send();
        assert_eq!(p.queue.front().map(String::as_str), Some("/compact"), "the CLI's commands pass through");
    }

    fn feed(p: &mut AgentPane, events: Vec<Event>) -> Poll {
        let mut out = Poll::default();
        for e in events {
            p.apply(e, &mut out);
        }
        out
    }

    #[test]
    fn streamed_text_becomes_one_entry_that_the_finished_message_replaces() {
        let mut p = pane();
        feed(
            &mut p,
            vec![
                Event::BlockStart { message_id: "m1".into(), index: 0, block: Block::Text(String::new()) },
                Event::TextDelta { message_id: "m1".into(), index: 0, text: "Hel".into() },
                Event::TextDelta { message_id: "m1".into(), index: 0, text: "lo".into() },
            ],
        );
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].kind, Kind::Text("Hello".into()));
        assert!(p.items[0].provisional);
        while p.animate() {}

        feed(
            &mut p,
            vec![Event::Assistant { message_id: "m1".into(), blocks: vec![Block::Text("Hello".into())] }],
        );
        assert_eq!(p.items.len(), 1, "the draft is replaced, not duplicated");
        assert!(!p.items[0].provisional);
    }

    #[test]
    fn a_tool_call_gets_its_arguments_and_then_its_result() {
        let mut p = pane();
        let out = feed(
            &mut p,
            vec![
                Event::BlockStart {
                    message_id: "m1".into(),
                    index: 0,
                    block: Block::ToolUse { id: "t1".into(), name: "Edit".into(), input: json!({}) },
                },
                Event::Assistant {
                    message_id: "m1".into(),
                    blocks: vec![Block::ToolUse {
                        id: "t1".into(),
                        name: "Edit".into(),
                        input: json!({"file_path": "C:\\proj\\src\\main.rs"}),
                    }],
                },
                Event::ToolResult {
                    tool_use_id: "t1".into(),
                    is_error: false,
                    text: "The file has been updated.\nmore".into(),
                },
            ],
        );
        assert_eq!(p.items.len(), 1);
        match &p.items[0].kind {
            Kind::Tool { summary, result, .. } => {
                assert_eq!(summary, "Edit  C:\\proj\\src\\main.rs");
                assert_eq!(result, &Some((false, "The file has been updated.".to_string())));
            }
            other => panic!("expected a tool row, got {other:?}"),
        }
        assert!(out.edited, "a successful Edit means open files may be stale");
    }

    #[test]
    fn a_failed_edit_and_a_read_do_not_count_as_edits() {
        let mut p = pane();
        let out = feed(
            &mut p,
            vec![
                Event::Assistant {
                    message_id: "m1".into(),
                    blocks: vec![
                        Block::ToolUse { id: "t1".into(), name: "Read".into(), input: json!({"file_path": "x"}) },
                        Block::ToolUse { id: "t2".into(), name: "Write".into(), input: json!({"file_path": "y"}) },
                    ],
                },
                Event::ToolResult { tool_use_id: "t1".into(), is_error: false, text: "1: fn".into() },
                Event::ToolResult { tool_use_id: "t2".into(), is_error: true, text: "denied".into() },
            ],
        );
        assert!(!out.edited);
    }

    #[test]
    fn the_result_ends_the_turn_and_keeps_the_running_cost() {
        let mut p = pane();
        feed(
            &mut p,
            vec![Event::Result {
                is_error: false,
                cost_usd: Some(0.02),
                turns: 2,
                duration_ms: 3400,
                text: Some("done".into()),
            }],
        );
        assert!(!p.busy());
        assert_eq!(p.cost_usd, Some(0.02));
        assert_eq!(p.turns, 2);
        assert_eq!(p.items.last().map(|i| &i.kind), Some(&Kind::Note("3.4s".into())));
    }

    #[test]
    fn a_subscription_sees_its_windows_and_never_a_price() {
        use kb_agent::{Limits, Window};
        let mut p = pane();
        feed(
            &mut p,
            vec![
                Event::RateLimit(Limits {
                    status: "allowed".into(),
                    five_hour: Some(Window { utilization: 0.64, resets_at: 10_000 }),
                    seven_day: Some(Window { utilization: 0.1, resets_at: 20_000 }),
                    using_overage: false,
                }),
                Event::Result { is_error: false, cost_usd: Some(0.27), turns: 1, duration_ms: 10, text: None },
            ],
        );
        let hint = p.hint_at(2_020);
        assert_eq!(hint, "5h 64% \u{b7} 7d 10% \u{b7} resets in 2h13m \u{b7} 1 turn \u{b7} Enter sends");
        assert!(!hint.contains('$'), "{hint}");
    }

    #[test]
    fn an_api_key_sees_the_price_it_pays() {
        let mut p = pane();
        feed(
            &mut p,
            vec![Event::Result { is_error: false, cost_usd: Some(0.27), turns: 1, duration_ms: 10, text: None }],
        );
        assert!(p.hint_at(0).starts_with("$0.270"));
    }

    #[test]
    fn hitting_the_wall_is_said_once() {
        use kb_agent::{Limits, Window};
        let mut p = pane();
        let hit = Limits {
            status: "rejected".into(),
            five_hour: Some(Window { utilization: 1.0, resets_at: 3_600 }),
            seven_day: None,
            using_overage: false,
        };
        feed(&mut p, vec![Event::RateLimit(hit.clone()), Event::RateLimit(hit)]);
        let errors = p.items.iter().filter(|i| matches!(i.kind, Kind::Error(_))).count();
        assert_eq!(errors, 1);
    }

    #[test]
    fn time_until_reads_like_a_person_says_it() {
        assert_eq!(in_time(100, 90), "under a minute");
        assert_eq!(in_time(60 * 48, 0), "48m");
        assert_eq!(in_time(3600 * 2 + 60 * 5, 0), "2h05m");
        assert_eq!(in_time(5, 10), "under a minute");
    }

    #[test]
    fn an_exit_mid_turn_is_said_out_loud() {
        let mut p = pane();
        feed(&mut p, vec![Event::Exited(None)]);
        assert!(!p.busy());
        assert!(matches!(p.items.last().map(|i| &i.kind), Some(Kind::Error(_))));
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_splits_long_words() {
        assert_eq!(wrap("the quick brown fox", 9), vec!["the quick", "brown fox"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
        assert_eq!(wrap("fits", 4), vec!["fits"]);
    }

    #[test]
    fn rows_indent_continuations_under_their_prefix() {
        let items = vec![Item {
            kind: Kind::User("one two three four".into()),
            message_id: None,
            provisional: false,
            revealed: None,
        }];
        let rows = layout(&items, 12, false);
        assert_eq!(rows[0].text, "\u{203a} one two");
        assert_eq!(rows[1].text, "  three four");
        assert!(rows.iter().all(|r| r.style == Style::User));
    }

    #[test]
    fn following_the_bottom_stops_when_the_user_scrolls_up() {
        let mut p = pane();
        for i in 0..30 {
            p.push_item(Kind::Note(format!("{i}")), None, false);
        }
        p.rows(40);
        p.ensure_visible(10);
        let bottom = p.top;
        assert!(bottom > 0);

        p.scroll(3, 10);
        assert!(!p.follow);
        p.push_item(Kind::Note("new".into()), None, false);
        p.rows(40);
        p.ensure_visible(10);
        assert_eq!(p.top, bottom - 3, "new rows must not yank the view back down");

        p.scroll(-100, 10);
        assert!(p.follow, "scrolling to the end resumes following");
    }

    #[test]
    fn the_summary_names_the_argument_a_person_wants() {
        assert_eq!(summarize("Bash", &json!({"command": "cargo test\n--all"})), "Bash  cargo test");
        assert_eq!(summarize("Grep", &json!({"pattern": "fn main"})), "Grep  fn main");
        assert_eq!(summarize("Read", &json!({})), "Read");
        assert_eq!(summarize("Odd", &json!({"k": 1})), "Odd  {\"k\":1}");
    }
}
