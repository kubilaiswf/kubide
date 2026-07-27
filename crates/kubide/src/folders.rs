//! The folder picker: Explorer's open dialog, stroke for stroke, in the
//! editor's own material.
//!
//! Back, forward and up in the corner; the path in a boxed address bar;
//! a search box that narrows the listing; a details list with name, date,
//! type and size; places down the left; a field and two buttons across the
//! bottom. Nobody has to learn this dialog — Windows already taught it —
//! they only have to recognise it in our colours. (Reusing the real one was
//! tried first: it will not wear our theme, and forcing it through uxtheme's
//! ordinal exports crashed the process. Closed road, not an unexplored one.)
//!
//! Files are listed as well as folders, exactly as the reference dialog
//! does, and Enter on one opens it in the editor — a folder browser that
//! shows you `Cargo.toml` is also telling you which folder you want.

use std::path::{Path, PathBuf};

/// One clickable segment of the address bar.
pub struct Crumb {
    pub name: String,
    /// The path up to and including this segment: a click is a jump.
    pub dir: PathBuf,
}

/// One row, its column texts already formatted for drawing.
#[derive(Clone)]
pub struct Row {
    pub name: String,
    pub is_dir: bool,
    /// "26.07.2026 10:33", local time, or empty when the file system
    /// declined to say.
    pub modified: String,
    /// "folder" for directories, the extension for files, "file" without.
    pub kind: String,
    /// "30 KB" for files, blank for folders — Explorer leaves it blank too.
    pub size: String,
}

pub struct Picker {
    /// The directory being browsed.
    pub dir: PathBuf,
    /// Everything in `dir`, folders first, columns preformatted.
    entries: Vec<Row>,
    /// Indices into `entries` that survive the search box, best first.
    matches: Vec<kb_find::Match>,
    /// What has been typed into the search box.
    pub filter: String,
    /// Into `matches`. `None` until a row is chosen: the dialog opens with
    /// nothing selected, like the real one.
    pub selected: Option<usize>,
    /// First visible match — the list is longer than the box.
    pub top: usize,
    /// Whether drawing should pull the selection into view. Keyboard moves
    /// set it; the wheel clears it, because wheeling is looking around and
    /// being yanked back to the selection is the opposite of looking.
    follow: bool,
    /// The left rail, in three labelled groups.
    pub quick: Vec<PathBuf>,
    pub recents: Vec<PathBuf>,
    pub drives: Vec<PathBuf>,
    /// Where Back goes: every directory browsed, oldest first.
    history: Vec<PathBuf>,
    /// The current position in `history`.
    at: usize,
    /// Minutes to add to UTC for wall-clock times, measured once at open.
    /// A picker is not open long enough to cross a DST change worth caring
    /// about, and asking Windows per row would be a syscall per line.
    utc_offset_min: i64,
}

/// More rows than this and the search box is the interface.
const MAX_ROWS: usize = 1000;

impl Picker {
    pub fn new(start: &Path, mut recents: Vec<PathBuf>, quick: Vec<PathBuf>) -> Self {
        // A recent that is also a quick-access folder is one place: the
        // quick group keeps it, because that is where the eye knows it.
        recents.retain(|r| !quick.contains(r));
        let mut me = Self {
            dir: PathBuf::new(),
            entries: Vec::new(),
            matches: Vec::new(),
            filter: String::new(),
            selected: None,
            top: 0,
            follow: false,
            quick,
            recents,
            drives: kb_fs::drives(),
            history: vec![start.to_path_buf()],
            at: 0,
            utc_offset_min: local_utc_offset_minutes(),
        };
        me.load(start.to_path_buf());
        me
    }

    /// Browses into `dir` as a new step: anything Forward pointed at is
    /// gone, the way every browser's history works.
    pub fn navigate(&mut self, dir: PathBuf) {
        if dir == self.dir {
            return;
        }
        self.history.truncate(self.at + 1);
        self.history.push(dir.clone());
        self.at += 1;
        self.load(dir);
    }

    pub fn can_back(&self) -> bool {
        self.at > 0
    }

    pub fn can_forward(&self) -> bool {
        self.at + 1 < self.history.len()
    }

    pub fn back(&mut self) {
        if self.can_back() {
            self.at -= 1;
            self.load(self.history[self.at].clone());
        }
    }

