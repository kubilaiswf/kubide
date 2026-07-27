//! Spike 2 — DirectWrite metin kalitesi.
//!
//! The question: on your monitor, at your DPI, is DirectWrite's code text
//! good enough? Do ligatures join correctly? Do Nerd Font glyphs sit on the
//! cell grid?
//!
//! The screen is split: LEFT is ClearType (subpixel), RIGHT is grayscale.
//! The split isn't arbitrary — the claim was that ClearType cannot physically
//! work over a translucent surface, so an opaque editor pane would get the
//! left-hand quality and a translucent sidebar the right-hand one. If the
//! right side looks unacceptable, the translucency plan has to shrink. Better
//! to know now than in six months.
//!
//! Keys: L ligatures · +/- size · S one/two panels · wheel scrolls · Q quit

use std::mem::size_of;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
// D2D's point type. The windows crate doesn't re-export it; separate crate.
use windows_numerics::Vector2;

/// Fonts to try, in order. The first one found wins.
const FONT_CANDIDATES: &[&str] = &[
    "JetBrainsMono Nerd Font",
    "JetBrainsMono NF",
    "FiraCode Nerd Font",
    "FiraCode NF",
    "CaskaydiaCove Nerd Font",
    "Iosevka Nerd Font",
    "Cascadia Code",
    "Consolas",
];

struct App {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    rt: Option<ID2D1HwndRenderTarget>,
    format: Option<IDWriteTextFormat>,
    brush: Option<ID2D1SolidColorBrush>,
    dim: Option<ID2D1SolidColorBrush>,
    layouts: Vec<IDWriteTextLayout>,
    lines: Vec<Vec<u16>>,
    font: String,
    size: f32,
    line_h: f32,
    top: usize,
    ligatures: bool,
    split: bool,
    /// Light background. The difference between ClearType and grayscale is
    /// nearly invisible on dark; it shows up with dark text on light. A "no
    /// difference" verdict is only trustworthy if it holds here too.
    light: bool,
    frame_ms: f64,
}

static mut APP: Option<App> = None;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Sample code chosen to stress ligatures and Nerd Font glyphs. The point is
/// to expose breakage, not to look good.
fn build_lines() -> Vec<Vec<u16>> {
    let template: &[&str] = &[
        "// ligature test: these should join into single glyphs",
        "let a = x -> y => z !== w === v <= u >= t;",
        "let b = p |> q <| r <-> s <- m -> n;",
        "if (a != b && c || d) { /* yorum */ } // son",
        "let path = a::b::c::<T>::new(...);",
        "let opt = maybe?.value ?? fallback;",
        "let range = 0..=10; let spread = [...arr];",
        "let arrow = |x| x * 2; let fat = () => {};",
        "",
        "// Nerd Font / powerline — on the cell grid, or overflowing?",
        "  main       kubide     3   12",
        "        󰊢  󰅩  󰆍  ",
        "  src/main.rs    Cargo.toml    README.md",
        "",
        "// CJK + emoji + combining marks (font fallback test)",
        "let s = \"Türkçe ğüşiöç ĞÜŞİÖÇ — apostrophe' test\";",
        "let cjk = \"日本語 中文 한국어\"; let emo = \"🚀 ✅ 🔥\";",
        "let combining = \"e\u{0301} a\u{0308} o\u{0303}\";",
        "",
        "fn process<T: Iterator<Item = u32>>(iter: T) -> Vec<u32> {",
        "    iter.filter(|&x| x % 2 == 0)",
        "        .map(|x| x * x)",
        "        .take_while(|&x| x <= 1_000)",
        "        .collect::<Vec<_>>()",
        "}",
        "",
        "impl Renderer for GpuBackend {",
        "    fn draw(&mut self, cmds: &[Command]) -> Result<(), Error> {",
        "        for cmd in cmds.iter().filter(|c| c.visible) {",
        "            self.queue.push(cmd.clone());",
        "        }",
        "        Ok(())",
        "    }",
        "}",
        "",
    ];

    let mut out = Vec::with_capacity(5000);
    let mut n = 0usize;
    while out.len() < 5000 {
        for t in template {
            if out.len() >= 5000 {
                break;
            }
            n += 1;
            // Numbering makes every line unique, so the layout cache is
            // actually exercised and scrolling stays honest.
            out.push(wide(&format!("{n:>5} │ {t}")));
        }
    }
    out
}

unsafe fn pick_font(dw: &IDWriteFactory) -> String {
    let mut coll: Option<IDWriteFontCollection> = None;
    if dw.GetSystemFontCollection(&mut coll, false).is_err() {
        return "Consolas".into();
    }
    let Some(coll) = coll else {
        return "Consolas".into();
    };
    for cand in FONT_CANDIDATES {
        let w = wide(cand);
        let mut index = 0u32;
        let mut exists = BOOL(0);
        if coll
            .FindFamilyName(PCWSTR(w.as_ptr()), &mut index, &mut exists)
            .is_ok()
            && exists.as_bool()
        {
            return (*cand).into();
        }
    }
    "Consolas".into()
}

