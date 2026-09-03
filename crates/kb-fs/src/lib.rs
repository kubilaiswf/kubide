//! File explorer model: a lazily expanded directory tree flattened into rows.
//!
//! No drawing and no window here. What is actually hard about an explorer is
//! the bookkeeping — which directories are open, what the visible list looks
//! like after a toggle, where the selection lands when a row disappears — and
//! all of that is testable without a GPU.
//!
//! Directories are read on expand, never up front. A recursive read of a Rust
//! project would walk `target/` and take seconds for rows nobody asked for.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// One visible line in the explorer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// Nesting level; the root's children are 0.
    pub depth: usize,
    /// Directories only: whether this one is expanded.
    pub open: bool,
}

pub struct FileTree {
    root: PathBuf,
    /// Expanded directories. A set of paths rather than flags on the rows,
    /// because rows are rebuilt from scratch and would lose their state.
    open: HashSet<PathBuf>,
    rows: Vec<Row>,
    selected: usize,
    /// Last error, e.g. a directory that can't be read. Shown rather than
    /// swallowed — an explorer that silently skips folders is a liar.
    problem: Option<String>,
}

impl FileTree {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let mut me = Self {
            root: root.into(),
            open: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            problem: None,
        };
        me.rebuild();
        me.selected = me.opening_row();
        me
    }

    /// Where the cursor sits when a tree is first shown.
    ///
    /// `.git` sorts first among the directories and is the one folder in a
    /// project nobody opens on purpose; landing on it means the first Enter
    /// of a session expands plumbing. It stays in the list — hiding a folder
    /// someone might genuinely want is the worse trade — it just does not get
    /// handed the cursor.
    fn opening_row(&self) -> usize {
        self.rows.iter().position(|r| r.name != ".git").unwrap_or(0)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }
    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    /// Rereads everything currently expanded, keeping the selected path where
    /// possible. Used after an external change.
    ///
    /// Says whether anything actually changed, because the caller redraws
    /// only for real news — a refresh that finds a new file and stays
    /// silent leaves it invisible until some unrelated repaint, which is
    /// exactly how `cargo new` looked like it took half a minute.
    pub fn refresh(&mut self) -> bool {
        let keep = self.selected_row().map(|r| r.path.clone());
        let before = std::mem::take(&mut self.rows);
        self.rebuild();
        if let Some(p) = keep {
            self.select_path(&p);
        }
        before != self.rows
    }

    /// Moves the root, e.g. on "open parent". Expansion state is dropped
    /// because it belongs to the old tree.
    pub fn set_root(&mut self, root: impl Into<PathBuf>) {
        self.root = root.into();
        self.open.clear();
        self.selected = 0;
        self.rebuild();
        self.selected = self.opening_row();
    }

    fn rebuild(&mut self) {
        self.problem = None;
        let mut rows = Vec::new();
        let root = self.root.clone();
        self.walk(&root, 0, &mut rows);
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn walk(&mut self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        let entries = match read_sorted(dir) {
            Ok(e) => e,
            Err(e) => {
                // Only report the root failing; an unreadable subdirectory is
                // common (permissions) and shouldn't take over the UI.
                if depth == 0 {
                    self.problem = Some(format!("{}: {e}", dir.display()));
                }
                return;
            }
        };
        for (name, path, is_dir) in entries {
            let open = is_dir && self.open.contains(&path);
            out.push(Row {
                name,
                path: path.clone(),
                is_dir,
                depth,
                open,
            });
            if open {
                self.walk(&path, depth + 1, out);
            }
        }
    }

    /// Expands or collapses the selected directory. Returns false when the
    /// selection is a file, so the caller can open it instead.
    pub fn toggle_selected(&mut self) -> bool {
        let Some(row) = self.rows.get(self.selected) else { return false };
        if !row.is_dir {
            return false;
        }
        let path = row.path.clone();
        if !self.open.remove(&path) {
            self.open.insert(path.clone());
        }
        self.rebuild();
        self.select_path(&path);
        true
    }

    /// Collapses the selection, or jumps to its parent when it's already
    /// closed. This is what Left does in every tree view, and it's the only
    /// fast way back out of a deep directory.
    pub fn collapse_or_parent(&mut self) {
        let Some(row) = self.rows.get(self.selected).cloned() else { return };
        if row.is_dir && row.open {
            self.open.remove(&row.path);
            self.rebuild();
            self.select_path(&row.path);
            return;
        }
        let Some(parent) = row.path.parent().map(Path::to_path_buf) else { return };
        if parent == self.root {
            return;
        }
        self.open.remove(&parent);
        self.rebuild();
        self.select_path(&parent);
    }

    pub fn select(&mut self, index: usize) {
        if !self.rows.is_empty() {
            self.selected = index.min(self.rows.len() - 1);
        }
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as i32 - 1;
        self.selected = (self.selected as i32 + delta).clamp(0, last) as usize;
    }

    /// Selects a path if it's currently visible. Silent when it isn't — after
    /// a collapse the path legitimately no longer has a row.
    pub fn select_path(&mut self, path: &Path) -> bool {
        if let Some(i) = self.rows.iter().position(|r| r.path == path) {
            self.selected = i;
            return true;
        }
        false
    }
}

