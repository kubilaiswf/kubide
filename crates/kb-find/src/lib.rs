//! Fuzzy matching for the file finder.
//!
//! The pattern has to appear in order but not consecutively, so `krs` finds
//! `kubide/render/scene.rs`. What separates a good finder from an irritating
//! one is entirely the ranking, so the scoring rules are spelled out here
//! rather than buried in a dependency:
//!
//! - a run of consecutive characters beats scattered ones
//! - matching the start of a word or a path segment beats matching the middle
//! - matching in the file name beats matching in the directory
//! - shorter candidates win ties, because they are more likely what you meant
//!
//! Positions are character indices, not bytes, so the caller can underline the
//! matched characters without shifting them on a non-ASCII path.

/// One ranked candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Index into the list that was searched.
    pub index: usize,
    pub score: i32,
    /// Character positions that matched, in order.
    pub positions: Vec<usize>,
}

const BONUS_CONSECUTIVE: i32 = 12;
const BONUS_WORD_START: i32 = 10;
const BONUS_AFTER_SEPARATOR: i32 = 14;
/// Matching inside the file name rather than a parent directory.
const BONUS_IN_NAME: i32 = 8;
/// Charged per skipped character, so scattered matches sink.
const PENALTY_GAP: i32 = 2;

/// Scores one candidate. `None` when the pattern isn't a subsequence of it.
///
/// Smart case: an all-lowercase pattern ignores case, a pattern with any
/// uppercase respects it. Typing `readme` should find `README.md`, but typing
/// `README` should not have to wade through every `readme`.
pub fn score(pattern: &str, text: &str) -> Option<Match> {
    if pattern.is_empty() {
        return Some(Match { index: 0, score: 0, positions: Vec::new() });
    }
    let sensitive = pattern.chars().any(char::is_uppercase);
    let chars: Vec<char> = text.chars().collect();
    // Where the file name starts, for the in-name bonus.
    let name_start = chars
        .iter()
        .rposition(|c| *c == '/' || *c == '\\')
        .map(|i| i + 1)
        .unwrap_or(0);

    let mut positions = Vec::new();
    let mut total = 0;
    let mut at = 0usize;
    let mut previous: Option<usize> = None;

    for want in pattern.chars() {
        let found = chars[at..].iter().position(|c| eq(*c, want, sensitive))? + at;

        let mut points = 1;
        if previous == Some(found.saturating_sub(1)) && found > 0 {
            points += BONUS_CONSECUTIVE;
        }
        if found == 0 {
            points += BONUS_WORD_START;
        } else {
            let before = chars[found - 1];
            if before == '/' || before == '\\' {
                points += BONUS_AFTER_SEPARATOR;
            } else if before == '_' || before == '-' || before == '.' || before == ' ' {
                points += BONUS_WORD_START;
            } else if before.is_lowercase() && chars[found].is_uppercase() {
                // camelCase boundary.
                points += BONUS_WORD_START;
            }
        }
        if found >= name_start {
            points += BONUS_IN_NAME;
        }
        if let Some(p) = previous {
            points -= (found - p - 1).min(10) as i32 * PENALTY_GAP;
        }

        total += points;
        positions.push(found);
        previous = Some(found);
        at = found + 1;
    }

    // Shorter candidates win ties: with `main` matching both `main.rs` and
    // `domain/maintenance.rs`, the short one is nearly always the intent.
    total -= (chars.len() / 8) as i32;

    Some(Match { index: 0, score: total, positions })
}

/// Every place `needle` occurs in `text`, in character positions.
///
/// Literal, and deliberately so. Finding text in a file is a different
/// question from finding a file by name, and answering it with the fuzzy
/// matcher above gives nonsense: searching a document for `kubi` returns a
/// line reading "KB number, or doc link" because those letters appear along it
/// in order. Here, `kubi` means `kubi`.
///
/// Same smart case as [`score`]: an all-lowercase needle ignores case, one
/// with an uppercase letter respects it.
///
/// Characters, not bytes, all the way through — the caller uses these both to
/// underline the match and to put the caret on it, and `str::to_lowercase`
/// would have made that unsafe anyway: Turkish `İ` lowercases into two
/// characters, which slides every position after it.
pub fn occurrences(needle: &str, text: &str) -> Vec<usize> {
    let pattern: Vec<char> = needle.chars().collect();
    let hay: Vec<char> = text.chars().collect();
    if pattern.is_empty() || pattern.len() > hay.len() {
        return Vec::new();
    }
    let sensitive = pattern.iter().copied().any(char::is_uppercase);

    let mut out = Vec::new();
    let mut at = 0;
    while at + pattern.len() <= hay.len() {
        if (0..pattern.len()).all(|k| eq(hay[at + k], pattern[k], sensitive)) {
            out.push(at);
            // Non-overlapping: `aa` occurs twice in `aaaa`, not three times.
            // Overlapping hits would put two of them under one highlight.
            at += pattern.len();
        } else {
            at += 1;
        }
    }
    out
}

fn eq(a: char, b: char, sensitive: bool) -> bool {
    if sensitive {
        a == b
    } else {
        a.eq_ignore_ascii_case(&b) || a.to_lowercase().eq(b.to_lowercase())
    }
}

