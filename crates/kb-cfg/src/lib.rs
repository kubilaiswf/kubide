//! Configuration and theming.
//!
//! Deliberate order: the `Config` struct and a TOML surface first, Luau later.
//! Nothing else in the app reads a file — everyone reads this struct. When the
//! surface changes the struct won't, so the layers above stay untouched.
//!
//! This crate has no Windows dependency on purpose: config is pure data and
//! stays testable without opening a window. Types like `Backdrop` are declared
//! here and mapped to the platform enum at the call site.

pub mod color;
pub mod keys;
pub mod settings;
pub mod snippets;
pub mod theme;
pub mod watch;

pub use color::Color;
pub use keys::{Action, Chord, Keymap, Scope};
pub use settings::Setting;
pub use theme::{Ansi, Caption, GitColors, SyntaxColors, TerminalColors, Theme};
pub use watch::Watcher;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub window: Window,
    pub font: Font,
    pub terminal: Terminal,
    pub theme: Theme,
    /// The name the theme was loaded from, when it came from a file. Not
    /// part of the TOML surface — the load and save layers spell it
    /// `theme = "name"` themselves — but carried here so the settings
    /// screen can show it and cycle it.
    #[serde(skip)]
    pub theme_name: Option<String>,
    pub status: Status,
    pub pomodoro: Pomodoro,
    pub help: Help,
    pub editor: Editing,
    pub cursor: Cursor,
    pub vim: Vim,
    pub agent: Agent,
    /// Chord to action. Merges over the defaults rather than replacing them.
    pub keys: Keymap,
}

/// The agent pane: how Claude Code is started.
///
/// Passed through to the CLI rather than interpreted. Its permission modes
/// and tool patterns are its own vocabulary, documented on its side, and a
/// translation layer here would only lag behind it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Agent {
    /// The executable: `claude` on PATH, or a full path to it.
    pub command: String,
    /// `None` leaves the CLI's own default.
    pub model: Option<String>,
    /// `default`, `acceptEdits`, `plan`, `bypassPermissions`, …
    pub permission_mode: String,
    /// `--allowedTools` patterns: what never needs asking. Everything
    /// else the CLI would ask about comes up as a question in the pane.
    pub allowed_tools: Vec<String>,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            command: "claude".into(),
            model: None,
            // The CLI's own default: edits and commands are asked about,
            // in the same box the editor asks about unsaved work.
            permission_mode: "default".into(),
            allowed_tools: Vec::new(),
        }
    }
}

/// The mouse pointer, drawn by kubide rather than borrowed from Windows.
///
/// Everything about it is a setting because a pointer is taste and nothing
/// but: too small for one person is exactly right for another, and the only
/// wrong answer is deciding for them.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cursor {
    /// Off by default: out of the box kubide wears Windows' own pointers,
    /// and the right roles still appear in the right places — I-beam over
    /// text, hand over the clickable, resize arrows on the dividers. `true`
    /// swaps them for the drawn set below (or your own files), which is a
    /// taste someone should choose, not inherit.
    pub custom: bool,
    /// Pointer canvas in pixels, 12 to 128. `0` follows the system cursor
    /// size, which is where Windows' accessibility setting lives.
    pub size: u32,
    /// `"accent"` follows the theme; any `"#rrggbb"` pins a colour.
    pub color: String,
    /// Colours for one role only, when the one `color` should not dress
    /// everything: a classic near-black arrow over a theme-coloured I-beam,
    /// say. Empty follows `color`. The outline picks its own side — light
    /// around a dark body, dark around a light one — so any colour works.
    pub pointer_color: String,
    pub text_color: String,
    /// The shape over everything that is not text.
    pub pointer: PointerStyle,
    /// The shape over text.
    pub text: TextPointerStyle,
    /// Paths to `.cur` or `.ani` files, one per role; empty means "draw it".
    /// A file beats the drawn shape, so any downloaded cursor pack drops
    /// straight in. An unreadable file falls back to the drawn shape rather
    /// than to nothing.
    pub pointer_file: String,
    pub text_file: String,
    pub hand_file: String,
    /// The two resize arrows are separate files because no single file can
    /// face both ways.
    pub resize_we_file: String,
    pub resize_ns_file: String,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            custom: false,
            size: 0,
            color: "accent".into(),
            pointer_color: String::new(),
            text_color: String::new(),
            pointer: PointerStyle::Arrow,
            text: TextPointerStyle::Ibeam,
            pointer_file: String::new(),
            text_file: String::new(),
            hand_file: String::new(),
            resize_we_file: String::new(),
            resize_ns_file: String::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PointerStyle {
    /// The classic pointer, redrawn slimmer and softer than the system's.
    Arrow,
    /// A four-point dart — nothing like the stock pointer, on purpose.
    Dart,
    /// Just the tip — a minimal sliver of a pointer.
    Triangle,
    /// The TempleOS pointer, traced from the original, hard pixels kept.
    /// In loving memory of Terry A. Davis.
    Temple,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextPointerStyle {
    /// Stem and serifs, the shape everyone knows.
    Ibeam,
    /// The stem alone, for people who find serifs fussy.
    Bar,
}

/// Editing behaviour switches. Both on by default and both plain on/off:
/// people who hate auto-closing hate it completely, and a half setting
/// would satisfy nobody.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Editing {
    /// `(` brings `)` with the caret between; the closer steps over
    /// itself; backspace between a pair takes both.
    pub auto_close: bool,
    /// Tab expands trigger words from the snippets folder.
    pub snippets: bool,
}