    pub fn forward(&mut self) {
        if self.can_forward() {
            self.at += 1;
            self.load(self.history[self.at].clone());
        }
    }

    /// One level up. At a drive root there is nowhere to go and nothing
    /// happens — an error would be scolding someone for pressing a key.
    pub fn up(&mut self) {
        if let Some(parent) = self.dir.parent() {
            self.navigate(parent.to_path_buf());
        }
    }

    /// Reads `dir` and drops the search: it described names that are no
    /// longer on screen. History is the caller's business.
    fn load(&mut self, dir: PathBuf) {
        let offset = self.utc_offset_min;
        self.entries = kb_fs::listing(&dir)
            .into_iter()
            .map(|e| Row {
                modified: e.modified.map(|t| stamp(t, offset)).unwrap_or_default(),
                kind: if e.is_dir {
                    "folder".to_string()
                } else {
                    Path::new(&e.name)
                        .extension()
                        .map(|x| x.to_string_lossy().to_lowercase())
                        .unwrap_or_else(|| "file".to_string())
                },
                size: if e.is_dir { String::new() } else { kb_size(e.size) },
                name: e.name,
                is_dir: e.is_dir,
            })
            .collect();
        self.dir = dir;
        self.filter.clear();
        self.refilter();
    }

    /// The address bar, one segment per directory.
    pub fn crumbs(&self) -> Vec<Crumb> {
        let mut out = Vec::new();
        for anc in self.dir.ancestors() {
            let name = anc
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                // The drive root has no file name; its own path, minus the
                // trailing slash, is already a short label.
                .unwrap_or_else(|| {
                    anc.display().to_string().trim_end_matches('\\').to_string()
                });
            out.push(Crumb { name, dir: anc.to_path_buf() });
        }
        out.reverse();
        out
    }

    /// The row the selection is on, and the full path it names.
    pub fn selected_entry(&self) -> Option<(Row, PathBuf)> {
        let m = self.matches.get(self.selected?)?;
        let row = self.entries.get(m.index)?;
        Some((row.clone(), self.dir.join(&row.name)))
    }

    /// What "Select folder" answers with: the highlighted folder when one is
    /// highlighted, otherwise the folder being browsed. Explorer's rule —
    /// picking a folder in the list is picking it, and no selection means
    /// "this one, the one I am standing in". A highlighted *file* is not an
    /// answer to a folder question, so it falls back to the same "this one".
    pub fn chosen(&self) -> PathBuf {
        match self.selected_entry() {
            Some((row, path)) if row.is_dir => path,
            _ => self.dir.clone(),
        }
    }

    pub fn push(&mut self, c: char) {
        self.filter.push(c);
        self.refilter();
    }

    pub fn backspace(&mut self) {
        self.filter.pop();
        self.refilter();
    }

    /// Arrows land on the first row when nothing is selected yet.
    pub fn move_selection(&mut self, delta: i32) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() as i32 - 1;
        self.selected = Some(match self.selected {
            None => 0,
            Some(s) => (s as i32 + delta).clamp(0, last) as usize,
        });
        self.follow = true;
    }

    /// The wheel: moves what is shown and nothing else. The selection
    /// stays where it was, off screen if need be — Explorer's rule.
    pub fn scroll(&mut self, lines: i32, visible: usize) {
        let max = self.matches.len().saturating_sub(visible) as i32;
        self.top = (self.top as i32 - lines).clamp(0, max.max(0)) as usize;
        self.follow = false;
    }

    /// Scrolls just enough to keep the selection in view — but only after
    /// the keyboard moved it. Called from drawing, because only the
    /// renderer knows how many rows fit.
    pub fn ensure_visible(&mut self, visible: usize) {
        if visible == 0 {
            return;
        }
        if self.follow {
            self.follow = false;
            if let Some(s) = self.selected {
                if s < self.top {
                    self.top = s;
                } else if s >= self.top + visible {
                    self.top = s + 1 - visible;
                }
            }
        }
        self.top = self.top.min(self.matches.len().saturating_sub(visible));
    }

    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// Rows to draw from the scroll position, with where the search landed
    /// in each name so the match is visible rather than implied.
    pub fn rows(&self, limit: usize) -> Vec<(Row, Vec<usize>)> {
        self.matches
            .iter()
            .skip(self.top)
            .take(limit)
            .filter_map(|m| Some((self.entries.get(m.index)?.clone(), m.positions.clone())))
            .collect()
    }

    /// Which drawn row is selected, if the selection is on screen.
    pub fn selected_row(&self) -> Option<usize> {
        self.selected?.checked_sub(self.top)
    }

    /// Selects the row `index` rows below the top of what is drawn.
    /// True when that row was already selected — a second click, an open.
    pub fn select_visible(&mut self, index: usize) -> bool {
        let target = self.top + index;
        if target >= self.matches.len() {
            return false;
        }
        let again = self.selected == Some(target);
        self.selected = Some(target);
        again
    }

    fn refilter(&mut self) {
        self.matches = kb_find::rank(&self.filter, &self.names(), MAX_ROWS);
        self.selected = None;
        self.top = 0;
    }

    fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }
}

