//! cosmic-text: the Linux engine.
//!
//! The font system and the glyph cache are process-wide, in a thread local:
//! the engine shapes lines and the renderer rasterises them, and both need
//! the same font data. A thread local rather than a mutex because everything
//! that touches text runs on the window thread anyway.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cosmic_text::fontdb::{Family, Query, Stretch, Style, Weight};
use cosmic_text::{
    Attrs, Buffer, FontSystem, LayoutGlyph, Metrics, Shaping, SwashCache, SwashContent, Wrap,
};

use crate::{Error, DEFAULT_FONTS};

/// What is tried when nothing from the config is installed. Whatever the
/// distribution ships as its monospace face comes after these, so a machine
/// with any font at all still renders.
const FALLBACKS: &[&str] = &[
    "JetBrainsMono Nerd Font Mono",
    "JetBrains Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Hack",
    "Source Code Pro",
    "Fira Mono",
    "Ubuntu Mono",
];

struct Fonts {
    system: FontSystem,
    swash: SwashCache,
}

thread_local! {
    static FONTS: RefCell<Option<Fonts>> = const { RefCell::new(None) };
}

/// Runs `f` with the shared font system, loading the system fonts on the
/// first call. That first call is the slow one — fontconfig's directories
/// are walked once — which is why it is shared rather than per engine.
fn with_fonts<R>(f: impl FnOnce(&mut Fonts) -> R) -> R {
    FONTS.with(|cell| {
        let mut slot = cell.borrow_mut();
        let fonts = slot.get_or_insert_with(|| Fonts {
            system: FontSystem::new(),
            swash: SwashCache::new(),
        });
        f(fonts)
    })
}

/// The regular face of a family, if the family is installed.
fn face_of(db: &cosmic_text::fontdb::Database, family: &str) -> Option<cosmic_text::fontdb::ID> {
    let families = [Family::Name(family)];
    db.query(&Query {
        families: &families,
        weight: Weight::NORMAL,
        stretch: Stretch::Normal,
        style: Style::Normal,
    })
}

/// A shaped line, ready to be drawn at a point. Cheap to clone: it is one
/// reference-counted glyph list.
#[derive(Clone)]
pub struct Layout(Rc<Shaped>);

struct Shaped {
    glyphs: Vec<LayoutGlyph>,
    /// To the last inked glyph — DirectWrite's `width`.
    width: f32,
    /// To the end of the last glyph, spaces included —
    /// `widthIncludingTrailingWhitespace`.
    advance: f32,
    /// Distance from the top of the line box to the baseline.
    baseline: f32,
}

/// How a glyph's pixels are to be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlyphKind {
    /// One coverage byte per pixel, to be tinted with the text colour.
    Mask,
    /// Straight RGBA, colour and all — emoji and other bitmap glyphs.
    Color,
    /// Three coverage channels per pixel; drawn as their average, since the
    /// surface is translucent and subpixel colouring would fringe.
    Subpixel,
}

/// One rasterised glyph, borrowed from the cache for the length of a call.
pub struct GlyphImage<'a> {
    pub width: u32,
    pub height: u32,
    pub kind: GlyphKind,
    pub data: &'a [u8],
}

impl Layout {
    /// Every glyph's raster with the pixel position of its top-left corner,
    /// for the line drawn with its top-left corner at `(x, y)`.
    ///
    /// Rasterised here rather than at shaping time because the position
    /// decides the subpixel offset, and the same line is drawn at many
    /// columns. The cache under it is keyed by that offset, quantised.
    pub fn for_each_image(&self, x: f32, y: f32, mut f: impl FnMut(i32, i32, GlyphImage<'_>)) {
        with_fonts(|fonts| {
            for g in &self.0.glyphs {
                let p = g.physical((x, y + self.0.baseline), 1.0);
                let Some(img) = fonts.swash.get_image(&mut fonts.system, p.cache_key) else {
                    continue;
                };
                if img.placement.width == 0 || img.placement.height == 0 {
                    continue;
                }
                let kind = match img.content {
                    SwashContent::Mask => GlyphKind::Mask,
                    SwashContent::Color => GlyphKind::Color,
                    SwashContent::SubpixelMask => GlyphKind::Subpixel,
                };
                f(
                    p.x + img.placement.left,
                    p.y - img.placement.top,
                    GlyphImage {
                        width: img.placement.width,
                        height: img.placement.height,
                        kind,
                        data: &img.data,
                    },
                );
            }
        })
    }
}

pub struct TextEngine {
    family: String,
    size: f32,
    line_h: f32,
    baseline: f32,
    cell: (f32, f32),
    /// Line text → shaped layout. Shaping is too expensive to redo every
    /// frame, and an editor draws the same lines over and over.
    cache: HashMap<String, Layout>,
}

impl TextEngine {
    pub fn new(size: f32) -> crate::Result<Self> {
        Self::with_fonts(DEFAULT_FONTS, size)
    }

