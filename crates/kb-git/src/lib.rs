//! Git status, read by running `git`.
//!
//! Two things drive the design.
//!
//! `git status` takes long enough on a large repository to be felt, so it runs
//! on a background thread and the UI reads whatever the last completed run
//! produced. Stale status for a moment is fine; a stalled window is not.
//!
//! And every `git` invocation has to suppress the console window. This is a
//! `windows_subsystem = "windows"` process, so without CREATE_NO_WINDOW a black
//! console flashes on screen every refresh.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

/// What happened to a file, collapsed to what a file tree can show.
///
/// Git's real state is two characters, index and worktree, and 30-odd
/// combinations. An explorer needs a color, not a full state machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Changed, staged or not.
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    /// Unmerged. Loud on purpose: it needs a decision, not a glance.
    Conflicted,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub branch: Option<String>,
    /// Absolute path to status. Absolute because the explorer works in
    /// absolute paths and converting on every row would be silly.
    pub files: HashMap<PathBuf, Status>,
    /// Directories containing something changed. Precomputed so a collapsed
    /// folder can be marked without walking its children per frame.
    pub dirs: HashMap<PathBuf, Status>,
}

impl Snapshot {
    pub fn status_of(&self, path: &Path) -> Option<Status> {
        self.files.get(path).or_else(|| self.dirs.get(path)).copied()
    }
}

/// What happened to one line of a file, for an editor gutter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineChange {
    Added,
    Modified,
    /// Lines vanished here. Carried by the line *after* the gap, because the
    /// gap itself has no line to carry it.
    Deleted,
}

/// One search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// Zero-based, ready for a buffer position. git counts from one.
    pub line: usize,
    pub text: String,
}

/// One row of the git panel's file list.
///
/// A file changed in both the index and the worktree appears twice — once
/// staged, once not — because those really are two different changes, and
/// staging the second half is exactly what the panel is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Absolute, for acting on the file.
    pub path: PathBuf,
    /// Repo-relative with forward slashes, for reading.
    pub rel: String,
    pub status: Status,
    pub staged: bool,
}

/// What kind of line a diff line is, for colouring and nothing else.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind {
    /// File headers, index lines — the plumbing between hunks.
    Meta,
    /// `@@ … @@`, the coordinates.
    Hunk,
    Add,
    Del,
    Context,
}

/// One commit in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
    /// Relative — "3 days ago" — because that is how people think about
    /// their own recent history.
    pub when: String,
    pub author: String,
}

/// A remote operation the panel can start. Its own type so the panel cannot
/// ask for an arbitrary git command by string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RemoteOp {
    Push,
    /// `--ff-only`: a pull that would need a merge is a decision, and this
    /// panel is not the place it should be made silently.
    Pull,
}

pub struct Git {
    root: Option<PathBuf>,
    snapshot: Snapshot,
    /// A run in flight. `Some` means one is already going, which is how a
    /// refresh request while busy is dropped instead of piling up.
    pending: Option<Receiver<Snapshot>>,
    /// A remote operation in flight: its name and the channel its result
    /// arrives on. One at a time — two concurrent pushes help nobody, and
    /// the second would sit on the first one's index lock anyway.
    op: Option<(&'static str, Receiver<Result<String, String>>)>,
}

impl Git {
    /// Finds the repository containing `dir`. Not being in one is normal, not
    /// an error: kubide has to open any folder.
    pub fn discover(dir: &Path) -> Self {
        let root = run(dir, &["rev-parse", "--show-toplevel"])
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| p.exists());
        let mut me = Self {
            root,
            snapshot: Snapshot::default(),
            pending: None,
            op: None,
        };
        me.refresh();
        me
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn is_repo(&self) -> bool {
        self.root.is_some()
    }

