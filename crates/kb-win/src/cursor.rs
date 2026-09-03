//! The drawn pointers, as pixels. Pure: no window, no platform.
//!
//! Drawn rather than loaded from a resource: the colour comes from a theme
//! file we have never seen and the size from a setting, so there is nothing
//! to bake at compile time. Each platform wraps the result in whatever its
//! cursor object is — a DIB on Windows, an RGBA buffer on Linux — and the
//! shapes are the same on both because this is the only place they exist.

use crate::{ThemedCursor, ThemedKind};

/// A finished pointer: a square of premultiplied pixels and its hotspot.
pub(crate) struct Pixels {
    pub size: i32,
    pub hot: (i32, i32),
    /// Top-down rows of premultiplied `0xAARRGGBB`, which is what an alpha
    /// cursor's DIB has to hold anyway.
    pub argb: Vec<u32>,
}

impl Pixels {
    /// The same pixels as straight-alpha RGBA bytes, for a platform that
    /// premultiplies itself. Windows takes the DIB as it is.
    #[cfg_attr(windows, allow(dead_code))]
    pub fn rgba_straight(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.argb.len() * 4);
        for &p in &self.argb {
            let a = (p >> 24) & 0xFF;
            let un = |v: u32| (v * 255 + a / 2).checked_div(a).unwrap_or(0).min(255) as u8;
            out.push(un((p >> 16) & 0xFF));
            out.push(un((p >> 8) & 0xFF));
            out.push(un(p & 0xFF));
            out.push(a as u8);
        }
        out
    }
}

