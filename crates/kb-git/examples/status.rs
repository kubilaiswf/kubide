//! Prints what kubide sees, without opening a window.
//!
//! Same idea as `kb-term`'s dump: when the explorer shows the wrong colors,
//! this says whether the problem is in reading git or in drawing.
//!
//!     cargo run -p kb-git --example status

use std::time::{Duration, Instant};

fn main() {
    let dir = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let mut git = kb_git::Git::discover(&dir);
    let Some(root) = git.root().map(|p| p.to_path_buf()) else {
        println!("not a git repository: {}", dir.display());
        return;
    };
    println!("repo: {}", root.display());

    // discover() starts a refresh on a background thread; wait for it here,
    // which is exactly what the UI does not do.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if git.poll() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let snap = git.snapshot();
    println!("branch: {}", snap.branch.as_deref().unwrap_or("(unknown)"));
    println!("changed files: {}", snap.files.len());

    let mut files: Vec<_> = snap.files.iter().collect();
    files.sort_by_key(|(p, _)| (*p).clone());
    for (path, status) in files.iter().take(40) {
        let rel = path.strip_prefix(&root).unwrap_or(path);
        println!("  {:?}\t{}", status, rel.display());
    }
    if files.len() > 40 {
        println!("  ... {} more", files.len() - 40);
    }
    println!("directories marked: {}", snap.dirs.len());
}
