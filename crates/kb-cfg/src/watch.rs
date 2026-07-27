//! Live config reload.
//!
//! Two things here are not obvious and both cost an afternoon if missed:
//!
//! 1. We watch the config's **parent directory**, not the file. Editors save
//!    by writing a temp file and renaming it over the target, which replaces
//!    the inode — a watch on the file itself goes deaf after the first save.
//! 2. Paths are canonicalized. A symlinked config directory otherwise reports
//!    events under a path that never matches what we compare against.
//!
//! Events are debounced: one save can produce several filesystem events, and
//! reparsing three times per keystroke-save is wasteful and can catch the file
//! mid-write.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult};

/// How long to wait for the writes to settle.
const DEBOUNCE: Duration = Duration::from_millis(200);

pub struct Watcher {
    /// Kept alive; dropping it stops the watch.
    _inner: Box<dyn std::any::Any>,
    rx: Receiver<()>,
}

impl Watcher {
    /// Starts watching the config file. Returns `None` if the directory can't
    /// be watched — reload is a convenience, not a reason to fail startup.
    pub fn new(config_path: &Path) -> Option<Self> {
        let dir = config_path.parent()?.to_path_buf();
        // The directory may not exist yet; creating it means a config saved
        // later is picked up without a restart.
        let _ = std::fs::create_dir_all(&dir);
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let file = canonical_name(config_path);

        let (tx, rx) = channel();
        let mut debouncer = new_debouncer(DEBOUNCE, None, move |res: DebounceEventResult| {
            let Ok(events) = res else { return };
            let hit = events.iter().any(|e| {
                e.paths
                    .iter()
                    .any(|p| p.file_name() == file.as_ref().map(|f| f.as_os_str()))
            });
            if hit {
                // A full channel means a reload is already pending.
                let _ = tx.send(());
            }
        })
        .ok()?;

        debouncer.watch(&dir, RecursiveMode::NonRecursive).ok()?;
        Some(Self {
            _inner: Box::new(debouncer),
            rx,
        })
    }

    /// Whether the config changed since the last call. Drains the queue, so a
    /// burst of events causes exactly one reload.
    pub fn changed(&self) -> bool {
        let mut any = false;
        loop {
            match self.rx.try_recv() {
                Ok(()) => any = true,
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return any,
            }
        }
    }
}

fn canonical_name(p: &Path) -> Option<PathBuf> {
    p.file_name().map(PathBuf::from)
}