/// Builds a pointer from its description on a canvas `s` pixels square.
///
/// The shape is laid down as a signed distance field, then ringed with a
/// one-pixel dark outline by dilation — an accent-coloured pointer over code
/// in the same accent would otherwise be invisible exactly where it is
/// pointing.
pub(crate) fn themed_pixels(t: ThemedCursor, s: i32) -> Pixels {
    let s = s.clamp(12, 128);
    // The Temple pointer is pixel art and skips the whole smooth pipeline
    // below — anti-aliasing it would be vandalism.
    if t.kind == ThemedKind::Temple {
        return temple_pixels(s, t.rgb);
    }
    let w = s as usize;
    let k = s as f32;

    // The shape as a signed distance: negative inside, in pixels. One
    // function instead of a boolean mask because everything that makes a
    // pointer look finished falls out of it — anti-aliased edges from the
    // zero crossing, the dark ring from a one-pixel band outside, the soft
    // shadow from the same field sampled at an offset. A hard mask gives
    // staircased diagonals, which is exactly the cheap look this replaces.
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

    // Signed distance to a polygon given in pixels: even-odd for the sign,
    // nearest edge for the magnitude.
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
        // The hand points with its fingertip, where the reference cursor
        // puts it: just inside the tip, not on its outline.
        ThemedKind::Hand => ((k * 0.492) as i32, (k * 0.281) as i32),
        // Everything else points from its middle.
        _ => (s / 2, s / 2),
    };

    let kind = t.kind;
    let sd = move |px_: f32, py_: f32| -> f32 {
        match kind {
            // Returned before the field is ever built; the arm exists for
            // the compiler, not the pixels.
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
                // The arrow deliberately does not trace the stock Windows
                // silhouette: the left edge runs longer, the notch sits
                // higher and the tail is slimmer, so at a glance it reads
                // as this program's pointer and not the system's in a
                // costume. The dart shares nothing with it at all.
                let pts: &[(f32, f32)] = match kind {
                    // The classic solid pointer — near-vertical left edge,
                    // angled shoulder, a proper notched tail. Filled body
                    // with the outline on the other side of the lightness
                    // is the whole look.
                    ThemedKind::Arrow => &[
                        (0.00, 0.00),
                        (0.00, 0.517),
                        (0.124, 0.396),
                        (0.196, 0.559),
                        (0.284, 0.520),
                        (0.211, 0.360),
                        (0.377, 0.360),
                    ],
                    ThemedKind::Dart => &[(0.00, 0.00), (0.30, 0.115), (0.16, 0.16), (0.115, 0.30)],
                    _ => &[(0.00, 0.00), (0.34, 0.24), (0.10, 0.38)],
                };
                let scaled: Vec<(f32, f32)> =
                    pts.iter().map(|(x, y)| (x * k + o, y * k + o)).collect();
                // Dilating the field by a hair rounds every corner — the
                // difference between clip-art and something drawn.
                poly_sd(px_, py_, &scaled) - (k * 0.015).max(0.4)
            }
            // The classic link hand, as one traced silhouette rather than a
            // heap of capsules — capsules gave a mitten, because a hand is
            // its outline and an outline is what they cannot make. Walked
            // once: up the index finger, over the three folded knuckles,
            // down the outside of the palm, across the cuff, and back up
            // past the thumb.
            ThemedKind::Hand => {
                // Proportioned off a decoded professional cursor rather than
                // from imagination: a fist seen from its thumb side, one
                // short finger raised, occupying about three fifths of the
                // canvas and sitting low in it. An earlier pass drew the
                // hand nearly canvas-tall with grooves between the knuckles;
                // at this size that reads as a mitten with scratches, and
                // the reference has neither.
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
                let scaled: Vec<(f32, f32)> = HAND.iter().map(|(x, y)| (x * k, y * k)).collect();
                poly_sd(px_, py_, &scaled) - (k * 0.014).max(0.4)
            }
            // A rounded shaft with a triangular head at each end, centred,
            // replacing the system's double arrows over the pane dividers.
            ThemedKind::SizeWE | ThemedKind::SizeNS => {
                // Drawn horizontal; the NS variant just swaps the axes.
                let (qx, qy) = if kind == ThemedKind::SizeWE { (px_, py_) } else { (py_, px_) };
                let c = k * 0.5;
                let r = (k / 36.0).max(0.8);
                let half = k * 0.26; // tip to centre
                let head = k * 0.13; // head length
                let hw = (k * 0.085).max(1.5); // head half-width
                let shaft = dist_seg(qx, qy, c - half + head * 0.7, c, c + half - head * 0.7, c) - r;
                let left = [(c - half, c), (c - half + head, c - hw), (c - half + head, c + hw)];
                let right = [(c + half, c), (c + half - head, c - hw), (c + half - head, c + hw)];
                shaft.min(poly_sd(qx, qy, &left)).min(poly_sd(qx, qy, &right))
            }
        }
    };

    // Three layers out of one field, bottom to top: a soft shadow the shape
    // casts down-right, a ring hugging the edge, the body in the asked-for
    // colour. The ring takes whichever side of the lightness the body did
    // not — white around a near-black arrow, dark around an accent one — so
    // no colour can dress a cursor that disappears. Premultiplied alpha
    // throughout.
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
            // The ring fades over a pixel; multiplied down so it reads as
            // an edge, not a border.
            let ring_a = (1.6 - d).clamp(0.0, 1.0) * ring_strength;
            let dsh = sd(fx - 1.2, fy - 1.6);
            let shadow_a = 0.22 * ((2.4 - dsh) / 2.4).clamp(0.0, 1.0);

            // Shadow, then ring over it, then body over both. The shadow is
            // always dark; only the ring switches sides.
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

    Pixels { size: s, hot: (hx, hy), argb: px }
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
fn temple_pixels(s: i32, rgb: u32) -> Pixels {
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
    let body = 0xFF_00_00_00u32 | ((rgb >> 16) & 0xFF) << 16 | ((rgb >> 8) & 0xFF) << 8 | (rgb & 0xFF);
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
    Pixels { size: s, hot: (hot, hot), argb: px }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A platform falls back to a stock pointer when a shape comes out
    /// empty, which looks exactly like the feature not existing. So the
    /// failure this guards against is silent by design, and only a test
    /// makes it loud. Every shape, at two sizes.
    #[test]
    fn every_themed_cursor_has_ink_and_a_hotspot_on_the_canvas() {
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
            for size in [20, 32] {
                let p = themed_pixels(ThemedCursor { kind, size: size as u16, rgb: 0x00c4a7e7 }, size);
                assert_eq!(p.argb.len(), (size * size) as usize);
                let opaque = p.argb.iter().filter(|px| (*px >> 24) > 200).count();
                assert!(opaque > 8, "{kind:?} at {size} has {opaque} solid pixels");
                assert!(p.hot.0 >= 0 && p.hot.0 < size && p.hot.1 >= 0 && p.hot.1 < size);
                // Straight alpha must never exceed the channel range.
                assert!(p.rgba_straight().len() == p.argb.len() * 4);
            }
        }
    }
}