/// Ranks a list, best first. `limit` caps the result, not the work.
///
/// An empty pattern returns everything in its original order, which is what
/// makes the finder useful the moment it opens rather than after a keystroke.
pub fn rank(pattern: &str, items: &[String], limit: usize) -> Vec<Match> {
    if pattern.is_empty() {
        return items
            .iter()
            .enumerate()
            .take(limit)
            .map(|(index, _)| Match { index, score: 0, positions: Vec::new() })
            .collect();
    }

    let mut out: Vec<Match> = items
        .iter()
        .enumerate()
        .filter_map(|(index, text)| score(pattern, text).map(|m| Match { index, ..m }))
        .collect();

    // Sorted by score, then by index, so equal scores keep a stable order
    // instead of shuffling as you type.
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn best(pattern: &str, items: &[&str]) -> String {
        let owned: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        let ranked = rank(pattern, &owned, 10);
        items[ranked.first().expect("something should match").index].to_string()
    }

    #[test]
    fn characters_may_be_scattered() {
        assert!(score("krs", "kubide/render/scene.rs").is_some());
    }

    #[test]
    fn a_literal_search_refuses_the_scattered_match() {
        // The bug this pins. Find-in-file used the fuzzy matcher, so searching
        // a document for `kubi` returned "the issue number, KB number, or doc
        // link" — k, u, b, i, in order, spread across the whole line.
        assert!(score("kubi", "the issue number, KB number, or doc link").is_some());
        assert!(occurrences("kubi", "the issue number, KB number, or doc link").is_empty());
        assert_eq!(occurrences("kubi", "kubide exists to be native"), vec![0]);
    }

    #[test]
    fn every_occurrence_is_reported_in_order() {
        assert_eq!(occurrences("ab", "ab_ab_ab"), vec![0, 3, 6]);
        assert_eq!(occurrences("x", "axbxc"), vec![1, 3]);
    }

    #[test]
    fn repeats_do_not_overlap() {
        // Otherwise two hits share characters and their highlights collide.
        assert_eq!(occurrences("aa", "aaaa"), vec![0, 2]);
        assert_eq!(occurrences("aa", "aaa"), vec![0]);
    }

    #[test]
    fn literal_search_is_smart_case_too() {
        assert_eq!(occurrences("todo", "TODO and todo"), vec![0, 9]);
        // An uppercase letter in the needle means you meant it.
        assert_eq!(occurrences("TODO", "TODO and todo"), vec![0]);
    }

    #[test]
    fn a_literal_position_is_a_character_not_a_byte() {
        // The caller puts the caret on this number. Counting bytes would land
        // it several columns to the right of the match on any non-ASCII line.
        assert_eq!(occurrences("son", "ilk satır son"), vec![10]);
        assert_eq!("ilk satır ".chars().count(), 10);
    }

    #[test]
    fn nothing_to_find_finds_nothing() {
        assert!(occurrences("", "anything").is_empty());
        assert!(occurrences("longer than the text", "short").is_empty());
        assert!(occurrences("a", "").is_empty());
    }

    #[test]
    fn out_of_order_does_not_match() {
        assert!(score("sr", "rs").is_none());
    }

    #[test]
    fn consecutive_beats_scattered() {
        assert_eq!(best("main", &["m_a_i_n.rs", "main.rs"]), "main.rs");
    }

    #[test]
    fn the_file_name_beats_the_directory() {
        assert_eq!(
            best("config", &["config/deep/other.rs", "src/config.rs"]),
            "src/config.rs"
        );
    }

    #[test]
    fn a_path_segment_start_beats_the_middle() {
        assert_eq!(best("term", &["src/subterm.rs", "src/term.rs"]), "src/term.rs");
    }

    #[test]
    fn shorter_wins_a_tie() {
        assert_eq!(best("main", &["domain/maintenance.rs", "main.rs"]), "main.rs");
    }

    #[test]
    fn lowercase_ignores_case() {
        assert!(score("readme", "README.md").is_some());
    }

    #[test]
    fn uppercase_demands_it() {
        // Typing README should not wade through every readme.
        assert!(score("README", "readme.md").is_none());
        assert!(score("README", "README.md").is_some());
    }

    #[test]
    fn positions_are_characters_not_bytes() {
        // The caller underlines these; byte offsets would shift the underline.
        let m = score("ş", "ğüş.rs").unwrap();
        assert_eq!(m.positions, [2]);
    }

    #[test]
    fn an_empty_pattern_lists_everything_in_order() {
        let items: Vec<String> = ["b.rs", "a.rs"].iter().map(|s| s.to_string()).collect();
        let r = rank("", &items, 10);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].index, 0, "original order, not sorted");
    }

    #[test]
    fn equal_scores_keep_a_stable_order() {
        // Otherwise the list reshuffles under the cursor as you type.
        let items: Vec<String> = ["a/x.rs", "b/x.rs", "c/x.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let first = rank("x", &items, 10);
        let again = rank("x", &items, 10);
        assert_eq!(first, again);
    }

    #[test]
    fn the_limit_caps_the_result() {
        let items: Vec<String> = (0..100).map(|i| format!("file{i}.rs")).collect();
        assert_eq!(rank("file", &items, 5).len(), 5);
    }
}
