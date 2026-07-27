//! The settings screen's model: named values over a [`Config`], each one
//! steppable with a left or right arrow.
//!
//! Here rather than next to the drawing code because none of it is drawing.
//! What a setting is called, what it reads as, and what the arrows do to it
//! are all decisions about the config, and keeping them beside the struct they
//! act on is what stops the screen from quietly drifting out of date when a
//! field is renamed — that becomes a compile error instead.
//!
//! Not every field is here. The theme is a hundred colours and the keymap is
//! its own screen; those want a file and an editor, not two arrow keys.

use crate::{Backdrop, Config, Theme};

/// One line on the settings screen.
///
/// An enum with the metadata in match arms, the same shape as [`crate::Action`]
/// — the compiler then refuses to let a new variant through without a label,
/// a value and a meaning for the arrows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    StatusCursor,
    StatusFont,
    StatusPanes,
    StatusFrameTime,
    StatusGit,
    StatusPomodoro,
    StatusClock,
    StatusClock24h,
    WindowBackdrop,
    WindowPadding,
    WindowCaptionHeight,
    ThemeFile,
    FontSize,
    EditorAutoClose,
    EditorSnippets,
    TerminalScrollback,
    PomodoroWork,
    PomodoroShortBreak,
    PomodoroLongBreak,
    PomodoroRounds,
    PomodoroAutoAdvance,
    HelpVisible,
}

/// Backdrops in the order the arrows walk them, dimmest to busiest.
const BACKDROPS: [Backdrop; 4] = [
    Backdrop::None,
    Backdrop::Mica,
    Backdrop::MicaAlt,
    Backdrop::Acrylic,
];