/// Directories first, then files, both case-insensitive.
///
/// Nothing is hidden, not even dotfiles: in a code project `.gitignore` and
/// `.github/` are exactly what people are looking for, and a filter you can't
/// see is worse than a long list.
fn read_sorted(dir: &Path) -> std::io::Result<Vec<(String, PathBuf, bool)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // `file_type` avoids a stat per entry; on a symlink it describes the
        // link itself, which is what a file manager should show.
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push((entry.file_name().to_string_lossy().into_owned(), path, is_dir));
    }
    out.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    Ok(out)
}

/// Creating, renaming and removing entries.
///
/// Every one of these refuses rather than overwrites. There is no undo for a
/// clobbered file and no recycle bin behind these calls, so the only safe
/// default is to fail and say why.
pub mod ops {
    use std::path::{Path, PathBuf};

    /// Creates an empty file. Fails if anything is already there.
    pub fn create_file(path: &Path) -> Result<(), String> {
        if path.exists() {
            return Err(format!("{} already exists", name_of(path)));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, "").map_err(|e| e.to_string())
    }

    pub fn create_dir(path: &Path) -> Result<(), String> {
        if path.exists() {
            return Err(format!("{} already exists", name_of(path)));
        }
        std::fs::create_dir_all(path).map_err(|e| e.to_string())
    }

    /// Renames within the same directory.
    ///
    /// Refuses to overwrite: on Windows `rename` onto an existing file
    /// replaces it silently, which is a data loss you cannot take back.
    pub fn rename(from: &Path, new_name: &str) -> Result<PathBuf, String> {
        let name = new_name.trim();
        if name.is_empty() {
            return Err("a name is required".into());
        }
        if name.contains(['/', '\\']) {
            // Renaming is not moving. A slash here would quietly relocate the
            // file somewhere the user is not looking.
            return Err("a name cannot contain a path separator".into());
        }
        let to = from.parent().unwrap_or(Path::new(".")).join(name);
        if to == from {
            return Ok(to);
        }
        if to.exists() {
            return Err(format!("{name} already exists"));
        }
        std::fs::rename(from, &to).map_err(|e| e.to_string())?;
        Ok(to)
    }

    /// Deletes a file, or an empty directory.
    ///
    /// A non-empty directory is refused on purpose. Recursive delete with no
    /// recycle bin behind it turns one mistaken keystroke into a lost tree,
    /// and this editor has no undo for the filesystem.
    pub fn delete(path: &Path) -> Result<(), String> {
        if path.is_dir() {
            let empty = std::fs::read_dir(path)
                .map_err(|e| e.to_string())?
                .next()
                .is_none();
            if !empty {
                return Err(format!("{} is not empty", name_of(path)));
            }
            return std::fs::remove_dir(path).map_err(|e| e.to_string());
        }
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }

