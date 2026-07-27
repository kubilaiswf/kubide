//! Spike 3 — kubide's real compositing architecture, at minimum size.
//!
//! Spike 1 showed the backdrop but painted with GDI. Spike 2 showed the text
//! but drew onto an opaque surface. The two don't combine: **a swapchain bound
//! directly to an HWND cannot be transparent.** You set Mica/Acrylic, the
//! opaque swapchain paints over it, and you see nothing.
//!
//! The fix — and what Zed, Firefox and Chromium all do:
//!   D3D11 → CreateSwapChainForComposition (PREMULTIPLIED alpha)
//!         → DirectComposition visual tree → bind to the HWND
//!   A D2D device context draws onto that swapchain's surface.
//!
//! The question here is no longer antialiasing: **is code readable over a
//! live, colorful, moving wallpaper?** The left panel is bare Acrylic with no
//! tint, the right one is tinted. Use `[` and `]` to find where the code
//! becomes comfortable to read — that number goes straight into the config.
//!
//! Keys: [ ] tint · 1-4 backdrop · drag the top 40px · Q quit

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::Vector2;

const CAPTION_H: f32 = 40.0;
const FONT_CANDIDATES: &[&str] = &[
    "JetBrainsMono Nerd Font",
    "FiraCode Nerd Font",
    "CaskaydiaCove Nerd Font",
    "Cascadia Code",
    "Consolas",
];

struct Gfx {
    swap: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    _comp_device: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    text: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    tint: ID2D1SolidColorBrush,
}

struct App {
    dw: IDWriteFactory,
    d2d: ID2D1Factory1,
    d3d: ID3D11Device,
    gfx: Option<Gfx>,
    format: IDWriteTextFormat,
    layouts: Vec<IDWriteTextLayout>,
    line_h: f32,
    /// Tint opacity of the right panel. This is the number we're after.
    tint_a: f32,
    backdrop: DWM_SYSTEMBACKDROP_TYPE,
    frame_ms: f64,
}

static mut APP: Option<App> = None;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
fn wide0(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

const SAMPLE: &[&str] = &[
    "impl Renderer for GpuBackend {",
    "    fn draw(&mut self, cmds: &[Command]) -> Result<()> {",
    "        for cmd in cmds.iter().filter(|c| c.visible) {",
    "            let bounds = cmd.rect.intersect(self.clip)?;",
    "            if bounds.is_empty() { continue; }",
    "            self.queue.push(DrawCall {",
    "                atlas: self.atlas_for(cmd.glyph)?,",
    "                uv:    cmd.uv,  // premultiplied!",
    "                color: cmd.color * cmd.alpha,",
    "            });",
    "        }",
    "        self.flush()",
    "    }",
    "}",
    "",
    "// ligature + nerd font + unicode",
    "let a = x -> y => z != w == v <= u >= t;",
    "  main    kubide   3  12   src/main.rs",
    "let s = \"Türkçe ğüşiöç — 日本語 中文 🚀\";",
    "",
    "fn process<T: Iterator<Item = u32>>(it: T) -> Vec<u32> {",
    "    it.filter(|&x| x % 2 == 0)",
    "      .map(|x| x * x)",
    "      .take_while(|&x| x <= 1_000)",
    "      .collect::<Vec<_>>()",
    "}",
];

unsafe fn pick_font(dw: &IDWriteFactory) -> String {
    let mut coll: Option<IDWriteFontCollection> = None;
    if dw.GetSystemFontCollection(&mut coll, false).is_err() {
        return "Consolas".into();
    }
    let Some(coll) = coll else { return "Consolas".into() };
    for c in FONT_CANDIDATES {
        let w = wide0(c);
        let (mut i, mut ex) = (0u32, BOOL(0));
        if coll.FindFamilyName(PCWSTR(w.as_ptr()), &mut i, &mut ex).is_ok() && ex.as_bool() {
            return (*c).into();
        }
    }
    "Consolas".into()
}

/// Kompozisyon zinciri. Bu fonksiyon projenin teknik kalbi.
unsafe fn create_gfx(app: &mut App, hwnd: HWND) -> Result<()> {
    let mut rc = RECT::default();
    GetClientRect(hwnd, &mut rc)?;
    let (w, h) = (
        (rc.right - rc.left).max(1) as u32,
        (rc.bottom - rc.top).max(1) as u32,
    );

    let dxgi_device: IDXGIDevice = app.d3d.cast()?;
    let adapter = dxgi_device.GetAdapter()?;
    let factory: IDXGIFactory2 = adapter.GetParent()?;

    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: w,
        Height: h,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        // Composition swapchain'ler SADECE STRETCH destekliyor.
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        // All of the transparency lives on this line.
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        ..Default::default()
    };
    let swap = factory.CreateSwapChainForComposition(&app.d3d, &desc, None)?;

    // The DirectComposition tree binds the swapchain to the HWND through a
    // visual. Binding it directly would lose the transparency.
    let comp_device: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)?;
    let target = comp_device.CreateTargetForHwnd(hwnd, true)?;
    let visual = comp_device.CreateVisual()?;
    visual.SetContent(&swap)?;
    target.SetRoot(&visual)?;
    comp_device.Commit()?;

    // Put D2D on the same D3D device and target the swapchain surface.
    let d2d_device = app.d2d.CreateDevice(&dxgi_device)?;
    let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
    bind_target(&swap, &dc)?;

    let text = dc.CreateSolidColorBrush(
        &D2D1_COLOR_F { r: 0.93, g: 0.92, b: 0.90, a: 1.0 },
        None,
    )?;
    let dim = dc.CreateSolidColorBrush(
        &D2D1_COLOR_F { r: 0.72, g: 0.71, b: 0.70, a: 1.0 },
        None,
    )?;
    let tint = dc.CreateSolidColorBrush(
        &D2D1_COLOR_F { r: 0.07, g: 0.065, b: 0.06, a: app.tint_a },
        None,
    )?;

    app.gfx = Some(Gfx {
        swap,
        dc,
        _comp_device: comp_device,
        _target: target,
        _visual: visual,
        text,
        dim,
        tint,
    });
    Ok(())
}