unsafe fn rebuild_text(app: &mut App) {
    let fam = wide(&app.font);
    let locale = wide("en-us");
    let Ok(format) = app.dw.CreateTextFormat(
        PCWSTR(fam.as_ptr()),
        None,
        DWRITE_FONT_WEIGHT_NORMAL,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        app.size,
        PCWSTR(locale.as_ptr()),
    ) else {
        return;
    };

    // Disabling ligatures means setting 'calt' (contextual alternates) to 0.
    // In DirectWrite that lives on the layout, not the format.
    let typo: Option<IDWriteTypography> = if app.ligatures {
        None
    } else {
        app.dw.CreateTypography().ok().inspect(|t| {
            let _ = t.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_ALTERNATES,
                parameter: 0,
            });
            let _ = t.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES,
                parameter: 0,
            });
        })
    };

    app.layouts.clear();
    app.layouts.reserve(app.lines.len());
    for line in &app.lines {
        let len = line.len().saturating_sub(1); // excluding the NUL
        let Ok(layout) = app
            .dw
            .CreateTextLayout(&line[..len], &format, 100_000.0, 100.0)
        else {
            continue;
        };
        if let Some(t) = &typo {
            let _ = layout.SetTypography(
                t,
                DWRITE_TEXT_RANGE {
                    startPosition: 0,
                    length: len as u32,
                },
            );
        }
        app.layouts.push(layout);
    }

    // Take the line height from the font metrics rather than guessing.
    let mut metrics = DWRITE_TEXT_METRICS::default();
    if let Some(l) = app.layouts.first() {
        let _ = l.GetMetrics(&mut metrics);
        app.line_h = if metrics.height > 0.0 {
            metrics.height
        } else {
            app.size * 1.4
        };
    } else {
        app.line_h = app.size * 1.4;
    }

    app.format = Some(format);
}

unsafe fn ensure_rt(app: &mut App, hwnd: HWND) {
    if app.rt.is_some() {
        return;
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_IGNORE,
        },
        dpiX: 0.0,
        dpiY: 0.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
        hwnd,
        pixelSize: D2D_SIZE_U {
            width: (rc.right - rc.left).max(1) as u32,
            height: (rc.bottom - rc.top).max(1) as u32,
        },
        presentOptions: D2D1_PRESENT_OPTIONS_NONE,
    };
    let Ok(rt) = app.d2d.CreateHwndRenderTarget(&props, &hprops) else {
        return;
    };
    let fg = D2D1_COLOR_F {
        r: 0.87,
        g: 0.86,
        b: 0.84,
        a: 1.0,
    };
    let dim = D2D1_COLOR_F {
        r: 0.45,
        g: 0.44,
        b: 0.43,
        a: 1.0,
    };
    app.brush = rt.CreateSolidColorBrush(&fg, None).ok();
    app.dim = rt.CreateSolidColorBrush(&dim, None).ok();
    app.rt = Some(rt);
}

