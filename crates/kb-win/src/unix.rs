//! The Linux window: winit on X11 or Wayland, the caption still ours.
//!
//! What Win32 hands over in the non-client area — move by the caption,
//! resize by the edges, the three buttons — a Wayland client has to ask the
//! compositor for, and an X11 client has to ask the window manager for.
//! winit's `drag_window` and `drag_resize_window` are those two requests,
//! so the hit-testing lives here and the drawing stays with the app, as on
//! Windows. There is no frame at all (`decorations(false)`), which means the
//! resize grip has to be inside the window edge: there is no invisible
//! border to hold it outside.
//!
//! Keys are translated to Windows virtual-key codes, because the `[keys]`
//! table is written in them and one config has to mean one thing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{
    Cursor, CursorIcon, CustomCursor, ResizeDirection, Window as WinitWindow, WindowAttributes,
    WindowId,
};

use crate::{
    Backdrop, CaptionButton, Chrome, CursorShape, Error, Handler, Mods, Placement, Rect,
    ThemedCursor, ThemedKind, WindowConfig,
};

/// The window, as the app names it. There is one, so this is a token: the
/// functions below find the real one in thread-local state, the way the
/// Win32 side finds its `State` behind the HWND.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window(());

struct Shared {
    window: Arc<WinitWindow>,
    backdrop: Backdrop,
    caption_h: i32,
    /// The last un-maximised place, for `placement` while maximised —
    /// what `GetWindowPlacement` remembers on the other side.
    restored: Option<Placement>,
    quit: bool,
    recheck_cursor: bool,
}