    /// Every file git knows about, tracked or not, honouring .gitignore.
    ///
    /// This is why the finder asks git first: walking the directory would
    /// descend into `target/` and hand back thousands of build artefacts, and
    /// reimplementing .gitignore to avoid that is a project of its own.
    pub fn list_files(&self) -> Option<Vec<PathBuf>> {
        let root = self.root.as_ref()?;
        let out = run(
            root,
            &["ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        )?;
        Some(
            out.split('\0')
                .filter(|p| !p.is_empty())
                .map(|p| root.join(p))
                .collect(),
        )
    }

    /// Literal search across the repository, honouring .gitignore.
    ///
    /// `-F` because the pattern is what the user typed, not a regex: someone
    /// searching for `foo(` should find `foo(`, not a syntax error. `-I`
    /// skips binaries, which are noise at best and megabytes of garbage at
    /// worst.
    pub fn grep(&self, pattern: &str, limit: usize) -> Option<Vec<Hit>> {
        let root = self.root.as_ref()?;
        let out = run(
            root,
            &[
                "grep",
                "--no-color",
                "-I",
                "-n",
                "-F",
                "--untracked",
                "-e",
                pattern,
            ],
        )?;
        Some(
            out.lines()
                .filter_map(|line| {
                    // path:line:text — and the path may itself contain a colon
                    // on a drive letter, so split from the left, twice.
                    let (path, rest) = line.split_once(':')?;
                    let (number, text) = rest.split_once(':')?;
                    Some(Hit {
                        path: root.join(path),
                        line: number.parse::<usize>().ok()?.saturating_sub(1),
                        text: text.trim_end().to_string(),
                    })
                })
                .take(limit)
                .collect(),
        )
    }

    /// The changed files, split into staged and unstaged rows.
    ///
    /// Synchronous, unlike the tree's snapshot: the panel runs this when it
    /// opens or when the user acts, not sixty times a second, and acting on
    /// a list that is even two seconds stale means staging the wrong thing.
    pub fn entries(&self) -> Vec<Entry> {
        let Some(root) = &self.root else { return Vec::new() };
        let out = run(root, &["status", "--porcelain=v1", "-z", "--untracked-files=normal"]);
        parse_entries(&out.unwrap_or_default(), root)
    }

    /// `git add` one path. The error is git's own words: "could not stage"
    /// with the reason cut off would send someone to a terminal to find out.
    pub fn stage(&self, path: &Path) -> Result<(), String> {
        let Some(root) = &self.root else { return Err("not a repository".into()) };
        let Some(p) = path.to_str() else { return Err("unreadable path".into()) };
        run_checked(root, &["add", "--", p]).map(|_| ())
    }

    pub fn unstage(&self, path: &Path) -> Result<(), String> {
        let Some(root) = &self.root else { return Err("not a repository".into()) };
        let Some(p) = path.to_str() else { return Err("unreadable path".into()) };
        // `restore --staged` rather than `reset`: it works the same on the
        // very first commit, where HEAD does not exist for reset to move to.
        run_checked(root, &["restore", "--staged", "--", p]).map(|_| ())
    }

    /// Commits what is staged. Returns git's own summary line on success.
    pub fn commit(&self, message: &str) -> Result<String, String> {
        let Some(root) = &self.root else { return Err("not a repository".into()) };
        run_checked(root, &["commit", "-m", message]).map(|out| {
            out.lines().next().unwrap_or("committed").trim().to_string()
        })
    }

    /// Throws away a file's unstaged changes. The caller confirms first;
    /// this function assumes the question was already asked and answered.
    pub fn discard(&self, path: &Path) -> Result<(), String> {
        let Some(root) = &self.root else { return Err("not a repository".into()) };
        let Some(p) = path.to_str() else { return Err("unreadable path".into()) };
        run_checked(root, &["restore", "--", p]).map(|_| ())
    }

    /// Starts a push or pull on its own thread, because either can sit on
    /// the network for seconds and a frozen window reads as a crash.
    /// Returns the operation's name for the "pushing…" notice.
    pub fn start_remote(&mut self, op: RemoteOp) -> Result<&'static str, String> {
        let Some(root) = self.root.clone() else { return Err("not a repository".into()) };
        if let Some((name, _)) = &self.op {
            return Err(format!("a {name} is already running"));
        }
        let (name, args): (&'static str, &'static [&'static str]) = match op {
            RemoteOp::Push => ("push", &["push"]),
            RemoteOp::Pull => ("pull", &["pull", "--ff-only"]),
        };
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(run_reported(&root, args));
        });
        self.op = Some((name, rx));
        Ok(name)
    }