    /// Builds an engine from a candidate list, first installed one wins.
    pub fn with_fonts<S: AsRef<str>>(candidates: &[S], size: f32) -> crate::Result<Self> {
        let family = pick_family(candidates)?;
        let mut me = Self {
            family,
            size,
            line_h: size * 1.4,
            baseline: size,
            cell: (size * 0.6, size * 1.4),
            cache: HashMap::new(),
        };
        me.measure();
        Ok(me)
    }

    /// Width and height of one cell.
    ///
    /// The terminal works on a grid of cells, not lines: every character must
    /// advance exactly one cell. We measure the font's real advance rather
    /// than guessing, or long lines drift and the grid breaks.
    pub fn cell_size(&self) -> (f32, f32) {
        self.cell
    }

    /// Line height and baseline from the font's own metrics, the cell from
    /// a shaped `M` — the same two measurements DirectWrite answers with
    /// `GetMetrics` on "Xg" and "M".
    fn measure(&mut self) {
        if let Some((ascent, descent, leading)) = font_metrics(&self.family, self.size) {
            let h = ascent + descent + leading;
            if h > 0.0 {
                self.line_h = h;
                // DirectWrite puts the line gap above the ascender, so the
                // baseline sits that much lower than the ascent alone.
                self.baseline = leading + ascent;
            }
        }
        let w = self.shape("M").advance;
        self.cell = (if w > 0.0 { w } else { self.size * 0.6 }, self.line_h);
    }

    fn shape(&self, text: &str) -> Shaped {
        with_fonts(|fonts| {
            let mut buffer = Buffer::new_empty(Metrics::new(self.size, self.line_h.max(1.0)));
            buffer.set_wrap(Wrap::None);
            buffer.set_size(None, None);
            let mut attrs = Attrs::new();
            attrs.family = Family::Name(&self.family);
            buffer.set_text(text, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut fonts.system, false);

            let mut glyphs = Vec::new();
            let (mut width, mut advance) = (0.0f32, 0.0f32);
            // A line is one run. Should a string ever carry a newline, the
            // later runs continue to the right rather than stacking: the
            // renderer draws lines, and a layout is one of them.
            let mut x_off = 0.0;
            for run in buffer.layout_runs() {
                for g in run.glyphs {
                    let mut g = g.clone();
                    g.x += x_off;
                    let end = g.x + g.w;
                    advance = advance.max(end);
                    let blank = run.text.get(g.start..g.end).is_some_and(|s| s.trim().is_empty());
                    if !blank {
                        width = width.max(end);
                    }
                    glyphs.push(g);
                }
                x_off = advance;
            }
            Shaped { glyphs, width, advance, baseline: self.baseline }
        })
    }

    /// A shaped line layout, served from cache on repeat.
    pub fn line(&mut self, text: &str) -> crate::Result<Layout> {
        if let Some(l) = self.cache.get(text) {
            return Ok(l.clone());
        }
        let l = Layout(Rc::new(self.shape(text)));
        self.cache.insert(text.to_owned(), l.clone());
        Ok(l)
    }

    /// For text that changes every call — status bar, counters, timings.
    ///
    /// Caching those with `line()` is a leak: a new string per frame grows the
    /// map without bound.
    pub fn volatile(&self, text: &str) -> crate::Result<Layout> {
        Ok(Layout(Rc::new(self.shape(text))))
    }