impl Default for Editing {
    fn default() -> Self {
        Self { auto_close: true, snippets: true }
    }
}

/// Modal editing. Off by default: vim is a language, and nobody should find
/// themselves speaking it because they opened a file.
///
/// The four search and clipboard options mirror vim's own `ignorecase`,
/// `smartcase`, `hlsearch` and `clipboard=unnamedplus`, and `:set` changes
/// them for the session; this table is where they start.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Vim {
    pub enabled: bool,
    /// Whether vim's Ctrl chords (Ctrl+R redo, Ctrl+D half a page, Ctrl+W
    /// window commands, …) beat the `[keys]` table while an editor is in a
    /// vim mode. Off, the table always wins and vim only gets the chords it
    /// leaves unbound.
    pub ctrl_keys: bool,
    /// The unnamed register is the system clipboard, so `y` copies and `p`
    /// pastes what other programs see. Vim's `clipboard=unnamedplus`.
    pub clipboard: bool,
    pub ignorecase: bool,
    pub smartcase: bool,
    pub hlsearch: bool,
}

impl Default for Vim {
    fn default() -> Self {
        Self { enabled: false, ctrl_keys: true, clipboard: false, ignorecase: false, smartcase: false, hlsearch: true }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Window {
    pub backdrop: Backdrop,
    /// Title bar height in DIPs.
    pub caption_height: f32,
    /// Gap between the window edge and the pane area.
    pub padding: f32,
}

/// Mirrors DWM's backdrop materials. Kept as our own enum so this crate stays
/// platform-free; `kubide` maps it to `kb_win::Backdrop`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backdrop {
    None,
    /// Samples the wallpaper once. Cheap, but opaque.
    Mica,
    MicaAlt,
    /// Blurs live content behind the window. The default.
    #[default]
    Acrylic,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    /// Tried in order; the first one installed wins. A list rather than one
    /// name because a config that's unusable on a machine missing the font is
    /// worse than one that quietly falls back.
    pub family: Vec<String>,
    pub size: f32,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Terminal {
    /// Shell to launch. `None` uses the system default.
    pub shell: Option<String>,
    pub args: Vec<String>,
    /// Scrollback lines. Without it you can't see the top of a build log.
    pub scrollback: usize,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            backdrop: Backdrop::Acrylic,
            caption_height: 40.0,
            padding: 14.0,
        }
    }
}

impl Default for Font {
    fn default() -> Self {
        Self {
            // Mono variants first. Their icons are one cell wide, which is
            // what a grid renderer needs; in the default variants an icon is
            // nearly two cells and shifts every name in the file tree.
            //
            // Two spellings each. Nerd Fonts abbreviates the Win32 family name
            // to fit LOGFONT.lfFaceName, which is LF_FACESIZE = 32 wide chars
            // including the terminator — "NFM" is "Nerd Font Mono" — and which
            // spelling a machine has depends on when the font was downloaded.
            // Listing both beats betting on a release date.
            family: vec![
                "JetBrainsMono NFM".into(),
                "JetBrainsMono Nerd Font Mono".into(),
                "CaskaydiaCove NFM".into(),
                "CaskaydiaCove Nerd Font Mono".into(),
                "FiraCode NFM".into(),
                "FiraCode Nerd Font Mono".into(),
                // Double-width icons still beat no icons.
                "JetBrainsMono Nerd Font".into(),
                "CaskaydiaCove Nerd Font".into(),
                "FiraCode Nerd Font".into(),
                // Nothing below here has icons; the tree falls back to
                // triangle expanders and the two symbols are dropped.
                "JetBrains Mono".into(),
                "Cascadia Code".into(),
                "Consolas".into(),
            ],
            size: 14.0,
        }
    }
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            shell: None,
            args: Vec::new(),
            scrollback: 10_000,
        }
    }
}