    /// Takes a finished remote operation's result, if one just landed.
    pub fn poll_remote(&mut self) -> Option<(&'static str, Result<String, String>)> {
        let (name, rx) = self.op.as_ref()?;
        let name = *name;
        match rx.try_recv() {
            Ok(result) => {
                self.op = None;
                Some((name, result))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.op = None;
                Some((name, Err("the git process never reported back".into())))
            }
        }
    }

    /// A file's diff, staged or unstaged side, as lines ready to colour.
    pub fn diff_file(&self, path: &Path, staged: bool) -> Vec<(DiffKind, String)> {
        let Some(root) = &self.root else { return Vec::new() };
        let Some(p) = path.to_str() else { return Vec::new() };
        let args: &[&str] = if staged {
            &["diff", "--cached", "--no-color", "--", p]
        } else {
            &["diff", "--no-color", "--", p]
        };
        classify_diff(&run(root, args).unwrap_or_default())
    }

    /// The last `limit` commits, newest first.
    pub fn log(&self, limit: usize) -> Vec<Commit> {
        let Some(root) = &self.root else { return Vec::new() };
        let n = limit.to_string();
        // NUL-separated fields for the same reason status uses -z: subjects
        // hold anything, and anything includes whatever separator was cute.
        let out = run(root, &["log", "--format=%h%x00%s%x00%cr%x00%an%x00", "-n", &n]);
        parse_log(&out.unwrap_or_default())
    }

    /// One commit's full change, as lines ready to colour.
    pub fn show(&self, hash: &str) -> Vec<(DiffKind, String)> {
        let Some(root) = &self.root else { return Vec::new() };
        classify_diff(&run(root, &["show", "--no-color", hash]).unwrap_or_default())
    }

    /// Which lines of a file differ from HEAD, for the editor gutter.
    ///
    /// Against HEAD rather than the index, because the question a gutter
    /// answers is "what did I change since the last commit" — staging a hunk
    /// must not make its mark disappear from the editor.
    ///
    /// Synchronous, unlike status: this is one file with `-U0`, which is a
    /// few milliseconds, and it only runs when something actually changed.
    /// An empty answer covers every kind of nothing — clean file, untracked
    /// file, no repository — because the gutter draws them all the same way.
    pub fn diff_marks(&self, path: &Path) -> Vec<(usize, LineChange)> {
        let Some(root) = &self.root else { return Vec::new() };
        let Some(p) = path.to_str() else { return Vec::new() };
        let out = run(root, &["diff", "HEAD", "--no-color", "-U0", "--", p]);
        parse_diff_marks(&out.unwrap_or_default())
    }

    /// Starts a refresh unless one is already running.
    pub fn refresh(&mut self) {
        let Some(root) = self.root.clone() else { return };
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = channel();
        // Detached: nothing waits on this thread. If it outlives the pane the
        // send simply fails on a dropped receiver.
        std::thread::spawn(move || {
            let _ = tx.send(collect(&root));
        });
        self.pending = Some(rx);
    }

    /// Takes a finished refresh, if there is one. Returns true when the state
    /// changed, so the caller only redraws for real news.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.pending else { return false };
        match rx.try_recv() {
            Ok(snapshot) => {
                let changed = snapshot.branch != self.snapshot.branch
                    || snapshot.files != self.snapshot.files;
                self.snapshot = snapshot;
                self.pending = None;
                changed
            }
            Err(TryRecvError::Empty) => false,
            // The thread died without sending; drop it so a later refresh works.
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                false
            }
        }
    }
}