    fn name_of(path: &Path) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    }
}

/// Directory names never worth walking into for a file list.
///
/// A fallback for when there is no git repository to ask. Without it, one
/// `target/` turns a file list into tens of thousands of build artefacts and
/// the finder becomes useless.
const SKIP: &[&str] = &[
    ".git", "target", "node_modules", ".venv", "venv", "__pycache__", "dist",
    "build", ".next", ".cache", "vendor", "Debug", "Release",
];

/// Walks a directory for files, breadth-first, stopping at `limit`.
///
/// Breadth-first on purpose: hitting the limit should cost you the deepest
/// files rather than everything after one unlucky subdirectory.
pub fn list_files(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);

    while let Some(dir) = queue.pop_front() {
        if out.len() >= limit {
            break;
        }
        let Ok(entries) = read_sorted(&dir) else { continue };
        for (name, path, is_dir) in entries {
            if is_dir {
                if !SKIP.contains(&name.as_str()) {
                    queue.push_back(path);
                }
            } else if out.len() < limit {
                out.push(path);
            }
        }
    }
    out
}

/// Takes canonicalize's `\\?\` armour off on Windows.
///
/// `std::fs::canonicalize` returns extended-length paths, which are correct
/// and which almost nothing downstream enjoys: a PowerShell started there
/// prints its prompt as `Microsoft.PowerShell.Core\FileSystem::\\?\C:\…`,
/// and the window title reads like a registry key. Everything here wants
/// the plain spelling, so the armour comes off right after it goes on.
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    p
}

/// One row of a directory listing, with what a details view shows about it.
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub modified: Option<std::time::SystemTime>,
    /// Zero for directories — Explorer leaves their size column blank too,
    /// because answering it honestly means walking the whole tree.
    pub size: u64,
}

/// Everything directly inside `dir`, folders first, each name once — the
/// order and the columns of an Explorer details view.
///
/// Nothing is hidden, not even `.git` or `target`: a browser that quietly
/// omits a folder you can see in Explorer is one you stop trusting. This
/// pays a stat per row where the tree deliberately doesn't — the columns
/// are the point here, and it is one directory, not a walk.
pub fn listing(dir: &Path) -> Vec<Entry> {
    let Ok(rows) = read_sorted(dir) else { return Vec::new() };
    rows.into_iter()
        .map(|(name, path, is_dir)| {
            let meta = std::fs::metadata(&path).ok();
            Entry {
                name,
                is_dir,
                modified: meta.as_ref().and_then(|m| m.modified().ok()),
                size: if is_dir { 0 } else { meta.map(|m| m.len()).unwrap_or(0) },
            }
        })
        .collect()
}

/// The drive roots that exist right now, `C:\` first.
///
/// Probed rather than remembered: a USB stick plugged in after startup is
/// exactly the drive someone is about to want.
#[cfg(windows)]
pub fn drives() -> Vec<PathBuf> {
    (b'A'..=b'Z')
        .map(|l| PathBuf::from(format!("{}:\\", l as char)))
        .filter(|p| p.is_dir())
        .collect()
}

/// The places a Linux desktop calls "this PC": the root, then whatever is
/// mounted where removable media lands — `/run/media/<user>`, `/media`,
/// `/mnt` — each mount as its own row, the way a file manager's sidebar
/// lists them. Probed on every open, like the drive letters.
#[cfg(not(windows))]
pub fn drives() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("/")];
    let mut bays = vec![PathBuf::from("/media"), PathBuf::from("/mnt")];
    if let Some(user) = std::env::var_os("USER") {
        bays.insert(0, PathBuf::from("/run/media").join(user));
    }
    for bay in bays {
        let Ok(entries) = std::fs::read_dir(&bay) else { continue };
        let mut mounts: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        mounts.sort();
        out.extend(mounts);
    }
    out
}

