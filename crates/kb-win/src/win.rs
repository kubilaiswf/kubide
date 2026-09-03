//! Frameless Win32 window, DWM backdrop and non-client area handling.
//!
//! Every oddity here answers a Windows behaviour; none of it is decoration.
//! The details sit above the function they belong to.

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

use crate::{
    Backdrop, CaptionButton, Chrome, CursorShape, Handler, Mods, Placement, Rect, ThemedCursor,
    ThemedKind, WindowConfig,
};

/// The window, as the app names it.
pub type Window = HWND;

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

/// Where the window is now, ready to be written down for the next run.
///
/// `GetWindowPlacement` rather than `GetWindowRect` because it reports the
/// restored rectangle while the window is maximized, which is the whole
/// difficulty of saving a window's place.
pub fn placement(hwnd: HWND) -> Option<Placement> {
    let mut wp = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    unsafe { GetWindowPlacement(hwnd, &mut wp).ok()? };
    let r = wp.rcNormalPosition;
    let (width, height) = (r.right - r.left, r.bottom - r.top);
    // A minimized window reports a placement worth keeping, but a degenerate
    // rectangle is not worth restoring anyone into.
    (width > 0 && height > 0).then_some(Placement {
        x: r.left,
        y: r.top,
        width,
        height,
        maximized: wp.showCmd == SW_SHOWMAXIMIZED.0 as u32,
    })
}

/// Whether a saved rectangle still lands on a monitor that exists.
///
/// Monitors get unplugged. A window restored onto the coordinates of a screen
/// that is no longer there is invisible, and an invisible window reads as a
/// program that failed to start.
fn on_screen(p: Placement) -> bool {
    let rect = RECT { left: p.x, top: p.y, right: p.x + p.width, bottom: p.y + p.height };
    use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONULL};
    !unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// The client area, in pixels — what the renderer has to be sized to.
pub fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rc = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rc) }.is_err() {
        return (1, 1);
    }
    ((rc.right - rc.left).max(1) as u32, (rc.bottom - rc.top).max(1) as u32)
}

/// Ends the message loop, which ends the program.
pub fn quit() {
    unsafe { PostQuitMessage(0) }
}

/// Asks the window to re-evaluate the cursor right now.
///
/// Windows only re-queries WM_SETCURSOR when the mouse moves, so a theme
/// switch would otherwise wear the old colour until the hand twitches.
pub fn refresh_cursor(hwnd: HWND) {
    unsafe {
        let _ = SendMessageW(
            hwnd,
            WM_SETCURSOR,
            Some(WPARAM(hwnd.0 as usize)),
            Some(LPARAM(HTCLIENT as isize)),
        );
    }
}

/// Builds a pointer from its description, hand-plotted into a 32-bit DIB.
///
/// Returns an invalid handle when any GDI call refuses; the caller falls
/// back to the nearest system pointer rather than showing none at all.
fn themed_cursor(t: ThemedCursor) -> HCURSOR {
    // SM_CXCURSOR tracks the accessibility cursor-size setting, so `0`
    // means "as big as the user asked Windows for".
    let s = match t.size {
        0 => unsafe { GetSystemMetrics(SM_CXCURSOR) }.max(16),
        n => (n as i32).clamp(12, 128),
    };
    let px = crate::cursor::themed_pixels(t, s);
    cursor_from_pixels(&px.argb, px.size, px.hot.0, px.hot.1)
}

/// Wraps a premultiplied-BGRA pixel square into a real Windows cursor.
///
/// Returns an invalid handle when any GDI call refuses.
fn cursor_from_pixels(px: &[u32], s: i32, hx: i32, hy: i32) -> HCURSOR {
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS,
    };
    unsafe {
        let w = s as usize;
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: s,
                biHeight: -s, // top-down, so the pixel vec maps straight in
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let Ok(colour_bmp) = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
        else {
            return HCURSOR::default();
        };
        if bits.is_null() {
            let _ = DeleteObject(colour_bmp.into());
            return HCURSOR::default();
        }
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits.cast::<u32>(), w * w);

        // The mask is required by the API and mostly ignored in favour of
        // the alpha channel — but "mostly" is doing work there. Created with
        // no bits its contents are undefined, and an undefined AND mask is a
        // cursor that renders as noise on some drivers. Zeroed explicitly.
        // Scan lines of a CreateBitmap bitmap are word-aligned.
        let mask_row_bytes = (s as usize).div_ceil(16) * 2;
        let mask_bits = vec![0u8; mask_row_bytes * s as usize];
        let mask_bmp = CreateBitmap(s, s, 1, 1, Some(mask_bits.as_ptr().cast()));

        let info = ICONINFO {
            fIcon: FALSE, // a cursor, so the hotspot fields matter
            xHotspot: hx as u32,
            yHotspot: hy as u32,
            hbmMask: mask_bmp,
            hbmColor: colour_bmp,
        };
        let icon = CreateIconIndirect(&info);
        // CreateIconIndirect copies the bitmaps; ours must not outlive this.
        let _ = DeleteObject(colour_bmp.into());
        let _ = DeleteObject(mask_bmp.into());
        match icon {
            Ok(i) => HCURSOR(i.0),
            Err(_) => HCURSOR::default(),
        }
    }
}

