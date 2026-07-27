//! Spike 1 — the OS layer of the look, without a GPU.
//!
//! One question: does the wallpaper show through the window? If it doesn't,
//! nothing further up the stack (GPUI, wgpu, DirectComposition) will fix it.
//!
//! It also draws a fake layout so "opaque editor pane + translucent sidebar"
//! can be compared by eye — the design the research argued for, on the theory
//! that ClearType cannot work over translucency.
//!
//! Keys: 1 None · 2 Mica · 3 Acrylic · 4 Mica Alt · D dark/light · Q quit
//! Drag: top 40 px · resize from the edges · Snap Layouts when maximized

use std::ffi::c_void;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Height of the draggable title strip, at 96 DPI.
const CAPTION_H: i32 = 40;
/// Width of the translucent left sidebar.
const SIDEBAR_W: i32 = 260;
/// Gap around the opaque content pane, where the backdrop shows through.
const PANE_INSET: i32 = 12;

// Acrylic: a live blur. Mica is opaque and samples the wallpaper once, which
// is not the feel we want.
static mut BACKDROP: DWM_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW;
static mut DARK: bool = true;

fn main() -> Result<()> {
    unsafe {
        // Text over a blurred backdrop is already delicate; the wrong DPI
        // awareness smears it entirely at fractional scaling.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(None)?;
        let class_name = w!("kubide_spike_mica");

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // NULL brush: her pikseli biz sahipleniyoruz. Sistem boyarsa
            // would paint over the backdrop.
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_thread());
        }

        let hwnd = CreateWindowExW(
            WS_EX_APPWINDOW,
            class_name,
            w!("kubide — spike 1: backdrop"),
            // WS_THICKFRAME is critical: rounded corners, shadow, snap and
            // resize all come free from DWM. WS_POPUP kills every one.
            WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1100,
            720,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        // A maximized frameless window looks "fullscreen" to Windows and hides
        // the taskbar. Raymond Chen's NonRudeHWND prop prevents that.
        let _ = SetPropW(hwnd, w!("NonRudeHWND"), Some(HANDLE(1isize as *mut c_void)));

        apply_dwm(hwnd);

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

        println!("kubide spike 1 — backdrop testi");
        println!("1 None · 2 Mica · 3 Acrylic · 4 Mica Alt · D dark/light · Q quit");
        warn_if_transparency_off();
        print_state();
        println!("  tip: click away from the window and watch it flatten.");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

/// Windows' "Transparency effects" setting.
///
/// With it off, Mica and Acrylic fall back to a solid color — so this spike
/// would report a FALSE NEGATIVE and look like the backdrop is broken. DWM has
/// no query API, so the registry is the only way. The same flat state happens
/// on every alt-tab, in Energy Saver and in High Contrast: a normal state to
/// design for, not a failure.
unsafe fn transparency_enabled() -> Option<bool> {
    let mut val: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let r = RegGetValueW(
        HKEY_CURRENT_USER,
        w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
        w!("EnableTransparency"),
        RRF_RT_REG_DWORD,
        None,
        Some(&mut val as *mut u32 as *mut c_void),
        Some(&mut size),
    );
    if r.is_ok() {
        Some(val != 0)
    } else {
        None
    }
}

unsafe fn warn_if_transparency_off() {
    match transparency_enabled() {
        Some(false) => {
            println!();
            println!("  !!  'Transparency effects' is OFF in Windows.");
            println!("      Settings > Personalization > Colors > Transparency effects");
            println!("      Mica/Acrylic render as a flat color in this state.");
            println!("      Turn it on before concluding the backdrop is broken.");
            println!();
        }
        Some(true) => println!("  transparency effects: on"),
        None => println!("  transparency effects: unreadable (registry)"),
    }
}

/// Applies every DWM attribute. Must be re-applied on
/// WM_DWMCOMPOSITIONCHANGED and WM_SETTINGCHANGE, which can reset them.
unsafe fn apply_dwm(hwnd: HWND) {
    // Extend the frame inward so the backdrop reaches the client area. On the
    // GPU-less (GDI) path this is what makes it visible at all: pixels painted
    // black count as transparent.
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

    let backdrop = BACKDROP;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_SYSTEMBACKDROP_TYPE,
        &backdrop as *const _ as *const c_void,
        std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
    );

    let dark = BOOL::from(DARK);
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

    // DWMWA_COLOR_NONE — no border at all. The floating-panel look comes from
    // this line.
    let border: u32 = 0xFFFF_FFFE;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_BORDER_COLOR,
        &border as *const _ as *const c_void,
        std::mem::size_of::<u32>() as u32,
    );
}

unsafe fn print_state() {
    let name = match BACKDROP {
        DWMSBT_NONE => "None (backdrop off)",
        DWMSBT_MAINWINDOW => "Mica — samples the wallpaper, cheap",
        DWMSBT_TRANSIENTWINDOW => "Acrylic — blurs live content behind, costly",
        DWMSBT_TABBEDWINDOW => "Mica Alt",
        _ => "Auto",
    };
    let dark = DARK;
    println!("  backdrop = {name}   dark = {dark}");
}