    /// Drawn width of a string.
    ///
    /// Needed to right-align text against an edge. Guessing from the character
    /// count and cell width is close but not exact, and "close" here means a
    /// message that runs off the edge and gets clipped mid-word.
    pub fn width_of(&self, text: &str) -> f32 {
        match self.cache.get(text) {
            Some(l) => l.0.width,
            None => self.shape(text).width,
        }
    }

    /// Where the next character would start after `text`.
    ///
    /// Unlike [`Self::width_of`], trailing spaces count: DirectWrite's
    /// `width` stops at the last glyph, which is right for aligning a label
    /// and wrong for a caret — typing a space moved nothing, and the space
    /// looked like it had not been typed.
    pub fn advance_of(&self, text: &str) -> f32 {
        match self.cache.get(text) {
            Some(l) => l.0.advance,
            None => self.shape(text).advance,
        }
    }

    pub fn set_size(&mut self, size: f32) -> crate::Result<()> {
        self.size = size.clamp(6.0, 72.0);
        self.cache.clear(); // layouts are bound to the old size
        self.measure();
        Ok(())
    }

    /// Reselects the family from a new candidate list. Used on config reload.
    pub fn set_fonts<S: AsRef<str>>(&mut self, candidates: &[S], size: f32) -> crate::Result<()> {
        self.family = pick_family(candidates)?;
        self.set_size(size)
    }

    /// Whether the chosen family can actually draw a character.
    ///
    /// The family is picked from a list of candidates, so which font you get
    /// depends on the machine — and the icons are Nerd Font private-use
    /// codepoints that most fonts have never heard of. Drawing one anyway
    /// produces a notdef box, and a column of boxes reads as a broken editor
    /// rather than as a missing font. Callers ask first and draw something
    /// plain when the answer is no.
    ///
    /// The regular face answers. Coverage can differ between weights, but the
    /// editor draws everything in one weight, so that is the one that decides.
    pub fn has_glyph(&self, c: char) -> bool {
        with_fonts(|fonts| {
            let Some(id) = face_of(fonts.system.db(), &self.family) else { return false };
            let Some(font) = fonts.system.get_font(id, Weight::NORMAL) else { return false };
            font.as_swash().charmap().map(c) != 0
        })
    }

    pub fn family(&self) -> &str {
        &self.family
    }
    pub fn size(&self) -> f32 {
        self.size
    }
    pub fn line_height(&self) -> f32 {
        self.line_h
    }
    pub fn cached_lines(&self) -> usize {
        self.cache.len()
    }
}

/// Ascent, descent and line gap of the family's regular face, in pixels at
/// `size`.
fn font_metrics(family: &str, size: f32) -> Option<(f32, f32, f32)> {
    with_fonts(|fonts| {
        let id = face_of(fonts.system.db(), family)?;
        let font = fonts.system.get_font(id, Weight::NORMAL)?;
        let m = font.as_swash().metrics(&[]).scale(size);
        Some((m.ascent, m.descent, m.leading))
    })
}

/// The first candidate fontconfig knows, then the first of [`FALLBACKS`],
/// then whatever the desktop calls `monospace`, then any monospaced face,
/// then any face at all. A wrong font still renders; a missing one does not.
fn pick_family<S: AsRef<str>>(candidates: &[S]) -> crate::Result<String> {
    with_fonts(|fonts| {
        let db = fonts.system.db();
        let installed = |name: &str| face_of(db, name).is_some();
        for c in candidates {
            if installed(c.as_ref()) {
                return Ok(c.as_ref().to_string());
            }
        }
        for c in FALLBACKS {
            if installed(c) {
                return Ok((*c).to_string());
            }
        }
        let generic = db.family_name(&Family::Monospace).to_string();
        if installed(&generic) {
            return Ok(generic);
        }
        if let Some(face) = db.faces().find(|f| f.monospaced).or_else(|| db.faces().next()) {
            if let Some((name, _)) = face.families.first() {
                return Ok(name.clone());
            }
        }
        Err(Error("no fonts installed — fontconfig found nothing to draw with".into()))
    })
}
