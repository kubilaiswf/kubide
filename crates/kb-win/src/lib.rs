//! Frameless Win32 window, DWM backdrop and non-client area handling.
//!
//! Every oddity here answers a Windows behaviour; none of it is decoration.
//! The details sit above the function they belong to.

pub mod clipboard;

use std::ffi::c_void;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, ScreenToClient, HBRUSH, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TME_NONCLIENT,
    TRACKMOUSEEVENT, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    None,
    /// Samples the wallpaper once. Cheap, but an opaque material.
    Mica,
    /// Blurs the live content behind the window. kubide's default.
    Acrylic,
    MicaAlt,
}

impl Backdrop {
    fn raw(self) -> DWM_SYSTEMBACKDROP_TYPE {
        match self {
            Backdrop::None => DWMSBT_NONE,
            Backdrop::Mica => DWMSBT_MAINWINDOW,
            Backdrop::Acrylic => DWMSBT_TRANSIENTWINDOW,
            Backdrop::MicaAlt => DWMSBT_TABBEDWINDOW,
        }
    }
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
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "kubide".into(),
            width: 1280,
            height: 800,
            caption_h: 40,
            backdrop: Backdrop::Acrylic,
        }
    }
}

/// Cursor shapes the app can ask for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorShape {
    #[default]
    Arrow,
    /// Vertical divider — horizontal resize.
    SizeWE,
    /// Horizontal divider — vertical resize.
    SizeNS,
    Text,
}

#[derive(Clone, Copy, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

pub trait Handler {
    fn on_create(&mut self, _hwnd: HWND) {}
    fn on_paint(&mut self, hwnd: HWND, chrome: &Chrome);
    fn on_resize(&mut self, _width: u32, _height: u32) {}
    /// Returning `true` triggers a redraw.
    fn on_key(&mut self, _vk: u8, _mods: Mods) -> bool {
        false
    }
    /// Text input — the real input path for the terminal and editor. `on_key`
    /// ignores the keyboard layout; `WM_CHAR` applies it, which is the only
    /// way `ğ`, `ş` and `İ` arrive correctly on a Turkish layout. Ctrl+letter
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

fn mods_now() -> Mods {
    unsafe {
        Mods {
            ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0,
            shift: GetKeyState(VK_SHIFT.0 as i32) < 0,
            alt: GetKeyState(VK_MENU.0 as i32) < 0,
        }
    }
}

struct State {
    handler: Box<dyn Handler>,
    caption_h: i32,
    backdrop: Backdrop,
    hovered: Option<CaptionButton>,
    pressed: Option<CaptionButton>,
    active: bool,
    tracking: bool,
    /// When `on_key` handles a key, the `WM_CHAR` that `TranslateMessage`
    /// generates from it must be swallowed.
    ///
    /// Otherwise shortcuts fire twice: Ctrl+Shift+C copies AND sends 0x03
    /// (SIGINT) to the shell, Ctrl+W leaks 0x17, Ctrl+T leaks 0x14. Consuming
    /// WM_KEYDOWN does not suppress WM_CHAR, so the flag is required.
    swallow_char: bool,
}

/// Button rects in client coordinates, laid out right to left. Windows uses
/// 46x32 at 96 DPI; we keep the width and take the height from our caption.
fn caption_buttons(hwnd: HWND, caption_h: i32) -> [Rect; 3] {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let scale = dpi as f32 / 96.0;
        let bw = 46.0 * scale;
        let bh = caption_h as f32 * scale;

        let mut rc = RECT::default();
        if GetClientRect(hwnd, &mut rc).is_err() {
            return Default::default();
        }
        let right = (rc.right - rc.left) as f32;

        let mut out = [Rect::default(); 3];
        for (i, slot) in out.iter_mut().enumerate() {
            // Close sits rightmost, and index 0 is minimize, so count back.
            let from_right = (3 - i) as f32;
            *slot = Rect {
                left: right - from_right * bw,
                top: 0.0,
                right: right - (from_right - 1.0) * bw,
                bottom: bh,
            };
        }
        out
    }
}

