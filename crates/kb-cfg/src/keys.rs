//! Keymap: chord strings to actions.
//!
//! This exists because a hardcoded shortcut does not work on every keyboard.
//! `Ctrl+\` is unpressable on a Turkish Q layout — backslash needs AltGr, and
//! AltGr is Ctrl+Alt on Windows, so the chord can never be produced. Rather
//! than collect special cases forever, the bindings are data.
//!
//! Virtual-key codes are plain numbers here, so parsing stays testable without
//! linking anything Windows-specific.

use std::collections::HashMap;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Everything a key can be bound to.
///
/// Deliberately coarse: one variant per thing a user would name. Splitting
/// these into parameterized commands would buy nothing until there is a
/// command palette to feed them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    SplitRight,
    SplitDown,
    ClosePane,
    OpenTerminal,
    OpenExplorer,
    ToggleExplorer,
    OpenSettings,
    ToggleHelp,
    NewFile,
    NewFolder,
    Rename,
    Delete,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    /// Jump straight to a pane by its position on screen, counted in reading
    /// order. Nine of them rather than one parameterized action because a
    /// binding is a string in a config file, and `focus-pane-3` is a name a
    /// person can write; a payload would need a second syntax for arguments.
    #[serde(rename = "focus-pane-1")]
    FocusPane1,
    #[serde(rename = "focus-pane-2")]
    FocusPane2,
    #[serde(rename = "focus-pane-3")]
    FocusPane3,
    #[serde(rename = "focus-pane-4")]
    FocusPane4,
    #[serde(rename = "focus-pane-5")]
    FocusPane5,
    #[serde(rename = "focus-pane-6")]
    FocusPane6,
    #[serde(rename = "focus-pane-7")]
    FocusPane7,
    #[serde(rename = "focus-pane-8")]
    FocusPane8,
    #[serde(rename = "focus-pane-9")]
    FocusPane9,
    /// Resizing says what happens to the focused pane, not which divider
    /// moves. A pane against the window edge has no divider on that side, so
    /// "move the boundary right" would grow one pane and shrink another
    /// depending on where it sat; "wider" is true wherever it is.
    GrowPaneWidth,
    ShrinkPaneWidth,
    GrowPaneHeight,
    ShrinkPaneHeight,
    MinimizeWindow,
    ToggleMaximize,
    Copy,
    Paste,
    Cut,
    Save,
    Undo,
    Redo,
    SelectAll,
    SelectLine,
    /// Jump the caret to the bracket matching the one it touches.
    GoToBracket,
    /// Opens the command list. Not itself listed there — running "open the
    /// list" from the list is a joke, not a feature.
    Commands,
    GoToFile,
    /// Swaps the focused pane back to the file it held most recently — the
    /// alt-tab of files. The editor has no tabs on purpose, and this is the
    /// piece of tabs actually worth having.
    LastFile,
    Find,
    Replace,
    FindInProject,
    GoToLine,
    /// The git panel: stage, commit, read diffs and the log.
    GitPanel,
    /// Prompt for a directory and make it the workspace.
    OpenFolder,
    /// Make the explorer's selection the workspace — the tree's own `cd`.
    WorkspaceHere,
    ToggleComment,
    MoveLineUp,
    MoveLineDown,
    DuplicateLine,
    DeleteLine,
    PomodoroToggle,
    PomodoroReset,
    PomodoroSkip,
    FontLarger,
    FontSmaller,
    Quit,
}

/// Where a binding applies.
///
/// Ctrl+A, Ctrl+Z and Ctrl+X are control characters a shell needs — start of
/// line, suspend, and so on. Claiming them globally would quietly break the
/// terminal, so editor actions step aside when a terminal has focus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Global,
    Editor,
    /// Only while the file tree has focus. These are bound to keys the editor
    /// and terminal also want — Delete, F2 — so they have to step aside
    /// rather than claim them.
    Explorer,
}