/// Loads a `.cur` or `.ani` from disk at the system cursor size.
///
/// `LoadImageW` rather than parsing anything ourselves: Windows already
/// knows its own cursor formats, hotspots and animation included. An
/// unreadable file is an invalid handle, which the caller treats as "use
/// the drawn shape instead".
fn load_cursor_file(path: &std::path::Path) -> HCURSOR {
    unsafe {
        let wide_path = wide(&path.to_string_lossy());
        match LoadImageW(
            None,
            PCWSTR(wide_path.as_ptr()),
            IMAGE_CURSOR,
            0,
            0,
            LR_LOADFROMFILE | LR_DEFAULTSIZE,
        ) {
            Ok(h) => HCURSOR(h.0),
            Err(_) => HCURSOR::default(),
        }
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
    /// Custom pointers already built, by description. WM_SETCURSOR fires on
    /// every mouse move, and building a cursor per move would be a leak with
    /// a framerate.
    themed_cursors: std::collections::HashMap<ThemedCursor, HCURSOR>,
    /// Cursor files already loaded, by path. A failed load is cached too —
    /// an invalid handle — so a missing file is one disk miss, not one per
    /// mouse move.
    file_cursors: std::collections::HashMap<std::path::PathBuf, HCURSOR>,
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

/// The wall clock, hour and minute, in the local zone.
///
/// From the system clock rather than a date library: this needs hours and
/// minutes and nothing else, and Windows hands exactly that over.
pub fn local_clock() -> (u32, u32) {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    (u32::from(t.wHour), u32::from(t.wMinute))
}

/// The wall clock's distance from UTC right now, in minutes.
///
/// Measured by comparing Windows' two clocks rather than by reading time
/// zone rules: the difference of the two answers *is* the offset, rules,
/// DST and all, with nothing to get wrong.
pub fn local_utc_offset_minutes() -> i64 {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    let (l, u) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let minutes = |t: &SYSTEMTIME| {
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
fn tail_of_month(t: &SYSTEMTIME) -> i64 {
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

pub fn run(config: WindowConfig, handler: Box<dyn Handler>) -> crate::Result<()> {
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
            return Err(Error::from_thread().into());
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
            themed_cursors: std::collections::HashMap::new(),
            file_cursors: std::collections::HashMap::new(),
        });
        let state_ptr = Box::into_raw(state);

        // A remembered place, unless the monitor it named has since gone.
        let place = config.place.filter(|p| on_screen(*p));
        let (x, y, w, h) = match place {
            Some(p) => (p.x, p.y, p.width, p.height),
            None => (CW_USEDEFAULT, CW_USEDEFAULT, config.width, config.height),
        };

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
            x,
            y,
            w,
            h,
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
        let _ = ShowWindow(
            hwnd,
            if place.is_some_and(|p| p.maximized) { SW_SHOWMAXIMIZED } else { SW_SHOW },
        );

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
                    let shape = state.handler.cursor();
                    // A file first, its drawn fallback second, the nearest
                    // system pointer last. Each step only on the failure of
                    // the one before, and every failure is cached.
                    let mut themed = |t: ThemedCursor| -> Option<HCURSOR> {
                        let c = *state
                            .themed_cursors
                            .entry(t)
                            .or_insert_with(|| themed_cursor(t));
                        (!c.is_invalid()).then_some(c)
                    };
                    let custom = match &shape {
                        CursorShape::File { path, fallback } => {
                            let c = *state
                                .file_cursors
                                .entry(path.clone())
                                .or_insert_with(|| load_cursor_file(path));
                            (!c.is_invalid()).then_some(c).or_else(|| themed(*fallback))
                        }
                        CursorShape::Themed(t) => themed(*t),
                        _ => None,
                    };
                    if let Some(c) = custom {
                        SetCursor(Some(c));
                        return LRESULT(1);
                    }
                    let of_kind = |k: ThemedKind| match k {
                        ThemedKind::IBeam | ThemedKind::Bar => IDC_IBEAM,
                        ThemedKind::Arrow
                        | ThemedKind::Dart
                        | ThemedKind::Triangle
                        | ThemedKind::Temple => IDC_ARROW,
                        ThemedKind::Hand => IDC_HAND,
                        ThemedKind::SizeWE => IDC_SIZEWE,
                        ThemedKind::SizeNS => IDC_SIZENS,
                    };
                    let id = match &shape {
                        CursorShape::Arrow => IDC_ARROW,
                        CursorShape::SizeWE => IDC_SIZEWE,
                        CursorShape::SizeNS => IDC_SIZENS,
                        CursorShape::Text => IDC_IBEAM,
                        CursorShape::Hand => IDC_HAND,
                        CursorShape::Themed(t) => of_kind(t.kind),
                        CursorShape::File { fallback, .. } => of_kind(fallback.kind),
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
                    let state = Box::from_raw(p);
                    // Cursors we created or loaded are ours to destroy;
                    // system ones from LoadCursorW never enter these maps.
                    for cur in state.themed_cursors.values() {
                        if !cur.is_invalid() {
                            let _ = DestroyIcon(HICON(cur.0));
                        }
                    }
                    for cur in state.file_cursors.values() {
                        if !cur.is_invalid() {
                            let _ = DestroyCursor(*cur);
                        }
                    }
                    drop(state);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_themed_cursor_actually_builds() {
        // The window falls back to a system pointer when this returns an
        // invalid handle, which looks exactly like the feature not existing.
        // So the failure this guards against is silent by design, and only a
        // test makes it loud. Every shape, at the follow-the-system size and
        // at a pinned one.
        for kind in [
            ThemedKind::Arrow,
            ThemedKind::Dart,
            ThemedKind::Triangle,
            ThemedKind::Temple,
            ThemedKind::IBeam,
            ThemedKind::Bar,
            ThemedKind::Hand,
            ThemedKind::SizeWE,
            ThemedKind::SizeNS,
        ] {
            for size in [0u16, 20] {
                let cur = themed_cursor(ThemedCursor { kind, size, rgb: 0x00c4a7e7 });
                assert!(
                    !cur.is_invalid(),
                    "CreateIconIndirect refused {kind:?} at size {size}"
                );
                unsafe {
                    let _ = DestroyIcon(HICON(cur.0));
                }
            }
        }
    }
}
