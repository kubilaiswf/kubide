//! The Linux surface: a tiny-skia pixmap drawn on the CPU and handed to the
//! compositor through shared memory.
//!
//! No GPU on purpose. The frame is a few hundred rectangles and a screen of
//! glyphs, which a CPU rasteriser finishes in a millisecond or two, and a
//! software path has no driver to fall over and no shader to compile. What
//! makes the window translucent is the alpha channel of the buffer itself:
//! X11 gets a depth-32 visual through softbuffer, Wayland gets an ARGB8888
//! `wl_shm` buffer from the presenter next door. Whether anything behind it
//! is blurred is the compositor's decision — Hyprland and KWin blur windows
//! with alpha when told to, GNOME does not — so the material is painted
//! here as a tint and the blur is whatever the desktop adds.
//!
//! The pixmap is premultiplied, as every shared-memory window format is, and
//! tiny-skia takes straight colours and premultiplies on the way in. Text is
//! blended by hand for the same reason: the glyph cache hands back coverage
//! and the surface wants premultiplied results.

use std::cell::RefCell;
use std::num::NonZeroU32;
use std::sync::Arc;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tiny_skia::{
    FillRule, Mask, Paint, Path, PathBuilder, Pixmap, PremultipliedColorU8, Stroke, Transform,
};

use crate::{Color, Error, Point, Rect, RoundedRect};

type WinitWindow = Arc<winit::window::Window>;

/// A solid colour. Nothing to allocate on this backend, but the type keeps
/// draw.rs identical on both.
#[derive(Clone, Copy, Debug)]
pub struct Brush(tiny_skia::Color);

/// One frame's worth of drawing. Owns the pixmap until [`Renderer::end`]
/// takes it back to present.
pub struct Canvas {
    inner: RefCell<Inner>,
}

struct Inner {
    pixmap: Pixmap,
    /// Every clip pushed and not yet popped. The effective clip is their
    /// intersection with the pixmap; kept whole so a pop restores exactly.
    clips: Vec<Rect>,
    /// A mask for the current clip, built the first time a shape that is
    /// not a plain rectangle crosses the clip edge, and dropped when the
    /// clip changes. Rectangles are clipped by intersection and never need
    /// it, which is nearly everything that is drawn.
    mask: Option<(Rect, Mask)>,
}

fn paint(b: &Brush) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(b.0);
    p.anti_alias = true;
    p
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let r = Rect {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    };
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

fn contains(outer: Rect, inner: tiny_skia::Rect) -> bool {
    inner.left() >= outer.left
        && inner.top() >= outer.top
        && inner.right() <= outer.right
        && inner.bottom() <= outer.bottom
}

fn skia_rect(r: Rect) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_ltrb(r.left, r.top, r.right, r.bottom)
}

/// A rectangle with its corners rounded by two radii, as four cubic arcs.
/// The kappa is the usual circle-from-Béziers constant.
fn rounded_path(r: &RoundedRect) -> Option<Path> {
    let Rect { left, top, right, bottom } = r.rect;
    let rx = r.radius_x.min((right - left) * 0.5).max(0.0);
    let ry = r.radius_y.min((bottom - top) * 0.5).max(0.0);
    if rx <= 0.0 || ry <= 0.0 {
        return Some(PathBuilder::from_rect(skia_rect(r.rect)?));
    }
    const K: f32 = 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(left + rx, top);
    pb.line_to(right - rx, top);
    pb.cubic_to(right - rx + rx * K, top, right, top + ry - ry * K, right, top + ry);
    pb.line_to(right, bottom - ry);
    pb.cubic_to(right, bottom - ry + ry * K, right - rx + rx * K, bottom, right - rx, bottom);
    pb.line_to(left + rx, bottom);
    pb.cubic_to(left + rx - rx * K, bottom, left, bottom - ry + ry * K, left, bottom - ry);
    pb.line_to(left, top + ry);
    pb.cubic_to(left, top + ry - ry * K, left + rx - rx * K, top, left + rx, top);
    pb.close();
    pb.finish()
}

impl Inner {
    fn clip_rect(&self) -> Rect {
        let mut r = Rect {
            left: 0.0,
            top: 0.0,
            right: self.pixmap.width() as f32,
            bottom: self.pixmap.height() as f32,
        };
        for c in &self.clips {
            match intersect(r, *c) {
                Some(i) => r = i,
                None => return Rect::default(),
            }
        }
        r
    }