impl Action {
    /// Every action, for the command palette.
    ///
    /// Written out rather than derived: a macro or a strum dependency would
    /// save nothing here, and the compiler still catches a missing arm in
    /// `title` below.
    pub const ALL: &'static [Action] = &[
        Action::SplitRight,
        Action::SplitDown,
        Action::ClosePane,
        Action::OpenTerminal,
        Action::OpenExplorer,
        Action::ToggleExplorer,
        Action::OpenSettings,
        Action::ToggleHelp,
        Action::NewFile,
        Action::NewFolder,
        Action::Rename,
        Action::Delete,
        Action::FocusLeft,
        Action::FocusRight,
        Action::FocusUp,
        Action::FocusDown,
        Action::FocusPane1,
        Action::FocusPane2,
        Action::FocusPane3,
        Action::FocusPane4,
        Action::FocusPane5,
        Action::FocusPane6,
        Action::FocusPane7,
        Action::FocusPane8,
        Action::FocusPane9,
        Action::GrowPaneWidth,
        Action::ShrinkPaneWidth,
        Action::GrowPaneHeight,
        Action::ShrinkPaneHeight,
        Action::MinimizeWindow,
        Action::ToggleMaximize,
        Action::Copy,
        Action::Paste,
        Action::Cut,
        Action::Save,
        Action::Undo,
        Action::Redo,
        Action::SelectAll,
        Action::SelectLine,
        Action::GoToBracket,
        Action::GoToFile,
        Action::LastFile,
        Action::Find,
        Action::Replace,
        Action::FindInProject,
        Action::GoToLine,
        Action::GitPanel,
        Action::OpenFolder,
        Action::WorkspaceHere,
        Action::ToggleComment,
        Action::MoveLineUp,
        Action::MoveLineDown,
        Action::DuplicateLine,
        Action::DeleteLine,
        Action::PomodoroToggle,
        Action::PomodoroReset,
        Action::PomodoroSkip,
        Action::FontLarger,
        Action::FontSmaller,
        Action::Quit,
    ];

    /// How the action reads in a list. Words people would search for, not the
    /// identifier: nobody types "split-right" looking for a split.
    pub fn title(self) -> &'static str {
        use Action::*;
        match self {
            SplitRight => "Split pane right",
            SplitDown => "Split pane down",
            ClosePane => "Close pane",
            OpenTerminal => "Open terminal",
            OpenExplorer => "Open file tree",
            ToggleExplorer => "Toggle file tree",
            OpenSettings => "Settings",
            ToggleHelp => "Shortcut list on or off",
            NewFile => "New file",
            NewFolder => "New folder",
            Rename => "Rename",
            Delete => "Delete",
            FocusLeft => "Focus pane left",
            FocusRight => "Focus pane right",
            FocusUp => "Focus pane up",
            FocusDown => "Focus pane down",
            FocusPane1 => "Focus pane 1",
            FocusPane2 => "Focus pane 2",
            FocusPane3 => "Focus pane 3",
            FocusPane4 => "Focus pane 4",
            FocusPane5 => "Focus pane 5",
            FocusPane6 => "Focus pane 6",
            FocusPane7 => "Focus pane 7",
            FocusPane8 => "Focus pane 8",
            FocusPane9 => "Focus pane 9",
            GrowPaneWidth => "Make pane wider",
            ShrinkPaneWidth => "Make pane narrower",
            GrowPaneHeight => "Make pane taller",
            ShrinkPaneHeight => "Make pane shorter",
            MinimizeWindow => "Minimise window",
            ToggleMaximize => "Maximise or restore window",
            Copy => "Copy",
            Paste => "Paste",
            Cut => "Cut",
            Save => "Save file",
            Undo => "Undo",
            Redo => "Redo",
            SelectAll => "Select all",
            SelectLine => "Select line",
            GoToBracket => "Go to matching bracket",
            Commands => "Show all commands",
            GoToFile => "Go to file",
            LastFile => "Switch to last file",
            Find => "Find in file",
            Replace => "Replace in file",
            FindInProject => "Find in project",
            GoToLine => "Go to line",
            GitPanel => "Git: status and commit",
            OpenFolder => "Open folder",
            WorkspaceHere => "Open selected folder as workspace",
            ToggleComment => "Toggle comment",
            MoveLineUp => "Move line up",
            MoveLineDown => "Move line down",
            DuplicateLine => "Duplicate line",
            DeleteLine => "Delete line",
            PomodoroToggle => "Timer: start or pause",
            PomodoroReset => "Timer: reset",
            PomodoroSkip => "Timer: skip to next period",
            FontLarger => "Increase font size",
            FontSmaller => "Decrease font size",
            Quit => "Quit",
        }
    }

    /// The pane a numbered jump means, counted from zero. `None` for anything
    /// that is not one.
    ///
    /// Here rather than at the call site because the mapping belongs with the
    /// variants: adding a tenth would otherwise compile fine and silently do
    /// nothing.
    pub fn pane_index(self) -> Option<usize> {
        use Action::*;
        Some(match self {
            FocusPane1 => 0,
            FocusPane2 => 1,
            FocusPane3 => 2,
            FocusPane4 => 3,
            FocusPane5 => 4,
            FocusPane6 => 5,
            FocusPane7 => 6,
            FocusPane8 => 7,
            FocusPane9 => 8,
            _ => return None,
        })
    }

    pub fn scope(self) -> Scope {
        use Action::*;
        match self {
            // Replace sits here for the same reason Find does; Ctrl+H is
            // backspace in a shell besides, and must never be taken from it.
            // NewFile and NewFolder live here too: Ctrl+N from an editor is
            // a habit every other editor installed, and only a shell — where
            // Ctrl+N walks history — has a better claim on the key.
            Cut | Save | Undo | Redo | SelectAll | SelectLine | GoToBracket | Find | Replace
            | GoToLine | ToggleComment
            | MoveLineUp | MoveLineDown | DuplicateLine | DeleteLine | NewFile | NewFolder => {
                Scope::Editor
            }
            Rename | Delete | WorkspaceHere => Scope::Explorer,
            // Copy and paste stay global: they're bound to the shifted pair,
            // which is not a control character in any shell.
            _ => Scope::Global,
        }
    }
}