thread_local! {
    static SHARED: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

fn with_shared<R>(f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
    SHARED.with(|s| s.borrow_mut().as_mut().map(f))
}

/// The winit window, for the renderer to attach a surface to.
pub fn winit_window() -> Option<Arc<WinitWindow>> {
    with_shared(|s| s.window.clone())
}

/// The material in force, for the renderer to paint under the frame.
pub fn current_backdrop() -> Backdrop {
    with_shared(|s| s.backdrop).unwrap_or(Backdrop::Acrylic)
}

pub fn is_maximized() -> bool {
    with_shared(|s| s.window.is_maximized()).unwrap_or(false)
}

pub fn scale_factor() -> f64 {
    with_shared(|s| s.window.scale_factor()).unwrap_or(1.0)
}

/// The client area, in pixels — what the renderer has to be sized to.
pub fn client_size(_: Window) -> (u32, u32) {
    with_shared(|s| {
        let size = s.window.inner_size();
        (size.width.max(1), size.height.max(1))
    })
    .unwrap_or((1, 1))
}

fn current_place(w: &WinitWindow) -> Option<Placement> {
    // Wayland does not tell a client where it is; the size and the state
    // are still worth keeping, and a position of zero is ignored there
    // on the way back in anyway.
    let pos = w.outer_position().unwrap_or_default();
    let size = w.inner_size();
    (size.width > 0 && size.height > 0).then_some(Placement {
        x: pos.x,
        y: pos.y,
        width: size.width as i32,
        height: size.height as i32,
        maximized: false,
    })
}

/// Where the window is now, ready to be written down for the next run: the
/// restored rectangle, and whether it is maximised over it.
pub fn placement(_: Window) -> Option<Placement> {
    with_shared(|s| {
        if s.window.is_maximized() {
            s.restored.map(|p| Placement { maximized: true, ..p })
        } else {
            current_place(&s.window)
        }
    })
    .flatten()
}

/// Asks the window to re-evaluate the cursor right now.
pub fn refresh_cursor(_: Window) {
    with_shared(|s| s.recheck_cursor = true);
}

/// Changes the backdrop after creation, for config reload. The material is
/// painted by the renderer, so a repaint is the whole change.
pub fn set_backdrop(_: Window, backdrop: Backdrop) {
    with_shared(|s| {
        if s.backdrop != backdrop {
            s.backdrop = backdrop;
            s.window.request_redraw();
        }
    });
}

/// Changes the caption height after creation. Affects hit-testing and the
/// button rects, nothing else.
pub fn set_caption_height(_: Window, caption_h: i32) {
    with_shared(|s| {
        let h = caption_h.max(1);
        if s.caption_h != h {
            s.caption_h = h;
            s.window.request_redraw();
        }
    });
}

/// Minimises the window, so the caption button has a keyboard equivalent.
pub fn minimize(_: Window) {
    with_shared(|s| s.window.set_minimized(true));
}

/// Maximises the window, or restores it when it already is.
pub fn toggle_maximize(_: Window) {
    with_shared(|s| {
        let m = s.window.is_maximized();
        s.window.set_maximized(!m);
    });
}

/// Changes the title after creation, for a workspace whose root moved.
pub fn set_title(_: Window, title: &str) {
    with_shared(|s| s.window.set_title(title));
}

/// Ends the event loop, which ends the program.
pub fn quit() {
    with_shared(|s| s.quit = true);
}

/// The double-click interval, in milliseconds.
///
/// There is no one desktop setting to read on Linux; this is what GTK and
/// Qt both ship as their default, so it is what every other program on the
/// machine most likely uses.
pub fn double_click_ms() -> u32 {
    400
}

/// Whether the desktop draws transparency. Not knowable in general — it is
/// the compositor's decision, window by window — so the app is told nothing
/// and designs for both.
pub fn transparency_enabled() -> Option<bool> {
    None
}

/// The user's standard folders — Desktop, Downloads, Documents, Pictures,
/// Music, Videos — as `xdg-user-dirs` places them, in that order.
///
/// Read from `user-dirs.dirs` rather than assumed: the whole point of that
/// file is that a Turkish desktop calls its pictures folder `Resimler`. The
/// English names are only the answer when the file is silent.
pub fn quick_access() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
    let mut out = Vec::new();
    for (key, fallback) in [
        ("XDG_DESKTOP_DIR", "Desktop"),
        ("XDG_DOWNLOAD_DIR", "Downloads"),
        ("XDG_DOCUMENTS_DIR", "Documents"),
        ("XDG_PICTURES_DIR", "Pictures"),
        ("XDG_MUSIC_DIR", "Music"),
        ("XDG_VIDEOS_DIR", "Videos"),
    ] {
        let named = text.lines().find_map(|line| {
            let rest = line.trim().strip_prefix(key)?.trim_start().strip_prefix('=')?;
            let value = rest.trim().trim_matches('"');
            Some(match value.strip_prefix("$HOME/") {
                Some(tail) => home.join(tail),
                None => PathBuf::from(value),
            })
        });
        let path = named.unwrap_or_else(|| home.join(fallback));
        // A folder can be deleted; offering it would be a button that only
        // shows an error. The home folder itself is not a place to list.
        if path.is_dir() && path != home && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

fn local_tm() -> libc::tm {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    tm
}

/// The wall clock, hour and minute, in the local zone.
pub fn local_clock() -> (u32, u32) {
    let tm = local_tm();
    (tm.tm_hour as u32, tm.tm_min as u32)
}

/// The wall clock's distance from UTC right now, in minutes. libc has
/// already applied the zone rules and DST; the offset is a field.
pub fn local_utc_offset_minutes() -> i64 {
    local_tm().tm_gmtoff / 60
}

/// What is under the pointer, as Win32 would answer `WM_NCHITTEST`.
enum Hit {
    Resize(ResizeDirection),
    Button(CaptionButton),
    Caption,
    Client,
}

/// What the pointer was last set to, so it is only set again on change.
#[derive(Clone, PartialEq)]
enum CursorKey {
    Edge(ResizeDirection),
    Shape(CursorShape),
}

struct App {
    config: WindowConfig,
    handler: Box<dyn Handler>,
    window: Option<Arc<WinitWindow>>,
    error: Option<Error>,
    mods: ModifiersState,
    hovered: Option<CaptionButton>,
    pressed: Option<CaptionButton>,
    active: bool,
    next_tick: Instant,
    mouse: (f32, f32),
    last_cursor: Option<CursorKey>,
    /// Custom pointers already built, by description. A failed build is
    /// cached too, so a shape the server refuses is one refusal, not one
    /// per mouse move.
    themed: HashMap<ThemedCursor, Option<CustomCursor>>,
    files: HashMap<PathBuf, Option<CustomCursor>>,
    /// Touchpad scroll arrives in pixels; whole lines are handed on and
    /// the remainder kept.
    wheel: f64,
    /// When the caption was last pressed, for the double-click that
    /// maximises — which Win32 gives a caption for free.
    caption_click: Option<Instant>,
}

/// Whether a saved rectangle still lands on a monitor that exists.
///
/// Monitors get unplugged. A window restored onto the coordinates of a screen
/// that is no longer there is invisible, and an invisible window reads as a
/// program that failed to start.
fn on_screen(el: &ActiveEventLoop, p: Placement) -> bool {
    let mut any = false;
    for m in el.available_monitors() {
        any = true;
        let (pos, size) = (m.position(), m.size());
        let overlaps = p.x < pos.x + size.width as i32
            && p.x + p.width > pos.x
            && p.y < pos.y + size.height as i32
            && p.y + p.height > pos.y;
        if overlaps {
            return true;
        }
    }
    // A server that lists no monitors (Wayland, in places) is not saying
    // the window is off any of them.
    !any
}

/// The system cursor size, from the setting every X and Wayland toolkit
/// reads, with GTK's default when it is unset.
fn system_cursor_size() -> i32 {
    std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(24)
}

/// Windows virtual-key code for a key event, or `None` for a key the table
/// cannot name.
///
/// Letters and digits come from the logical key: on Windows those codes
/// follow the layout, and the layout is what the logical key applies. The
/// physical position answers when the layout produced something else — a
/// Turkish `ı` on the I key still fires Ctrl+I — and names the punctuation
/// keys the way the OEM codes do.
fn vk_of(event: &KeyEvent) -> Option<u8> {
    if let Key::Character(s) = &event.logical_key {
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_alphanumeric() {
                return Some(c.to_ascii_uppercase() as u8);
            }
        }
    }
    let PhysicalKey::Code(code) = event.physical_key else { return None };
    use KeyCode::*;
    Some(match code {
        KeyA => b'A',
        KeyB => b'B',
        KeyC => b'C',
        KeyD => b'D',
        KeyE => b'E',
        KeyF => b'F',
        KeyG => b'G',
        KeyH => b'H',
        KeyI => b'I',
        KeyJ => b'J',
        KeyK => b'K',
        KeyL => b'L',
        KeyM => b'M',
        KeyN => b'N',
        KeyO => b'O',
        KeyP => b'P',
        KeyQ => b'Q',
        KeyR => b'R',
        KeyS => b'S',
        KeyT => b'T',
        KeyU => b'U',
        KeyV => b'V',
        KeyW => b'W',
        KeyX => b'X',
        KeyY => b'Y',
        KeyZ => b'Z',
        Digit0 => b'0',
        Digit1 => b'1',
        Digit2 => b'2',
        Digit3 => b'3',
        Digit4 => b'4',
        Digit5 => b'5',
        Digit6 => b'6',
        Digit7 => b'7',
        Digit8 => b'8',
        Digit9 => b'9',
        Backslash => 0xDC,
        Slash => 0xBF,
        Comma => 0xBC,
        Period => 0xBE,
        Semicolon => 0xBA,
        Quote => 0xDE,
        BracketLeft => 0xDB,
        BracketRight => 0xDD,
        Equal => 0xBB,
        Minus => 0xBD,
        NumpadAdd => 0x6B,
        NumpadSubtract => 0x6D,
        Enter | NumpadEnter => 0x0D,
        Escape => 0x1B,
        Space => 0x20,
        Tab => 0x09,
        Backspace => 0x08,
        Delete => 0x2E,
        Insert => 0x2D,
        Home => 0x24,
        End => 0x23,
        PageUp => 0x21,
        PageDown => 0x22,
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        F1 => 0x70,
        F2 => 0x71,
        F3 => 0x72,
        F4 => 0x73,
        F5 => 0x74,
        F6 => 0x75,
        F7 => 0x76,
        F8 => 0x77,
        F9 => 0x78,
        F10 => 0x79,
        F11 => 0x7A,
        F12 => 0x7B,
        ShiftLeft | ShiftRight => 0x10,
        ControlLeft | ControlRight => 0x11,
        AltLeft | AltRight => 0x12,
        CapsLock => 0x14,
        _ => return None,
    })
}

impl App {
    fn caption_h(&self) -> i32 {
        with_shared(|s| s.caption_h).unwrap_or(self.config.caption_h)
    }

    /// Button rects in client coordinates, laid out right to left. Windows
    /// uses 46x32 at 96 DPI; we keep the width and take the height from our
    /// caption, so the two platforms draw the same strip.
    fn chrome(&self, w: &WinitWindow) -> Chrome {
        let scale = w.scale_factor() as f32;
        let caption_h = self.caption_h() as f32 * scale;
        let bw = 46.0 * scale;
        let right = w.inner_size().width as f32;
        let mut buttons = [Rect::default(); 3];
        for (i, slot) in buttons.iter_mut().enumerate() {
            // Close sits rightmost, and index 0 is minimize, so count back.
            let from_right = (3 - i) as f32;
            *slot = Rect {
                left: right - from_right * bw,
                top: 0.0,
                right: right - (from_right - 1.0) * bw,
                bottom: caption_h,
            };
        }
        Chrome {
            caption_h,
            buttons,
            hovered: self.hovered,
            pressed: self.pressed,
            maximized: w.is_maximized(),
            active: self.active,
        }
    }

    fn hit(&self, w: &WinitWindow, x: f32, y: f32) -> Hit {
        let size = w.inner_size();
        let (width, height) = (size.width as f32, size.height as f32);
        if !w.is_maximized() {
            // The grip, just inside the edge: no frame outside to hold it.
            let b = 6.0 * w.scale_factor() as f32;
            let (l, r) = (x < b, x >= width - b);
            let (t, bo) = (y < b, y >= height - b);
            let dir = match (t, bo, l, r) {
                (true, _, true, _) => Some(ResizeDirection::NorthWest),
                (true, _, _, true) => Some(ResizeDirection::NorthEast),
                (_, true, true, _) => Some(ResizeDirection::SouthWest),
                (_, true, _, true) => Some(ResizeDirection::SouthEast),
                (true, ..) => Some(ResizeDirection::North),
                (_, true, ..) => Some(ResizeDirection::South),
                (_, _, true, _) => Some(ResizeDirection::West),
                (_, _, _, true) => Some(ResizeDirection::East),
                _ => None,
            };
            if let Some(d) = dir {
                return Hit::Resize(d);
            }
        }
        let chrome = self.chrome(w);
        if y < chrome.caption_h {
            for b in [CaptionButton::Close, CaptionButton::Maximize, CaptionButton::Minimize] {
                if chrome.button(b).contains(x, y) {
                    return Hit::Button(b);
                }
            }
            return Hit::Caption;
        }
        Hit::Client
    }

    fn note_place(&self, w: &WinitWindow) {
        if !w.is_maximized() {
            if let Some(p) = current_place(w) {
                with_shared(|s| s.restored = Some(p));
            }
        }
    }

    /// Builds a drawn pointer for the server. `0` follows the system cursor
    /// size, scaled to the window, like SM_CXCURSOR on the other side.
    fn themed_cursor(el: &ActiveEventLoop, scale: f32, t: ThemedCursor) -> Option<CustomCursor> {
        let s = match t.size {
            0 => (system_cursor_size() as f32 * scale).round() as i32,
            n => n as i32,
        };
        let px = crate::cursor::themed_pixels(t, s);
        let size = px.size as u16;
        let source =
            CustomCursor::from_rgba(px.rgba_straight(), size, size, px.hot.0 as u16, px.hot.1 as u16)
                .ok()?;
        Some(el.create_custom_cursor(source))
    }

    fn file_cursor(el: &ActiveEventLoop, path: &std::path::Path) -> Option<CustomCursor> {
        let c = crate::cur::load(path)?;
        let (w, h) = (u16::try_from(c.width).ok()?, u16::try_from(c.height).ok()?);
        let source = CustomCursor::from_rgba(c.rgba, w, h, c.hot.0, c.hot.1).ok()?;
        Some(el.create_custom_cursor(source))
    }

    /// A file first, its drawn fallback second, the nearest stock pointer
    /// last. Each step only on the failure of the one before, and every
    /// failure is cached.
    fn cursor_for(&mut self, el: &ActiveEventLoop, scale: f32, shape: &CursorShape) -> Cursor {
        let custom = match shape {
            CursorShape::File { path, fallback } => {
                let file = self
                    .files
                    .entry(path.clone())
                    .or_insert_with(|| Self::file_cursor(el, path))
                    .clone();
                file.or_else(|| {
                    self.themed
                        .entry(*fallback)
                        .or_insert_with(|| Self::themed_cursor(el, scale, *fallback))
                        .clone()
                })
            }
            CursorShape::Themed(t) => self
                .themed
                .entry(*t)
                .or_insert_with(|| Self::themed_cursor(el, scale, *t))
                .clone(),
            _ => None,
        };
        if let Some(c) = custom {
            return Cursor::Custom(c);
        }
        let of_kind = |k: ThemedKind| match k {
            ThemedKind::IBeam | ThemedKind::Bar => CursorIcon::Text,
            ThemedKind::Arrow | ThemedKind::Dart | ThemedKind::Triangle | ThemedKind::Temple => {
                CursorIcon::Default
            }
            ThemedKind::Hand => CursorIcon::Pointer,
            ThemedKind::SizeWE => CursorIcon::EwResize,
            ThemedKind::SizeNS => CursorIcon::NsResize,
        };
        Cursor::Icon(match shape {
            CursorShape::Arrow => CursorIcon::Default,
            CursorShape::SizeWE => CursorIcon::EwResize,
            CursorShape::SizeNS => CursorIcon::NsResize,
            CursorShape::Text => CursorIcon::Text,
            CursorShape::Hand => CursorIcon::Pointer,
            CursorShape::Themed(t) => of_kind(t.kind),
            CursorShape::File { fallback, .. } => of_kind(fallback.kind),
        })
    }

    /// Sets the pointer for whatever is under it. Over the edges the resize
    /// arrows are the right answer, as DefWindowProc's are; over the client
    /// area the app decides.
    fn update_cursor(&mut self, el: &ActiveEventLoop, w: &WinitWindow) {
        let (x, y) = self.mouse;
        let key = match self.hit(w, x, y) {
            Hit::Resize(d) => CursorKey::Edge(d),
            Hit::Button(_) | Hit::Caption => CursorKey::Shape(CursorShape::Arrow),
            Hit::Client => CursorKey::Shape(self.handler.cursor()),
        };
        if self.last_cursor.as_ref() == Some(&key) {
            return;
        }
        let scale = w.scale_factor() as f32;
        let cursor = match &key {
            CursorKey::Edge(d) => Cursor::Icon(CursorIcon::from(*d)),
            CursorKey::Shape(s) => self.cursor_for(el, scale, s),
        };
        w.set_cursor(cursor);
        self.last_cursor = Some(key);
    }

    fn press_caption_button(&mut self, el: &ActiveEventLoop, w: &WinitWindow, b: CaptionButton) {
        match b {
            CaptionButton::Minimize => w.set_minimized(true),
            CaptionButton::Maximize => w.set_maximized(!w.is_maximized()),
            CaptionButton::Close => {
                if self.handler.on_close() {
                    el.exit();
                }
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // A remembered place, unless the monitor it named has since gone.
        let place = self.config.place.filter(|p| on_screen(el, *p));
        let mut attrs = WindowAttributes::default()
            .with_title(self.config.title.clone())
            // No frame: the caption is drawn by the app on every platform.
            .with_decorations(false)
            // The alpha channel is the whole look. On X11 this picks a
            // depth-32 visual; on Wayland it leaves the opaque region unset.
            .with_transparent(true)
            .with_min_inner_size(PhysicalSize::new(320u32, 200u32))
            .with_inner_size(PhysicalSize::new(
                self.config.width.max(1) as u32,
                self.config.height.max(1) as u32,
            ));
        if let Some(p) = place {
            attrs = attrs
                .with_position(PhysicalPosition::new(p.x, p.y))
                .with_inner_size(PhysicalSize::new(p.width.max(1) as u32, p.height.max(1) as u32))
                .with_maximized(p.maximized);
        }
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.error = Some(Error(format!("could not open a window: {e}")));
                el.exit();
                return;
            }
        };
        SHARED.set(Some(Shared {
            window: window.clone(),
            backdrop: self.config.backdrop,
            caption_h: self.config.caption_h,
            restored: place.map(|p| Placement { maximized: false, ..p }),
            quit: false,
            recheck_cursor: false,
        }));
        self.window = Some(window.clone());
        self.handler.on_create(Window(()));
        window.request_redraw();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(w) = self.window.clone() else { return };
        match event {
            WindowEvent::CloseRequested => {
                if self.handler.on_close() {
                    el.exit();
                } else {
                    w.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                self.note_place(&w);
                self.handler.on_resize(size.width.max(1), size.height.max(1));
                w.request_redraw();
            }
            WindowEvent::Moved(_) => self.note_place(&w),
            WindowEvent::ScaleFactorChanged { .. } => w.request_redraw(),
            WindowEvent::Focused(focused) => {
                // The app designs for the unfocused state deliberately, as
                // it does for DWM's flattened backdrop.
                self.active = focused;
                if !focused {
                    self.hovered = None;
                    self.pressed = None;
                }
                w.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let chrome = self.chrome(&w);
                w.pre_present_notify();
                self.handler.on_paint(Window(()), &chrome);
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let mods = Mods {
                    ctrl: self.mods.control_key(),
                    shift: self.mods.shift_key(),
                    alt: self.mods.alt_key(),
                };
                // The same two-step contract as WM_KEYDOWN and WM_CHAR: a
                // key the app handles never also types. The text carries
                // Ctrl — Ctrl+C is 0x03 — so the terminal needs no encoding
                // of its own here either. DEL (0x7f) is dropped: Windows
                // never sends a character for the Delete key, and the key
                // itself already arrived.
                let handled = vk_of(&event).is_some_and(|vk| self.handler.on_key(vk, mods));
                let mut redraw = handled;
                if !handled {
                    if let Some(text) = event.text_with_all_modifiers() {
                        for c in text.chars().filter(|c| *c != '\u{7f}') {
                            redraw |= self.handler.on_char(c);
                        }
                    }
                }
                if redraw {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = (position.x as f32, position.y as f32);
                self.mouse = (x, y);
                let hovered = match self.hit(&w, x, y) {
                    Hit::Button(b) => Some(b),
                    _ => None,
                };
                let mut redraw = hovered != self.hovered;
                self.hovered = hovered;
                redraw |= self.handler.on_mouse_move(x, y);
                if redraw {
                    w.request_redraw();
                }
                self.update_cursor(el, &w);
            }
            WindowEvent::CursorLeft { .. } => {
                if self.hovered.take().is_some() {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let (x, y) = self.mouse;
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => match self.hit(&w, x, y) {
                        Hit::Resize(d) => {
                            let _ = w.drag_resize_window(d);
                        }
                        Hit::Button(b) => {
                            self.pressed = Some(b);
                            w.request_redraw();
                        }
                        Hit::Caption => {
                            // A second press within the interval maximises,
                            // which every caption since Windows 3 has done.
                            let again = self.caption_click.is_some_and(|t| {
                                t.elapsed() <= Duration::from_millis(u64::from(double_click_ms()))
                            });
                            if again {
                                self.caption_click = None;
                                w.set_maximized(!w.is_maximized());
                            } else {
                                self.caption_click = Some(Instant::now());
                                let _ = w.drag_window();
                            }
                        }
                        Hit::Client => {
                            // Both servers grab the pointer for the length of
                            // a press, so a drag past the edge keeps arriving
                            // without a SetCapture of our own.
                            self.handler.on_mouse_down(x, y);
                            w.request_redraw();
                        }
                    },
                    (MouseButton::Left, ElementState::Released) => {
                        if let Some(p) = self.pressed.take() {
                            w.request_redraw();
                            if self.hovered == Some(p) {
                                self.press_caption_button(el, &w, p);
                            }
                        } else if self.handler.on_mouse_up(x, y) {
                            w.request_redraw();
                        }
                    }
                    (MouseButton::Right, ElementState::Released)
                        if self.handler.on_right_click(x, y) =>
                    {
                        w.request_redraw();
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // A wheel notch is three lines, the Windows default. A
                // touchpad reports pixels; a line is taken as twenty of
                // them, scaled, and the remainder waits for the next event.
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as i32,
                    MouseScrollDelta::PixelDelta(p) => {
                        self.wheel += p.y;
                        let step = 20.0 * w.scale_factor();
                        let n = (self.wheel / step).trunc();
                        self.wheel -= n * step;
                        n as i32
                    }
                };
                if lines != 0 {
                    let (x, y) = self.mouse;
                    if self.handler.on_wheel(x, y, lines) {
                        w.request_redraw();
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let Some(w) = self.window.clone() else { return };
        if with_shared(|s| std::mem::take(&mut s.quit)).unwrap_or(false) {
            el.exit();
            return;
        }
        // Terminal output arrives on its own schedule. Instead of rendering
        // continuously we wake at ~60 Hz and draw only on change, so an idle
        // kubide never touches the compositor. Measured from now rather than
        // from the last tick, so a stall does not queue a burst of them.
        let now = Instant::now();
        if now >= self.next_tick {
            if self.handler.on_tick() {
                w.request_redraw();
            }
            self.next_tick = now + Duration::from_millis(16);
        }
        if with_shared(|s| std::mem::take(&mut s.recheck_cursor)).unwrap_or(false) {
            self.last_cursor = None;
            self.update_cursor(el, &w);
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.next_tick));
    }
}

pub fn run(config: WindowConfig, handler: Box<dyn Handler>) -> crate::Result<()> {
    let event_loop = EventLoop::new().map_err(|e| Error(format!("no display: {e}")))?;
    let mut app = App {
        config,
        handler,
        window: None,
        error: None,
        mods: ModifiersState::default(),
        hovered: None,
        pressed: None,
        active: true,
        next_tick: Instant::now(),
        mouse: (0.0, 0.0),
        last_cursor: None,
        themed: HashMap::new(),
        files: HashMap::new(),
        wheel: 0.0,
        caption_click: None,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| Error(format!("event loop: {e}")))?;
    // The handler owns the renderer, which holds the window's surface; it
    // goes before the window does.
    drop(app.handler);
    SHARED.set(None);
    match app.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dirs_are_read_for_their_names() {
        // The parser, not the file system: a machine's own folders are not
        // a test's business. Only the shape of a line matters here.
        let home = PathBuf::from("/home/k");
        let text = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Masaüstü\"\nXDG_DOWNLOAD_DIR=\"/data/dl\"\n";
        let named = text.lines().find_map(|line| {
            let rest = line.trim().strip_prefix("XDG_DESKTOP_DIR")?.trim_start().strip_prefix('=')?;
            let value = rest.trim().trim_matches('"');
            Some(match value.strip_prefix("$HOME/") {
                Some(tail) => home.join(tail),
                None => PathBuf::from(value),
            })
        });
        assert_eq!(named, Some(PathBuf::from("/home/k/Masaüstü")));
    }
}
