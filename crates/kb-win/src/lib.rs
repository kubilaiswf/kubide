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
    /// Where a previous run left the window. `None` lets Windows choose,
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
    /// asks for a new one. If Windows refuses to make it, the nearest system
    /// pointer stands in — a cursor must never simply vanish.
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

/// Builds a pointer from its description, hand-plotted into a 32-bit DIB.
///
/// Drawn rather than loaded from a resource: the colour comes from a theme
/// file we have never seen and the size from a setting, so there is nothing
/// to bake at compile time. The shape is laid down as a boolean mask, then
/// ringed with a one-pixel dark outline by dilation — an accent-coloured
/// pointer over code in the same accent would otherwise be invisible exactly
/// where it is pointing.
///
/// Returns an invalid handle when any GDI call refuses; the caller falls
/// back to the nearest system pointer rather than showing none at all.
fn themed_cursor(t: ThemedCursor) -> HCURSOR {
    unsafe {
        // SM_CXCURSOR tracks the accessibility cursor-size setting, so `0`
        // means "as big as the user asked Windows for".
        let s = match t.size {
            0 => GetSystemMetrics(SM_CXCURSOR).max(16),
            n => (n as i32).clamp(12, 128),
        };
        // The Temple pointer is pixel art and skips the whole smooth
        // pipeline below — anti-aliasing it would be vandalism.
        if t.kind == ThemedKind::Temple {
            return temple_cursor(s, t.rgb);
        }
        let w = s as usize;
        let k = s as f32;

        // The shape as a signed distance: negative inside, in pixels. One
        // function instead of a boolean mask because everything that makes a
        // pointer look finished falls out of it — anti-aliased edges from the
        // zero crossing, the dark ring from a one-pixel band outside, the
        // soft shadow from the same field sampled at an offset. A hard mask
        // gives staircased diagonals, which is exactly the cheap look this
        // replaces.
        let dist_seg = |px_: f32, py_: f32, ax: f32, ay: f32, bx: f32, by: f32| -> f32 {
            let (dx, dy) = (bx - ax, by - ay);
            let len2 = dx * dx + dy * dy;
            let t_ = if len2 == 0.0 {
                0.0
            } else {
                (((px_ - ax) * dx + (py_ - ay) * dy) / len2).clamp(0.0, 1.0)
            };
            let (ex, ey) = (px_ - (ax + t_ * dx), py_ - (ay + t_ * dy));
            (ex * ex + ey * ey).sqrt()
        };

        // Signed distance to a polygon given in pixels: even-odd for the
        // sign, nearest edge for the magnitude.
        let poly_sd = move |qx: f32, qy: f32, pts: &[(f32, f32)]| -> f32 {
            let n = pts.len();
            let mut inside = false;
            let mut d = f32::MAX;
            for i in 0..n {
                let (x1, y1) = pts[i];
                let (x2, y2) = pts[(i + 1) % n];
                if (y1 > qy) != (y2 > qy) && qx < (x2 - x1) * (qy - y1) / (y2 - y1) + x1 {
                    inside = !inside;
                }
                d = d.min(dist_seg(qx, qy, x1, y1, x2, y2));
            }
            if inside {
                -d
            } else {
                d
            }
        };

        // Hotspot first; the field closure needs the same offsets.
        let o = (k * 0.08).max(2.0); // margin so ring and shadow stay on canvas
        let (hx, hy) = match t.kind {
            ThemedKind::Arrow | ThemedKind::Dart | ThemedKind::Triangle => (o as i32, o as i32),
            // The hand points with its fingertip, where the reference
            // cursor puts it: just inside the tip, not on its outline.
            ThemedKind::Hand => ((k * 0.492) as i32, (k * 0.281) as i32),
            // Everything else points from its middle.
            _ => (s / 2, s / 2),
        };

        let kind = t.kind;
        let sd = move |px_: f32, py_: f32| -> f32 {
            match kind {
                // Returned before the field is ever built; the arm exists
                // for the compiler, not the pixels.
                ThemedKind::Temple => f32::MAX,
                // Capsule strokes — segments with a radius — so the stem and
                // serifs get rounded ends instead of sawn-off ones.
                ThemedKind::IBeam | ThemedKind::Bar => {
                    let cx = k * 0.5;
                    let r = (k / 40.0).max(0.7); // hairline, kept visible
                    let (top, bottom) = (k * 0.30, k * 0.70);
                    let serif = (k / 11.0).max(1.5);
                    let mut d = dist_seg(px_, py_, cx, top, cx, bottom) - r;
                    if kind == ThemedKind::IBeam {
                        d = d
                            .min(dist_seg(px_, py_, cx - serif, top, cx + serif, top) - r)
                            .min(dist_seg(px_, py_, cx - serif, bottom, cx + serif, bottom) - r);
                    }
                    d
                }
                ThemedKind::Arrow | ThemedKind::Dart | ThemedKind::Triangle => {
                    // The arrow deliberately does not trace the stock
                    // Windows silhouette: the left edge runs longer, the
                    // notch sits higher and the tail is slimmer, so at a
                    // glance it reads as this program's pointer and not the
                    // system's in a costume. The dart shares nothing with
                    // it at all.
                    let pts: &[(f32, f32)] = match kind {
                        // The classic solid pointer — near-vertical left
                        // edge, angled shoulder, a proper notched tail.
                        // Filled body with the outline on the other side of
                        // the lightness is the whole look.
                        ThemedKind::Arrow => &[
                            (0.00, 0.00),
                            (0.00, 0.517),
                            (0.124, 0.396),
                            (0.196, 0.559),
                            (0.284, 0.520),
                            (0.211, 0.360),
                            (0.377, 0.360),
                        ],
                        ThemedKind::Dart => {
                            &[(0.00, 0.00), (0.30, 0.115), (0.16, 0.16), (0.115, 0.30)]
                        }
                        _ => &[(0.00, 0.00), (0.34, 0.24), (0.10, 0.38)],
                    };
                    let scaled: Vec<(f32, f32)> =
                        pts.iter().map(|(x, y)| (x * k + o, y * k + o)).collect();
                    // Dilating the field by a hair rounds every corner —
                    // the difference between clip-art and something drawn.
                    poly_sd(px_, py_, &scaled) - (k * 0.015).max(0.4)
                }
                // The classic link hand, as one traced silhouette rather
                // than a heap of capsules — capsules gave a mitten, because
                // a hand is its outline and an outline is what they cannot
                // make. Walked once: up the index finger, over the three
                // folded knuckles, down the outside of the palm, across the
                // cuff, and back up past the thumb.
                ThemedKind::Hand => {
                    // Proportioned off a decoded professional cursor rather
                    // than from imagination: a fist seen from its thumb
                    // side, one short finger raised, occupying about three
                    // fifths of the canvas and sitting low in it. An earlier
                    // pass drew the hand nearly canvas-tall with grooves
                    // between the knuckles; at this size that reads as a
                    // mitten with scratches, and the reference has neither.
                    const HAND: &[(f32, f32)] = &[
                        (0.437, 0.250), // index, left of the tip
                        (0.455, 0.219),
                        (0.530, 0.219),
                        (0.552, 0.250), // index, right of the tip
                        (0.552, 0.372),
                        (0.630, 0.392), // knuckles, swelling right
                        (0.722, 0.420),
                        (0.795, 0.452),
                        (0.812, 0.500), // the outside edge of the fist
                        (0.812, 0.790),
                        (0.440, 0.790), // the cuff
                        (0.410, 0.745),
                        (0.340, 0.665), // heel of the hand
                        (0.278, 0.590),
                        (0.250, 0.530), // thumb
                        (0.250, 0.462),
                        (0.300, 0.432),
                        (0.395, 0.418),
                        (0.437, 0.400), // back up to the index
                    ];
                    let scaled: Vec<(f32, f32)> =
                        HAND.iter().map(|(x, y)| (x * k, y * k)).collect();
                    poly_sd(px_, py_, &scaled) - (k * 0.014).max(0.4)
                }
                // A rounded shaft with a triangular head at each end,
                // centred, replacing the system's double arrows over the
                // pane dividers.
                ThemedKind::SizeWE | ThemedKind::SizeNS => {
                    // Drawn horizontal; the NS variant just swaps the axes.
                    let (qx, qy) = if kind == ThemedKind::SizeWE {
                        (px_, py_)
                    } else {
                        (py_, px_)
                    };
                    let c = k * 0.5;
                    let r = (k / 36.0).max(0.8);
                    let half = k * 0.26; // tip to centre
                    let head = k * 0.13; // head length
                    let hw = (k * 0.085).max(1.5); // head half-width
                    let shaft =
                        dist_seg(qx, qy, c - half + head * 0.7, c, c + half - head * 0.7, c) - r;
                    let left =
                        [(c - half, c), (c - half + head, c - hw), (c - half + head, c + hw)];
                    let right =
                        [(c + half, c), (c + half - head, c - hw), (c + half - head, c + hw)];
                    shaft.min(poly_sd(qx, qy, &left)).min(poly_sd(qx, qy, &right))
                }
            }
        };

        // Three layers out of one field, bottom to top: a soft shadow the
        // shape casts down-right, a ring hugging the edge, the body in the
        // asked-for colour. The ring takes whichever side of the lightness
        // the body did not — white around a near-black arrow, dark around an
        // accent one — so no colour can dress a cursor that disappears.
        // Premultiplied alpha throughout, which is what an alpha cursor's
        // DIB has to hold anyway.
        let (br, bg, bb) = (
            ((t.rgb >> 16) & 0xFF) as f32 / 255.0,
            ((t.rgb >> 8) & 0xFF) as f32 / 255.0,
            (t.rgb & 0xFF) as f32 / 255.0,
        );
        let lum = 0.299 * br + 0.587 * bg + 0.114 * bb;
        let (ring_v, ring_strength) = if lum < 0.45 {
            // A light ring needs more presence than a dark one to read.
            (1.0f32, 0.9f32)
        } else {
            (0.0f32, 0.6f32)
        };
        let mut px = vec![0u32; w * w];
        for y in 0..s {
            for x in 0..s {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let d = sd(fx, fy);

                let body_a = (0.5 - d).clamp(0.0, 1.0);
                // The ring fades over a pixel; multiplied down so it reads
                // as an edge, not a border.
                let ring_a = (1.6 - d).clamp(0.0, 1.0) * ring_strength;
                let dsh = sd(fx - 1.2, fy - 1.6);
                let shadow_a = 0.22 * ((2.4 - dsh) / 2.4).clamp(0.0, 1.0);

                // Shadow, then ring over it, then body over both. The
                // shadow is always dark; only the ring switches sides.
                let mut a = shadow_a;
                let mut r = ring_v * ring_a;
                let mut g = ring_v * ring_a;
                let mut b = ring_v * ring_a;
                a = ring_a + a * (1.0 - ring_a);
                r = br * body_a + r * (1.0 - body_a);
                g = bg * body_a + g * (1.0 - body_a);
                b = bb * body_a + b * (1.0 - body_a);
                a = body_a + a * (1.0 - body_a);

                px[y as usize * w + x as usize] = ((a * 255.0) as u32) << 24
                    | (((r * 255.0) as u32) << 16)
                    | (((g * 255.0) as u32) << 8)
                    | ((b * 255.0) as u32);
            }
        }

        cursor_from_pixels(&px, s, hx, hy)
    }
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

/// The TempleOS mouse pointer, in loving memory of Terry A. Davis.
///
/// Traced from the original: a ↖ built from a corner — one arm along the
/// top, one down the left, a filled wedge in the crook — a diagonal shaft
/// running to the south-east, and the signature dotted crosshair ticks
/// escaping past the tip. Drawn on a 28-cell grid scaled by whole pixels,
/// with NO anti-aliasing and NO shadow: TempleOS was 640x480 with 16
/// colours because God said so, and smoothing its cursor would be missing
/// the entire point.
fn temple_cursor(s: i32, rgb: u32) -> HCURSOR {
    const GRID: i32 = 28;
    // (x, y) cells that are lit, tip at (6, 6).
    let mut cells: Vec<(i32, i32)> = Vec::new();
    for d in [0, 2, 4] {
        cells.push((6, d)); // dotted tick, up from the tip
        cells.push((d, 6)); // and left
    }
    for i in 6..=16 {
        cells.push((i, 6)); // top arm
        cells.push((6, i)); // left arm
    }
    // The wedge in the crook of the corner.
    for (x, y) in [(7, 7), (8, 7), (9, 7), (7, 8), (8, 8), (7, 9)] {
        cells.push((x, y));
    }
    for i in 7..=26 {
        cells.push((i, i)); // the shaft, running south-east
    }

    let cell = (s / GRID).max(1);
    let w = s as usize;
    let mut lit = vec![false; w * w];
    let mut set = |x: i32, y: i32| {
        if x >= 0 && y >= 0 && x < s && y < s {
            lit[y as usize * w + x as usize] = true;
        }
    };
    for (cx, cy) in cells {
        for dy in 0..cell {
            for dx in 0..cell {
                set(cx * cell + dx, cy * cell + dy);
            }
        }
    }

    // Body flat, rim hard: one pixel of near-black around every lit pixel,
    // no gradients anywhere. The rim is not authentic — TempleOS drew over
    // its own wallpaper and could afford to vanish — but a pointer here has
    // to survive every theme, and it stays as blocky as the rest.
    let body = 0xFF_00_00_00u32
        | ((rgb >> 16) & 0xFF) << 16
        | ((rgb >> 8) & 0xFF) << 8
        | (rgb & 0xFF);
    let rim = 0xE6_00_00_00u32;
    let mut px = vec![0u32; w * w];
    for y in 0..s {
        for x in 0..s {
            let i = y as usize * w + x as usize;
            if lit[i] {
                px[i] = body;
            } else {
                let ringed = (-1..=1).any(|dy| {
                    (-1..=1).any(|dx| {
                        let (nx, ny) = (x + dx, y + dy);
                        nx >= 0 && ny >= 0 && nx < s && ny < s && lit[ny as usize * w + nx as usize]
                    })
                });
                if ringed {
                    px[i] = rim;
                }
            }
        }
    }

    let hot = 6 * cell + cell / 2;
    cursor_from_pixels(&px, s, hot, hot)
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