unsafe fn bind_target(swap: &IDXGISwapChain1, dc: &ID2D1DeviceContext) -> Result<()> {
    let surface: IDXGISurface = swap.GetBuffer(0)?;
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    let bitmap = dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
    dc.SetTarget(&bitmap);
    Ok(())
}

unsafe fn apply_dwm(hwnd: HWND, backdrop: DWM_SYSTEMBACKDROP_TYPE) {
    let b = backdrop;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_SYSTEMBACKDROP_TYPE,
        &b as *const _ as _,
        std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
    );
    let dark = BOOL(1);
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
    let corner = DWMWCP_ROUND;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        &corner as *const _ as _,
        std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
    );
    let border: u32 = 0xFFFF_FFFE;
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
}

unsafe fn rebuild_layouts(app: &mut App) {
    app.layouts.clear();
    for _ in 0..3 {
        for line in SAMPLE {
            let w = wide(line);
            if let Ok(l) = app.dw.CreateTextLayout(&w, &app.format, 4000.0, 100.0) {
                app.layouts.push(l);
            }
        }
    }
    let mut m = DWRITE_TEXT_METRICS::default();
    if let Some(l) = app.layouts.first() {
        let _ = l.GetMetrics(&mut m);
        app.line_h = if m.height > 0.0 { m.height } else { 20.0 };
    }
}