unsafe fn scaled(hwnd: HWND, v: i32) -> i32 {
    let dpi = GetDpiForWindow(hwnd) as i32;
    if dpi <= 0 {
        v
    } else {
        v * dpi / 96
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // The heart of framelessness. NEVER plain `return 0` — it kills
            // the shadow and the resize grips. Take DefWindowProc's result and
            // restore only the top edge: the caption strip becomes client
            // area, the rest of the frame stays with DWM.
            WM_NCCALCSIZE if wparam.0 != 0 => {
                let params = lparam.0 as *mut NCCALCSIZE_PARAMS;
                let original_top = (*params).rgrc[0].top;
                DefWindowProcW(hwnd, msg, wparam, lparam);
                (*params).rgrc[0].top = original_top;

                // Without adding this back, a maximized window overflows.
                if IsZoomed(hwnd).as_bool() {
                    let dpi = GetDpiForWindow(hwnd);
                    // SM_CXSIZEFRAME, not CY. Zed's events.rs uses
                    // get_frame_thicknessx here; a ...y variant exists but is
                    // not what this fixup uses.
                    let frame = GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
                        + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                    (*params).rgrc[0].top += frame;
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
                let border = GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
                    + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                let maximized = IsZoomed(hwnd).as_bool();

                if !maximized {
                    let left = x < rc.left + border;
                    let right = x >= rc.right - border;
                    let top = y < rc.top + border;
                    let bottom = y >= rc.bottom - border;
                    let hit = match (top, bottom, left, right) {
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

                // Top strip: the drag region.
                if y < rc.top + scaled(hwnd, CAPTION_H) {
                    return LRESULT(HTCAPTION as isize);
                }
                LRESULT(HTCLIENT as isize)
            }

            WM_ERASEBKGND => LRESULT(1), // we paint the background ourselves

            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);

                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);

                // 1) Paint everything black. With the extended frame, black
                //    means transparent, so DWM's material shows through.
                let black = GetStockObject(BLACK_BRUSH);
                FillRect(hdc, &rc, HBRUSH(black.0));

                // 2) The left sidebar stays translucent; only a hairline
                //    marks where it ends.
                let sidebar_w = scaled(hwnd, SIDEBAR_W);
                let sep = CreateSolidBrush(COLORREF(0x00_3A_32_2E)); // BGR
                let sep_rc = RECT {
                    left: sidebar_w,
                    top: 0,
                    right: sidebar_w + 1,
                    bottom: rc.bottom,
                };
                FillRect(hdc, &sep_rc, sep);
                let _ = DeleteObject(sep.into());

                // 3) OPAQUE content pane on the right. This is the actual
                //    burada metin net olur, sidebar'da olmaz.
                let inset = scaled(hwnd, PANE_INSET);
                let caption = scaled(hwnd, CAPTION_H);
                let pane = CreateSolidBrush(COLORREF(0x00_24_1E_1C)); // dark, slightly warm
                let pane_rc = RECT {
                    left: sidebar_w + inset,
                    top: caption,
                    right: rc.right - inset,
                    bottom: rc.bottom - inset,
                };
                let old = SelectObject(hdc, pane.into());
                let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
                let _ = RoundRect(
                    hdc,
                    pane_rc.left,
                    pane_rc.top,
                    pane_rc.right,
                    pane_rc.bottom,
                    scaled(hwnd, 20),
                    scaled(hwnd, 20),
                );
                SelectObject(hdc, old_pen);
                SelectObject(hdc, old);
                let _ = DeleteObject(pane.into());

                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            WM_KEYDOWN => {
                let changed = match wparam.0 as u8 {
                    b'1' => {
                        BACKDROP = DWMSBT_NONE;
                        true
                    }
                    b'2' => {
                        BACKDROP = DWMSBT_MAINWINDOW;
                        true
                    }
                    b'3' => {
                        BACKDROP = DWMSBT_TRANSIENTWINDOW;
                        true
                    }
                    b'4' => {
                        BACKDROP = DWMSBT_TABBEDWINDOW;
                        true
                    }
                    b'D' => {
                        DARK = !DARK;
                        true
                    }
                    b'Q' => {
                        PostQuitMessage(0);
                        false
                    }
                    _ => false,
                };
                if changed {
                    apply_dwm(hwnd);
                    let _ = InvalidateRect(Some(hwnd), None, true);
                    print_state();
                }
                LRESULT(0)
            }

            // DWM can reset the attributes on either of these.
            WM_DWMCOMPOSITIONCHANGED | WM_SETTINGCHANGE => {
                apply_dwm(hwnd);
                // The user can turn transparency off while we run.
                warn_if_transparency_off();
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
                PostQuitMessage(0);
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