/// The project `start` belongs to, if any.
///
/// Walking up beats making people declare a root: `kubide` from
/// `target\release` or from a crate's own directory means the project, not
/// the folder the shell happened to be sitting in.
///
/// Where you are wins when where you are is already a project — asking for a
/// folder that carries its own manifest and being handed its parent would be
/// the same disrespect in the other direction. Above that the marks are tried
/// in order rather than nearest-first: a crate inside a cargo workspace has a
/// `Cargo.toml` of its own, so nearest-first would cut the project in half.
/// A `.kubide` beats a repository because someone put it there on purpose,
/// and a repository beats a manifest because the repository is the project.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    const MANIFESTS: [&str; 4] = ["Cargo.toml", "package.json", "go.mod", "pyproject.toml"];
    let marked = |dir: &Path| {
        // `.git` is a file in a worktree and a directory everywhere else.
        dir.join(".kubide").is_dir()
            || dir.join(".git").exists()
            || MANIFESTS.iter().any(|m| dir.join(m).is_file())
    };
    if marked(start) {
        return Some(start.to_path_buf());
    }
    for mark in [".kubide", ".git"] {
        if let Some(dir) = start.ancestors().find(|d| d.join(mark).exists()) {
            return Some(dir.to_path_buf());
        }
    }
    start
        .ancestors()
        .find(|d| MANIFESTS.iter().any(|m| d.join(m).is_file()))
        .map(Path::to_path_buf)
}