/// "26.07.2026 10:33" — the reference dialog's date column, day first.
fn stamp(t: std::time::SystemTime, utc_offset_min: i64) -> String {
    let Ok(since) = t.duration_since(std::time::UNIX_EPOCH) else { return String::new() };
    let secs = since.as_secs() as i64 + utc_offset_min * 60;
    let (days, of_day) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{d:02}.{m:02}.{y} {:02}:{:02}", of_day / 3600, (of_day % 3600) / 60)
}

/// Days since 1970-01-01 to a calendar date. Howard Hinnant's algorithm,
/// the standard way to do this without a calendar dependency.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The wall clock's distance from UTC right now, in minutes.
///
/// Measured by comparing Windows' two clocks rather than by reading time
/// zone rules: the difference of the two answers *is* the offset, rules,
/// DST and all, with nothing to get wrong.
fn local_utc_offset_minutes() -> i64 {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    let (l, u) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let minutes = |t: &windows::Win32::Foundation::SYSTEMTIME| {
        i64::from(t.wDay) * 1440 + i64::from(t.wHour) * 60 + i64::from(t.wMinute)
    };
    let mut diff = minutes(&l) - minutes(&u);
    // The two clocks can sit on either side of both midnight and a month
    // boundary; anything beyond ±14h can only be a month's worth of days.
    if diff > 14 * 60 {
        diff -= tail_of_month(&u);
    } else if diff < -14 * 60 {
        diff += tail_of_month(&l);
    }
    diff
}

