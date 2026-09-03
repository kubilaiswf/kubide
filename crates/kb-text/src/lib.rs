//! Text layer: font selection, shaping and a layout cache.
//!
//! One API, two engines. On Windows it is DirectWrite; on Linux it is
//! cosmic-text, which shapes and rasterises the way DirectWrite does and
//! reads the same fontconfig the rest of the desktop does. The renderer
//! only ever sees a [`Layout`] — a shaped line it can draw at a point — so
//! draw.rs is written once.
//!
//! No antialiasing mode is set, on purpose. ClearType and grayscale were
//! indistinguishable by eye on both dark and light backgrounds, and a
//! translucent surface has to fall back to grayscale anyway. One mode means
//! none of Windows Terminal's opacity-dependent mode switching, and the
//! Linux engine draws grayscale for the same reason.

/// Preference order; the first installed one wins.
///
/// Mono variants lead: their icons are one cell wide, and everything drawn
/// here sits on a grid. "NFM" is Nerd Fonts' abbreviation of "Nerd Font Mono",
/// used because the Win32 family name has to fit LOGFONT.lfFaceName
/// (LF_FACESIZE = 32 wide chars including the terminator); both spellings are
/// listed because which one a machine has depends on when it was downloaded —
/// and on Linux the long spelling is the only one fontconfig knows. Nothing
/// from `Cascadia Code` down has icons at all; see `has_glyph`, which is how
/// the caller finds out.
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

/// Why the text engine could not do something. Carried as a sentence: the
/// status bar is the only place these are ever read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

#[cfg(windows)]
impl From<windows::core::Error> for Error {
    fn from(e: windows::core::Error) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::{Layout, TextEngine};

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::{GlyphImage, GlyphKind, Layout, TextEngine};

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage has to come from the font, not from a list of family names we
    /// believe are patched. Guessing was the bug: kubide drew Nerd Font
    /// codepoints into Cascadia Code and filled the file tree with boxes.
    #[test]
    fn a_font_is_asked_what_it_can_draw() {
        let Ok(text) = TextEngine::new(14.0) else {
            // No font engine here; nothing to assert and nothing wrong.
            return;
        };
        assert!(text.has_glyph('A'), "{} cannot draw 'A'", text.family());
        // Plane 16 private use. No font ships this, whatever is installed.
        assert!(!text.has_glyph('\u{10FFFD}'));
    }

    /// The grid rests on these two numbers; a zero in either is a blank
    /// window rather than an error message.
    #[test]
    fn metrics_are_positive_and_a_space_advances() {
        let Ok(text) = TextEngine::new(14.0) else { return };
        let (cw, ch) = text.cell_size();
        assert!(cw > 0.0 && ch > 0.0, "cell {cw}x{ch}");
        assert!(text.line_height() > 0.0);
        // DirectWrite's `width` stops at the last glyph; the caret needs the
        // trailing space counted, which is what `advance_of` is for.
        assert!(text.advance_of("a ") > text.width_of("a "));
        assert!((text.width_of("MM") - cw * 2.0).abs() < 1.0);
    }
}
