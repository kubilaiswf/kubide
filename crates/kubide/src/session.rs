//! Remembering the layout between runs.
//!
//! Stored per workspace under `sessions` in kubide's folder, keyed by a hash of
//! the directory. One file per project rather than one big one, so a corrupt
//! session costs you that project's layout and nothing else.
//!
//! Everything here is best-effort. A session that fails to read, or that
//! describes files which have since been deleted, must never stop the editor
//! from starting — the whole feature is a convenience, and a convenience that
//! can prevent startup is a liability.

use std::path::{Path, PathBuf};

use kb_ui::{Axis, Desc, PaneId};

/// What a pane held.
#[derive(Clone, Debug, PartialEq)]
pub enum Pane {
    Empty,
    Explorer,
    Terminal,
    /// A file, and where the caret was in it.
    File(PathBuf, usize, usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub layout: Desc,
    pub panes: Vec<(PaneId, Pane)>,
    pub focus: PaneId,
}

/// Where a workspace's session lives.
///
/// A marked workspace keeps its own, inside the `.kubide` it already has: a
/// project that is renamed or moved to another drive takes its layout with
/// it, where a file keyed by the old path is simply lost. Everything else
/// goes to kubide's own folder, hashed rather than derived from the path — a
/// directory name can contain anything, including characters that are not
/// legal in a file name, and escaping them correctly is more work than
/// hashing.
pub fn path_for(root: &Path) -> Option<PathBuf> {
    let marked = root.join(".kubide");
    if marked.is_dir() {
        return Some(marked.join("session"));
    }
    Some(
        kb_cfg::data_dir()
            .join("sessions")
            .join(format!("{:016x}.session", hash(&root.to_string_lossy()))),
    )
}

/// Where the recent-workspaces list lives: one absolute path per line,
/// newest first. Plain text because it is a list of paths, and the welcome
/// screen reading it must never be able to fail interestingly.
fn recent_path() -> Option<PathBuf> {
    Some(kb_cfg::data_dir().join("recent"))
}

/// The workspaces opened before, newest first — only the ones that still
/// exist, because a welcome screen offering a directory that is gone would
/// be an error message with extra steps.
pub fn recent_workspaces() -> Vec<PathBuf> {
    let Some(path) = recent_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let mut out: Vec<PathBuf> = Vec::new();
    for line in text.lines() {
        // De-armoured on the way in, not only on the way out: a run that
        // wrote `\\?\C:\...` and a run that wrote `C:\...` meant the same
        // folder, and the list was offering it twice under one name.
        let dir = kb_fs::strip_verbatim(PathBuf::from(line));
        if dir.as_os_str().is_empty() || out.contains(&dir) || !dir.is_dir() {
            continue;
        }
        out.push(dir);
        if out.len() == 8 {
            break;
        }
    }
    out
}

/// Puts `dir` at the top of the recents. Best effort, like everything here.
pub fn note_workspace(dir: &Path) {
    let Some(path) = recent_path() else { return };
    // Stored in the one form the list compares in; see `recent_workspaces`.
    let dir = kb_fs::strip_verbatim(dir.to_path_buf());
    let list = bumped(&dir, recent_workspaces());
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    let _ = std::fs::write(path, text);
}

/// Where the window's last size and place is written down.
///
/// One file for the whole program, not one per workspace: a window is a
/// window, and being handed a different size for every project would read as
/// the editor resizing itself. Plain text for the same reason the recents
/// are — five numbers do not need a format.
fn window_path() -> Option<PathBuf> {
    Some(kb_cfg::data_dir().join("window"))
}

/// The size and place the window was left at, if it was ever noted.
///
/// Anything unreadable, short or non-numeric is simply no answer: a first run
/// and a corrupt file should both end up letting Windows choose.
pub fn window_place() -> Option<kb_win::Placement> {
    let text = std::fs::read_to_string(window_path()?).ok()?;
    let n: Vec<i32> = text.split_whitespace().filter_map(|w| w.parse().ok()).collect();
    let &[x, y, width, height, maximized] = n.as_slice() else { return None };
    (width > 0 && height > 0).then_some(kb_win::Placement {
        x,
        y,
        width,
        height,
        maximized: maximized != 0,
    })
}

/// Writes the window's place down. Best effort, like everything here.
pub fn note_window_place(p: kb_win::Placement) {
    let Some(path) = window_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        path,
        format!("{} {} {} {} {}\n", p.x, p.y, p.width, p.height, i32::from(p.maximized)),
    );
}

