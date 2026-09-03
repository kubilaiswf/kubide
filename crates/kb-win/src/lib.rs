//! The window: frameless, translucent, with the caption drawn by the app.
//!
//! One contract, two platforms. On Windows it is a raw Win32 window with a
//! DWM backdrop and hand-rolled non-client handling (`win.rs`); on Linux it
//! is a winit window on X11 or Wayland with the same drag-to-move and
//! drag-to-resize requests a frameless window needs there (`unix.rs`). The
//! app implements [`Handler`] once and never learns which it got.
//!
//! Every oddity in the platform files answers a platform behaviour; none of
//! it is decoration. The details sit above the function they belong to.

pub mod clipboard;
mod cursor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    None,
    /// Samples the wallpaper once. Cheap, but an opaque material.
    Mica,
    /// Blurs the live content behind the window. kubide's default.
    Acrylic,
    MicaAlt,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptionButton {
    Minimize,
    Maximize,
    Close,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// Window chrome state — the app draws it, kb-win owns hit-testing.
#[derive(Clone, Copy, Default)]
pub struct Chrome {
    pub caption_h: f32,
    /// Minimize, Maximize, Close in that order, in client coordinates.
    pub buttons: [Rect; 3],
    pub hovered: Option<CaptionButton>,
    pub pressed: Option<CaptionButton>,
    pub maximized: bool,
    /// Whether the window is focused. DWM flattens the backdrop to a solid
    /// color when it isn't, so that state has to be designed for rather than
    /// left looking like a glitch.
    pub active: bool,
}

impl Chrome {
    pub fn button(&self, b: CaptionButton) -> Rect {
        self.buttons[match b {
            CaptionButton::Minimize => 0,
            CaptionButton::Maximize => 1,
            CaptionButton::Close => 2,
        }]
    }

    /// Right edge of the draggable strip; tabs may be drawn up to here.
    pub fn drag_limit(&self) -> f32 {
        self.buttons[0].left
    }
}

pub struct WindowConfig {
    pub title: String,
    pub width: i32,
    pub height: i32,
    /// Height of the draggable strip, at 96 DPI.
    pub caption_h: i32,
    pub backdrop: Backdrop,
    /// Where a previous run left the window. `None` lets the system choose,
    /// which is what a first run wants.
    pub place: Option<Placement>,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "kubide".into(),
            width: 1280,
            height: 800,
            caption_h: 40,
            backdrop: Backdrop::Acrylic,
            place: None,
        }
    }
}

/// A window's size and position, in physical screen pixels, plus whether it
/// was maximized.
///
/// The rectangle is always the *restored* one even when `maximized` is set:
/// that is what un-maximizing has to go back to, and remembering a maximized
/// window as a screen-sized normal one loses the state and the size at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

/// Cursor shapes the app can ask for. `Clone` but not `Copy`: the file
/// variant carries a path.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum CursorShape {
    #[default]
    Arrow,
    /// The system I-beam, drawn over editable text.
    Text,
    /// The system hand, over things that are clicked rather than typed in.
    Hand,
    /// A pointer drawn by us — shape, size and colour all the app's choice —
    /// so the cursor wears the theme like everything else in the window.
    ///
    /// Built once per description and cached; a theme or setting change just
    /// asks for a new one. If the system refuses to make it, the nearest
    /// stock pointer stands in — a cursor must never simply vanish.
    Themed(ThemedCursor),
    /// A `.cur` or `.ani` file from disk — any cursor pack the user likes
    /// better than our drawings. `fallback` is the drawn shape that stands
    /// in when the file refuses to load.
    File {
        path: std::path::PathBuf,
        fallback: ThemedCursor,
    },
    /// Vertical divider — horizontal resize.
    SizeWE,
    /// Horizontal divider — vertical resize.
    SizeNS,
}

/// A custom pointer, fully described. `Hash + Eq` because the description is
/// also the cache key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ThemedCursor {
    pub kind: ThemedKind,
    /// Canvas edge in pixels, clamped to 12..=128. `0` follows the system
    /// cursor size, which is where the accessibility setting lives.
    pub size: u16,
    /// `0xRRGGBB`.
    pub rgb: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ThemedKind {
    /// The classic pointer, redrawn slimmer and softer than the system's.
    Arrow,
    /// A four-point dart — nothing like the stock pointer, on purpose.
    Dart,
    /// Just the tip — a minimal sliver of a pointer.
    Triangle,
    /// The TempleOS pointer, hard pixels and all. RIP Terry.
    Temple,
    /// Stem and serifs, for text.
    IBeam,
    /// The stem alone.
    Bar,
    /// A pointing hand, for things that are clicked rather than typed in.
    Hand,
    /// Double-headed ↔, for dragging a vertical divider.
    SizeWE,
    /// Double-headed ↕, for dragging a horizontal divider.
    SizeNS,
}

#[derive(Clone, Copy, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

pub trait Handler {
    fn on_create(&mut self, _window: Window) {}
    fn on_paint(&mut self, window: Window, chrome: &Chrome);
    fn on_resize(&mut self, _width: u32, _height: u32) {}
    /// Keys arrive as Windows virtual-key codes on every platform: the
    /// `[keys]` table is written in them, and Linux translates its own
    /// codes to the same numbers so one config means one thing.
    ///
    /// Returning `true` triggers a redraw.
    fn on_key(&mut self, _vk: u8, _mods: Mods) -> bool {
        false
    }
    /// Text input — the real input path for the terminal and editor. `on_key`
    /// ignores the keyboard layout; this applies it, which is the only way
    /// `ğ`, `ş` and `İ` arrive correctly on a Turkish layout. Ctrl+letter
    /// also arrives here as a control character (Ctrl+C = 0x03), so the
    /// terminal needs no encoding of its own.
    fn on_char(&mut self, _c: char) -> bool {
        false
    }
    /// Periodic wake-up for content that changes on its own, like terminal
    /// output. Returning `true` triggers a redraw.
    fn on_tick(&mut self) -> bool {
        false
    }
    fn on_mouse_move(&mut self, _x: f32, _y: f32) -> bool {
        false
    }
    /// Mouse wheel. Positive `lines` is up.
    fn on_wheel(&mut self, _x: f32, _y: f32, _lines: i32) -> bool {
        false
    }
    /// Right click. The Windows console behaviour: copy if there's a
    /// selection, otherwise paste.
    fn on_right_click(&mut self, _x: f32, _y: f32) -> bool {
        false
    }
    /// Returning `true` captures the mouse, meaning a drag started.
    fn on_mouse_down(&mut self, _x: f32, _y: f32) -> bool {
        false
    }
    fn on_mouse_up(&mut self, _x: f32, _y: f32) -> bool {
        false
    }
    fn cursor(&self) -> CursorShape {
        CursorShape::Arrow
    }
    /// The system theme or transparency setting changed.
    fn on_system_change(&mut self) {}
    /// The window is about to close. Return `false` to keep it open.
    ///
    /// Protecting only the quit shortcut would be a false comfort: the title
    /// bar's close button, Alt+F4 and the taskbar all arrive here instead.
    fn on_close(&mut self) -> bool {
        true
    }
}

/// Why the window layer refused. A sentence: it ends up in a log line or on
/// the status bar and nowhere else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

#[cfg(windows)]
impl From<windows::core::Error> for Error {
    fn from(e: windows::core::Error) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod cur;
#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::*;