impl State {
    fn chrome(&self, hwnd: HWND) -> Chrome {
        let dpi = unsafe { GetDpiForWindow(hwnd).max(96) };
        Chrome {
            caption_h: self.caption_h as f32 * dpi as f32 / 96.0,
            buttons: caption_buttons(hwnd, self.caption_h),
            hovered: self.hovered,
            pressed: self.pressed,
            maximized: unsafe { IsZoomed(hwnd).as_bool() },
            active: self.active,
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The user's double-click interval, in milliseconds.
///
/// A setting, not a constant. Picking our own number would quietly ignore
/// someone who had deliberately changed theirs.
pub fn double_click_ms() -> u32 {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }
}

/// Windows' "Transparency effects" setting.
///
/// With it off, Mica and Acrylic fall back to a solid color. DWM offers no
/// query API, so the registry is the only way. The same flat state happens on
/// focus loss, in Energy Saver and in High Contrast — a normal state to design
/// for, not a failure.
pub fn transparency_enabled() -> Option<bool> {
    unsafe {
        let mut val: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let ok = RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("EnableTransparency"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut val as *mut u32 as *mut c_void),
            Some(&mut size),
        );
        ok.is_ok().then_some(val != 0)
    }
}

pub fn run(config: WindowConfig, handler: Box<dyn Handler>) -> Result<()> {
    unsafe {
        // Text over a blurred backdrop is already delicate; the wrong DPI
        // awareness smears it entirely at fractional scaling.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(None)?;
        let class_name = w!("kubide_window");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // We own every pixel; a system-painted background hides the
            // backdrop.
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_thread());
        }

        let state = Box::new(State {
            handler,
            caption_h: config.caption_h,
            backdrop: config.backdrop,
            hovered: None,
            pressed: None,
            active: true,
            tracking: false,
            swallow_char: false,
        });
        let state_ptr = Box::into_raw(state);

        let title = wide(&config.title);
        let hwnd = CreateWindowExW(
            // NOREDIRECTIONBITMAP is required, or white pixels left over from
            // the initial redirection bitmap leak through after a resize.
            WS_EX_APPWINDOW | WS_EX_NOREDIRECTIONBITMAP,
            class_name,
            PCWSTR(title.as_ptr()),
            // WS_THICKFRAME is critical: rounded corners, shadow, snap and
            // resize edges all come free from DWM. WS_POPUP kills them.
            WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            config.width,
            config.height,
            None,
            None,
            Some(instance.into()),
            Some(state_ptr as *const c_void),
        )?;

        // A maximized frameless window looks "fullscreen" to Windows and
        // hides the taskbar. (Raymond Chen, oldnewthing 20250522)
        // The value is a sentinel, not a pointer — 1 is enough.
        let sentinel = HANDLE(std::ptr::without_provenance_mut(1));
        let _ = SetPropW(hwnd, w!("NonRudeHWND"), Some(sentinel));

        apply_dwm(hwnd, config.backdrop);

