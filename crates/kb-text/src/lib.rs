//! DirectWrite text layer: font selection, shaping and a layout cache.
//!
//! No antialiasing mode is set, on purpose. ClearType and grayscale were
//! indistinguishable by eye on both dark and light backgrounds, and a
//! translucent surface has to fall back to grayscale anyway. One mode means
//! none of Windows Terminal's opacity-dependent mode switching.

use std::collections::HashMap;

use windows::core::*;
use windows::Win32::Graphics::DirectWrite::*;

/// Preference order; the first installed one wins.
///
/// Mono variants lead: their icons are one cell wide, and everything drawn
/// here sits on a grid. "NFM" is Nerd Fonts' abbreviation of "Nerd Font Mono",
/// used because the Win32 family name has to fit LOGFONT.lfFaceName
/// (LF_FACESIZE = 32 wide chars including the terminator); both spellings are
/// listed because which one a machine has depends on when it was downloaded.
/// Nothing from `Cascadia Code` down has icons at all; see `has_glyph`, which
/// is how the caller finds out.
pub const DEFAULT_FONTS: &[&str] = &[
    "JetBrainsMono NFM",
    "JetBrainsMono Nerd Font Mono",
    "CaskaydiaCove NFM",
    "CaskaydiaCove Nerd Font Mono",
    "FiraCode NFM",
    "FiraCode Nerd Font Mono",
    "JetBrainsMono Nerd Font",
    "CaskaydiaCove Nerd Font",
    "Cascadia Code",
    "Consolas",
];