    /// The mask to draw a shape with: none when it lies inside the clip,
    /// the clip's mask when it crosses the edge.
    fn mask_for(&mut self, bounds: tiny_skia::Rect) -> bool {
        let clip = self.clip_rect();
        if contains(clip, bounds) {
            return false;
        }
        if self.mask.as_ref().is_none_or(|(r, _)| *r != clip) {
            let mut mask = Mask::new(self.pixmap.width(), self.pixmap.height());
            if let (Some(m), Some(r)) = (mask.as_mut(), skia_rect(clip)) {
                // Aliased, like the Direct2D clip: an antialiased clip edge
                // shows as a faint seam wherever two clips meet.
                m.fill_path(&PathBuilder::from_rect(r), FillRule::Winding, false, Transform::identity());
            }
            self.mask = mask.map(|m| (clip, m));
        }
        true
    }

    fn fill(&mut self, path: &Path, b: &Brush) {
        if self.clip_rect() == Rect::default() {
            return;
        }
        let masked = self.mask_for(path.bounds());
        let Inner { pixmap, mask, .. } = self;
        let mask = if masked { mask.as_ref().map(|(_, m)| m) } else { None };
        pixmap.fill_path(path, &paint(b), FillRule::Winding, Transform::identity(), mask);
    }

    fn stroke(&mut self, path: &Path, b: &Brush, width: f32) {
        if self.clip_rect() == Rect::default() {
            return;
        }
        // The stroke reaches half its width past the path on each side.
        let bounds = path.bounds();
        let grown = tiny_skia::Rect::from_ltrb(
            bounds.left() - width,
            bounds.top() - width,
            bounds.right() + width,
            bounds.bottom() + width,
        )
        .unwrap_or(bounds);
        let masked = self.mask_for(grown);
        let Inner { pixmap, mask, .. } = self;
        let mask = if masked { mask.as_ref().map(|(_, m)| m) } else { None };
        let stroke = Stroke { width, ..Stroke::default() };
        pixmap.stroke_path(path, &paint(b), &stroke, Transform::identity(), mask);
    }
}

impl Canvas {
    pub fn solid(&self, c: Color) -> crate::Result<Brush> {
        let color = tiny_skia::Color::from_rgba(
            c.r.clamp(0.0, 1.0),
            c.g.clamp(0.0, 1.0),
            c.b.clamp(0.0, 1.0),
            c.a.clamp(0.0, 1.0),
        )
        .unwrap_or(tiny_skia::Color::BLACK);
        Ok(Brush(color))
    }

    pub fn fill_rect(&self, r: &Rect, b: &Brush) {
        let mut inner = self.inner.borrow_mut();
        let Some(r) = intersect(*r, inner.clip_rect()) else { return };
        let Some(rect) = skia_rect(r) else { return };
        inner.pixmap.fill_rect(rect, &paint(b), Transform::identity(), None);
    }

    pub fn fill_rounded(&self, r: &RoundedRect, b: &Brush) {
        let Some(path) = rounded_path(r) else { return };
        self.inner.borrow_mut().fill(&path, b);
    }

    pub fn stroke_rounded(&self, r: &RoundedRect, b: &Brush, width: f32) {
        let Some(path) = rounded_path(r) else { return };
        self.inner.borrow_mut().stroke(&path, b, width);
    }

    pub fn stroke_rect(&self, r: &Rect, b: &Brush, width: f32) {
        let Some(rect) = skia_rect(*r) else { return };
        self.inner.borrow_mut().stroke(&PathBuilder::from_rect(rect), b, width);
    }

    pub fn line(&self, from: Point, to: Point, b: &Brush, width: f32) {
        let mut pb = PathBuilder::new();
        pb.move_to(from.x, from.y);
        pb.line_to(to.x, to.y);
        let Some(path) = pb.finish() else { return };
        self.inner.borrow_mut().stroke(&path, b, width);
    }

    pub fn fill_ellipse(&self, center: Point, radius_x: f32, radius_y: f32, b: &Brush) {
        let Some(oval) = tiny_skia::Rect::from_ltrb(
            center.x - radius_x,
            center.y - radius_y,
            center.x + radius_x,
            center.y + radius_y,
        ) else {
            return;
        };
        let Some(path) = PathBuilder::from_oval(oval) else { return };
        self.inner.borrow_mut().fill(&path, b);
    }