fn collect(root: &Path) -> Snapshot {
    let branch = run(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s == "HEAD" {
                // Detached: show the short hash instead of the useless "HEAD".
                run(root, &["rev-parse", "--short", "HEAD"])
                    .map(|h| format!("detached {}", h.trim()))
                    .unwrap_or(s)
            } else {
                s
            }
        });

    let files = run(root, &["status", "--porcelain=v1", "-z", "--untracked-files=normal"])
        .map(|out| parse_porcelain(&out, root))
        .unwrap_or_default();

    Snapshot {
        branch,
        dirs: roll_up(&files, root),
        files,
    }
}

/// Marks every ancestor directory of a changed file.
///
/// Conflicts beat everything else: a folder with one conflict and twenty
/// modifications must not look merely modified.
fn roll_up(files: &HashMap<PathBuf, Status>, root: &Path) -> HashMap<PathBuf, Status> {
    let mut dirs: HashMap<PathBuf, Status> = HashMap::new();
    for (path, status) in files {
        let mut cur = path.parent();
        while let Some(dir) = cur {
            if !dir.starts_with(root) {
                break;
            }
            let entry = dirs.entry(dir.to_path_buf()).or_insert(*status);
            if *status == Status::Conflicted {
                *entry = Status::Conflicted;
            } else if *entry != Status::Conflicted && *entry == Status::Untracked {
                // A tracked change is more interesting than an untracked one.
                *entry = *status;
            }
            if dir == root {
                break;
            }
            cur = dir.parent();
        }
    }
    dirs
}

/// Parses `git status --porcelain=v1 -z`.
///
/// `-z` matters: without it, paths with spaces or non-ASCII characters come
/// back quoted and escaped, and unquoting them correctly is its own parser.
/// With `-z` every path is literal and NUL-terminated.
///
/// Renames are the awkward case — the entry is `XY new` and the ORIGINAL path
/// follows as the next NUL-terminated field, so parsing has to consume it or
/// everything after it shifts by one.
pub fn parse_porcelain(out: &str, root: &Path) -> HashMap<PathBuf, Status> {
    let mut files = HashMap::new();
    let mut fields = out.split('\0').filter(|f| !f.is_empty()).peekable();

    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let bytes = entry.as_bytes();
        let (x, y) = (bytes[0] as char, bytes[1] as char);
        let path = &entry[3..];

        let status = match (x, y) {
            // Any unmerged combination. These come first: 'D' or 'A' in an
            // unmerged pair would otherwise be read as a plain delete or add.
            ('U', _) | (_, 'U') | ('D', 'D') | ('A', 'A') => Status::Conflicted,
            ('?', _) => Status::Untracked,
            ('R', _) | (_, 'R') => Status::Renamed,
            ('A', _) => Status::Added,
            ('D', _) | (_, 'D') => Status::Deleted,
            _ => Status::Modified,
        };

        if status == Status::Renamed {
            // Consume the original path so it isn't read as its own entry.
            fields.next();
        }

        files.insert(root.join(path), status);
    }
    files
}