/// What the status bar shows.
///
/// Every segment is a plain on/off. A status bar you cannot turn things off in
/// becomes a place other people's ideas accumulate.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Status {
    /// Ln/Col and the selection size.
    pub cursor: bool,
    pub font: bool,
    pub panes: bool,
    /// Frame time. On by default because it is the honesty check: the moment
    /// this creeps up, something got slow.
    pub frame_time: bool,
    pub git: bool,
    pub clock: bool,
    /// 24-hour clock. `false` gives 12-hour.
    pub clock_24h: bool,
    pub pomodoro: bool,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            cursor: true,
            font: true,
            panes: true,
            frame_time: true,
            git: true,
            clock: false,
            clock_24h: true,
            pomodoro: false,
        }
    }
}

/// The quiet reminder on the status bar that a command list exists.
///
/// Its own table rather than a flag in `[status]`: those switches choose what
/// the bar reports about the session, and this is not a readout — it names
/// the keys that open the rest of the program.
///
/// Only the reminder is configurable. The list it points at is opened and
/// closed with the key, and is deliberately not remembered — twenty-odd rows
/// pinned over the code permanently is a panel, not a reminder.
///
/// On by default, because nothing else here says how to leave a file, close a
/// pane or reach the settings, and a few grey words at the end of the bar are
/// a small price for not being stuck in whatever you opened.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Help {
    pub visible: bool,
}

impl Default for Help {
    fn default() -> Self {
        Self { visible: true }
    }
}

/// The work timer.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Pomodoro {
    /// Minutes of work before a break.
    pub work: u32,
    pub short_break: u32,
    pub long_break: u32,
    /// Work periods before the long break.
    pub rounds: u32,
    /// Start the next period automatically. Off by default: a timer that
    /// starts counting your break while you are still typing is nagging, not
    /// helping.
    pub auto_advance: bool,
}

impl Default for Pomodoro {
    fn default() -> Self {
        Self {
            work: 25,
            short_break: 5,
            long_break: 15,
            rounds: 4,
            auto_advance: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Loading

/// Result of a load attempt.
///
/// There is no error variant on purpose. A broken config must not stop the
/// editor from starting — that's exactly when you need it to fix the file.
/// The message is carried instead so the UI can show it.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    /// The theme file actually read, when it came from disk. Watched, so
    /// editing a theme repaints live exactly like editing the config does.
    /// The name itself rides in `config.theme_name`.
    pub theme_path: Option<PathBuf>,
    /// The workspace's own `.kubide\config.toml`, when it exists — watched
    /// for the same reason the others are.
    pub workspace_path: Option<PathBuf>,
    /// `None` if the file parsed, or if there was no file at all.
    pub problem: Option<String>,
}

/// Where kubide keeps what is its own: `%APPDATA%\kubide` on Windows,
/// `$XDG_CONFIG_HOME/kubide` (so `~/.config/kubide`) on Linux.
///
/// One folder for the config, the themes, the snippets and the sessions,
/// found the way the desktop says to find it — Explorer's AppData on one,
/// the XDG base directories on the other — so nothing is left where a
/// dotfile manager or a backup would not look.
pub fn data_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("kubide")
}

/// `config.toml` in [`data_dir`], or `$KUBIDE_CONFIG` if set.
///
/// The override exists for testing and for portable installs; without it the
/// only way to try a config is to overwrite your real one.
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("KUBIDE_CONFIG") {
        return PathBuf::from(p);
    }
    data_dir().join("config.toml")
}

pub fn load() -> Loaded {
    load_from(config_path())
}

/// Header on a written config, so nobody wonders where the file came from.
const WRITTEN_BY: &str = "\
# Written by kubide's settings screen.
#
# Editing it by hand is still fine — the screen only covers what two arrow keys
# can express. config.example.toml in the repository documents every option,
# including the ones that are not on the screen.
#
# Anything left at its default is not written, so this file stays a list of
# what you changed.

";

/// Writes a config back, keeping only what differs from the defaults.
///
/// Comments in the old file do not survive; a format-preserving TOML editor is
/// a dependency this does not justify, and the settings screen says as much
/// before it writes.
pub fn save(config: &Config, path: &Path) -> Result<(), String> {
    save_named(config, None, path)
}