impl Setting {
    /// The order they are drawn in. Grouped by section, because the screen
    /// draws a heading wherever the section changes and a list that jumped
    /// back to an earlier one would print it twice.
    pub const ALL: &'static [Setting] = &[
        Setting::StatusCursor,
        Setting::StatusFont,
        Setting::StatusPanes,
        Setting::StatusFrameTime,
        Setting::StatusGit,
        Setting::StatusPomodoro,
        Setting::StatusClock,
        Setting::StatusClock24h,
        Setting::WindowBackdrop,
        Setting::WindowPadding,
        Setting::WindowCaptionHeight,
        Setting::ThemeFile,
        Setting::FontSize,
        Setting::EditorAutoClose,
        Setting::EditorSnippets,
        Setting::TerminalScrollback,
        Setting::PomodoroWork,
        Setting::PomodoroShortBreak,
        Setting::PomodoroLongBreak,
        Setting::PomodoroRounds,
        Setting::PomodoroAutoAdvance,
        Setting::HelpVisible,
    ];

    pub fn section(self) -> &'static str {
        use Setting::*;
        match self {
            StatusCursor | StatusFont | StatusPanes | StatusFrameTime | StatusGit
            | StatusPomodoro | StatusClock | StatusClock24h => "STATUS BAR",
            WindowBackdrop | WindowPadding | WindowCaptionHeight => "WINDOW",
            ThemeFile => "THEME",
            FontSize => "FONT",
            EditorAutoClose | EditorSnippets => "EDITOR",
            TerminalScrollback => "TERMINAL",
            PomodoroWork | PomodoroShortBreak | PomodoroLongBreak | PomodoroRounds
            | PomodoroAutoAdvance => "TIMER",
            HelpVisible => "HELP",
        }
    }

    /// What the row is called. Words, not field names: nobody is looking for
    /// `clock_24h`.
    pub fn label(self) -> &'static str {
        use Setting::*;
        match self {
            StatusCursor => "Cursor position",
            StatusFont => "Font name",
            StatusPanes => "Pane count",
            StatusFrameTime => "Frame time",
            StatusGit => "Git branch",
            StatusPomodoro => "Work timer",
            StatusClock => "Clock",
            StatusClock24h => "24-hour clock",
            WindowBackdrop => "Backdrop",
            WindowPadding => "Padding",
            WindowCaptionHeight => "Title bar height",
            ThemeFile => "Theme",
            FontSize => "Size",
            EditorAutoClose => "Close brackets and quotes as you type",
            EditorSnippets => "Expand snippets on Tab",
            TerminalScrollback => "Scrollback",
            PomodoroWork => "Work",
            PomodoroShortBreak => "Short break",
            PomodoroLongBreak => "Long break",
            PomodoroRounds => "Rounds before long break",
            PomodoroAutoAdvance => "Start the next period on its own",
            HelpVisible => "Shortcut hint on the status bar",
        }
    }

    /// The value as it is drawn.
    pub fn value(self, cfg: &Config) -> String {
        use Setting::*;
        match self {
            WindowBackdrop => match cfg.window.backdrop {
                Backdrop::None => "NONE".into(),
                Backdrop::Mica => "MICA".into(),
                Backdrop::MicaAlt => "MICA ALT".into(),
                Backdrop::Acrylic => "ACRYLIC".into(),
            },
            WindowPadding => format!("{:.0}", cfg.window.padding),
            WindowCaptionHeight => format!("{:.0}", cfg.window.caption_height),
            // The name when one is followed; CUSTOM when the colours came
            // from an inline [theme] table nobody named.
            ThemeFile => match &cfg.theme_name {
                Some(name) => name.to_uppercase(),
                None if cfg.theme == Theme::default() => "DEFAULT".into(),
                None => "CUSTOM".into(),
            },
            FontSize => format!("{:.0}", cfg.font.size),
            TerminalScrollback => format!("{}", cfg.terminal.scrollback),
            PomodoroWork => format!("{} min", cfg.pomodoro.work),
            PomodoroShortBreak => format!("{} min", cfg.pomodoro.short_break),
            PomodoroLongBreak => format!("{} min", cfg.pomodoro.long_break),
            PomodoroRounds => format!("{}", cfg.pomodoro.rounds),
            // Everything else is a switch.
            _ => if self.is_on(cfg) { "ON" } else { "OFF" }.into(),
        }
    }

    /// Whether a switch is on. Meaningless for the others, which report false
    /// and are never asked.
    pub fn is_on(self, cfg: &Config) -> bool {
        use Setting::*;
        match self {
            StatusCursor => cfg.status.cursor,
            StatusFont => cfg.status.font,
            StatusPanes => cfg.status.panes,
            StatusFrameTime => cfg.status.frame_time,
            StatusGit => cfg.status.git,
            StatusPomodoro => cfg.status.pomodoro,
            StatusClock => cfg.status.clock,
            StatusClock24h => cfg.status.clock_24h,
            PomodoroAutoAdvance => cfg.pomodoro.auto_advance,
            HelpVisible => cfg.help.visible,
            EditorAutoClose => cfg.editor.auto_close,
            EditorSnippets => cfg.editor.snippets,
            _ => false,
        }
    }

    /// Moves the value one place.
    ///
    /// `delta` is -1 or 1. A switch ignores the sign: there are two states, so
    /// whichever arrow you press should reach the other one — waiting to find
    /// out which arrow turns a thing off is not a puzzle worth having.
    pub fn step(self, cfg: &mut Config, delta: i32) {
        use Setting::*;
        match self {
            StatusCursor => cfg.status.cursor = !cfg.status.cursor,
            StatusFont => cfg.status.font = !cfg.status.font,
            StatusPanes => cfg.status.panes = !cfg.status.panes,
            StatusFrameTime => cfg.status.frame_time = !cfg.status.frame_time,
            StatusGit => cfg.status.git = !cfg.status.git,
            StatusPomodoro => cfg.status.pomodoro = !cfg.status.pomodoro,
            StatusClock => cfg.status.clock = !cfg.status.clock,
            StatusClock24h => cfg.status.clock_24h = !cfg.status.clock_24h,
            PomodoroAutoAdvance => cfg.pomodoro.auto_advance = !cfg.pomodoro.auto_advance,
            HelpVisible => cfg.help.visible = !cfg.help.visible,
            EditorAutoClose => cfg.editor.auto_close = !cfg.editor.auto_close,
            EditorSnippets => cfg.editor.snippets = !cfg.editor.snippets,

            // Wraps like the backdrop, and resolves as it goes so the window
            // repaints under the arrows — a theme picker you cannot see is a
            // list of file names.
            ThemeFile => {
                let names = crate::available_themes();
                let current = cfg.theme_name.clone().unwrap_or_else(|| "default".to_string());
                let at = names.iter().position(|n| *n == current).unwrap_or(0);
                let next = (at as i32 + delta).rem_euclid(names.len() as i32) as usize;
                if names[next] == "default" {
                    cfg.theme_name = None;
                    cfg.theme = Theme::default();
                } else if let Ok((theme, _)) =
                    crate::resolve_theme_in(&crate::themes_dir(), &names[next])
                {
                    cfg.theme_name = Some(names[next].clone());
                    cfg.theme = theme;
                }
                // A theme that fails to resolve is skipped by pressing the
                // arrow again; swallowing it here would strand the row.
            }

            // Wrapping, because a list of four materials has no natural end to
            // stop against and stopping would hide two of them behind a guess
            // about which way to press.
            WindowBackdrop => {
                let at = BACKDROPS
                    .iter()
                    .position(|b| *b == cfg.window.backdrop)
                    .unwrap_or(0);
                let next = (at as i32 + delta).rem_euclid(BACKDROPS.len() as i32);
                cfg.window.backdrop = BACKDROPS[next as usize];
            }

            // Numbers clamp instead. There is a smallest sensible font and a
            // largest sensible title bar, and wrapping from one to the other
            // would look like a bug.
            WindowPadding => cfg.window.padding = number(cfg.window.padding, delta, 2.0, 0.0, 48.0),
            WindowCaptionHeight => {
                cfg.window.caption_height = number(cfg.window.caption_height, delta, 2.0, 24.0, 80.0)
            }
            FontSize => cfg.font.size = number(cfg.font.size, delta, 1.0, 6.0, 72.0),
            TerminalScrollback => {
                cfg.terminal.scrollback =
                    number(cfg.terminal.scrollback as f32, delta, 1000.0, 0.0, 200_000.0) as usize
            }
            PomodoroWork => {
                cfg.pomodoro.work = number(cfg.pomodoro.work as f32, delta, 5.0, 1.0, 180.0) as u32
            }
            PomodoroShortBreak => {
                cfg.pomodoro.short_break =
                    number(cfg.pomodoro.short_break as f32, delta, 1.0, 1.0, 60.0) as u32
            }
            PomodoroLongBreak => {
                cfg.pomodoro.long_break =
                    number(cfg.pomodoro.long_break as f32, delta, 5.0, 1.0, 120.0) as u32
            }
            PomodoroRounds => {
                cfg.pomodoro.rounds = number(cfg.pomodoro.rounds as f32, delta, 1.0, 1.0, 12.0) as u32
            }
        }
    }
}