/// Short labels for a list of paths, grown until no two read the same.
///
/// A list of remembered projects is a list of last segments — `kubide`,
/// `promptly-app` — right up until two of them are called `release`, and then
/// the list is asking someone to pick between two identical rows. Only the
/// rows that actually clash take a parent, so one awkward pair does not push
/// a path onto every other line.
///
/// Comparison is case-insensitive: `Release` and `release` are the same row to
/// the eye, and the eye is what this is for.
pub fn distinct_labels(paths: &[PathBuf]) -> Vec<String> {
    // How many trailing segments each label shows. Grows only where needed.
    let mut depth = vec![1usize; paths.len()];
    let segments: Vec<Vec<String>> = paths
        .iter()
        .map(|p| {
            // The root separator is dropped: on Windows `C:\a` is prefix,
            // root, name, and keeping the root would join back as `C:\\a`.
            // The prefix stays, because it is the last thing that can tell
            // two otherwise identical paths apart.
            p.components()
                .filter(|c| !matches!(c, std::path::Component::RootDir))
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect()
        })
        .collect();

    let label_of = |i: usize, depth: &[usize]| -> String {
        let segs = &segments[i];
        let take = depth[i].min(segs.len()).max(1);
        segs[segs.len().saturating_sub(take)..].join(std::path::MAIN_SEPARATOR_STR)
    };

    // Bounded by the deepest path: every round grows a clashing row by one
    // segment, and a row that has run out of parents stops growing.
    let rounds = segments.iter().map(Vec::len).max().unwrap_or(1);
    for _ in 0..rounds {
        let labels: Vec<String> = (0..paths.len()).map(|i| label_of(i, &depth)).collect();
        let mut grew = false;
        for i in 0..paths.len() {
            let clashes = labels
                .iter()
                .enumerate()
                .any(|(j, l)| j != i && l.eq_ignore_ascii_case(&labels[i]));
            if clashes && depth[i] < segments[i].len() {
                depth[i] += 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    (0..paths.len()).map(|i| label_of(i, &depth)).collect()
}

/// Nerd Font glyph for a name. Falls back to a plain file glyph, so a missing
/// mapping looks ordinary rather than broken.
pub fn icon(name: &str, is_dir: bool, open: bool) -> char {
    if is_dir {
        return if open { '\u{f07c}' } else { '\u{f07b}' };
    }
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "rs" => '\u{e7a8}',
        "toml" | "ini" | "cfg" | "conf" => '\u{e615}',
        "md" => '\u{f48a}',
        "json" => '\u{e60b}',
        "lock" => '\u{f023}',
        "ps1" | "sh" | "bat" | "cmd" => '\u{f489}',
        "png" | "jpg" | "jpeg" | "gif" | "ico" | "svg" | "webp" => '\u{f1c5}',
        "zip" | "gz" | "7z" | "rar" | "tar" => '\u{f1c6}',
        "exe" | "dll" | "pdb" => '\u{f085}',
        "txt" | "log" => '\u{f15c}',
        _ if lower.starts_with(".git") => '\u{e702}',
        _ => '\u{f15b}',
    }
}

/// Which marker set a font can actually draw.
///
/// The icons above are Nerd Font private-use codepoints. A font without them
/// draws a notdef box for every row, and a column of boxes reads as a broken
/// editor rather than as a missing font — so the set is chosen from what the
/// font has, not from what we hoped for. The caller measures; see
/// `kb_text::TextEngine::has_glyph`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icons {
    /// Nerd Font: a distinct glyph per file type.
    Nerd,
    /// Triangles. No file-type detail — one character cannot carry it — but an
    /// expander column is what a tree view reads as anyway. Cascadia Code and
    /// Consolas both have `▸`/`▾`; Courier New and Lucida Console do not,
    /// which is why there is a tier below this one.
    Shapes,
    /// For a font with neither. Nothing here can fail to render.
    Ascii,
}

impl Icons {
    /// The marker for one row.
    pub fn of(self, name: &str, is_dir: bool, open: bool) -> char {
        match self {
            Icons::Nerd => icon(name, is_dir, open),
            Icons::Shapes if is_dir => {
                if open {
                    '\u{25be}'
                } else {
                    '\u{25b8}'
                }
            }
            Icons::Ascii if is_dir => {
                if open {
                    '-'
                } else {
                    '+'
                }
            }
            // Files get nothing. The column exists to make directories stand
            // out; a marker on every row would only be noise, and without a
            // Nerd Font there is no per-type marker worth drawing anyway.
            _ => ' ',
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The three label tests spell their paths with drive letters and
    // backslashes, which only parse as roots and separators on Windows; the
    // labelling itself is the same on both.
    #[test]
    #[cfg(windows)]
    fn labels_grow_only_where_they_clash() {
        let paths: Vec<PathBuf> = [
            r"C:\work\kubide",
            r"C:\3d-kubi\release",
            r"C:\rust-kubi\release",
            r"C:\Users\me\Documents",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(
            distinct_labels(&paths),
            [r"kubide", r"3d-kubi\release", r"rust-kubi\release", r"Documents"],
            "only the two `release` rows pay for the ambiguity"
        );
    }

    #[test]
    #[cfg(windows)]
    fn labels_stop_growing_when_the_parents_run_out() {
        // Same last segment, same parent, different roots: the labels can only
        // separate at the drive, and must not loop trying to go further.
        let paths: Vec<PathBuf> =
            [r"C:\a\release", r"D:\a\release"].iter().map(PathBuf::from).collect();
        assert_eq!(distinct_labels(&paths), [r"C:\a\release", r"D:\a\release"]);

        // Genuinely identical paths cannot be told apart; the answer is the
        // full path twice, not an infinite loop.
        let same: Vec<PathBuf> = [r"C:\a", r"C:\a"].iter().map(PathBuf::from).collect();
        assert_eq!(distinct_labels(&same), [r"C:\a", r"C:\a"]);
    }

    #[test]
    #[cfg(windows)]
    fn a_drive_root_labels_itself_without_the_slash() {
        let paths: Vec<PathBuf> = [r"C:\", r"D:\"].iter().map(PathBuf::from).collect();
        assert_eq!(distinct_labels(&paths), ["C:", "D:"]);
    }

    /// Builds a small tree on disk. Real files rather than a mock: the thing
    /// being tested is how std::fs actually behaves, sorting included.
    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kubide-fs-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src").join("deep")).unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("README.md"), "").unwrap();
        std::fs::write(dir.join("src").join("main.rs"), "").unwrap();
        std::fs::write(dir.join("src").join("deep").join("mod.rs"), "").unwrap();
        dir
    }

    fn names(t: &FileTree) -> Vec<&str> {
        t.rows().iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn directories_come_first_then_files_alphabetically() {
        let t = FileTree::new(fixture("sort"));
        assert_eq!(names(&t), ["assets", "src", "Cargo.toml", "README.md"]);
    }

    #[test]
    fn the_cursor_does_not_open_on_dot_git() {
        let dir = fixture("dotgit");
        std::fs::create_dir_all(dir.join(".git").join("objects")).unwrap();
        let t = FileTree::new(&dir);
        assert_eq!(t.rows()[0].name, ".git", "still listed, still first");
        assert_eq!(t.selected_row().unwrap().name, "assets", "but not handed the cursor");

        // And the same on a workspace switch, which is the common way in.
        let mut t = FileTree::new(dir.join("src"));
        t.set_root(&dir);
        assert_eq!(t.selected_row().unwrap().name, "assets");
    }

    #[test]
    fn nothing_is_expanded_at_first() {
        // Reading recursively up front would walk target/ and take seconds.
        let t = FileTree::new(fixture("lazy"));
        assert!(t.rows().iter().all(|r| !r.open));
        assert_eq!(t.rows().len(), 4);
    }

    #[test]
    fn expanding_inserts_children_below_the_parent() {
        let mut t = FileTree::new(fixture("expand"));
        t.select_path(&t.root().join("src"));
        assert!(t.toggle_selected());
        assert_eq!(names(&t), ["assets", "src", "deep", "main.rs", "Cargo.toml", "README.md"]);
        assert_eq!(t.rows()[2].depth, 1);
    }

    #[test]
    fn the_selection_stays_on_the_toggled_directory() {
        // Rows are rebuilt from scratch, so an index-based selection would
        // silently drift onto a different entry.
        let mut t = FileTree::new(fixture("keep"));
        let src = t.root().join("src");
        t.select_path(&src);
        t.toggle_selected();
        assert_eq!(t.selected_row().unwrap().path, src);
    }

    #[test]
    fn collapsing_removes_the_children_again() {
        let mut t = FileTree::new(fixture("collapse"));
        t.select_path(&t.root().join("src"));
        t.toggle_selected();
        t.toggle_selected();
        assert_eq!(names(&t), ["assets", "src", "Cargo.toml", "README.md"]);
    }

    #[test]
    fn toggling_a_file_reports_that_it_is_not_a_directory() {
        let mut t = FileTree::new(fixture("file"));
        t.select_path(&t.root().join("README.md"));
        assert!(!t.toggle_selected(), "a file has nothing to expand");
    }

    #[test]
    fn left_collapses_then_walks_up() {
        let mut t = FileTree::new(fixture("left"));
        let src = t.root().join("src");
        t.select_path(&src);
        t.toggle_selected();
        t.select_path(&src.join("main.rs"));

        // From a child, Left goes to the parent and closes it.
        t.collapse_or_parent();
        assert_eq!(t.selected_row().unwrap().path, src);
        assert!(!t.selected_row().unwrap().open);
    }

    #[test]
    fn selection_stays_inside_the_list() {
        let mut t = FileTree::new(fixture("bounds"));
        t.move_selection(-10);
        assert_eq!(t.selected(), 0);
        t.move_selection(1000);
        assert_eq!(t.selected(), t.rows().len() - 1);
    }

    #[test]
    fn an_unreadable_root_is_reported_not_swallowed() {
        let t = FileTree::new(std::env::temp_dir().join("kubide-does-not-exist"));
        assert!(t.rows().is_empty());
        assert!(t.problem().is_some(), "the user must be told why it's empty");
    }

    #[test]
    fn refresh_picks_up_a_new_file_and_keeps_the_selection() {
        let dir = fixture("refresh");
        let mut t = FileTree::new(&dir);
        t.select_path(&dir.join("README.md"));

        std::fs::write(dir.join("AAA.md"), "").unwrap();
        t.refresh();

        assert!(names(&t).contains(&"AAA.md"));
        assert_eq!(t.selected_row().unwrap().name, "README.md");
    }

    mod verbatim {
        use super::super::strip_verbatim;
        use std::path::PathBuf;

        #[test]
        fn the_extended_length_armour_comes_off() {
            assert_eq!(
                strip_verbatim(PathBuf::from(r"\\?\C:\Users\k\Documents")),
                PathBuf::from(r"C:\Users\k\Documents")
            );
        }

        #[test]
        fn unc_shares_keep_their_two_slashes() {
            assert_eq!(
                strip_verbatim(PathBuf::from(r"\\?\UNC\server\share\dir")),
                PathBuf::from(r"\\server\share\dir")
            );
        }

        #[test]
        fn plain_paths_pass_through() {
            for p in [r"C:\plain\path", "relative/path", ""] {
                assert_eq!(strip_verbatim(PathBuf::from(p)), PathBuf::from(p));
            }
        }
    }

    mod places {
        use super::super::find_root;
        use std::path::PathBuf;

        fn base(name: &str) -> PathBuf {
            let d = std::env::temp_dir().join(format!("kubide-places-{name}"));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        #[test]
        fn a_folder_that_is_already_a_project_is_the_root() {
            // Naming a crate must not hand back its parent workspace.
            let d = base("self");
            std::fs::create_dir_all(d.join("crate")).unwrap();
            std::fs::write(d.join("Cargo.toml"), "").unwrap();
            std::fs::write(d.join("crate").join("Cargo.toml"), "").unwrap();
            assert_eq!(find_root(&d.join("crate")), Some(d.join("crate")));
        }

        #[test]
        fn a_repository_beats_a_manifest_further_down() {
            // The case this pins: a build directory inside a cargo workspace
            // used to root wherever the shell was standing.
            let d = base("repo");
            std::fs::create_dir_all(d.join(".git")).unwrap();
            std::fs::write(d.join("Cargo.toml"), "").unwrap();
            std::fs::create_dir_all(d.join("crates").join("one")).unwrap();
            std::fs::write(d.join("crates").join("one").join("Cargo.toml"), "").unwrap();
            std::fs::create_dir_all(d.join("target").join("release")).unwrap();
            assert_eq!(find_root(&d.join("target").join("release")), Some(d.clone()));
        }

        #[test]
        fn a_mark_put_there_on_purpose_wins() {
            let d = base("mark");
            std::fs::create_dir_all(d.join(".kubide")).unwrap();
            std::fs::create_dir_all(d.join("sub").join(".git")).unwrap();
            std::fs::create_dir_all(d.join("sub").join("deep")).unwrap();
            // `sub` is its own repository, so naming it answers for itself...
            assert_eq!(find_root(&d.join("sub")), Some(d.join("sub")));
            // ...but from below, the deliberate mark outranks the repository
            // it contains. Marking a folder is the way to say "this one".
            assert_eq!(find_root(&d.join("sub").join("deep")), Some(d.clone()));
        }

        #[test]
        fn an_unmarked_folder_is_not_its_own_project() {
            let d = base("bare");
            let empty = d.join("empty");
            std::fs::create_dir_all(&empty).unwrap();
            // Whatever sits above the temp directory is not this test's
            // business; what matters is that nothing here invented a root.
            let found = find_root(&empty);
            assert!(found != Some(empty) && found != Some(d), "{found:?}");
        }
    }

    mod file_ops {
        use super::super::ops;
        use std::path::PathBuf;

        fn dir(name: &str) -> PathBuf {
            let d = std::env::temp_dir().join(format!("kubide-ops-{name}"));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        #[test]
        fn creating_refuses_to_clobber() {
            // There is no undo for an overwritten file.
            let d = dir("create");
            let f = d.join("a.txt");
            std::fs::write(&f, "important").unwrap();
            assert!(ops::create_file(&f).is_err());
            assert_eq!(std::fs::read_to_string(&f).unwrap(), "important");
        }

        #[test]
        fn creating_makes_missing_parents() {
            let d = dir("parents");
            let f = d.join("deep").join("nested").join("a.txt");
            ops::create_file(&f).unwrap();
            assert!(f.exists());
        }

        #[test]
        fn renaming_refuses_to_overwrite() {
            // Windows rename replaces silently, which is a loss you cannot
            // take back.
            let d = dir("rename");
            let a = d.join("a.txt");
            let b = d.join("b.txt");
            std::fs::write(&a, "a").unwrap();
            std::fs::write(&b, "b").unwrap();
            assert!(ops::rename(&a, "b.txt").is_err());
            assert_eq!(std::fs::read_to_string(&b).unwrap(), "b");
        }

        #[test]
        fn renaming_rejects_path_separators() {
            // Renaming is not moving; a slash would relocate the file
            // somewhere the user is not looking.
            let d = dir("slash");
            let a = d.join("a.txt");
            std::fs::write(&a, "").unwrap();
            assert!(ops::rename(&a, "sub/a.txt").unwrap_err().contains("separator"));
            assert!(ops::rename(&a, "").unwrap_err().contains("required"));
        }

        #[test]
        fn renaming_to_the_same_name_is_not_an_error() {
            let d = dir("same");
            let a = d.join("a.txt");
            std::fs::write(&a, "x").unwrap();
            assert_eq!(ops::rename(&a, "a.txt").unwrap(), a);
            assert!(a.exists());
        }

        #[test]
        fn a_non_empty_directory_is_refused() {
            // One keystroke must not be able to take a whole tree with it.
            let d = dir("rmdir");
            let sub = d.join("sub");
            std::fs::create_dir(&sub).unwrap();
            std::fs::write(sub.join("keep.txt"), "").unwrap();
            assert!(ops::delete(&sub).unwrap_err().contains("not empty"));
            assert!(sub.exists());
        }

        #[test]
        fn empty_directories_and_files_delete() {
            let d = dir("delete");
            let f = d.join("a.txt");
            let sub = d.join("empty");
            std::fs::write(&f, "").unwrap();
            std::fs::create_dir(&sub).unwrap();
            ops::delete(&f).unwrap();
            ops::delete(&sub).unwrap();
            assert!(!f.exists() && !sub.exists());
        }
    }

    #[test]
    fn directory_icons_show_open_state() {
        assert_ne!(icon("src", true, true), icon("src", true, false));
        assert_eq!(icon("main.rs", false, false), '\u{e7a8}');
    }

    #[test]
    fn every_set_tells_open_from_closed_and_a_directory_from_a_file() {
        // Whatever the font can draw, these two distinctions have to survive;
        // they are the only thing the column is for.
        for set in [Icons::Nerd, Icons::Shapes, Icons::Ascii] {
            assert_ne!(set.of("src", true, true), set.of("src", true, false), "{set:?}");
            assert_ne!(set.of("src", true, false), set.of("a.rs", false, false), "{set:?}");
        }
    }

    #[test]
    fn the_ascii_set_cannot_fail_to_render() {
        // It is the last resort, reached because the font turned out not to
        // have what we wanted. Reaching for anything non-ASCII here would be
        // the same bet again with worse odds.
        for c in [
            Icons::Ascii.of("src", true, true),
            Icons::Ascii.of("src", true, false),
            Icons::Ascii.of("a.rs", false, false),
        ] {
            assert!(c.is_ascii(), "{c:?} is not ASCII");
        }
    }
}