/// Like [`save`], but keeps a `theme = "name"` reference alive.
///
/// The loaded config carries the resolved colours, and writing those out
/// would pin them: the file would stop following the theme it named, and a
/// later edit to that theme would silently do nothing. So the reference is
/// written back as the reference, and the resolved colours are reset to the
/// default first, so the pruning drops every one of them.
pub fn save_named(config: &Config, theme_name: Option<&str>, path: &Path) -> Result<(), String> {
    let mut header = String::new();
    let pruned = match theme_name {
        Some(name) => {
            header = format!("theme = {name:?}\n\n");
            let mut c = config.clone();
            c.theme = Theme::default();
            c
        }
        None => config.clone(),
    };
    let body = minimal_toml(&pruned)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, format!("{WRITTEN_BY}{header}{body}")).map_err(|e| e.to_string())
}

/// The config as TOML with every default left out.
///
/// Writing the whole struct would copy all hundred-odd theme colours into the
/// user's file. That pins them: a later kubide with a better default would be
/// silently overridden by a copy nobody chose. It also buries the four things
/// someone actually changed among two hundred lines they did not.
pub fn minimal_toml(config: &Config) -> Result<String, String> {
    let current = toml::Value::try_from(config).map_err(|e| e.to_string())?;
    let default = toml::Value::try_from(Config::default()).map_err(|e| e.to_string())?;
    match prune(current, &default) {
        Some(v) => toml::to_string_pretty(&v).map_err(|e| e.to_string()),
        // Everything is default, so the file says nothing rather than nothing
        // being written — an empty config is a valid one.
        None => Ok(String::new()),
    }
}

/// Drops anything equal to the default, and any table left empty by that.
fn prune(value: toml::Value, default: &toml::Value) -> Option<toml::Value> {
    match (value, default) {
        (toml::Value::Table(table), toml::Value::Table(defaults)) => {
            let kept: toml::Table = table
                .into_iter()
                .filter_map(|(key, v)| match defaults.get(&key) {
                    Some(d) => prune(v, d).map(|v| (key, v)),
                    // A key the defaults have never heard of is a change by
                    // definition — a rebound chord, most likely.
                    None => Some((key, v)),
                })
                .collect();
            (!kept.is_empty()).then_some(toml::Value::Table(kept))
        }
        (v, d) => (v != *d).then_some(v),
    }
}

/// Loads from an explicit path. Exists so tests don't have to set a
/// process-wide environment variable to try a config.
pub fn load_from(path: PathBuf) -> Loaded {
    let mut table = toml::Table::new();
    let problem = match read_table(&path) {
        Ok(Some(t)) => {
            table = t;
            None
        }
        Ok(None) => None,
        Err(e) => Some(e),
    };
    finish(table, path, None, problem)
}

/// The user's config with the workspace's own laid over it.
///
/// `.kubide\config.toml` in the project speaks last: a project that wants
/// its own theme, font size or shell says so in a file that travels with
/// the project, the way `.vscode` taught everyone to expect. Keys the
/// project does not mention keep the user's answer, and a broken project
/// file costs its own overrides, never the user's config.
pub fn load_workspace(root: &Path) -> Loaded {
    let user_path = config_path();
    let ws_path = workspace_config_path(root);

    let mut table = toml::Table::new();
    let mut problem = None;
    let mut note = |p: Option<String>| {
        if problem.is_none() {
            problem = p;
        }
    };

    match read_table(&user_path) {
        Ok(Some(t)) => merge(&mut table, t),
        Ok(None) => {}
        Err(e) => note(Some(format!("config: {e}"))),
    }
    let mut workspace_path = None;
    match read_table(&ws_path) {
        Ok(Some(mut t)) => {
            // The agent section is the user's alone. A project's `.kubide`
            // travels with the clone, and a clone that could hand itself
            // `bypassPermissions` or a Bash allowlist would be a project
            // that runs whatever it likes on first open.
            if t.remove("agent").is_some() {
                note(Some(".kubide: [agent] is ignored here — agent settings come from your own config".into()));
            }
            merge(&mut table, t);
            workspace_path = Some(ws_path);
        }
        Ok(None) => {}
        Err(e) => note(Some(format!(".kubide: {e}"))),
    }
    finish(table, user_path, workspace_path, problem)
}

/// Where a workspace's own overrides live.
pub fn workspace_config_path(root: &Path) -> PathBuf {
    root.join(".kubide").join("config.toml")
}