/// `dir` first, the others in their old order, duplicates gone.
fn bumped(dir: &Path, old: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut list = vec![dir.to_path_buf()];
    list.extend(old.into_iter().filter(|p| p != dir));
    list.truncate(8);
    list
}

fn hash(s: &str) -> u64 {
    // FNV-1a. Not cryptographic and does not need to be: a collision costs
    // two projects one shared layout file, which is a shrug, and pulling in a
    // hasher for this would be silly.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.to_lowercase().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl Session {
    /// Whether this layout holds anything a person would want back.
    ///
    /// A session of furniture only — an explorer and empty panes, the shape
    /// every pre-welcome kubide saved for any folder it was so much as
    /// started in — is not a place someone was working. Restoring one
    /// defeats the welcome screen with a listing of wherever the exe woke
    /// up last time, which is exactly what it exists to prevent. A file or
    /// a terminal is a sign of life; nothing else is.
    pub fn worth_restoring(&self) -> bool {
        self.panes
            .iter()
            .any(|(_, p)| matches!(p, Pane::File(..) | Pane::Terminal))
    }

    pub fn save(&self, path: &Path, root: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            // A layout is one machine's arrangement of one checkout, full of
            // paths that mean nothing on anyone else's. Kept out of the
            // project's history rather than left for someone to notice in a
            // diff — inside `.kubide`, so it only ever ignores our own file.
            let ignore = parent.join(".gitignore");
            if parent.file_name() == Some(std::ffi::OsStr::new(".kubide")) && !ignore.exists() {
                let _ = std::fs::write(ignore, "session\n");
            }
        }
        std::fs::write(path, self.encode(root))
    }

    pub fn load(path: &Path, root: &Path) -> Option<Session> {
        Self::decode(&std::fs::read_to_string(path).ok()?, root)
    }

    /// A small line-based format rather than TOML.
    ///
    /// The layout is a tree, and expressing a tree in TOML means either a
    /// nested table dance or a flattened encoding anyway. This is a handful of
    /// lines either way, and it keeps the session file out of the config
    /// crate's schema, which is a user-facing contract this is not.
    ///
    /// Files under the root are written relative to it, so that a workspace
    /// carrying its own session file survives being renamed or moved. Files
    /// from outside keep their absolute path, because nothing else can name
    /// them.
    fn encode(&self, root: &Path) -> String {
        let mut out = String::from("kubide-session 1\n");
        out.push_str(&format!("focus {}\n", self.focus.0));
        out.push_str("layout ");
        encode_desc(&self.layout, &mut out);
        out.push('\n');
        for (id, pane) in &self.panes {
            match pane {
                Pane::Empty => out.push_str(&format!("pane {} empty\n", id.0)),
                Pane::Explorer => out.push_str(&format!("pane {} explorer\n", id.0)),
                Pane::Terminal => out.push_str(&format!("pane {} terminal\n", id.0)),
                Pane::File(p, line, col) => out.push_str(&format!(
                    "pane {} file {line} {col} {}\n",
                    id.0,
                    p.strip_prefix(root).unwrap_or(p).display()
                )),
            }
        }
        out
    }

    fn decode(text: &str, root: &Path) -> Option<Session> {
        let mut lines = text.lines();
        // A version line, so a future format change can be ignored rather
        // than misread as this one.
        if lines.next()? != "kubide-session 1" {
            return None;
        }

        let mut focus = PaneId(0);
        let mut layout = None;
        let mut panes = Vec::new();

        for line in lines {
            let (key, rest) = line.split_once(' ')?;
            match key {
                "focus" => focus = PaneId(rest.trim().parse().ok()?),
                "layout" => layout = decode_desc(&mut rest.split_whitespace()),
                "pane" => {
                    let (id, rest) = rest.split_once(' ')?;
                    let id = PaneId(id.parse().ok()?);
                    let pane = match rest.split_once(' ') {
                        Some(("file", tail)) => {
                            let (line, tail) = tail.split_once(' ')?;
                            let (col, path) = tail.split_once(' ')?;
                            let path = PathBuf::from(path);
                            // Relative means "under the root", wherever the
                            // root has since moved to. Sessions written before
                            // this were absolute and still are.
                            let path = if path.is_relative() { root.join(path) } else { path };
                            Pane::File(path, line.parse().ok()?, col.parse().ok()?)
                        }
                        _ => match rest.trim() {
                            "explorer" => Pane::Explorer,
                            "terminal" => Pane::Terminal,
                            _ => Pane::Empty,
                        },
                    };
                    panes.push((id, pane));
                }
                // Unknown keys are skipped rather than fatal, so a newer
                // kubide's extra lines do not cost an older one its layout.
                _ => {}
            }
        }
        Some(Session { layout: layout?, panes, focus })
    }
}