    /// Draws a shaped line with its top-left corner at `at`.
    ///
    /// Blended by hand, glyph by glyph: the cache hands back coverage masks
    /// (and RGBA for colour glyphs), and the clip is applied as pixel
    /// bounds, which is what an aliased clip is.
    pub fn text(&self, at: Point, layout: &kb_text::Layout, b: &Brush) {
        let mut inner = self.inner.borrow_mut();
        let clip = inner.clip_rect();
        if clip == Rect::default() {
            return;
        }
        let (x0, y0) = (clip.left.round() as i32, clip.top.round() as i32);
        let (x1, y1) = (clip.right.round() as i32, clip.bottom.round() as i32);
        let stride = inner.pixmap.width() as i32;
        let (tr, tg, tb, ta) = (b.0.red(), b.0.green(), b.0.blue(), b.0.alpha());
        let pixels = inner.pixmap.pixels_mut();

        layout.for_each_image(at.x, at.y, |gx, gy, img| {
            for row in 0..img.height as i32 {
                let py = gy + row;
                if py < y0 || py >= y1 {
                    continue;
                }
                for col in 0..img.width as i32 {
                    let px = gx + col;
                    if px < x0 || px >= x1 {
                        continue;
                    }
                    let i = (row * img.width as i32 + col) as usize;
                    let (r, g, bl, a) = match img.kind {
                        kb_text::GlyphKind::Mask => {
                            (tr, tg, tb, ta * img.data[i] as f32 / 255.0)
                        }
                        kb_text::GlyphKind::Subpixel => {
                            let p = &img.data[i * 4..i * 4 + 3];
                            let cov = (p[0] as f32 + p[1] as f32 + p[2] as f32) / (3.0 * 255.0);
                            (tr, tg, tb, ta * cov)
                        }
                        kb_text::GlyphKind::Color => {
                            let p = &img.data[i * 4..i * 4 + 4];
                            (
                                p[0] as f32 / 255.0,
                                p[1] as f32 / 255.0,
                                p[2] as f32 / 255.0,
                                ta * p[3] as f32 / 255.0,
                            )
                        }
                    };
                    blend(&mut pixels[(py * stride + px) as usize], r, g, bl, a);
                }
            }
        });
    }

    /// Aliased on purpose: a clip edge that is antialiased shows as a faint
    /// seam wherever two clips meet.
    pub fn push_clip(&self, r: &Rect) {
        self.inner.borrow_mut().clips.push(*r);
    }

    pub fn pop_clip(&self) {
        self.inner.borrow_mut().clips.pop();
    }
}

/// Source-over of a straight-alpha colour onto a premultiplied pixel.
fn blend(dst: &mut PremultipliedColorU8, r: f32, g: f32, b: f32, a: f32) {
    if a <= 0.0 {
        return;
    }
    let a = a.min(1.0);
    let keep = 1.0 - a;
    let mix = |src: f32, old: u8| (src * a * 255.0 + old as f32 * keep).round().clamp(0.0, 255.0) as u8;
    let na = (a * 255.0 + dst.alpha() as f32 * keep).round().clamp(0.0, 255.0) as u8;
    let (nr, ng, nb) = (
        mix(r, dst.red()).min(na),
        mix(g, dst.green()).min(na),
        mix(b, dst.blue()).min(na),
    );
    if let Some(p) = PremultipliedColorU8::from_rgba(nr, ng, nb, na) {
        *dst = p;
    }
}

/// What the window is painted on before anything is drawn — the stand-in
/// for DWM's materials. The compositor may blur what is behind a window
/// with alpha; the tint is ours either way, and it is what makes text on a
/// bright wallpaper readable.
fn material(b: kb_win::Backdrop) -> tiny_skia::Color {
    let (v, a) = match b {
        kb_win::Backdrop::None => (0x1e, 1.0),
        kb_win::Backdrop::Mica => (0x20, 0.94),
        kb_win::Backdrop::MicaAlt => (0x0c, 0.94),
        kb_win::Backdrop::Acrylic => (0x2a, 0.78),
    };
    tiny_skia::Color::from_rgba8(v, v, v, (a * 255.0) as u8)
}

/// Rounds the four corners of a finished frame, the way DWM rounds a
/// Windows 11 window. Coverage is taken from the distance to the corner's
/// circle, so the edge is antialiased rather than stepped.
fn round_corners(pixmap: &mut Pixmap, radius: f32) {
    if radius < 1.0 {
        return;
    }
    let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
    let r = radius.min(w as f32 * 0.5).min(h as f32 * 0.5);
    let span = r.ceil() as i32;
    let pixels = pixmap.pixels_mut();
    for (cx, cy, sx, sy) in [
        (r, r, 0, 0),
        (w as f32 - r, r, w - span, 0),
        (r, h as f32 - r, 0, h - span),
        (w as f32 - r, h as f32 - r, w - span, h - span),
    ] {
        for y in sy..sy + span {
            for x in sx..sx + span {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // Inside the corner square but outside the quarter circle.
                let (dx, dy) = (px - cx, py - cy);
                let toward = (dx < 0.0) == (cx < w as f32 * 0.5) && (dy < 0.0) == (cy < h as f32 * 0.5);
                if !toward {
                    continue;
                }
                let d = (dx * dx + dy * dy).sqrt();
                let cover = (r + 0.5 - d).clamp(0.0, 1.0);
                if cover >= 1.0 {
                    continue;
                }
                let i = (y * w + x) as usize;
                let p = pixels[i];
                let scale = |v: u8| (v as f32 * cover).round() as u8;
                if let Some(np) = PremultipliedColorU8::from_rgba(
                    scale(p.red()),
                    scale(p.green()),
                    scale(p.blue()),
                    scale(p.alpha()),
                ) {
                    pixels[i] = np;
                }
            }
        }
    }
}