/// Writes a starter `.kubide\config.toml` — the mark `kubide workspace`
/// leaves. Only when missing: a mark is not a reset button.
pub fn seed_workspace(root: &Path) {
    let path = workspace_config_path(root);
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        // The config is meant to be committed; the layout kubide writes
        // beside it is one machine's arrangement of one checkout and is not.
        let ignore = parent.join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(ignore, "session\n");
        }
    }
    let _ = std::fs::write(
        path,
        "# This folder is a kubide workspace.\n\
         #\n\
         # Anything from config.toml works here and wins over your user config\n\
         # while this project is open — a per-project theme, font size, shell.\n\
         # config.example.toml in the kubide repository documents every option.\n\
         # Empty is fine: the file alone marks the folder as a workspace, so a\n\
         # bare `kubide` here opens straight in.\n\n\
         # theme = \"gruvbox\"\n\
         # [font]\n\
         # size = 15.0\n",
    );
}

/// Reads one config file as a raw table. `Ok(None)` means no file, which
/// is the normal state and not a problem.
fn read_table(path: &Path) -> Result<Option<toml::Table>, String> {
    let text = match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
        // toml's message carries line and column; ours would be worse.
        Ok(text) => text,
    };
    toml::from_str::<toml::Table>(&text)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Lays `over` onto `base`, table by table: a project that sets one key of
/// `[font]` must not wipe the rest of the user's `[font]`.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.remove(&key), value) {
            (Some(toml::Value::Table(mut b)), toml::Value::Table(o)) => {
                merge(&mut b, o);
                base.insert(key, toml::Value::Table(b));
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// The shared tail of every load: pull the theme name out, parse, resolve.
fn finish(
    mut table: toml::Table,
    path: PathBuf,
    workspace_path: Option<PathBuf>,
    earlier: Option<String>,
) -> Loaded {
    // `theme` has two spellings on purpose: `theme = "gruvbox"` names a
    // file, `[theme]` is the colours inline. TOML refuses a key given
    // twice, so the two can never fight — and the string form has to come
    // out before Config parses, which only knows the table.
    let theme_name = match table.get("theme") {
        Some(toml::Value::String(s)) => {
            let s = s.clone();
            table.remove("theme");
            Some(s)
        }
        _ => None,
    };
    let (mut config, mut problem) = match toml::Value::Table(table).try_into::<Config>() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e.to_string())),
    };

    let mut theme_path = None;
    if let Some(name) = &theme_name {
        match resolve_theme_in(&themes_dir(), name) {
            Ok((theme, from)) => {
                config.theme = theme;
                theme_path = from;
            }
            // The name failing must not cost the rest of the config.
            Err(e) => problem = problem.or(Some(e)),
        }
    }
    config.theme_name = theme_name;
    Loaded {
        config,
        path,
        theme_path,
        workspace_path,
        // The earliest problem wins the status bar; one line is all it has.
        problem: earlier.or(problem),
    }
}

// ---------------------------------------------------------------------------
// Theme files

/// The themes that ship inside the binary, name to TOML text.
///
/// In the binary rather than only on disk, so `theme = "gruvbox"` works on a
/// machine that has never had a themes folder — and seeded to disk at
/// startup, so there is always a real file to copy from. Modding starts by
/// reading someone else's file.
pub const BUILTIN_THEMES: &[(&str, &str)] = &[
    ("gruvbox", include_str!("../themes/gruvbox.toml")),
    ("catppuccin", include_str!("../themes/catppuccin.toml")),
    ("tokyonight", include_str!("../themes/tokyonight.toml")),
    ("nord", include_str!("../themes/nord.toml")),
    ("rose-pine", include_str!("../themes/rose-pine.toml")),
    // Built from colorhunt.co's most liked palettes rather than ported from
    // other editors — each one grows a four-colour palette into the full
    // format, so the herd stays recognisably that palette.
    ("midnight-teal", include_str!("../themes/midnight-teal.toml")),
    ("deep-ocean", include_str!("../themes/deep-ocean.toml")),
    ("neon-sushi", include_str!("../themes/neon-sushi.toml")),
    ("steel", include_str!("../themes/steel.toml")),
    ("coral-reef", include_str!("../themes/coral-reef.toml")),
    ("desert-night", include_str!("../themes/desert-night.toml")),
    ("moss", include_str!("../themes/moss.toml")),
    ("mulberry", include_str!("../themes/mulberry.toml")),
    ("harbor", include_str!("../themes/harbor.toml")),
    ("aurora", include_str!("../themes/aurora.toml")),
    ("evergreen", include_str!("../themes/evergreen.toml")),
    ("paper", include_str!("../themes/paper.toml")),
];

/// `themes` beside the config file, so `KUBIDE_CONFIG` moves both at once.
pub fn themes_dir() -> PathBuf {
    config_path()
        .parent()
        .map(|p| p.join("themes"))
        .unwrap_or_else(|| PathBuf::from("themes"))
}