        // Re-run WM_NCCALCSIZE with the new frame, or the old one stays on
        // screen until the first resize.
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);

        // Terminal output arrives on its own schedule. Instead of rendering
        // continuously we poll at ~60 Hz and draw only on change, so an idle
        // kubide never touches the GPU.
        let _ = SetTimer(Some(hwnd), 1, 16, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

/// Applies the DWM attributes. Must be re-applied on
/// `WM_DWMCOMPOSITIONCHANGED` and `WM_SETTINGCHANGE`, which can reset them.
pub fn apply_dwm(hwnd: HWND, backdrop: Backdrop) {
    unsafe {
        let b = backdrop.raw();
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &b as *const _ as *const c_void,
            std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        );
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const c_void,
            std::mem::size_of::<BOOL>() as u32,
        );
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const c_void,
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
        // DWMWA_COLOR_NONE — no border at all. The floating-panel look comes
        // from this line.
        let border: u32 = 0xFFFF_FFFE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border as *const _ as *const c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

/// Changes the backdrop after creation, for config reload. Also updates the
/// stored value, or the next WM_SETTINGCHANGE would revert it.
pub fn set_backdrop(hwnd: HWND, backdrop: Backdrop) {
    unsafe {
        if let Some(s) = state_of(hwnd) {
            if s.backdrop == backdrop {
                return;
            }
            s.backdrop = backdrop;
        }
        apply_dwm(hwnd, backdrop);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Changes the caption height after creation. Affects hit-testing and the
/// button rects, not the frame, so no SWP_FRAMECHANGED is needed.
pub fn set_caption_height(hwnd: HWND, caption_h: i32) {
    unsafe {
        if let Some(s) = state_of(hwnd) {
            let h = caption_h.max(1);
            if s.caption_h != h {
                s.caption_h = h;
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    }
}

/// Minimises the window, so the caption button has a keyboard equivalent.
pub fn minimize(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_MINIMIZE);
    }
}

/// Maximises the window, or restores it when it already is.
///
/// One action rather than two, because the caption button is one button. A key
/// that only maximised would look broken the second time you pressed it.
pub fn toggle_maximize(hwnd: HWND) {
    unsafe {
        let cmd = if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE };
        let _ = ShowWindow(hwnd, cmd);
    }
}

/// Changes the title after creation, for a workspace whose root moved.
///
/// The taskbar and Alt+Tab read this and nothing else, so a title left at the
/// directory the process started in names a project the window is no longer
/// showing.
pub fn set_title(hwnd: HWND, title: &str) {
    let text = wide(title);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(text.as_ptr()));
    }
}

/// The user's standard shell folders — Desktop, Downloads, Documents,
/// Pictures, Music, Videos — wherever they actually live, OneDrive
/// redirection and all.
///
/// This is what Explorer's Quick Access rail holds for almost everyone.
/// The true pin list sits inside Explorer's jump-list database, which has
/// no public reader — the six the shell was born with are the dependable
/// core of it, served by an API that is documented, which the last
/// undocumented shortcut taken here was not.
pub fn quick_access() -> Vec<std::path::PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::*;
    let ids = [
        &FOLDERID_Desktop,
        &FOLDERID_Downloads,
        &FOLDERID_Documents,
        &FOLDERID_Pictures,
        &FOLDERID_Music,
        &FOLDERID_Videos,
    ];
    let mut out = Vec::new();
    for id in ids {
        unsafe {
            let Ok(raw) = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None) else { continue };
            if let Ok(s) = raw.to_string() {
                let path = std::path::PathBuf::from(s);
                // A known folder can be deleted or on an unplugged drive;
                // offering it would be a button that only shows an error.
                if path.is_dir() {
                    out.push(path);
                }
            }
            CoTaskMemFree(Some(raw.as_ptr() as *const c_void));
        }
    }
    out
}

/// Client coordinates from LPARAM. Signed 16-bit: negatives arrive when the
/// mouse is captured and leaves the window, and must survive the cast.
fn mouse_xy(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
}

fn button_of(hit: u32) -> Option<CaptionButton> {
    match hit {
        HTMINBUTTON => Some(CaptionButton::Minimize),
        HTMAXBUTTON => Some(CaptionButton::Maximize),
        HTCLOSE => Some(CaptionButton::Close),
        _ => None,
    }
}