/// A key plus its modifiers.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Chord {
    pub vk: u8,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    /// Parses `ctrl+shift+c`, `alt+left`, `f5`. Order of modifiers is free.
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut chord = Chord { vk: 0, ctrl: false, shift: false, alt: false };
        let mut key: Option<&str> = None;

        // '+' is both the separator and a key name, so `ctrl++` means Ctrl
        // plus the plus key. A single trailing '+' stays an error: `ctrl+` is
        // a truncated binding, not a request for the plus key.
        let trimmed = s.trim();
        let body = if trimmed == "+" {
            key = Some("+");
            ""
        } else if let Some(rest) = trimmed.strip_suffix("++") {
            key = Some("+");
            rest
        } else {
            trimmed
        };

        for part in body.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" => chord.alt = true,
                _ => {
                    if key.is_some() {
                        return Err(format!(
                            "'{s}' has more than one key; only modifiers may be combined"
                        ));
                    }
                    key = Some(part);
                }
            }
        }

        let Some(key) = key else {
            return Err(format!("'{s}' is only modifiers, with no key"));
        };
        chord.vk = vk_of(key).ok_or_else(|| format!("unknown key '{key}' in '{s}'"))?;
        Ok(chord)
    }
}

impl Chord {
    /// Whether this chord is the same physical key on every layout.
    ///
    /// Letters, digits, function keys, the numpad and the named keys have
    /// fixed virtual-key codes everywhere. The OEM range does not: on a
    /// Turkish Q layout VK_OEM_2 is `ö` rather than `/`, and VK_OEM_5 is `ç`
    /// rather than `\`. An action whose only default sits on one of those is
    /// unreachable for anyone not on US QWERTY — which is the entire reason
    /// this module exists, so every action ships with at least one of these.
    pub fn is_portable(self) -> bool {
        matches!(
            self.vk,
            0x30..=0x39     // digits
            | 0x41..=0x5A   // letters
            | 0x70..=0x7B   // F1 to F12
            | 0x6B | 0x6D   // numpad plus and minus
            | 0x08 | 0x09 | 0x0D | 0x1B | 0x20 | 0x2E
            | 0x21..=0x28   // page up and down, end, home, the arrows
        )
    }
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ctrl {
            write!(f, "Ctrl+")?;
        }
        if self.shift {
            write!(f, "Shift+")?;
        }
        if self.alt {
            write!(f, "Alt+")?;
        }
        match name_of(self.vk) {
            Some(n) => write!(f, "{n}"),
            None => write!(f, "0x{:02X}", self.vk),
        }
    }
}

/// Named keys, and the letters and digits.
///
/// Letters and digits map to stable virtual-key codes on every layout, which
/// is exactly why the default bindings prefer them. The OEM keys below
/// (backslash, brackets, plus, minus) are layout-dependent: they are what
/// caused this module to exist, and binding one is a choice the user makes
/// with their own keyboard in front of them.
fn vk_of(key: &str) -> Option<u8> {
    let k = key.to_lowercase();
    if k.chars().count() == 1 {
        let c = k.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase() as u8);
        }
        if c.is_ascii_digit() {
            return Some(c as u8);
        }
    }
    Some(match k.as_str() {
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "enter" | "return" => 0x0D,
        "escape" | "esc" => 0x1B,
        "space" => 0x20,
        "tab" => 0x09,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        // Both spellings: the words are readable, the symbols are what people
        // actually type, and the symbols are what `Display` prints back.
        "backslash" | "\\" => 0xDC,
        "slash" | "/" => 0xBF,
        "comma" | "," => 0xBC,
        "period" | "dot" | "." => 0xBE,
        "semicolon" | ";" => 0xBA,
        "quote" | "'" => 0xDE,
        "leftbracket" | "[" => 0xDB,
        "rightbracket" | "]" => 0xDD,
        "plus" | "equals" | "+" => 0xBB,
        "minus" | "-" => 0xBD,
        // Named without a literal '+' so the parser can never mistake it for
        // the separator.
        "numplus" | "numpadplus" => 0x6B,
        "numminus" | "numpadminus" => 0x6D,
        "f1" => 0x70,
        "f2" => 0x71,
        "f3" => 0x72,
        "f4" => 0x73,
        "f5" => 0x74,
        "f6" => 0x75,
        "f7" => 0x76,
        "f8" => 0x77,
        "f9" => 0x78,
        "f10" => 0x79,
        "f11" => 0x7A,
        "f12" => 0x7B,
        _ => return None,
    })
}