/// Writes the built-in themes into the themes folder — only the missing
/// ones, because a seeded file is the user's the moment they touch it.
///
/// This is what makes the folder discoverable in a compiled build: an empty
/// directory teaches nothing, five working files teach the whole format.
pub fn seed_themes() {
    let dir = themes_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (name, text) in BUILTIN_THEMES {
        let path = dir.join(format!("{name}.toml"));
        if !path.exists() {
            let _ = std::fs::write(path, text);
        }
    }
}

/// Every theme that can be named right now: "default" first, then the
/// built-ins and whatever the folder holds, sorted, each once. What the
/// settings screen's arrows walk through.
pub fn available_themes() -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_THEMES.iter().map(|(n, _)| n.to_string()).collect();
    if let Ok(entries) = std::fs::read_dir(themes_dir()) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "toml") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    if !names.iter().any(|n| n == stem) {
                        names.push(stem.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    names.insert(0, "default".to_string());
    names
}

/// A named theme: the file in `dir` when one exists, the built-in
/// otherwise. A file wins over its built-in namesake, or editing a seeded
/// theme would do nothing forever with no error saying why.
///
/// Returns the theme and the path it came from, when it came from disk.
pub(crate) fn resolve_theme_in(dir: &Path, name: &str) -> Result<(Theme, Option<PathBuf>), String> {
    let file = dir.join(format!("{name}.toml"));
    if let Ok(text) = std::fs::read_to_string(&file) {
        return match toml::from_str::<Theme>(&text) {
            Ok(t) => Ok((t, Some(file))),
            Err(e) => Err(format!("theme '{name}': {e}")),
        };
    }
    if let Some((_, text)) = BUILTIN_THEMES.iter().find(|(n, _)| *n == name) {
        return match toml::from_str::<Theme>(text) {
            Ok(t) => Ok((t, None)),
            // Built-ins are covered by a test; reaching this is a bug here.
            Err(e) => Err(format!("built-in theme '{name}': {e}")),
        };
    }
    Err(format!(
        "theme '{name}' not found — expected {}",
        file.display()
    ))
}

// ---------------------------------------------------------------------------
// Reload

/// What has to be rebuilt after a config change.
///
/// Reloading everything would be simpler and wrong: changing one color would
/// tear down font atlases and drop terminal state. The point of hot reload is
/// that the edit lands without disturbing what you were doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Refresh {
    /// Font family or size changed — text engine and cell metrics rebuild,
    /// which also resizes every open terminal.
    pub font: bool,
    /// Colors only. A redraw is enough.
    pub paint: bool,
    /// DWM backdrop changed — needs a window attribute call.
    pub window: bool,
    /// Caption height or padding — recompute the layout.
    pub layout: bool,
    /// Shell or scrollback. Applies to terminals opened from now on; we don't
    /// restart running ones, because that would kill the user's session.
    pub terminal_next: bool,
    /// Bindings changed. Nothing to rebuild — the next key press reads the new
    /// map — but it's listed so a keymap edit still counts as a change.
    pub keys: bool,
    /// The `[vim]` table changed: the shared vim session re-reads its options
    /// from it. Only then, so a `:set` typed mid-session survives an unrelated
    /// config edit.
    pub vim: bool,
}

impl Refresh {
    pub fn any(self) -> bool {
        self.font || self.paint || self.window || self.layout || self.terminal_next || self.keys || self.vim
    }
}