unsafe fn state_of(hwnd: HWND) -> Option<&'static mut State> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
    (!p.is_null()).then(|| &mut *p)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = lparam.0 as *const CREATESTRUCTW;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
            if let Some(s) = state_of(hwnd) {
                s.handler.on_create(hwnd);
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        let Some(state) = state_of(hwnd) else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };

        match msg {
            // The heart of framelessness. NEVER plain `return 0` — it kills
            // the shadow and the resize grips. Take DefWindowProc's result and
            // restore only the top edge: the caption strip becomes client
            // area, the rest of the frame stays with DWM.
            WM_NCCALCSIZE if wparam.0 != 0 => {
                let p = lparam.0 as *mut NCCALCSIZE_PARAMS;
                let top = (*p).rgrc[0].top;
                DefWindowProcW(hwnd, msg, wparam, lparam);
                (*p).rgrc[0].top = top;
                if IsZoomed(hwnd).as_bool() {
                    // SM_CXSIZEFRAME, not CY — Zed's events.rs uses the
                    // x-axis thickness here too. Without it a maximized window
                    // overflows the screen.
                    let dpi = GetDpiForWindow(hwnd);
                    (*p).rgrc[0].top += GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
                        + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                }
                LRESULT(0)
            }

            WM_NCHITTEST => {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let mut rc = RECT::default();
                if GetWindowRect(hwnd, &mut rc).is_err() {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                let dpi = GetDpiForWindow(hwnd);
                let b = GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
                    + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);

                if !IsZoomed(hwnd).as_bool() {
                    let (l, r) = (x < rc.left + b, x >= rc.right - b);
                    let (t, bo) = (y < rc.top + b, y >= rc.bottom - b);
                    let hit = match (t, bo, l, r) {
                        (true, _, true, _) => Some(HTTOPLEFT),
                        (true, _, _, true) => Some(HTTOPRIGHT),
                        (_, true, true, _) => Some(HTBOTTOMLEFT),
                        (_, true, _, true) => Some(HTBOTTOMRIGHT),
                        (true, ..) => Some(HTTOP),
                        (_, true, ..) => Some(HTBOTTOM),
                        (_, _, true, _) => Some(HTLEFT),
                        (_, _, _, true) => Some(HTRIGHT),
                        _ => None,
                    };
                    if let Some(h) = hit {
                        return LRESULT(h as isize);
                    }
                }

                let caption = state.caption_h * dpi as i32 / 96;
                if y - rc.top < caption {
                    // Buttons are in client space; convert from screen.
                    let (cx, cy) = ((x - rc.left) as f32, (y - rc.top) as f32);
                    let b = caption_buttons(hwnd, state.caption_h);
                    if b[2].contains(cx, cy) {
                        return LRESULT(HTCLOSE as isize);
                    }
                    if b[1].contains(cx, cy) {
                        // Returning HTMAXBUTTON is what opens the Windows 11
                        // Snap Layouts flyout. HTCAPTION never will.
                        return LRESULT(HTMAXBUTTON as isize);
                    }
                    if b[0].contains(cx, cy) {
                        return LRESULT(HTMINBUTTON as isize);
                    }
                    return LRESULT(HTCAPTION as isize);
                }
                LRESULT(HTCLIENT as isize)
            }

            WM_CLOSE => {
                if state.handler.on_close() {
                    let _ = DestroyWindow(hwnd);
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            WM_ERASEBKGND => LRESULT(1),

            WM_PAINT => {
                let chrome = state.chrome(hwnd);
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                state.handler.on_paint(hwnd, &chrome);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            // The buttons live in the non-client area, so no ordinary
            // WM_MOUSEMOVE arrives; hover has to come from the NC messages.
            WM_NCMOUSEMOVE => {
                let hovered = button_of(wparam.0 as u32);
                if !state.tracking {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    state.tracking = TrackMouseEvent(&mut t).is_ok();
                }
                if state.hovered != hovered {
                    state.hovered = hovered;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_NCMOUSELEAVE => {
                state.tracking = false;
                if state.hovered.is_some() {
                    state.hovered = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            // DefWindowProc no longer owns these buttons, so we match press
            // and release ourselves.
            WM_NCLBUTTONDOWN => {
                if let Some(b) = button_of(wparam.0 as u32) {
                    state.pressed = Some(b);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_NCLBUTTONUP => {
                let released = button_of(wparam.0 as u32);
                if let (Some(p), Some(r)) = (state.pressed, released) {
                    state.pressed = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    if p == r {
                        match r {
                            CaptionButton::Minimize => {
                                let _ = ShowWindowAsync(hwnd, SW_MINIMIZE);
                            }
                            CaptionButton::Maximize => {
                                let cmd = if IsZoomed(hwnd).as_bool() {
                                    SW_NORMAL
                                } else {
                                    SW_MAXIMIZE
                                };
                                let _ = ShowWindowAsync(hwnd, cmd);
                            }
                            CaptionButton::Close => {
                                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                            }
                        }
                    }
                    return LRESULT(0);
                }
                if state.pressed.take().is_some() {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_ACTIVATE => {
                // DWM flattens the backdrop on focus loss; the app has to
                // handle that state deliberately.
                state.active = (wparam.0 & 0xFFFF) != WA_INACTIVE as usize;
                let _ = InvalidateRect(Some(hwnd), None, false);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_SIZE => {
                let w = (lparam.0 & 0xFFFF) as u32;
                let h = ((lparam.0 >> 16) & 0xFFFF) as u32;
                state.handler.on_resize(w.max(1), h.max(1));
                LRESULT(0)
            }

            // WM_SYSKEYDOWN matters too: Alt combinations such as Alt+arrow
            // never arrive as WM_KEYDOWN. We don't fall through to
            // DefWindowProc on the ones we handle, or the Alt menu logic kicks
            // in and beeps.
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let handled = state.handler.on_key(wparam.0 as u8, mods_now());
                state.swallow_char = handled;
                if handled {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
                if msg == WM_SYSKEYDOWN {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                LRESULT(0)
            }

            WM_CHAR if state.swallow_char => {
                state.swallow_char = false;
                LRESULT(0)
            }

            WM_CHAR => {
                if let Some(c) = char::from_u32(wparam.0 as u32) {
                    if state.handler.on_char(c) {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                LRESULT(0)
            }

            WM_TIMER => {
                if state.handler.on_tick() {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }

            WM_MOUSEMOVE => {
                let (x, y) = mouse_xy(lparam);
                if state.handler.on_mouse_move(x, y) {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }

            WM_RBUTTONUP => {
                let (x, y) = mouse_xy(lparam);
                if state.handler.on_right_click(x, y) {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }

            // Suppress the context menu; we interpret right click ourselves.
            WM_CONTEXTMENU => LRESULT(0),

            WM_MOUSEWHEEL => {
                // WM_MOUSEWHEEL coordinates arrive in SCREEN space, unlike
                // every other mouse message.
                let mut p = POINT {
                    x: (lparam.0 & 0xFFFF) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
                };
                let _ = ScreenToClient(hwnd, &mut p);
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
                let lines = delta as i32 * 3 / WHEEL_DELTA as i32;
                if state.handler.on_wheel(p.x as f32, p.y as f32, lines) {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }

            WM_LBUTTONDOWN => {
                let (x, y) = mouse_xy(lparam);
                // Capture on drag start, or dragging a divider breaks the
                // moment the cursor leaves the window.
                if state.handler.on_mouse_down(x, y) {
                    let _ = SetCapture(hwnd);
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            WM_LBUTTONUP => {
                let (x, y) = mouse_xy(lparam);
                let _ = ReleaseCapture();
                if state.handler.on_mouse_up(x, y) {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }

            WM_SETCURSOR => {
                // We only get a say inside the client area; on the edges
                // DefWindowProc's resize cursors are the correct ones.
                if (lparam.0 & 0xFFFF) as u32 == HTCLIENT {
                    let id = match state.handler.cursor() {
                        CursorShape::Arrow => IDC_ARROW,
                        CursorShape::SizeWE => IDC_SIZEWE,
                        CursorShape::SizeNS => IDC_SIZENS,
                        CursorShape::Text => IDC_IBEAM,
                    };
                    if let Ok(c) = LoadCursorW(None, id) {
                        SetCursor(Some(c));
                    }
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_DWMCOMPOSITIONCHANGED | WM_SETTINGCHANGE => {
                apply_dwm(hwnd, state.backdrop);
                state.handler.on_system_change();
                LRESULT(0)
            }

            WM_DPICHANGED => {
                let rc = *(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rc.left,
                    rc.top,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                LRESULT(0)
            }

            WM_DESTROY => {
                // Drop the handler here; nothing touches it after wndproc.
                let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if !p.is_null() {
                    drop(Box::from_raw(p));
                }
                PostQuitMessage(0);
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
