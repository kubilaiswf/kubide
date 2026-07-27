//! Hot reload, end to end.
//!
//! Worth a slow test: a file watcher that silently stops working looks exactly
//! like one that works, and nothing in the type system says otherwise. The
//! rename case below is the one that actually breaks in the wild — most
//! editors save by writing a temp file and renaming it over the target.

use std::path::PathBuf;
use std::time::{Duration, Instant};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kubide-reload-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Waits for the watcher to report a change. The debounce is 200 ms, so a
/// fixed sleep would either be flaky or needlessly slow.
fn wait_for_change(w: &kb_cfg::Watcher) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if w.changed() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn saving_the_config_is_noticed_and_reparsed() {
    let dir = temp_dir("save");
    let path = dir.join("config.toml");
    std::fs::write(&path, "[font]\nsize = 14.0\n").unwrap();

    let loaded = kb_cfg::load_from(path.clone());
    assert_eq!(loaded.config.font.size, 14.0);
    assert_eq!(loaded.problem, None);

    let watcher = kb_cfg::Watcher::new(&path).expect("watcher should start");
    std::fs::write(&path, "[font]\nsize = 20.0\n").unwrap();

    assert!(wait_for_change(&watcher), "the write was never reported");

    let after = kb_cfg::load_from(path);
    assert_eq!(after.config.font.size, 20.0);
    assert!(after.config.refresh_from(&loaded.config).font);
}

#[test]
fn a_save_by_rename_is_still_noticed() {
    // The reason we watch the directory instead of the file: renaming over the
    // target replaces the inode, and a file watch goes deaf after it.
    let dir = temp_dir("rename");
    let path = dir.join("config.toml");
    std::fs::write(&path, "[font]\nsize = 14.0\n").unwrap();

    let watcher = kb_cfg::Watcher::new(&path).expect("watcher should start");

    let tmp = dir.join("config.toml.tmp");
    std::fs::write(&tmp, "[font]\nsize = 18.0\n").unwrap();
    std::fs::rename(&tmp, &path).unwrap();

    assert!(wait_for_change(&watcher), "the rename was never reported");
    assert_eq!(kb_cfg::load_from(path).config.font.size, 18.0);
}

#[test]
fn a_config_written_later_is_still_watched() {
    // Nothing exists yet — the normal state on a fresh install. Watching must
    // still work, or the first config you write needs a restart.
    let dir = temp_dir("later");
    let path = dir.join("kubide").join("config.toml");

    let watcher = kb_cfg::Watcher::new(&path).expect("watcher should start");
    let loaded = kb_cfg::load_from(path.clone());
    assert_eq!(loaded.problem, None, "a missing file is not a problem");

    std::fs::write(&path, "[window]\nbackdrop = \"mica\"\n").unwrap();
    assert!(wait_for_change(&watcher), "the new file was never reported");
    assert_eq!(
        kb_cfg::load_from(path).config.window.backdrop,
        kb_cfg::Backdrop::Mica
    );
}