impl Config {
    /// Diff against the previously applied config.
    pub fn refresh_from(&self, old: &Config) -> Refresh {
        Refresh {
            font: self.font != old.font,
            window: self.window.backdrop != old.window.backdrop,
            layout: self.window.caption_height != old.window.caption_height
                || self.window.padding != old.window.padding,
            terminal_next: self.terminal != old.terminal,
            keys: self.keys != old.keys,
            vim: self.vim != old.vim,
            paint: self.theme != old.theme
                || self.status != old.status
                || self.pomodoro != old.pomodoro
                || self.help != old.help,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_the_default_config() {
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
    }

    #[test]
    fn partial_config_keeps_the_rest() {
        let c: Config = toml::from_str("[font]\nsize = 18.0\n").unwrap();
        assert_eq!(c.font.size, 18.0);
        assert_eq!(c.font.family, Font::default().family);
        assert_eq!(c.window, Window::default());
    }

    #[test]
    fn backdrop_is_written_in_kebab_case() {
        let c: Config = toml::from_str("[window]\nbackdrop = \"mica-alt\"\n").unwrap();
        assert_eq!(c.window.backdrop, Backdrop::MicaAlt);
    }

    #[test]
    fn a_broken_file_does_not_stop_startup() {
        let dir = std::env::temp_dir().join("kubide-cfg-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("broken.toml");
        std::fs::write(&p, "font = ][").unwrap();

        let loaded = load_from(p);
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.problem.is_some(), "the message must survive");
    }

    #[test]
    fn missing_file_is_not_a_problem() {
        let loaded = load_from(PathBuf::from("no-such-file-here.toml"));
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.problem, None);
    }

    #[test]
    fn a_color_change_does_not_rebuild_fonts() {
        let old = Config::default();
        let mut new = old.clone();
        new.theme.accent = Color::rgb(0xff, 0, 0);

        let r = new.refresh_from(&old);
        assert!(r.paint);
        assert!(!r.font, "a color must not touch the font atlas");
        assert!(!r.layout);
        assert!(!r.terminal_next);
    }

    #[test]
    fn a_font_size_change_asks_for_a_font_rebuild() {
        let old = Config::default();
        let mut new = old.clone();
        new.font.size = 20.0;

        let r = new.refresh_from(&old);
        assert!(r.font);
        assert!(!r.paint);
    }

    #[test]
    fn no_change_means_no_work() {
        assert!(!Config::default().refresh_from(&Config::default()).any());
    }

    /// The shipped example must parse and must equal the defaults. An example
    /// that has drifted from the code is worse than no example: it teaches
    /// people options that no longer exist.
    #[test]
    fn an_untouched_config_writes_nothing() {
        // The file is meant to be a list of what you changed. Changing
        // nothing is a legal answer.
        assert_eq!(minimal_toml(&Config::default()).unwrap(), "");
    }

    #[test]
    fn only_what_changed_is_written() {
        let mut c = Config::default();
        c.status.clock = !c.status.clock;
        let text = minimal_toml(&c).unwrap();

        assert!(text.contains("clock"), "{text}");
        // The whole point: one toggle must not drag the theme in with it.
        assert!(!text.contains("[theme"), "{text}");
        assert!(!text.contains("keys"), "{text}");
        assert!(!text.contains("frame_time"), "{text}");
    }

    #[test]
    fn a_written_config_reads_back_the_same() {
        // Every setting moved off its default, so nothing is being carried by
        // the defaults filling it back in. The theme row is the exception:
        // its name travels outside Config's TOML, through save_named, and
        // has its own round-trip test.
        let mut c = Config::default();
        for s in Setting::ALL {
            if *s == Setting::ThemeFile {
                continue;
            }
            s.step(&mut c, 1);
        }
        let text = minimal_toml(&c).unwrap();
        let back: Config = toml::from_str(&text).expect("a written config must parse");
        assert_eq!(back, c);
    }

    #[test]
    fn a_rebound_key_survives_the_write() {
        // Keys merge over the defaults rather than replacing them, so a keymap
        // pruned to nothing would quietly drop the rebind.
        let c = Config {
            keys: toml::from_str(r#""ctrl+alt+j" = "save""#).unwrap(),
            ..Default::default()
        };
        let text = minimal_toml(&c).unwrap();
        assert!(text.contains("ctrl+alt+j"), "{text}");

        let back: Config = toml::from_str(&text).unwrap();
        let chord = Chord::parse("ctrl+alt+j").unwrap();
        assert_eq!(
            back.keys.lookup(chord.vk, chord.ctrl, chord.shift, chord.alt),
            Some(Action::Save)
        );
    }

    #[test]
    fn saving_creates_the_folder_it_needs() {
        // %APPDATA%\kubide does not exist until something writes there, and
        // the settings screen is now the first thing that might.
        let dir = std::env::temp_dir().join("kubide-cfg-save/nested");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        let path = dir.join("config.toml");

        let mut c = Config::default();
        c.font.size = 18.0;
        save(&c, &path).expect("save should create the folder");

        let back = load_from(path);
        assert_eq!(back.problem, None);
        assert_eq!(back.config.font.size, 18.0);
    }

    #[test]
    fn the_example_config_matches_the_defaults() {
        let text = include_str!("../../../config.example.toml");
        let c: Config = toml::from_str(text).expect("config.example.toml must parse");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn a_workspace_config_overrides_key_by_key() {
        let mut base: toml::Table =
            toml::from_str("[font]\nsize = 16.0\n[status]\nclock = true").unwrap();
        let over: toml::Table = toml::from_str("[font]\nsize = 12.0").unwrap();
        merge(&mut base, over);
        let cfg: Config = toml::Value::Table(base).try_into().unwrap();
        assert_eq!(cfg.font.size, 12.0, "the project speaks last");
        assert!(cfg.status.clock, "what it does not mention survives");
        assert_eq!(cfg.font.family, Font::default().family);
    }

    #[test]
    fn a_workspace_cannot_grant_the_agent_anything() {
        let root = std::env::temp_dir().join("kubide-ws-agent");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".kubide")).unwrap();
        std::fs::write(
            workspace_config_path(&root),
            "[agent]\npermission_mode = \"bypassPermissions\"\nallowed_tools = [\"Bash(*)\"]\n[font]\nsize = 11.0\n",
        )
        .unwrap();

        let loaded = load_workspace(&root);
        assert_eq!(loaded.config.agent, Agent::default(), "the clone must not choose its own permissions");
        assert_eq!(loaded.config.font.size, 11.0, "the rest of the project file still applies");
        assert!(loaded.problem.as_deref().is_some_and(|p| p.contains("[agent]")), "{:?}", loaded.problem);
    }

    #[test]
    fn seeding_a_workspace_marks_but_never_resets() {
        let root = std::env::temp_dir().join("kubide-ws-seed");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        seed_workspace(&root);
        let p = workspace_config_path(&root);
        assert!(p.exists());
        // The seeded file must itself be a valid (all-comment) config.
        assert!(read_table(&p).unwrap().unwrap().is_empty());

        std::fs::write(&p, "theme = \"nord\"\n").unwrap();
        seed_workspace(&root);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "theme = \"nord\"\n",
            "a mark is not a reset button"
        );
    }