/// Minutes in the whole month `t` sits in — the wrap distance when the
/// local and UTC clocks straddle a month boundary.
fn tail_of_month(t: &windows::Win32::Foundation::SYSTEMTIME) -> i64 {
    let leap = t.wYear.is_multiple_of(4) && (!t.wYear.is_multiple_of(100) || t.wYear.is_multiple_of(400));
    let days = match t.wMonth {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    i64::from(days) * 1440
}

/// "30 KB", rounded up with a floor of one, exactly as Explorer prints it.
fn kb_size(bytes: u64) -> String {
    let kb = bytes.div_ceil(1024).max(1);
    // Thousands separated the way the reference dialog does it.
    let s = kb.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    format!("{out} KB")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kubide-picker-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        for sub in ["alpha", "beta", "gamma"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        std::fs::write(d.join("notes.txt"), vec![0u8; 3000]).unwrap();
        d
    }

    #[test]
    fn folders_come_first_and_files_are_rows_too() {
        let d = base("rows");
        let p = Picker::new(&d, Vec::new(), Vec::new());
        let rows = p.rows(10);
        assert_eq!(rows.len(), 4, "the file is listed, like the real dialog");
        assert!(rows[0].0.is_dir && !rows[3].0.is_dir);
        assert_eq!(rows[3].0.kind, "txt");
        assert_eq!(rows[3].0.size, "3 KB", "2930-odd bytes round up");
        assert_eq!(rows[0].0.size, "", "folders leave the size column blank");
    }

    #[test]
    fn typing_narrows_and_navigation_clears_the_search() {
        let d = base("filter");
        let mut p = Picker::new(&d, Vec::new(), Vec::new());
        p.push('a');
        p.push('l');
        assert_eq!(p.rows(10)[0].0.name, "alpha");
        // Stepping elsewhere must not carry "al" along: it described names
        // that are no longer on screen.
        p.navigate(d.join("beta"));
        assert!(p.filter.is_empty());
    }

    #[test]
    fn back_and_forward_walk_the_trail_and_a_turn_burns_it() {
        let d = base("history");
        let mut p = Picker::new(&d, Vec::new(), Vec::new());
        p.navigate(d.join("alpha"));
        p.back();
        assert_eq!(p.dir, d);
        assert!(p.can_forward());
        p.forward();
        assert_eq!(p.dir, d.join("alpha"));
        p.back();
        // Turning off the trail throws the forward half away, like every
        // browser since the first one.
        p.navigate(d.join("beta"));
        assert!(!p.can_forward());
        assert!(p.can_back());
    }

    #[test]
    fn nothing_is_selected_until_someone_selects() {
        let d = base("selection");
        let mut p = Picker::new(&d, Vec::new(), Vec::new());
        assert!(p.selected_entry().is_none(), "the dialog opens neutral");
        p.move_selection(1);
        assert_eq!(p.selected_entry().unwrap().0.name, "alpha", "first press lands on the first row");
    }

    #[test]
    fn a_second_click_on_a_row_reads_as_an_open() {
        let d = base("click");
        let mut p = Picker::new(&d, Vec::new(), Vec::new());
        assert!(!p.select_visible(1), "first click only selects");
        assert!(p.select_visible(1), "second click on the same row opens");
        assert!(!p.select_visible(99), "past the list is not a click on it");
    }

    #[test]
    fn selecting_a_folder_is_answering_with_it() {
        let d = base("chosen");
        let mut p = Picker::new(&d, Vec::new(), Vec::new());
        assert_eq!(p.chosen(), d, "no selection means the folder being browsed");
        p.move_selection(1);
        assert_eq!(p.chosen(), d.join("alpha"), "one click in is not required");
        // "notes.txt" is not an answer to a folder question.
        p.move_selection(3);
        assert_eq!(p.selected_entry().unwrap().0.name, "notes.txt");
        assert_eq!(p.chosen(), d);
    }

    #[test]
    fn the_wheel_moves_the_view_and_leaves_the_selection() {
        let d = base("wheel");
        for i in 0..20 {
            std::fs::create_dir_all(d.join(format!("dir{i:02}"))).unwrap();
        }
        let mut p = Picker::new(&d, Vec::new(), Vec::new());
        p.move_selection(1);
        p.scroll(-3, 5); // wheel down three
        p.ensure_visible(5);
        assert_eq!(p.top, 3, "the view moved");
        assert_eq!(p.selected, Some(0), "the selection did not");
        // The next keyboard move snaps the view back to the selection.
        p.move_selection(1);
        p.ensure_visible(5);
        assert_eq!(p.top, 1);
        // And the wheel cannot scroll past either end.
        p.scroll(100, 5);
        assert_eq!(p.top, 0);
    }

    #[test]
    fn a_quick_access_folder_is_not_repeated_by_the_recents() {
        let d = base("quick");
        let docs = d.join("alpha");
        let p = Picker::new(&d, vec![docs.clone(), d.join("beta")], vec![docs.clone()]);
        assert_eq!(p.quick, [docs]);
        assert_eq!(p.recents, [d.join("beta")], "the quick group kept it");
    }

    #[test]
    fn up_from_a_drive_root_stays_put() {
        let root = PathBuf::from(r"C:\");
        let mut p = Picker::new(&root, Vec::new(), Vec::new());
        p.up();
        assert_eq!(p.dir, root);
    }

    #[test]
    fn crumbs_jump_rather_than_walk() {
        let d = base("crumbs").join("alpha");
        let p = Picker::new(&d, Vec::new(), Vec::new());
        let crumbs = p.crumbs();
        assert_eq!(crumbs.last().unwrap().name, "alpha");
        assert_eq!(crumbs[crumbs.len() - 2].dir, *d.parent().unwrap());
        assert!(!crumbs[0].name.ends_with('\\'), "the drive labels itself without the slash");
    }

    #[test]
    fn dates_print_like_the_reference_dialog() {
        // 2026-07-26 07:33 UTC, viewed from UTC+3.
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_785_051_180);
        assert_eq!(stamp(t, 180), "26.07.2026 10:33");
        assert_eq!(stamp(t, 0), "26.07.2026 07:33");
    }

    #[test]
    fn sizes_print_like_the_reference_dialog() {
        assert_eq!(kb_size(1), "1 KB", "Explorer never prints 0 KB");
        assert_eq!(kb_size(30_000), "30 KB");
        assert_eq!(kb_size(2_000_000), "1.954 KB");
    }
}