fn wide0(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct TextEngine {
    factory: IDWriteFactory,
    format: IDWriteTextFormat,
    family: String,
    size: f32,
    line_h: f32,
    cell: (f32, f32),
    /// Line text → shaped layout. Shaping is too expensive to redo every
    /// frame, and an editor draws the same lines over and over.
    cache: HashMap<String, IDWriteTextLayout>,
}

impl TextEngine {
    pub fn new(size: f32) -> Result<Self> {
        Self::with_fonts(DEFAULT_FONTS, size)
    }

    /// Builds an engine from a candidate list, first installed one wins.
    pub fn with_fonts<S: AsRef<str>>(candidates: &[S], size: f32) -> Result<Self> {
        unsafe {
            let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let family = pick_family(&factory, candidates);
            let format = make_format(&factory, &family, size)?;
            let mut me = Self {
                factory,
                format,
                family,
                size,
                line_h: size * 1.4,
                cell: (size * 0.6, size * 1.4),
                cache: HashMap::new(),
            };
            me.measure_line_height();
            me.measure_cell();
            Ok(me)
        }
    }

    /// Width and height of one cell.
    ///
    /// The terminal works on a grid of cells, not lines: every character must
    /// advance exactly one cell. We measure the font's real advance rather
    /// than guessing, or long lines drift and the grid breaks.
    pub fn cell_size(&self) -> (f32, f32) {
        self.cell
    }

    fn measure_cell(&mut self) {
        let w = self
            .layout_uncached("M")
            .ok()
            .and_then(|l| {
                let mut m = DWRITE_TEXT_METRICS::default();
                unsafe { l.GetMetrics(&mut m) }.ok().map(|_| m.width)
            })
            .filter(|w| *w > 0.0)
            .unwrap_or(self.size * 0.6);
        self.cell = (w, self.line_h);
    }

    fn measure_line_height(&mut self) {
        // Take it from the font metrics rather than guessing.
        if let Ok(l) = self.layout_uncached("Xg") {
            let mut m = DWRITE_TEXT_METRICS::default();
            if unsafe { l.GetMetrics(&mut m) }.is_ok() && m.height > 0.0 {
                self.line_h = m.height;
            }
        }
    }

    fn layout_uncached(&self, text: &str) -> Result<IDWriteTextLayout> {
        let w: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.factory
                .CreateTextLayout(&w, &self.format, f32::MAX / 4.0, self.line_h * 2.0)
        }
    }

    /// A shaped line layout, served from cache on repeat.
    pub fn line(&mut self, text: &str) -> Result<IDWriteTextLayout> {
        if let Some(l) = self.cache.get(text) {
            return Ok(l.clone());
        }
        let l = self.layout_uncached(text)?;
        self.cache.insert(text.to_owned(), l.clone());
        Ok(l)
    }

    /// For text that changes every call — status bar, counters, timings.
    ///
    /// Caching those with `line()` is a leak: a new string per frame grows the
    /// map without bound.
    pub fn volatile(&self, text: &str) -> Result<IDWriteTextLayout> {
        self.layout_uncached(text)
    }

    /// Drawn width of a string.
    ///
    /// Needed to right-align text against an edge. Guessing from the character
    /// count and cell width is close but not exact, and "close" here means a
    /// message that runs off the edge and gets clipped mid-word.
    pub fn width_of(&self, text: &str) -> f32 {
        self.layout_uncached(text)
            .ok()
            .and_then(|l| {
                let mut m = DWRITE_TEXT_METRICS::default();
                unsafe { l.GetMetrics(&mut m) }.ok().map(|_| m.width)
            })
            .unwrap_or_else(|| text.chars().count() as f32 * self.cell.0)
    }

    pub fn set_size(&mut self, size: f32) -> Result<()> {
        self.size = size.clamp(6.0, 72.0);
        self.rebuild_format()
    }

    /// Reselects the family from a new candidate list. Used on config reload.
    pub fn set_fonts<S: AsRef<str>>(&mut self, candidates: &[S], size: f32) -> Result<()> {
        self.family = unsafe { pick_family(&self.factory, candidates) };
        self.size = size.clamp(6.0, 72.0);
        self.rebuild_format()
    }

    fn rebuild_format(&mut self) -> Result<()> {
        self.format = unsafe { make_format(&self.factory, &self.family, self.size)? };
        self.cache.clear(); // layouts are bound to the old format
        self.measure_line_height();
        self.measure_cell();
        Ok(())
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
    /// Not cheap enough for a draw loop: this walks the system font collection
    /// on every call. Ask once when the font changes.
    pub fn has_glyph(&self, c: char) -> bool {
        unsafe {
            let mut coll: Option<IDWriteFontCollection> = None;
            if self.factory.GetSystemFontCollection(&mut coll, false).is_err() {
                return false;
            }
            let Some(coll) = coll else { return false };

            let w = wide0(&self.family);
            let (mut idx, mut exists) = (0u32, BOOL::default());
            if coll
                .FindFamilyName(PCWSTR(w.as_ptr()), &mut idx, &mut exists)
                .is_err()
                || !exists.as_bool()
            {
                return false;
            }
            let Ok(family) = coll.GetFontFamily(idx) else { return false };
            // The regular face. Coverage can differ between weights, but the
            // editor draws everything in one weight, so that is the one that
            // decides.
            let Ok(font) = family.GetFirstMatchingFont(
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
            ) else {
                return false;
            };
            font.HasCharacter(c as u32).is_ok_and(|has| has.as_bool())
        }
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
    pub fn factory(&self) -> &IDWriteFactory {
        &self.factory
    }
    pub fn cached_lines(&self) -> usize {
        self.cache.len()
    }
}

unsafe fn make_format(
    factory: &IDWriteFactory,
    family: &str,
    size: f32,
) -> Result<IDWriteTextFormat> {
    let fam = wide0(family);
    let loc = wide0("en-us");
    factory.CreateTextFormat(
        PCWSTR(fam.as_ptr()),
        None,
        DWRITE_FONT_WEIGHT_NORMAL,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        size,
        PCWSTR(loc.as_ptr()),
    )
}

unsafe fn pick_family<S: AsRef<str>>(factory: &IDWriteFactory, candidates: &[S]) -> String {
    let mut coll: Option<IDWriteFontCollection> = None;
    if factory.GetSystemFontCollection(&mut coll, false).is_err() {
        return "Consolas".into();
    }
    let Some(coll) = coll else {
        return "Consolas".into();
    };
    for cand in candidates {
        let cand = cand.as_ref();
        let w = wide0(cand);
        let (mut idx, mut exists) = (0u32, BOOL::default());
        if coll
            .FindFamilyName(PCWSTR(w.as_ptr()), &mut idx, &mut exists)
            .is_ok()
            && exists.as_bool()
        {
            return cand.into();
        }
    }
    // Always installed on Windows, so a bad config still renders.
    "Consolas".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage has to come from the font, not from a list of family names we
    /// believe are patched. Guessing was the bug: kubide drew Nerd Font
    /// codepoints into Cascadia Code and filled the file tree with boxes.
    #[test]
    fn a_font_is_asked_what_it_can_draw() {
        let Ok(text) = TextEngine::new(14.0) else {
            // No DirectWrite here; nothing to assert and nothing wrong.
            return;
        };
        assert!(text.has_glyph('A'), "{} cannot draw 'A'", text.family());
        // Plane 16 private use. No font ships this, whatever is installed.
        assert!(!text.has_glyph('\u{10FFFD}'));
    }
}