/// The printable name of a virtual key, for showing bindings back to the user.
/// The inverse of [`vk_of`] for everything that has one canonical spelling.
fn name_of(vk: u8) -> Option<String> {
    if vk.is_ascii_uppercase() || vk.is_ascii_digit() {
        return Some((vk as char).to_string());
    }
    Some(
        match vk {
            0x25 => "Left",
            0x26 => "Up",
            0x27 => "Right",
            0x28 => "Down",
            0x0D => "Enter",
            0x1B => "Esc",
            0x20 => "Space",
            0x09 => "Tab",
            0x08 => "Backspace",
            0x2E => "Delete",
            0x24 => "Home",
            0x23 => "End",
            0x21 => "PageUp",
            0x22 => "PageDown",
            0xDC => "\\",
            0xBF => "/",
            0xBC => ",",
            0xBE => ".",
            0xBA => ";",
            0xDE => "'",
            0xDB => "[",
            0xDD => "]",
            0xBB => "+",
            0xBD => "-",
            0x6B => "NumPlus",
            0x6D => "NumMinus",
            0x70..=0x7B => return Some(format!("F{}", vk - 0x6F)),
            _ => return None,
        }
        .to_string(),
    )
}

#[derive(Clone, PartialEq, Debug)]
pub struct Keymap {
    map: HashMap<Chord, Action>,
}

impl Keymap {
    pub fn lookup(&self, vk: u8, ctrl: bool, shift: bool, alt: bool) -> Option<Action> {
        self.map.get(&Chord { vk, ctrl, shift, alt }).copied()
    }