fn number(value: f32, delta: i32, step: f32, min: f32, max: f32) -> f32 {
    (value + delta as f32 * step).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_is_listed_once() {
        // The screen draws `ALL`; a variant missing from it is invisible and
        // one listed twice is two rows editing the same value.
        let mut seen = Setting::ALL.to_vec();
        let count = seen.len();
        seen.sort_by_key(|s| format!("{s:?}"));
        seen.dedup();
        assert_eq!(seen.len(), count, "a setting is listed twice");
        assert!(count >= 18, "only {count} settings listed");
    }

    #[test]
    fn sections_do_not_come_back() {
        // The heading is drawn wherever the section changes, so a section that
        // reappeared further down would print its title twice.
        let mut order: Vec<&str> = Vec::new();
        for s in Setting::ALL {
            if order.last() != Some(&s.section()) {
                assert!(!order.contains(&s.section()), "{} is split up", s.section());
                order.push(s.section());
            }
        }
    }

    #[test]
    fn every_row_has_a_label_and_a_value() {
        let cfg = Config::default();
        for s in Setting::ALL {
            assert!(!s.label().is_empty(), "{s:?}");
            assert!(!s.value(&cfg).is_empty(), "{s:?}");
            assert!(!s.section().is_empty(), "{s:?}");
        }
    }

    #[test]
    fn labels_are_unique_within_a_section() {
        // Two rows reading the same under one heading is unusable.
        for s in Setting::ALL {
            let clashes = Setting::ALL
                .iter()
                .filter(|o| o.section() == s.section() && o.label() == s.label())
                .count();
            assert_eq!(clashes, 1, "{} / {}", s.section(), s.label());
        }
    }

    #[test]
    fn either_arrow_flips_a_switch() {
        for delta in [-1, 1] {
            let mut cfg = Config::default();
            let was = Setting::StatusGit.is_on(&cfg);
            Setting::StatusGit.step(&mut cfg, delta);
            assert_ne!(Setting::StatusGit.is_on(&cfg), was, "delta {delta}");
            Setting::StatusGit.step(&mut cfg, delta);
            assert_eq!(Setting::StatusGit.is_on(&cfg), was, "delta {delta}");
        }
    }

    #[test]
    fn switches_read_as_on_or_off() {
        let mut cfg = Config::default();
        cfg.status.git = true;
        assert_eq!(Setting::StatusGit.value(&cfg), "ON");
        cfg.status.git = false;
        assert_eq!(Setting::StatusGit.value(&cfg), "OFF");
    }

    #[test]
    fn numbers_stop_at_their_limits() {
        let mut cfg = Config::default();
        for _ in 0..200 {
            Setting::FontSize.step(&mut cfg, -1);
        }
        assert_eq!(cfg.font.size, 6.0);
        for _ in 0..200 {
            Setting::FontSize.step(&mut cfg, 1);
        }
        assert_eq!(cfg.font.size, 72.0);
    }

    #[test]
    fn the_backdrop_wraps_both_ways() {
        // Four materials with no natural end; stopping at one would hide the
        // rest behind a guess about which arrow to press.
        let mut cfg = Config::default();
        cfg.window.backdrop = Backdrop::None;
        Setting::WindowBackdrop.step(&mut cfg, -1);
        assert_eq!(cfg.window.backdrop, Backdrop::Acrylic);
        Setting::WindowBackdrop.step(&mut cfg, 1);
        assert_eq!(cfg.window.backdrop, Backdrop::None);
    }

    #[test]
    fn the_backdrop_reaches_every_material() {
        let mut cfg = Config::default();
        let mut seen = Vec::new();
        for _ in 0..BACKDROPS.len() {
            Setting::WindowBackdrop.step(&mut cfg, 1);
            seen.push(cfg.window.backdrop);
        }
        seen.sort_by_key(|b| format!("{b:?}"));
        seen.dedup();
        assert_eq!(seen.len(), BACKDROPS.len());
    }

    #[test]
    fn stepping_changes_nothing_else() {
        // A row must edit its own value and only its own.
        for s in Setting::ALL {
            let before = Config::default();
            let mut after = before.clone();
            s.step(&mut after, 1);
            let differences = Setting::ALL
                .iter()
                .filter(|o| o.value(&before) != o.value(&after))
                .count();
            assert!(differences <= 1, "{s:?} moved {differences} rows");
        }
    }

    #[test]
    fn a_round_trip_through_toml_keeps_every_value() {
        // The screen writes the config back out; a setting that does not
        // survive serialization would silently revert on the next start.
        //
        // The theme row is the deliberate exception: its name is not part
        // of Config's TOML — the load and save layers spell it
        // `theme = "name"` themselves, and their own test covers the round
        // trip. It also cycles through whatever the machine's themes folder
        // holds, which no test should depend on.
        let mut cfg = Config::default();
        for s in Setting::ALL {
            if *s == Setting::ThemeFile {
                continue;
            }
            s.step(&mut cfg, 1);
        }
        let text = toml::to_string(&cfg).expect("config should serialize");
        let back: Config = toml::from_str(&text).expect("config should parse back");
        for s in Setting::ALL {
            if *s == Setting::ThemeFile {
                continue;
            }
            assert_eq!(s.value(&cfg), s.value(&back), "{s:?}");
        }
    }

    #[test]
    fn the_theme_row_reads_default_custom_or_the_name() {
        let mut cfg = Config::default();
        assert_eq!(Setting::ThemeFile.value(&cfg), "DEFAULT");

        // Inline colours nobody named.
        cfg.theme.accent = crate::Color::rgb(0xff, 0, 0);
        assert_eq!(Setting::ThemeFile.value(&cfg), "CUSTOM");

        cfg.theme_name = Some("gruvbox".into());
        assert_eq!(Setting::ThemeFile.value(&cfg), "GRUVBOX");
    }
}