unsafe fn draw(app: &mut App, hwnd: HWND) {
    ensure_rt(app, hwnd);
    let (Some(rt), Some(brush), Some(dim)) = (app.rt.clone(), app.brush.clone(), app.dim.clone())
    else {
        return;
    };

    let t0 = std::time::Instant::now();
    let size = rt.GetSize();

    let (bg, fg, dimc) = if app.light {
        (
            D2D1_COLOR_F { r: 0.97, g: 0.96, b: 0.94, a: 1.0 },
            D2D1_COLOR_F { r: 0.10, g: 0.09, b: 0.09, a: 1.0 },
            D2D1_COLOR_F { r: 0.45, g: 0.44, b: 0.43, a: 1.0 },
        )
    } else {
        (
            D2D1_COLOR_F { r: 0.086, g: 0.078, b: 0.074, a: 1.0 },
            D2D1_COLOR_F { r: 0.87, g: 0.86, b: 0.84, a: 1.0 },
            D2D1_COLOR_F { r: 0.45, g: 0.44, b: 0.43, a: 1.0 },
        )
    };
    brush.SetColor(&fg);
    dim.SetColor(&dimc);

    rt.BeginDraw();
    rt.Clear(Some(&bg));

    let pad = 16.0;
    let header_h = 46.0;
    let visible = (((size.height - header_h - pad) / app.line_h).ceil() as usize).max(1);
    let end = (app.top + visible).min(app.layouts.len());

    let halves: &[(f32, f32, D2D1_TEXT_ANTIALIAS_MODE, &str)] = if app.split {
        &[
            (
                0.0,
                0.5,
                D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE,
                "ClearType (subpixel) — what an opaque surface would use",
            ),
            (
                0.5,
                1.0,
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                "Grayscale — what a translucent surface is stuck with",
            ),
        ]
    } else {
        &[(
            0.0,
            1.0,
            D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE,
            "ClearType (subpixel) — tek panel",
        )]
    };

    for (a, b, mode, label) in halves {
        let x0 = size.width * a;
        let x1 = size.width * b;
        rt.PushAxisAlignedClip(
            &D2D_RECT_F {
                left: x0,
                top: 0.0,
                right: x1,
                bottom: size.height,
            },
            D2D1_ANTIALIAS_MODE_ALIASED,
        );
        rt.SetTextAntialiasMode(*mode);

        // Draw each heading in its own mode, so the label is itself a
        // sample of what it names.
        if let Some(fmt) = &app.format {
            let lw = wide(label);
            if let Ok(l) = app
                .dw
                .CreateTextLayout(&lw[..lw.len() - 1], fmt, x1 - x0 - pad, 40.0)
            {
                rt.DrawTextLayout(
                    Vector2 { X: x0 + pad, Y: 10.0 },
                    &l,
                    &dim,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        }

        for (row, i) in (app.top..end).enumerate() {
            let y = header_h + row as f32 * app.line_h;
            rt.DrawTextLayout(
                Vector2 { X: x0 + pad, Y: y },
                &app.layouts[i],
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        rt.PopAxisAlignedClip();
    }

    // Divider line
    if app.split {
        let mid = size.width * 0.5;
        rt.DrawLine(
            Vector2 { X: mid, Y: 0.0 },
            Vector2 {
                X: mid,
                Y: size.height,
            },
            &dim,
            1.0,
            None,
        );
    }

    let _ = rt.EndDraw(None, None);
    app.frame_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let title = format!(
        "kubide spike 2 — {} {:.0}px · ligatures {} · background {} · draw {:.2} ms",
        app.font,
        app.size,
        if app.ligatures { "ON" } else { "OFF" },
        if app.light { "LIGHT" } else { "DARK" },
        app.frame_ms
    );
    let tw = wide(&title);
    let _ = SetWindowTextW(hwnd, PCWSTR(tw.as_ptr()));
}

fn main() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let font = pick_font(&dw);

        println!("kubide spike 2 — metin kalitesi");
        println!("  font: {font}");
        if font == "Consolas" {
            println!("  !! No Nerd Font found. Ligatures and powerline glyphs");
            println!("     will be missing. Install JetBrainsMono Nerd Font:");
            println!("     winget install --id DEVCOM.JetBrainsMonoNerdFont");
        }
        println!("  L ligatures · +/- size · S one/two panels · wheel · Q quit");

        let mut app = App {
            d2d,
            dw,
            rt: None,
            format: None,
            brush: None,
            dim: None,
            layouts: Vec::new(),
            lines: build_lines(),
            font,
            size: 14.0,
            line_h: 20.0,
            top: 0,
            ligatures: true,
            split: true,
            light: false,
            frame_ms: 0.0,
        };
        rebuild_text(&mut app);
        APP = Some(app);

        let instance = GetModuleHandleW(None)?;
        let class_name = w!("kubide_spike_text");
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
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("kubide spike 2"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1400,
            900,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
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
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                draw(app, hwnd);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            WM_SIZE => {
                if let Some(rt) = &app.rt {
                    let w = (lparam.0 & 0xFFFF) as u32;
                    let h = ((lparam.0 >> 16) & 0xFFFF) as u32;
                    let _ = rt.Resize(&D2D_SIZE_U {
                        width: w.max(1),
                        height: h.max(1),
                    });
                }
                LRESULT(0)
            }

            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
                let step = 3usize;
                if delta > 0 {
                    app.top = app.top.saturating_sub(step);
                } else {
                    app.top = (app.top + step).min(app.layouts.len().saturating_sub(1));
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            WM_KEYDOWN => {
                let mut dirty = true;
                let mut rebuild = false;
                match wparam.0 as u8 {
                    b'L' => {
                        app.ligatures = !app.ligatures;
                        rebuild = true;
                    }
                    b'S' => app.split = !app.split,
                    b'B' => app.light = !app.light,
                    0xBB | 0x6B => {
                        // + / numpad +
                        app.size = (app.size + 1.0).min(48.0);
                        rebuild = true;
                    }
                    0xBD | 0x6D => {
                        // - / numpad -
                        app.size = (app.size - 1.0).max(6.0);
                        rebuild = true;
                    }
                    b'Q' => {
                        PostQuitMessage(0);
                        dirty = false;
                    }
                    _ => dirty = false,
                }
                if rebuild {
                    rebuild_text(app);
                }
                if dirty {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
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

const _: () = assert!(size_of::<usize>() == 8, "64-bit hedef bekleniyor");