    /// A chord bound to this action, for showing the user what to press.
    ///
    /// Sorted before picking, because a HashMap would otherwise hand back a
    /// different answer on different runs and the hints would flicker. Sorting
    /// by key code also happens to prefer digits over the OEM keys, which are
    /// the ones that work on every layout.
    pub fn binding_for(&self, action: Action) -> Option<Chord> {
        let mut found: Vec<Chord> = self
            .map
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(c, _)| *c)
            .collect();
        // Fewest modifiers first: an action bound to both F1 and Ctrl+Shift+P
        // should be advertised as F1. Sorting by key code alone put the
        // letter ahead of the function key and told people to press the
        // harder one.
        found.sort_by_key(|c| {
            let mods = c.ctrl as u8 + c.shift as u8 + c.alt as u8;
            (mods, c.vk, c.ctrl, c.shift, c.alt)
        });
        found.into_iter().next()
    }

    pub fn bind(&mut self, chord: Chord, action: Action) {
        self.map.insert(chord, action);
    }

    pub fn unbind(&mut self, chord: &Chord) {
        self.map.remove(chord);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Default for Keymap {
    fn default() -> Self {
        use Action::*;
        let pairs: &[(&str, Action)] = &[
            // Both are bound on purpose. Ctrl+\ is what people expect coming
            // from other editors; Ctrl+2/3 is what actually works on layouts
            // where backslash needs AltGr.
            ("ctrl+backslash", SplitRight),
            ("ctrl+2", SplitRight),
            ("ctrl+shift+backslash", SplitDown),
            ("ctrl+3", SplitDown),
            ("ctrl+w", ClosePane),
            ("ctrl+t", OpenTerminal),
            ("ctrl+e", OpenExplorer),
            ("ctrl+b", ToggleExplorer),
            // F1 is the reminder in the corner, which is what people press F1
            // for: tell me, do not take me anywhere. The settings pane is a
            // place you go, so it gets the shifted pair. Both are function
            // keys because Ctrl+, is an OEM key and cannot be the only way in.
            // F1 is where people press for help, and the command list is the
            // better answer: it is the same list, searchable, and every row
            // does something. The pinned panel is a preference, so it moves to
            // the modified key.
            ("f1", Commands),
            ("ctrl+f1", ToggleHelp),
            ("shift+f1", OpenSettings),
            ("ctrl+comma", OpenSettings),
            ("ctrl+n", NewFile),
            ("ctrl+shift+n", NewFolder),
            ("f2", Rename),
            ("delete", Delete),
            ("alt+left", FocusLeft),
            ("alt+right", FocusRight),
            ("alt+up", FocusUp),
            ("alt+down", FocusDown),
            ("alt+1", FocusPane1),
            ("alt+2", FocusPane2),
            ("alt+3", FocusPane3),
            ("alt+4", FocusPane4),
            ("alt+5", FocusPane5),
            ("alt+6", FocusPane6),
            ("alt+7", FocusPane7),
            ("alt+8", FocusPane8),
            ("alt+9", FocusPane9),
            // Ctrl+Alt is AltGr on Windows, which is normally disqualifying
            // here — it is the reason this module exists. Arrow keys are the
            // exception: no layout maps AltGr+arrow to a character, so there
            // is nothing for the binding to swallow.
            ("ctrl+alt+right", GrowPaneWidth),
            ("ctrl+alt+left", ShrinkPaneWidth),
            ("ctrl+alt+down", GrowPaneHeight),
            ("ctrl+alt+up", ShrinkPaneHeight),
            // The caption buttons were mouse-only. Alt+F9 and Alt+F10 are the
            // long-standing Windows chords for exactly these two.
            ("alt+f9", MinimizeWindow),
            ("alt+f10", ToggleMaximize),
            // Not Ctrl+C: in a terminal that is SIGINT and stops the running
            // command. The terminal convention is the shifted pair.
            ("ctrl+shift+c", Copy),
            ("ctrl+shift+v", Paste),
            ("ctrl+s", Save),
            ("ctrl+z", Undo),
            ("ctrl+y", Redo),
            ("ctrl+shift+z", Redo),
            ("ctrl+a", SelectAll),
            ("ctrl+l", SelectLine),
            // Editor scope, so a shell keeps its Ctrl+M (which is Enter).
            // Ctrl+Shift+\ is the chord VS Code uses, and it is both taken by
            // the split and unpressable on the layouts this file exists for.
            ("ctrl+m", GoToBracket),
            ("ctrl+x", Cut),
            ("ctrl+plus", FontLarger),
            ("ctrl+numpadplus", FontLarger),
            ("ctrl+minus", FontSmaller),
            ("ctrl+numpadminus", FontSmaller),
            ("ctrl+p", GoToFile),
            ("ctrl+shift+p", Commands),
            // The oldest "open" there is. A shell's Ctrl+O is obscure enough
            // to lose — Ctrl+W already set that precedent for close.
            ("ctrl+o", OpenFolder),
            // Enter toggles a folder open; Ctrl+Enter moves in for good.
            ("ctrl+enter", WorkspaceHere),
            // The alt-tab of files. Tab is portable and Ctrl+Tab produces no
            // character, so no layout and no shell loses anything to it.
            ("ctrl+tab", LastFile),
            ("ctrl+f", Find),
            ("ctrl+h", Replace),
            ("ctrl+shift+f", FindInProject),
            ("ctrl+g", GoToLine),
            // The shifted pair of go-to-line, and the letter is the point.
            ("ctrl+shift+g", GitPanel),
            // Ctrl+/ is the one everyone knows, and on a Turkish Q layout it
            // is unreachable: VK_OEM_2 is `ö` there, and the slash itself
            // lives on Shift+7. Ctrl+7 is the same key on every layout, and
            // on the layouts where the slash is shifted onto 7 it is even the
            // same finger. Same trick as Ctrl+2 for the split.
            ("ctrl+/", ToggleComment),
            ("ctrl+7", ToggleComment),
            // Not Alt+arrow, which VS Code uses: those move pane focus here,
            // and a pane tree needs that more than one editor needs this.
            ("ctrl+shift+up", MoveLineUp),
            ("ctrl+shift+down", MoveLineDown),
            ("ctrl+d", DuplicateLine),
            // The chord VS Code taught everyone; the line, not the file, and
            // a letter, so it works on every layout.
            ("ctrl+shift+k", DeleteLine),
            // F-keys: the timer is a background thing and should not take a
            // letter someone might want for editing.
            ("f9", PomodoroToggle),
            ("shift+f9", PomodoroReset),
            ("ctrl+f9", PomodoroSkip),
            ("ctrl+q", Quit),
        ];
        let mut map = HashMap::with_capacity(pairs.len());
        for (chord, action) in pairs {
            // The defaults are ours; a typo here is a bug, not user input.
            map.insert(Chord::parse(chord).expect("built-in binding"), *action);
        }
        Self { map }
    }
}

impl<'de> Deserialize<'de> for Keymap {
    /// Merges over the defaults instead of replacing them.
    ///
    /// Replacing would mean rebinding one key costs you every other shortcut,
    /// which nobody wants and few would notice until something is missing.
    /// Bind to `"none"` to remove a default.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = HashMap::<String, String>::deserialize(d)?;
        let mut me = Keymap::default();
        for (chord, action) in raw {
            let chord = Chord::parse(&chord).map_err(D::Error::custom)?;
            if action.eq_ignore_ascii_case(UNBOUND) {
                me.unbind(&chord);
                continue;
            }
            let action: Action = serde::Deserialize::deserialize(
                serde::de::value::StrDeserializer::<D::Error>::new(&action),
            )
            .map_err(|_| D::Error::custom(format!("unknown action '{action}'")))?;
            me.bind(chord, action);
        }
        Ok(me)
    }
}