unsafe fn draw(app: &mut App, hwnd: HWND) {
    if app.gfx.is_none() {
        let _ = create_gfx(app, hwnd);
    }
    let Some(gfx) = &app.gfx else { return };
    let t0 = std::time::Instant::now();

    let size = gfx.dc.GetSize();
    let mid = size.width * 0.5;

    gfx.dc.BeginDraw();
    // Clear to fully transparent — DWM's Acrylic shows through here.
    gfx.dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));

    // Tint only the right panel; the left stays bare. That's the comparison.
    gfx.tint.SetColor(&D2D1_COLOR_F {
        r: 0.07,
        g: 0.065,
        b: 0.06,
        a: app.tint_a,
    });
    gfx.dc.FillRectangle(
        &D2D_RECT_F { left: mid, top: 0.0, right: size.width, bottom: size.height },
        &gfx.tint,
    );

    // Translucent surface, so grayscale AA.
    gfx.dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

    for (a, b, label) in [
        (0.0f32, mid, "NO TINT — bare Acrylic"),
        (mid, size.width, "TINTED"),
    ] {
        gfx.dc.PushAxisAlignedClip(
            &D2D_RECT_F { left: a, top: 0.0, right: b, bottom: size.height },
            D2D1_ANTIALIAS_MODE_ALIASED,
        );
        let lw = wide(label);
        if let Ok(l) = app.dw.CreateTextLayout(&lw, &app.format, b - a - 24.0, 30.0) {
            gfx.dc.DrawTextLayout(
                Vector2 { X: a + 16.0, Y: 12.0 },
                &l,
                &gfx.dim,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        for (i, layout) in app.layouts.iter().enumerate() {
            let y = CAPTION_H + 8.0 + i as f32 * app.line_h;
            if y > size.height {
                break;
            }
            gfx.dc.DrawTextLayout(
                Vector2 { X: a + 16.0, Y: y },
                layout,
                &gfx.text,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        gfx.dc.PopAxisAlignedClip();
    }

    let _ = gfx.dc.EndDraw(None, None);
    let _ = gfx.swap.Present(1, DXGI_PRESENT(0));
    app.frame_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let name = match app.backdrop {
        DWMSBT_MAINWINDOW => "Mica",
        DWMSBT_TRANSIENTWINDOW => "Acrylic",
        DWMSBT_TABBEDWINDOW => "MicaAlt",
        _ => "None",
    };
    let t = wide0(&format!(
        "kubide spike 3 — {name} · tint {:.2} · frame {:.2} ms   ([ ] tint · 1-4 backdrop)",
        app.tint_a, app.frame_ms
    ));
    let _ = SetWindowTextW(hwnd, PCWSTR(t.as_ptr()));
}

fn main() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;

        // Without BGRA_SUPPORT, D2D can't bind to this device.
        let mut d3d: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d),
            None,
            None,
        )?;
        let d3d = d3d.expect("no D3D11 device");

        let font = pick_font(&dw);
        println!("kubide spike 3 — text over Acrylic");
        println!("  font: {font}");
        println!("  [ ] tint · 1 None · 2 Mica · 3 Acrylic · 4 MicaAlt · Q quit");

        let fam = wide0(&font);
        let loc = wide0("en-us");
        let format = dw.CreateTextFormat(
            PCWSTR(fam.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            14.0,
            PCWSTR(loc.as_ptr()),
        )?;

        let mut app = App {
            dw,
            d2d,
            d3d,
            gfx: None,
            format,
            layouts: Vec::new(),
            line_h: 20.0,
            tint_a: 0.55,
            backdrop: DWMSBT_TRANSIENTWINDOW,
            frame_ms: 0.0,
        };
        rebuild_layouts(&mut app);
        APP = Some(app);

        let instance = GetModuleHandleW(None)?;
        let class_name = w!("kubide_spike3");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_thread());
        }

        let hwnd = CreateWindowExW(
            // NOREDIRECTIONBITMAP is required, or white pixels left over from
            // the initial redirection bitmap leak through after a resize.
            WS_EX_APPWINDOW | WS_EX_NOREDIRECTIONBITMAP,
            class_name,
            w!("kubide spike 3"),
            WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1280,
            800,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let _ = SetPropW(hwnd, w!("NonRudeHWND"), Some(HANDLE(1isize as *mut _)));
        apply_dwm(hwnd, DWMSBT_TRANSIENTWINDOW);
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

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let app = &raw mut APP;
        let Some(app) = (*app).as_mut() else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };

        match msg {
            WM_NCCALCSIZE if wparam.0 != 0 => {
                let p = lparam.0 as *mut NCCALCSIZE_PARAMS;
                let top = (*p).rgrc[0].top;
                DefWindowProcW(hwnd, msg, wparam, lparam);
                (*p).rgrc[0].top = top;
                if IsZoomed(hwnd).as_bool() {
                    let dpi = GetDpiForWindow(hwnd);
                    (*p).rgrc[0].top += GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
                        + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                }
                LRESULT(0)
            }

            WM_NCHITTEST => {
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
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
                if (y - rc.top) < (CAPTION_H * GetDpiForWindow(hwnd) as f32 / 96.0) as i32 {
                    return LRESULT(HTCAPTION as isize);
                }
                LRESULT(HTCLIENT as isize)
            }

            WM_ERASEBKGND => LRESULT(1),

            WM_PAINT => {
                let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
                let _ = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
                draw(app, hwnd);
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            WM_SIZE => {
                if let Some(gfx) = &app.gfx {
                    let w = ((lparam.0 & 0xFFFF) as u32).max(1);
                    let h = (((lparam.0 >> 16) & 0xFFFF) as u32).max(1);
                    gfx.dc.SetTarget(None);
                    if gfx
                        .swap
                        .ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))
                        .is_ok()
                    {
                        let _ = bind_target(&gfx.swap, &gfx.dc);
                    }
                }
                LRESULT(0)
            }

            WM_KEYDOWN => {
                let mut redraw = true;
                match wparam.0 as u8 {
                    0xDB => app.tint_a = (app.tint_a - 0.05).max(0.0), // [
                    0xDD => app.tint_a = (app.tint_a + 0.05).min(1.0), // ]
                    b'1' => {
                        app.backdrop = DWMSBT_NONE;
                        apply_dwm(hwnd, app.backdrop);
                    }
                    b'2' => {
                        app.backdrop = DWMSBT_MAINWINDOW;
                        apply_dwm(hwnd, app.backdrop);
                    }
                    b'3' => {
                        app.backdrop = DWMSBT_TRANSIENTWINDOW;
                        apply_dwm(hwnd, app.backdrop);
                    }
                    b'4' => {
                        app.backdrop = DWMSBT_TABBEDWINDOW;
                        apply_dwm(hwnd, app.backdrop);
                    }
                    b'Q' => {
                        PostQuitMessage(0);
                        redraw = false;
                    }
                    _ => redraw = false,
                }
                if redraw {
                    draw(app, hwnd);
                }
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