fn encode_desc(desc: &Desc, out: &mut String) {
    match desc {
        Desc::Leaf(p) => out.push_str(&format!("L{} ", p.0)),
        Desc::Split { axis, ratios, children } => {
            out.push_str(match axis {
                Axis::Horizontal => "H",
                Axis::Vertical => "V",
            });
            out.push_str(&format!("{} ", children.len()));
            for r in ratios {
                out.push_str(&format!("{r:.4} "));
            }
            for c in children {
                encode_desc(c, out);
            }
        }
    }
}

fn decode_desc<'a>(tokens: &mut impl Iterator<Item = &'a str>) -> Option<Desc> {
    let token = tokens.next()?;
    if let Some(id) = token.strip_prefix('L') {
        return Some(Desc::Leaf(PaneId(id.parse().ok()?)));
    }
    let axis = match token.chars().next()? {
        'H' => Axis::Horizontal,
        'V' => Axis::Vertical,
        _ => return None,
    };
    let count: usize = token[1..].parse().ok()?;
    // A count from a file decides how many times we recurse, so it needs a
    // ceiling: a corrupt one would otherwise recurse until the stack runs out.
    if count == 0 || count > 64 {
        return None;
    }
    let ratios: Vec<f32> = (0..count).filter_map(|_| tokens.next()?.parse().ok()).collect();
    let children: Vec<Desc> = (0..count).filter_map(|_| decode_desc(tokens)).collect();
    Some(Desc::Split { axis, ratios, children })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Session {
        Session {
            layout: Desc::Split {
                axis: Axis::Horizontal,
                ratios: vec![0.25, 0.75],
                children: vec![
                    Desc::Leaf(PaneId(0)),
                    Desc::Split {
                        axis: Axis::Vertical,
                        ratios: vec![0.5, 0.5],
                        children: vec![Desc::Leaf(PaneId(1)), Desc::Leaf(PaneId(2))],
                    },
                ],
            },
            panes: vec![
                (PaneId(0), Pane::Explorer),
                (PaneId(1), Pane::File(root().join("src/main.rs"), 42, 7)),
                (PaneId(2), Pane::Terminal),
            ],
            focus: PaneId(1),
        }
    }

    /// The root a plain round-trip is written against.
    fn root() -> &'static Path {
        Path::new("C:/repo")
    }

    #[test]
    fn a_session_survives_a_round_trip() {
        let s = sample();
        assert_eq!(Session::decode(&s.encode(root()), root()).unwrap(), s);
    }

    #[test]
    fn paths_with_spaces_survive() {
        // The path is last on its line precisely so it can contain spaces.
        let s = Session {
            layout: Desc::Leaf(PaneId(0)),
            panes: vec![(PaneId(0), Pane::File(root().join("my notes/two words.md"), 1, 2))],
            focus: PaneId(0),
        };
        assert_eq!(Session::decode(&s.encode(root()), root()).unwrap(), s);
    }

    #[test]
    fn a_moved_workspace_keeps_its_files() {
        // The whole reason a marked workspace stores its own session: the
        // folder is renamed, the layout comes with it.
        let s = Session {
            layout: Desc::Leaf(PaneId(0)),
            panes: vec![(PaneId(0), Pane::File(root().join("src/main.rs"), 3, 0))],
            focus: PaneId(0),
        };
        let moved = Path::new("D:/elsewhere/repo");
        let back = Session::decode(&s.encode(root()), moved).unwrap();
        assert_eq!(back.panes[0].1, Pane::File(moved.join("src/main.rs"), 3, 0));
    }

    #[test]
    fn a_file_from_outside_the_workspace_keeps_its_own_path() {
        // A root that is not the workspace's on either platform.
        let outside = PathBuf::from(if cfg!(windows) { "D:/notes/todo.md" } else { "/notes/todo.md" });
        let s = Session {
            layout: Desc::Leaf(PaneId(0)),
            panes: vec![(PaneId(0), Pane::File(outside.clone(), 0, 0))],
            focus: PaneId(0),
        };
        let back = Session::decode(&s.encode(root()), root()).unwrap();
        assert_eq!(back.panes[0].1, Pane::File(outside, 0, 0));
    }

    #[test]
    fn a_different_version_is_ignored_rather_than_misread() {
        assert!(Session::decode("kubide-session 99\nfocus 0\n", root()).is_none());
        assert!(Session::decode("garbage", root()).is_none());
    }

    #[test]
    fn unknown_lines_do_not_cost_the_layout() {
        // A newer kubide writing extra keys must not break an older one.
        let text = "kubide-session 1\nfocus 0\nlayout L0 \nsomething new\n";
        assert!(Session::decode(text, root()).is_some());
    }

    #[test]
    fn truncated_input_returns_nothing_rather_than_panicking() {
        for text in [
            "kubide-session 1\n",
            "kubide-session 1\nlayout H2 0.5 0.5 L0 \n",
            "kubide-session 1\nlayout H\n",
            "kubide-session 1\nlayout \n",
        ] {
            let _ = Session::decode(text, root());
        }
    }

    #[test]
    fn an_absurd_child_count_is_refused() {
        // It comes from a file and decides how deep we recurse.
        assert!(Session::decode("kubide-session 1\nlayout H999999 L0 \n", root()).is_none());
    }

    #[test]
    fn furniture_only_sessions_are_not_worth_restoring() {
        // The shape every pre-welcome kubide saved for any folder it was
        // started in: an explorer and an empty pane. Bringing it back would
        // defeat the welcome screen with a listing of wherever the exe woke
        // up last time.
        let mut s = sample();
        s.panes = vec![(PaneId(0), Pane::Explorer), (PaneId(1), Pane::Empty)];
        assert!(!s.worth_restoring());

        assert!(sample().worth_restoring(), "a file or terminal is a sign of life");

        let mut s = sample();
        s.panes = vec![(PaneId(0), Pane::Explorer), (PaneId(1), Pane::Terminal)];
        assert!(s.worth_restoring(), "a terminal pane counts as work");
    }

    #[test]
    fn a_reopened_workspace_moves_to_the_top_once() {
        let old = vec![PathBuf::from("C:/a"), PathBuf::from("C:/b"), PathBuf::from("C:/c")];
        let list = bumped(Path::new("C:/b"), old);
        assert_eq!(
            list,
            [PathBuf::from("C:/b"), PathBuf::from("C:/a"), PathBuf::from("C:/c")]
        );
    }

    #[test]
    fn the_recents_list_has_a_ceiling() {
        let old: Vec<PathBuf> = (0..20).map(|i| PathBuf::from(format!("C:/p{i}"))).collect();
        assert_eq!(bumped(Path::new("C:/new"), old).len(), 8);
    }

    #[test]
    fn the_same_directory_always_gets_the_same_file() {
        let a = path_for(Path::new("C:/projects/thing"));
        let b = path_for(Path::new("C:/projects/thing"));
        let c = path_for(Path::new("C:/projects/other"));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