impl Serialize for Keymap {
    /// Writes every binding, and writes `"none"` for every default that is
    /// missing.
    ///
    /// The absence has to be spelled out. Deserializing merges over the
    /// defaults, so a chord that is simply not in the output comes back on the
    /// next load — an unbinding would survive being read but not being written,
    /// and the settings screen writes the whole config every time it saves.
    /// Leaving a gap here silently hands someone back a shortcut they took the
    /// trouble to remove.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        // Sorted so the output is stable; an unsorted HashMap would produce a
        // different file every time it was written.
        let mut pairs: Vec<(String, Option<Action>)> = self
            .map
            .iter()
            .map(|(c, a)| (c.to_string().to_lowercase(), Some(*a)))
            .collect();
        for chord in Keymap::default().map.keys() {
            if !self.map.contains_key(chord) {
                pairs.push((chord.to_string().to_lowercase(), None));
            }
        }
        pairs.sort_by(|a, b| a.0.cmp(&b.0));

        let mut m = s.serialize_map(Some(pairs.len()))?;
        for (chord, action) in pairs {
            match action {
                Some(a) => m.serialize_entry(&chord, &a)?,
                None => m.serialize_entry(&chord, UNBOUND)?,
            }
        }
        m.end()
    }
}

/// What a removed binding is written as, and read back as.
const UNBOUND: &str = "none";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_order_does_not_matter() {
        assert_eq!(Chord::parse("ctrl+shift+c"), Chord::parse("shift+ctrl+c"));
    }

    #[test]
    fn letters_and_digits_use_stable_codes() {
        // The whole reason the defaults prefer them.
        assert_eq!(Chord::parse("ctrl+c").unwrap().vk, b'C');
        assert_eq!(Chord::parse("ctrl+2").unwrap().vk, b'2');
    }

    #[test]
    fn case_is_ignored() {
        assert_eq!(Chord::parse("CTRL+Shift+C"), Chord::parse("ctrl+shift+c"));
    }

    #[test]
    fn plus_is_both_separator_and_key() {
        // `ctrl++` used to parse as "only modifiers": the key was eaten by the
        // separator split.
        assert_eq!(Chord::parse("ctrl++"), Chord::parse("ctrl+plus"));
        assert_eq!(Chord::parse("+").unwrap().vk, 0xBB);
    }

    #[test]
    fn errors_say_what_is_wrong() {
        assert!(Chord::parse("ctrl+").unwrap_err().contains("no key"));
        assert!(Chord::parse("ctrl+nope").unwrap_err().contains("unknown key"));
        assert!(Chord::parse("ctrl+a+b").unwrap_err().contains("more than one key"));
    }

    #[test]
    fn every_default_action_is_reachable_on_any_layout() {
        // The reason this module exists, as an assertion. An OEM key is a
        // different key on every layout — VK_OEM_2 is `ö` on a Turkish Q, and
        // the slash there is on Shift+7 — so an action whose only default sits
        // on one of those cannot be pressed at all by most of the world.
        // Ctrl+2 beside Ctrl+\ is this rule already being followed by hand;
        // this stops the next binding from forgetting.
        let k = Keymap::default();
        let stranded: Vec<&str> = Action::ALL
            .iter()
            .filter(|action| {
                let bound: Vec<Chord> = k
                    .map
                    .iter()
                    .filter(|(_, a)| *a == *action)
                    .map(|(c, _)| *c)
                    .collect();
                // Unbound by default is fine — the palette can still run it.
                !bound.is_empty() && !bound.iter().any(|c| c.is_portable())
            })
            .map(|a| a.title())
            .collect();

        assert!(stranded.is_empty(), "not pressable off US QWERTY: {stranded:?}");
    }

    #[test]
    fn the_oem_keys_are_the_ones_that_move() {
        // What `is_portable` is actually claiming, spelled out.
        for chord in ["ctrl+2", "ctrl+a", "f1", "alt+left", "ctrl+numpadplus", "delete"] {
            assert!(Chord::parse(chord).unwrap().is_portable(), "{chord}");
        }
        for chord in ["ctrl+/", "ctrl+backslash", "ctrl+comma", "ctrl+plus", "ctrl+["] {
            assert!(!Chord::parse(chord).unwrap().is_portable(), "{chord}");
        }
    }

    #[test]
    fn defaults_cover_the_turkish_layout_workaround() {
        let k = Keymap::default();
        let digit = Chord::parse("ctrl+2").unwrap();
        let backslash = Chord::parse("ctrl+backslash").unwrap();
        assert_eq!(k.lookup(digit.vk, true, false, false), Some(Action::SplitRight));
        assert_eq!(
            k.lookup(backslash.vk, true, false, false),
            Some(Action::SplitRight),
            "both must work: Ctrl+backslash is unpressable on some layouts"
        );
    }

    #[test]
    fn a_user_binding_does_not_wipe_the_defaults() {
        let k: Keymap = toml::from_str("\"ctrl+p\" = \"open-explorer\"").unwrap();
        assert_eq!(k.lookup(b'P', true, false, false), Some(Action::OpenExplorer));
        assert_eq!(k.lookup(b'T', true, false, false), Some(Action::OpenTerminal));
    }

    #[test]
    fn a_user_binding_overrides_a_default() {
        let k: Keymap = toml::from_str("\"ctrl+t\" = \"open-explorer\"").unwrap();
        assert_eq!(k.lookup(b'T', true, false, false), Some(Action::OpenExplorer));
    }

    #[test]
    fn an_unbinding_survives_being_written_out() {
        // The settings screen rewrites the whole config, and deserializing
        // merges over the defaults — so a chord left out of the output comes
        // straight back. Anyone who took the trouble to remove a shortcut
        // would silently get it handed back the next time they changed a
        // setting.
        let cfg: crate::Config = toml::from_str("[keys]
\"ctrl+q\" = \"none\"
").unwrap();
        let q = Chord::parse("ctrl+q").unwrap();
        assert_eq!(cfg.keys.lookup(q.vk, q.ctrl, q.shift, q.alt), None);

        let written = crate::minimal_toml(&cfg).unwrap();
        let back: crate::Config = toml::from_str(&written).unwrap();
        assert_eq!(
            back.keys.lookup(q.vk, q.ctrl, q.shift, q.alt),
            None,
            "written file was:
{written}"
        );
    }

    #[test]
    fn none_removes_a_default() {
        // Without this there is no way to get a shortcut out of the way.
        let k: Keymap = toml::from_str("\"ctrl+q\" = \"none\"").unwrap();
        assert_eq!(k.lookup(b'Q', true, false, false), None);
    }

    #[test]
    fn an_unknown_action_is_an_error() {
        let e = toml::from_str::<Keymap>("\"ctrl+p\" = \"make-coffee\"").unwrap_err();
        assert!(e.to_string().contains("make-coffee"), "{e}");
    }

    #[test]
    fn an_unparseable_chord_is_an_error() {
        let e = toml::from_str::<Keymap>("\"ctrl+nope\" = \"quit\"").unwrap_err();
        assert!(e.to_string().contains("unknown key"), "{e}");
    }

    #[test]
    fn what_we_print_is_what_we_can_parse() {
        // Display feeds the on-screen hints, and those strings must be valid
        // input — otherwise a hint tells the user something they can't type.
        for chord in Keymap::default().map.keys() {
            let printed = chord.to_string();
            assert_eq!(
                Chord::parse(&printed).unwrap(),
                *chord,
                "'{printed}' did not round-trip"
            );
        }
    }

    #[test]
    fn the_simplest_chord_is_the_one_advertised() {
        // Commands is on F1 and on Ctrl+Shift+P. Telling someone to press
        // three keys when one will do is a worse answer to "how do I open
        // this" than saying nothing.
        let k = Keymap::default();
        assert_eq!(k.binding_for(Action::Commands).unwrap().to_string(), "F1");
    }

    #[test]
    fn hints_are_stable_and_prefer_the_portable_binding() {
        // Split is bound twice; a HashMap would answer differently per run.
        let k = Keymap::default();
        let a = k.binding_for(Action::SplitRight).unwrap();
        let b = k.binding_for(Action::SplitRight).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.to_string(), "Ctrl+2");
    }

    #[test]
    fn a_serialized_keymap_parses_back() {
        let k = Keymap::default();
        let text = toml::to_string(&k).unwrap();
        assert_eq!(toml::from_str::<Keymap>(&text).unwrap(), k);
    }

    #[test]
    fn every_action_is_listed_and_named() {
        // A missing entry means an action nobody can find in the palette.
        // `title` is exhaustive by construction, so this only has to check
        // that ALL covers everything — which it does if the count matches the
        // number of arms, kept honest by the assertion below.
        assert!(Action::ALL.contains(&Action::Quit));
        assert!(!Action::ALL.contains(&Action::Commands), "not self-listing");
        for a in Action::ALL {
            assert!(!a.title().is_empty(), "{a:?} has no title");
        }
    }

    #[test]
    fn titles_are_unique() {
        // Two rows reading the same thing is a coin flip for the user.
        let mut seen = std::collections::HashSet::new();
        for a in Action::ALL {
            assert!(seen.insert(a.title()), "duplicate title: {}", a.title());
        }
    }

    #[test]
    fn no_default_binding_is_shadowed_by_another() {
        // Building the map from a list means a duplicate chord silently wins
        // over the one before it — which is how Alt+Up stopped moving pane
        // focus the moment it was also bound to moving a line.
        let mut seen: HashMap<Chord, Action> = HashMap::new();
        let map = Keymap::default();
        assert_eq!(
            map.len(),
            {
                for (chord, action) in map.map.iter() {
                    seen.insert(*chord, *action);
                }
                seen.len()
            },
            "two default bindings share a chord"
        );
    }

    #[test]
    fn the_keyboard_can_reach_panes_and_the_caption_buttons() {
        // The point of the project is never needing the mouse, so these have
        // to work out of the box rather than only after someone binds them.
        let k = Keymap::default();
        for (chord, want) in [
            ("alt+1", Action::FocusPane1),
            ("alt+9", Action::FocusPane9),
            ("ctrl+alt+right", Action::GrowPaneWidth),
            ("ctrl+alt+left", Action::ShrinkPaneWidth),
            ("ctrl+alt+down", Action::GrowPaneHeight),
            ("ctrl+alt+up", Action::ShrinkPaneHeight),
            ("alt+f9", Action::MinimizeWindow),
            ("alt+f10", Action::ToggleMaximize),
        ] {
            let c = Chord::parse(chord).unwrap();
            assert_eq!(k.lookup(c.vk, c.ctrl, c.shift, c.alt), Some(want), "{chord}");
        }
    }

    #[test]
    fn the_numbered_jumps_count_from_one_and_land_on_zero() {
        // One-based in the binding because people count panes from one,
        // zero-based coming out because that is what indexes a list.
        assert_eq!(Action::FocusPane1.pane_index(), Some(0));
        assert_eq!(Action::FocusPane9.pane_index(), Some(8));
        assert_eq!(Action::Quit.pane_index(), None);

        // Every one of them, in order, with no gaps or repeats.
        let indices: Vec<usize> = Action::ALL
            .iter()
            .filter_map(|a| a.pane_index())
            .collect();
        assert_eq!(indices, (0..9).collect::<Vec<_>>());
    }

    #[test]
    fn pane_focus_keeps_the_alt_arrows() {
        let k = Keymap::default();
        for (chord, want) in [
            ("alt+left", Action::FocusLeft),
            ("alt+right", Action::FocusRight),
            ("alt+up", Action::FocusUp),
            ("alt+down", Action::FocusDown),
        ] {
            let c = Chord::parse(chord).unwrap();
            assert_eq!(k.lookup(c.vk, c.ctrl, c.shift, c.alt), Some(want), "{chord}");
        }
    }

    #[test]
    fn editor_actions_do_not_claim_the_shell_control_keys() {
        // Ctrl+A, Ctrl+Z and Ctrl+X are line-start, suspend and cut in a shell.
        // Taking them globally would break the terminal silently.
        for chord in ["ctrl+a", "ctrl+z", "ctrl+x", "ctrl+s"] {
            let c = Chord::parse(chord).unwrap();
            let action = Keymap::default().lookup(c.vk, c.ctrl, c.shift, c.alt).unwrap();
            assert_eq!(action.scope(), Scope::Editor, "{chord}");
        }
    }

    #[test]
    fn copy_and_paste_stay_global() {
        // They're on the shifted pair, which no shell reads as a control char.
        assert_eq!(Action::Copy.scope(), Scope::Global);
        assert_eq!(Action::Paste.scope(), Scope::Global);
    }

    #[test]
    fn modifiers_are_part_of_the_match() {
        let k = Keymap::default();
        assert_eq!(k.lookup(b'C', true, true, false), Some(Action::Copy));
        assert_eq!(k.lookup(b'C', true, false, false), None, "plain Ctrl+C is SIGINT");
    }
}
