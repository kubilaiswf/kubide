//! Snippets: trigger words the editor expands on Tab, per file extension.
//!
//! One TOML file per extension in `%APPDATA%\kubide\snippets` — `rs.toml`,
//! `py.toml` — each a flat table of `trigger = "body"`, with `$0` marking
//! where the caret lands. That is the entire format; there is deliberately
//! no second feature. The folder is seeded with starters the same way the
//! themes folder is, and a user's file wins over the built-ins per trigger,
//! so adding one snippet does not cost the rest.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Trigger to body, per lowercase file extension.
pub struct Snippets {
    by_ext: HashMap<String, HashMap<String, String>>,
}

/// The starters. Small on purpose: these are the proof that the feature
/// works and the file people copy their own from, not a snippet library.
const BUILTIN: &[(&str, &[(&str, &str)])] = &[
    (
        "rs",
        &[
            ("print", "println!(\"$0\");"),
            ("eprint", "eprintln!(\"$0\");"),
            ("dbg", "dbg!($0);"),
            ("main", "fn main() {\n    $0\n}"),
            ("test", "#[test]\nfn $0() {\n}"),
            ("derive", "#[derive($0)]"),
        ],
    ),
    (
        "c",
        &[
            ("print", "printf(\"$0\\n\");"),
            ("main", "int main(void) {\n    $0\n    return 0;\n}"),
            ("inc", "#include <$0>"),
        ],
    ),
    (
        "cpp",
        &[
            ("print", "std::cout << $0 << '\\n';"),
            ("main", "int main() {\n    $0\n    return 0;\n}"),
            ("inc", "#include <$0>"),
        ],
    ),
    (
        "py",
        &[
            ("print", "print($0)"),
            ("main", "if __name__ == \"__main__\":\n    $0"),
            ("def", "def $0():"),
        ],
    ),
    (
        "js",
        &[
            ("print", "console.log($0);"),
            ("log", "console.log($0);"),
            ("fn", "function $0() {\n}"),
        ],
    ),
    (
        "ts",
        &[
            ("print", "console.log($0);"),
            ("log", "console.log($0);"),
            ("fn", "function $0() {\n}"),
        ],
    ),
];

/// `snippets` beside the config file, like the themes folder.
pub fn snippets_dir() -> PathBuf {
    crate::config_path()
        .parent()
        .map(|p| p.join("snippets"))
        .unwrap_or_else(|| PathBuf::from("snippets"))
}

/// Writes the starter files — only the missing ones, because a seeded file
/// is the user's the moment they touch it.
pub fn seed_snippets() {
    let dir = snippets_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (ext, entries) in BUILTIN {
        let path = dir.join(format!("{ext}.toml"));
        if path.exists() {
            continue;
        }
        let map: HashMap<&str, &str> = entries.iter().copied().collect();
        let Ok(body) = toml::to_string(&map) else { continue };
        let header = "# kubide snippets for this file extension.\n\
                      # trigger = \"body\" — type the trigger in the editor and press Tab.\n\
                      # $0 marks where the caret lands. \\n makes a new line.\n\n";
        let _ = std::fs::write(path, format!("{header}{body}"));
    }
}

/// The built-ins with the folder's files laid over them, per trigger.
pub fn load() -> Snippets {
    load_in(&snippets_dir())
}

pub(crate) fn load_in(dir: &Path) -> Snippets {
    let mut by_ext: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (ext, entries) in BUILTIN {
        let map = by_ext.entry(ext.to_string()).or_default();
        for (trigger, body) in *entries {
            map.insert(trigger.to_string(), body.to_string());
        }
    }
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "toml") {
                continue;
            }
            let Some(ext) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            // A broken file costs its own entries and nothing else; the
            // editor must not refuse to start over a snippet typo.
            let Ok(map) = toml::from_str::<HashMap<String, String>>(&text) else { continue };
            by_ext.entry(ext.to_lowercase()).or_default().extend(map);
        }
    }
    Snippets { by_ext }
}

impl Snippets {
    pub fn get(&self, ext: &str, trigger: &str) -> Option<&str> {
        self.by_ext.get(ext)?.get(trigger).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kubide-snip-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_builtins_answer_without_any_files() {
        let s = load_in(&empty_dir("builtin"));
        assert_eq!(s.get("rs", "print"), Some("println!(\"$0\");"));
        assert_eq!(s.get("py", "print"), Some("print($0)"));
        assert_eq!(s.get("rs", "nope"), None);
        assert_eq!(s.get("zig", "print"), None, "no table for an unknown extension");
    }

    #[test]
    fn a_user_file_overrides_per_trigger_not_per_file() {
        // One custom snippet must not delete the rest of the language.
        let d = empty_dir("overlay");
        std::fs::write(d.join("rs.toml"), "print = \"say!($0)\"\nmine = \"custom\"").unwrap();
        let s = load_in(&d);
        assert_eq!(s.get("rs", "print"), Some("say!($0)"));
        assert_eq!(s.get("rs", "mine"), Some("custom"));
        assert_eq!(s.get("rs", "dbg"), Some("dbg!($0);"), "built-ins survive around it");
    }

    #[test]
    fn a_broken_file_costs_only_itself() {
        let d = empty_dir("broken");
        std::fs::write(d.join("rs.toml"), "not toml ][").unwrap();
        let s = load_in(&d);
        assert_eq!(s.get("rs", "print"), Some("println!(\"$0\");"));
    }

    #[test]
    fn every_builtin_body_is_sane() {
        // At most one caret marker; a body with two would drop the second
        // silently and teach the format wrong.
        for (ext, entries) in BUILTIN {
            for (trigger, body) in *entries {
                assert!(body.matches("$0").count() <= 1, "{ext}/{trigger}");
                assert!(!trigger.is_empty() && !body.is_empty(), "{ext}/{trigger}");
            }
        }
    }
}