/// Parses porcelain status into panel rows: staged first, then unstaged,
/// each side sorted by path so the list holds still between refreshes.
///
/// The two porcelain columns are two lists in disguise — X is the index, Y
/// is the worktree — and one file can be on both. Conflicts collapse to a
/// single unstaged row: "resolve me" is one job, not two.
pub fn parse_entries(out: &str, root: &Path) -> Vec<Entry> {
    fn classify(c: char) -> Option<Status> {
        Some(match c {
            'M' | 'T' => Status::Modified,
            'A' => Status::Added,
            'D' => Status::Deleted,
            'R' | 'C' => Status::Renamed,
            _ => return None,
        })
    }

    let mut staged: Vec<Entry> = Vec::new();
    let mut unstaged: Vec<Entry> = Vec::new();
    let mut fields = out.split('\0').filter(|f| !f.is_empty()).peekable();

    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let bytes = entry.as_bytes();
        let (x, y) = (bytes[0] as char, bytes[1] as char);
        let rel = entry[3..].to_string();
        let path = root.join(&rel);

        if matches!((x, y), ('U', _) | (_, 'U') | ('D', 'D') | ('A', 'A')) {
            unstaged.push(Entry { path, rel, status: Status::Conflicted, staged: false });
            continue;
        }
        if x == '?' {
            unstaged.push(Entry { path, rel, status: Status::Untracked, staged: false });
            continue;
        }
        if x == 'R' {
            // The next field is the old name; unconsumed it would be read as
            // its own entry, same trap as the snapshot parser.
            fields.next();
        }
        if let Some(status) = classify(x) {
            staged.push(Entry { path: path.clone(), rel: rel.clone(), status, staged: true });
        }
        if let Some(status) = classify(y) {
            unstaged.push(Entry { path, rel, status, staged: false });
        }
    }

    staged.sort_by(|a, b| a.rel.cmp(&b.rel));
    unstaged.sort_by(|a, b| a.rel.cmp(&b.rel));
    staged.extend(unstaged);
    staged
}

/// Tags each diff line with what it is, for colouring.
///
/// `+++`/`---` are checked before `+`/`-`, or every file header would light
/// up as a change — which is exactly the mistake this function exists to
/// centralise.
pub fn classify_diff(out: &str) -> Vec<(DiffKind, String)> {
    out.lines()
        .map(|line| {
            let kind = if line.starts_with("@@") {
                DiffKind::Hunk
            } else if line.starts_with("+++")
                || line.starts_with("---")
                || line.starts_with("diff ")
                || line.starts_with("index ")
                || line.starts_with("new file")
                || line.starts_with("deleted file")
                || line.starts_with("similarity")
                || line.starts_with("rename ")
                || line.starts_with("commit ")
                || line.starts_with("Author:")
                || line.starts_with("Date:")
            {
                DiffKind::Meta
            } else if line.starts_with('+') {
                DiffKind::Add
            } else if line.starts_with('-') {
                DiffKind::Del
            } else {
                DiffKind::Context
            };
            (kind, line.to_string())
        })
        .collect()
}

/// Parses `%h%x00%s%x00%cr%x00%an%x00` log output.
pub fn parse_log(out: &str) -> Vec<Commit> {
    let mut commits = Vec::new();
    let mut fields = out.split('\0').map(str::trim);
    while let (Some(hash), Some(subject), Some(when), Some(author)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    {
        if hash.is_empty() {
            continue;
        }
        commits.push(Commit {
            hash: hash.to_string(),
            subject: subject.to_string(),
            when: when.to_string(),
            author: author.to_string(),
        });
    }
    commits
}

/// Parses `git diff -U0` output into per-line gutter marks, zero-based.
///
/// Only the hunk headers are read: with zero context, `@@ -a,b +c,d @@` says
/// everything — the body lines repeat it. `d` new lines starting at `c` are
/// additions when `b` is zero and changes otherwise; `d == 0` is a pure
/// deletion, and the mark lands on the line now sitting where the gap was.
/// (git's `c` in that case is the line *before* the gap, one-based — which is
/// exactly the zero-based line after it, so it is used as it stands.)
pub fn parse_diff_marks(diff: &str) -> Vec<(usize, LineChange)> {
    // "12,3" is start and count; a bare "12" means a count of one.
    fn span(s: &str) -> (usize, usize) {
        match s.split_once(',') {
            Some((start, n)) => (start.parse().unwrap_or(0), n.parse().unwrap_or(0)),
            None => (s.parse().unwrap_or(0), 1),
        }
    }

    let mut marks = Vec::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("@@ -") else { continue };
        let Some((old, rest)) = rest.split_once(" +") else { continue };
        let Some((new, _)) = rest.split_once(" @@") else { continue };
        let (_, old_n) = span(old);
        let (new_start, new_n) = span(new);

        if new_n == 0 {
            marks.push((new_start, LineChange::Deleted));
            continue;
        }
        let kind = if old_n == 0 { LineChange::Added } else { LineChange::Modified };
        for i in 0..new_n {
            // One-based in the header, zero-based in a buffer.
            marks.push((new_start.saturating_sub(1) + i, kind));
        }
    }
    marks
}