fn pack(p: PremultipliedColorU8) -> u32 {
    (p.alpha() as u32) << 24 | (p.red() as u32) << 16 | (p.green() as u32) << 8 | p.blue() as u32
}

enum Presenter {
    X11 {
        // The context has to outlive the surface; kept for that alone.
        _context: softbuffer::Context<WinitWindow>,
        surface: softbuffer::Surface<WinitWindow, WinitWindow>,
    },
    Wayland(Box<crate::wayland::Presenter>),
}

pub struct Renderer {
    presenter: Presenter,
    /// The frame being reused. Taken by `begin`, returned by `end`.
    pixmap: Option<Pixmap>,
    width: u32,
    height: u32,
}

impl Renderer {
    pub fn new(_window: kb_win::Window, width: u32, height: u32) -> crate::Result<Self> {
        let window = kb_win::winit_window().ok_or_else(|| Error("no window to draw on".into()))?;
        let raw = window
            .window_handle()
            .map_err(|e| Error(format!("window handle: {e}")))?
            .as_raw();
        let presenter = match raw {
            RawWindowHandle::Wayland(h) => {
                Presenter::Wayland(Box::new(crate::wayland::Presenter::new(window.clone(), h)?))
            }
            _ => {
                let context = softbuffer::Context::new(window.clone())
                    .map_err(|e| Error(format!("softbuffer: {e}")))?;
                let surface = softbuffer::Surface::new(&context, window.clone())
                    .map_err(|e| Error(format!("softbuffer: {e}")))?;
                Presenter::X11 { _context: context, surface }
            }
        };
        Ok(Self { presenter, pixmap: None, width: width.max(1), height: height.max(1) })
    }

    pub fn resize(&mut self, width: u32, height: u32) -> crate::Result<()> {
        self.width = width.max(1);
        self.height = height.max(1);
        Ok(())
    }

    /// Starts a frame on the material colour — the stand-in for DWM's
    /// backdrop showing through a cleared surface.
    pub fn begin(&mut self) -> crate::Result<Canvas> {
        let (w, h) = (self.width, self.height);
        let mut pixmap = match self.pixmap.take() {
            Some(p) if p.width() == w && p.height() == h => p,
            _ => Pixmap::new(w, h).ok_or_else(|| Error(format!("no pixmap for {w}x{h}")))?,
        };
        pixmap.fill(material(kb_win::current_backdrop()));
        Ok(Canvas { inner: RefCell::new(Inner { pixmap, clips: Vec::new(), mask: None }) })
    }

    pub fn end(&mut self, canvas: Canvas) -> crate::Result<()> {
        let Inner { mut pixmap, .. } = canvas.inner.into_inner();
        // A maximised window fills its screen edge to edge, as on Windows.
        if !kb_win::is_maximized() {
            round_corners(&mut pixmap, 8.0 * kb_win::scale_factor() as f32);
        }
        let (w, h) = (pixmap.width(), pixmap.height());
        match &mut self.presenter {
            Presenter::X11 { surface, .. } => {
                let size = (NonZeroU32::new(w), NonZeroU32::new(h));
                if let (Some(w), Some(h)) = size {
                    surface.resize(w, h).map_err(|e| Error(format!("softbuffer: {e}")))?;
                }
                let mut buffer =
                    surface.buffer_mut().map_err(|e| Error(format!("softbuffer: {e}")))?;
                for (dst, src) in buffer.iter_mut().zip(pixmap.pixels()) {
                    *dst = pack(*src);
                }
                buffer.present().map_err(|e| Error(format!("softbuffer: {e}")))?;
            }
            Presenter::Wayland(p) => {
                p.present(w as i32, h as i32, |dst| {
                    for (d, s) in dst.iter_mut().zip(pixmap.pixels()) {
                        *d = pack(*s);
                    }
                })?;
            }
        }
        self.pixmap = Some(pixmap);
        Ok(())
    }

    pub fn size(&self) -> (f32, f32) {
        (self.width as f32, self.height as f32)
    }
}