    #[test]
    fn every_builtin_theme_parses_and_changes_something() {
        // A theme that fails to parse is a crash dressed as a colour scheme,
        // and one equal to the default is a name that lies.
        for (name, text) in BUILTIN_THEMES {
            let theme: Theme =
                toml::from_str(text).unwrap_or_else(|e| panic!("theme '{name}': {e}"));
            assert_ne!(theme, Theme::default(), "'{name}' changes nothing");
        }
    }

    #[test]
    fn a_named_theme_resolves_from_the_builtins() {
        // No themes folder at all — the binary must still know its own.
        let empty = std::env::temp_dir().join("kubide-themes-none");
        let _ = std::fs::remove_dir_all(&empty);
        let (theme, from) = resolve_theme_in(&empty, "gruvbox").expect("built-in must resolve");
        assert_eq!(theme.fg, Color::rgb(0xeb, 0xdb, 0xb2));
        assert_eq!(from, None, "nothing on disk was read");
    }

    #[test]
    fn a_file_on_disk_beats_its_builtin_namesake() {
        // Editing a seeded theme has to do something, or the folder is a lie.
        let dir = std::env::temp_dir().join("kubide-themes-override");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gruvbox.toml"), "accent = \"#ff0000\"").unwrap();

        let (theme, from) = resolve_theme_in(&dir, "gruvbox").unwrap();
        assert_eq!(theme.accent, Color::rgb(0xff, 0, 0));
        assert!(from.is_some(), "the file is what gets watched");
        // Partial file: everything unsaid is the DEFAULT, not the built-in —
        // a theme file stands on its own.
        assert_eq!(theme.fg, Theme::default().fg);
    }

    #[test]
    fn an_unknown_theme_name_says_so_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join("kubide-themes-cfg");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(&p, "theme = \"no-such-theme-anywhere\"\n[font]\nsize = 18.0\n").unwrap();

        let loaded = load_from(p);
        assert!(loaded.problem.as_deref().unwrap().contains("no-such-theme-anywhere"));
        assert_eq!(loaded.config.font.size, 18.0, "the rest of the config still lands");
        assert_eq!(loaded.config.theme, Theme::default());
    }

    #[test]
    fn saving_writes_the_theme_name_back_not_the_colours() {
        let dir = std::env::temp_dir().join("kubide-themes-save");
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("config.toml");

        let loaded = load_from_text_for_test("theme = \"gruvbox\"");
        save_named(&loaded, Some("gruvbox"), &p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("theme = \"gruvbox\""), "{text}");
        assert!(!text.contains("[theme"), "the resolved colours must prune away:\n{text}");

        let back = load_from(p);
        assert_eq!(back.config.theme_name.as_deref(), Some("gruvbox"));
    }

    /// A config parsed from text with its theme resolved from the built-ins,
    /// for tests that must not depend on the machine's themes folder.
    fn load_from_text_for_test(text: &str) -> Config {
        let mut table: toml::Table = toml::from_str(text).unwrap();
        table.remove("theme");
        let mut c: Config = toml::Value::Table(table).try_into().unwrap();
        let empty = std::env::temp_dir().join("kubide-themes-none-2");
        let _ = std::fs::remove_dir_all(&empty);
        c.theme = resolve_theme_in(&empty, "gruvbox").unwrap().0;
        c
    }
}