/// Runs git and returns stdout, or `None` on any failure.
///
/// Failure is normal here — not a repo, git not installed, a broken index.
/// None of it should reach the user as an error dialog; the UI just shows no
/// git information.
fn run(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    no_window(&mut cmd);
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Like [`run_checked`], but for push and pull, which narrate on stderr
/// even when they succeed. The result either way is the one line worth
/// reading: "main -> main", "Everything up-to-date", or why not.
fn run_reported(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    no_window(&mut cmd);
    let out = cmd.output().map_err(|e| e.to_string())?;

    let last = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("hint:"))
            .map(str::to_string)
    };
    if out.status.success() {
        // Pull's useful line ("2 files changed…") is on stdout; push's
        // ("main -> main") is on stderr. Whichever spoke last, kept short.
        return Ok(last(&out.stdout)
            .or_else(|| last(&out.stderr))
            .unwrap_or_else(|| "done".to_string()));
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let line = err
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("error:") || l.starts_with("fatal:") || l.contains("[rejected]"))
        .or_else(|| err.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("git failed");
    Err(line.trim_start_matches("error: ").trim_start_matches("fatal: ").to_string())
}

/// Like [`run`], but failure carries git's own explanation.
///
/// For the commands the panel runs on purpose — add, restore, commit —
/// where "it failed" without the why (hook said no, nothing staged, index
/// locked) would send the user to a terminal to ask again.
fn run_checked(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    no_window(&mut cmd);
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    // The first line that says anything; git pads its errors generously.
    let line = err
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("git failed");
    Err(line.trim_start_matches("error: ").trim_start_matches("fatal: ").to_string())
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW. Without it a console flashes on every refresh, because
    // the editor itself is a windows-subsystem process with no console.
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/repo")
    }

    #[test]
    fn parses_the_common_states() {
        let out = " M src/main.rs\0?? notes.txt\0A  new.rs\0 D gone.rs\0";
        let f = parse_porcelain(out, &root());
        assert_eq!(f[&root().join("src/main.rs")], Status::Modified);
        assert_eq!(f[&root().join("notes.txt")], Status::Untracked);
        assert_eq!(f[&root().join("new.rs")], Status::Added);
        assert_eq!(f[&root().join("gone.rs")], Status::Deleted);
    }

    #[test]
    fn a_rename_consumes_its_original_path() {
        // The field after a rename is the OLD name. Reading it as an entry
        // would shift everything after it by one.
        let out = "R  new.rs\0old.rs\0 M after.rs\0";
        let f = parse_porcelain(out, &root());
        assert_eq!(f[&root().join("new.rs")], Status::Renamed);
        assert_eq!(f[&root().join("after.rs")], Status::Modified);
        assert!(!f.contains_key(&root().join("old.rs")), "the old name is not an entry");
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn unmerged_states_are_conflicts_not_adds_or_deletes() {
        let out = "UU both.rs\0AA added.rs\0DD deleted.rs\0DU half.rs\0";
        let f = parse_porcelain(out, &root());
        for name in ["both.rs", "added.rs", "deleted.rs", "half.rs"] {
            assert_eq!(f[&root().join(name)], Status::Conflicted, "{name}");
        }
    }

    #[test]
    fn paths_with_spaces_survive() {
        // The reason for -z: without it this comes back quoted.
        let out = " M my notes/two words.md\0";
        let f = parse_porcelain(out, &root());
        assert_eq!(f[&root().join("my notes/two words.md")], Status::Modified);
    }

    #[test]
    fn directories_inherit_the_loudest_status() {
        let mut files = HashMap::new();
        files.insert(root().join("src/a.rs"), Status::Untracked);
        files.insert(root().join("src/b.rs"), Status::Conflicted);
        let dirs = roll_up(&files, &root());
        assert_eq!(dirs[&root().join("src")], Status::Conflicted);
        assert_eq!(dirs[&root()], Status::Conflicted);
    }

    #[test]
    fn rollup_stops_at_the_repository_root() {
        let mut files = HashMap::new();
        files.insert(root().join("src/a.rs"), Status::Modified);
        let dirs = roll_up(&files, &root());
        assert!(dirs.contains_key(&root()));
        assert!(!dirs.contains_key(Path::new("/")), "must not walk above the repo");
    }

    #[test]
    fn not_a_repository_is_not_an_error() {
        let g = Git::discover(&std::env::temp_dir());
        // Whether temp is inside a repo depends on the machine; either way
        // this must not panic and must not block.
        let _ = g.is_repo();
        assert!(g.snapshot().files.is_empty());
    }

    /// git reports the root with forward slashes even on Windows, while
    /// read_dir hands back backslashes. If those did not compare equal, the
    /// explorer would look up every path and find nothing — no colors at all,
    /// and no error anywhere to say why.
    #[test]
    #[cfg(windows)]
    fn slash_and_backslash_paths_match() {
        let mut m: HashMap<PathBuf, Status> = HashMap::new();
        m.insert(PathBuf::from("C:/repo/src/main.rs"), Status::Modified);
        let native = PathBuf::from("C:\\repo\\src\\main.rs");
        assert_eq!(
            m.get(native.as_path()),
            Some(&Status::Modified),
            "lookup must survive the separator difference"
        );
    }

    #[test]
    fn a_search_finds_itself() {
        // End to end against this repository: the string below is in this
        // file, so a search for it must come back with this file.
        let g = Git::discover(Path::new("."));
        if !g.is_repo() {
            return;
        }
        let hits = g.grep("a_search_finds_itself", 50).unwrap_or_default();
        assert!(
            hits.iter().any(|h| h.path.ends_with("kb-git/src/lib.rs")),
            "{hits:?}"
        );
        assert!(hits.iter().all(|h| !h.text.is_empty()));
    }

    #[test]
    fn empty_output_means_a_clean_tree() {
        assert!(parse_porcelain("", &root()).is_empty());
    }

    #[test]
    fn diff_marks_tell_added_from_modified() {
        // -U0 headers: one line changed at 3, two lines added after 7.
        let diff = "@@ -3 +3 @@ fn main() {\n@@ -7,0 +8,2 @@ context\n";
        let marks = parse_diff_marks(diff);
        assert_eq!(
            marks,
            vec![
                (2, LineChange::Modified),
                (7, LineChange::Added),
                (8, LineChange::Added),
            ]
        );
    }

    #[test]
    fn a_deletion_marks_the_line_after_the_gap() {
        // Two lines deleted after (one-based) line 4: git says "+4,0". The
        // zero-based line now sitting where they were is 4.
        let marks = parse_diff_marks("@@ -5,2 +4,0 @@\n");
        assert_eq!(marks, vec![(4, LineChange::Deleted)]);
    }

    #[test]
    fn a_deletion_at_the_top_lands_on_line_zero() {
        let marks = parse_diff_marks("@@ -1,3 +0,0 @@\n");
        assert_eq!(marks, vec![(0, LineChange::Deleted)]);
    }

    #[test]
    fn diff_body_lines_are_not_read_as_headers() {
        // The body repeats what the header said; reading a context line that
        // happens to start with @@ would double-count.
        let diff = "@@ -1 +1 @@\n-old\n+new\n +not a header\n";
        assert_eq!(parse_diff_marks(diff).len(), 1);
    }

    #[test]
    fn an_empty_diff_has_no_marks() {
        assert!(parse_diff_marks("").is_empty());
    }

    #[test]
    fn entries_split_staged_from_unstaged() {
        // "MM" is one file with two changes: what is staged and what came
        // after. The panel must show both, or half the change is invisible.
        let out = "M  staged.rs\0 M worktree.rs\0MM both.rs\0?? new.txt\0";
        let e = parse_entries(out, &root());
        let staged: Vec<&str> = e.iter().filter(|x| x.staged).map(|x| x.rel.as_str()).collect();
        let unstaged: Vec<&str> = e.iter().filter(|x| !x.staged).map(|x| x.rel.as_str()).collect();
        assert_eq!(staged, ["both.rs", "staged.rs"]);
        assert_eq!(unstaged, ["both.rs", "new.txt", "worktree.rs"]);
    }

    #[test]
    fn entries_come_staged_first_and_sorted() {
        let out = " M z.rs\0M  b.rs\0M  a.rs\0";
        let e = parse_entries(out, &root());
        let rels: Vec<&str> = e.iter().map(|x| x.rel.as_str()).collect();
        assert_eq!(rels, ["a.rs", "b.rs", "z.rs"]);
        assert!(e[0].staged && e[1].staged && !e[2].staged);
    }

    #[test]
    fn a_conflict_is_one_row_not_two() {
        // "Resolve me" is one job; a staged half and an unstaged half of the
        // same conflict would read as two different files.
        let e = parse_entries("UU war.rs\0", &root());
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].status, Status::Conflicted);
        assert!(!e[0].staged);
    }

    #[test]
    fn an_entry_rename_consumes_the_old_name() {
        let e = parse_entries("R  new.rs\0old.rs\0 M after.rs\0", &root());
        let rels: Vec<&str> = e.iter().map(|x| x.rel.as_str()).collect();
        assert_eq!(rels, ["new.rs", "after.rs"]);
    }

    #[test]
    fn diff_lines_are_classified_for_colour() {
        let out = "diff --git a/x b/x\nindex 123..456 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n context\n";
        let lines = classify_diff(out);
        let kinds: Vec<DiffKind> = lines.iter().map(|(k, _)| *k).collect();
        use DiffKind::*;
        assert_eq!(kinds, [Meta, Meta, Meta, Meta, Hunk, Del, Add, Context]);
    }

    #[test]
    fn file_headers_do_not_read_as_changes() {
        // +++/--- start with the change characters; classifying them as
        // add/delete is the classic diff-colouring bug.
        let lines = classify_diff("--- a/x\n+++ b/x\n");
        assert!(lines.iter().all(|(k, _)| *k == DiffKind::Meta));
    }

    #[test]
    fn a_log_parses_back_into_commits() {
        let out = "abc123\0Fix the thing\u{0}3 days ago\0kubilay\0\ndef456\0Do: a % thing\0now\0other\0";
        let commits = parse_log(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "abc123");
        assert_eq!(commits[0].subject, "Fix the thing");
        assert_eq!(commits[1].subject, "Do: a % thing");
        assert_eq!(commits[1].author, "other");
    }

    #[test]
    fn an_empty_log_is_no_commits() {
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn the_log_reads_this_repository() {
        // End to end: this repository has commits, so the panel must see
        // them — hash, subject, the lot.
        let g = Git::discover(Path::new("."));
        if !g.is_repo() {
            return;
        }
        let log = g.log(3);
        assert!(!log.is_empty());
        assert!(log.iter().all(|c| !c.hash.is_empty() && !c.subject.is_empty()));
    }
}
