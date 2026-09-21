//! The language servers behind the open editors.
//!
//! One process per language, started the first time a file of that language
//! is shown and kept for the life of the workspace: rust-analyzer takes
//! seconds to load a crate graph, and paying that per file would make the
//! feature slower than not having it. A server that is not installed is
//! tried once and then left alone — no retry loop, no nagging — because the
//! editor without a server is still the editor.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use kb_lsp::{Client, Diagnostic, Event, Options, Pos, Severity};

pub struct Servers {
    root: PathBuf,
    clients: HashMap<&'static str, Client>,
    /// Languages whose server would not start, so they are asked once.
    absent: HashSet<&'static str>,
    /// The current complaints per file, as the servers last published them.
    diagnostics: HashMap<PathBuf, Vec<Diagnostic>>,
    /// Every document a server has been told about.
    open: HashSet<PathBuf>,
    /// Whether each document had unsaved edits at the last tick.
    modified: HashMap<PathBuf, bool>,
    /// What could not be started or has died, waiting to be shown once.
    pub notes: Vec<String>,
}

impl Servers {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            clients: HashMap::new(),
            absent: HashSet::new(),
            diagnostics: HashMap::new(),
            open: HashSet::new(),
            modified: HashMap::new(),
            notes: Vec::new(),
        }
    }

    /// Keeps the server for `lang` in step with a document, starting it if
    /// this is the first file of its language. Cheap when nothing changed.
    pub fn sync(
        &mut self,
        cfg: &kb_cfg::Lsp,
        lang: kb_syn::Lang,
        path: &Path,
        revision: u64,
        text: impl FnOnce() -> String,
    ) {
        if !cfg.enabled {
            return;
        }
        let id = lang.language_id();
        if !self.clients.contains_key(id) {
            if self.absent.contains(id) {
                return;
            }
            let Some(command) = cfg.servers.get(id).filter(|c| !c.is_empty()) else {
                self.absent.insert(id);
                return;
            };
            let options = Options {
                command: command[0].clone(),
                args: command[1..].to_vec(),
                root: self.root.clone(),
                language: id.to_string(),
            };
            match Client::spawn(&options) {
                Ok(client) => {
                    self.clients.insert(id, client);
                }
                // Not installed is the normal case for most languages on
                // most machines, and not worth a word.
                Err(_) => {
                    self.absent.insert(id);
                    return;
                }
            }
        }
        if let Some(client) = self.clients.get_mut(id) {
            client.sync(path, revision, text);
            if !self.open.contains(path) {
                self.open.insert(path.to_path_buf());
            }
        }
    }

    pub fn client(&mut self, lang: kb_syn::Lang) -> Option<&mut Client> {
        self.clients.get_mut(lang.language_id())
    }

    /// Notices a save as the moment a document stops being modified, which
    /// catches every way of saving without a hook in each of them.
    pub fn note_modified(&mut self, lang: kb_syn::Lang, path: &Path, modified: bool) {
        let was = self.modified.insert(path.to_path_buf(), modified).unwrap_or(false);
        if was && !modified {
            if let Some(c) = self.client(lang) {
                c.saved(path);
            }
        }
    }

    /// Tells the servers about files no editor shows any more.
    pub fn close_except(&mut self, open: &[PathBuf]) {
        if self.open.iter().all(|p| open.contains(p)) {
            return;
        }
        let gone: Vec<PathBuf> = self.open.iter().filter(|p| !open.contains(p)).cloned().collect();
        for path in &gone {
            self.open.remove(path);
            for client in self.clients.values_mut() {
                client.close(path);
            }
        }
    }

    /// Takes in what the servers said. Diagnostics are kept here; everything
    /// else is an answer to something the editor asked and is handed back.
    /// The flag says whether anything on screen may have changed.
    pub fn poll(&mut self) -> (Vec<Event>, bool) {
        let mut answers = Vec::new();
        let mut changed = false;
        let mut dead = Vec::new();
        for (id, client) in self.clients.iter_mut() {
            for event in client.poll() {
                changed = true;
                match event {
                    Event::Diagnostics { path, items } => {
                        if items.is_empty() {
                            self.diagnostics.remove(&path);
                        } else {
                            self.diagnostics.insert(path, items);
                        }
                    }
                    Event::Exited(last) => {
                        dead.push(*id);
                        let why = if last.is_empty() { String::new() } else { format!(": {last}") };
                        self.notes.push(format!("the {id} language server stopped{why}"));
                    }
                    Event::Ready => {}
                    other => answers.push(other),
                }
            }
        }
        for id in dead {
            self.clients.remove(id);
            // Not restarted: a server that died once on this workspace will
            // usually die again, and a crash loop is worse than its absence.
            self.absent.insert(id);
        }
        (answers, changed)
    }

    pub fn diagnostics(&self, path: &Path) -> &[Diagnostic] {
        self.diagnostics.get(path).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Errors and warnings across every file, for the status bar.
    pub fn counts(&self) -> (usize, usize) {
        let all = self.diagnostics.values().flatten();
        all.fold((0, 0), |(e, w), d| match d.severity {
            Severity::Error => (e + 1, w),
            Severity::Warning => (e, w + 1),
            _ => (e, w),
        })
    }
}

/// The most serious complaint covering a line, for the message shown while
/// the caret is on it.
pub fn worst_on_line(items: &[Diagnostic], line: usize) -> Option<&Diagnostic> {
    items.iter().filter(|d| d.start.line <= line && line <= d.end.line).min_by_key(|d| d.severity)
}

/// The columns of `line` a diagnostic covers, given that line's length.
/// A complaint spanning several lines covers each of them to its end.
pub fn span_on_line(d: &Diagnostic, line: usize, len: usize) -> Option<(usize, usize)> {
    if line < d.start.line || line > d.end.line {
        return None;
    }
    let from = if line == d.start.line { d.start.col } else { 0 };
    let to = if line == d.end.line { d.end.col } else { len };
    // An empty range still has to be findable: one cell wide.
    Some((from.min(len), to.max(from + 1)))
}

pub fn pos(p: kb_edit::Pos) -> Pos {
    Pos { line: p.line, col: p.col }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(sl: usize, sc: usize, el: usize, ec: usize, severity: Severity) -> Diagnostic {
        Diagnostic {
            start: Pos { line: sl, col: sc },
            end: Pos { line: el, col: ec },
            severity,
            message: String::new(),
            source: None,
        }
    }

    #[test]
    fn a_multi_line_complaint_covers_each_line_it_crosses() {
        let d = diag(2, 4, 4, 1, Severity::Error);
        assert_eq!(span_on_line(&d, 1, 10), None);
        assert_eq!(span_on_line(&d, 2, 10), Some((4, 10)));
        assert_eq!(span_on_line(&d, 3, 10), Some((0, 10)));
        assert_eq!(span_on_line(&d, 4, 10), Some((0, 1)));
        // An empty range is drawn one cell wide rather than not at all.
        assert_eq!(span_on_line(&diag(0, 3, 0, 3, Severity::Hint), 0, 10), Some((3, 4)));
    }

    #[test]
    fn the_error_wins_the_line_over_the_warning() {
        let items = [diag(1, 0, 1, 2, Severity::Warning), diag(1, 5, 1, 6, Severity::Error)];
        assert_eq!(worst_on_line(&items, 1).map(|d| d.severity), Some(Severity::Error));
        assert_eq!(worst_on_line(&items, 2), None);
    }
}
