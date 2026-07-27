//! Where things are drawn inside a pane.
//!
//! Drawing and hit-testing have to agree exactly. When they each work it out
//! separately they drift, and the cursor lands one column away from where you
//! clicked — a bug that looks like sloppiness and is impossible to spot in a
//! screenshot. So both call this.

use kb_ui::Rect;

/// Space between the pane edge and its content.
pub const INSET: f32 = 8.0;

/// Inset of an overlay's contents from its own edge.
///
/// Wider than a pane's: a floating panel needs air around its text or it reads
/// as a cramped dialog rather than as part of the surface.
pub const PAD: f32 = 18.0;

/// Shortest a scroll thumb is allowed to get.
///
/// Proportional length alone makes the thumb vanish in a long file — at 50,000
/// lines and 40 on screen it works out under a pixel — and an indicator you
/// cannot see is the bug this was meant to fix.
const MIN_THUMB: f32 = 18.0;

/// Where a scroll thumb sits inside a track, as (offset, length).
///
/// `None` when everything already fits, which is the signal to draw nothing:
/// a pane with no hidden content should have no furniture in it at all.
///
/// One piece of arithmetic for both axes — the caller decides whether the
/// track runs down the right edge or along the bottom. Two copies of this
/// would drift, and a scrollbar that lies about where you are is worse than
/// no scrollbar.
pub fn thumb(track: f32, first: usize, visible: usize, total: usize) -> Option<(f32, f32)> {
    if total <= visible || visible == 0 || track <= 0.0 {
        return None;
    }
    let length = (track * visible as f32 / total as f32).max(MIN_THUMB.min(track));
    // Against the last scrollable position, not the total: at the bottom the
    // thumb has to land flush with the end of the track, and dividing by
    // `total` leaves it short by its own length.
    let at = first as f32 / (total - visible) as f32;
    Some((at.clamp(0.0, 1.0) * (track - length), length))
}

/// Text geometry for an editor or viewer pane.
#[derive(Clone, Copy, Debug)]
pub struct TextArea {
    /// Left edge of the text itself, past the line-number gutter.
    pub text_x: f32,
    /// Top of the first content line, below the header.
    pub y0: f32,
    pub line_h: f32,
    pub cell_w: f32,
    /// Digits reserved for line numbers.
    pub digits: usize,
    /// How many lines fit.
    pub visible: usize,
    /// How many columns fit past the gutter.
    pub cols: usize,
}

impl TextArea {
    /// `top` is the first visible line, needed because the gutter is sized for
    /// the widest number actually on screen rather than the whole file.
    pub fn new(r: Rect, line_h: f32, cell_w: f32, top: usize) -> Self {
        let visible = (((r.h - INSET * 2.0 - line_h * 1.6) / line_h).floor()).max(1.0) as usize;
        let digits = ((top + visible).max(1) as f64).log10().floor() as usize + 1;
        let text_x = r.x + 10.0 + (digits as f32 + 1.0) * cell_w;
        Self {
            text_x,
            y0: r.y + INSET + line_h * 1.6,
            line_h,
            cell_w,
            digits,
            visible,
            cols: (((r.right() - INSET - text_x) / cell_w).floor()).max(1.0) as usize,
        }
    }

    /// Screen point to (line offset from `top`, column). Clamped, so a drag
    /// past the edge keeps extending instead of stopping.
    pub fn cell_at(&self, x: f32, y: f32) -> (usize, usize) {
        let row = ((y - self.y0) / self.line_h).floor().max(0.0) as usize;
        let col = ((x - self.text_x) / self.cell_w).round().max(0.0) as usize;
        (row, col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(top: usize) -> TextArea {
        TextArea::new(Rect::new(0.0, 0.0, 800.0, 600.0), 20.0, 10.0, top)
    }

    #[test]
    fn the_column_count_excludes_the_gutter() {
        // Counting the gutter would let the cursor sit off the right edge
        // while the code still believed it was visible.
        let a = area(0);
        assert!(a.cols < 80);
        assert!(a.text_x + a.cols as f32 * a.cell_w <= 800.0);
    }

    #[test]
    fn clicking_a_drawn_line_returns_that_line() {
        // The property that matters: drawing puts line n at y0 + n*line_h, so
        // hit-testing that exact y must give back n.
        let a = area(0);
        for n in 0..10 {
            let y = a.y0 + n as f32 * a.line_h + a.line_h * 0.5;
            assert_eq!(a.cell_at(a.text_x, y).0, n);
        }
    }

    #[test]
    fn columns_round_to_the_nearest_gap() {
        // Clicking the left half of a character puts the cursor before it, the
        // right half after it. Truncating instead would make it impossible to
        // put the cursor at the end of a line by clicking.
        let a = area(0);
        assert_eq!(a.cell_at(a.text_x + 4.0, a.y0).1, 0);
        assert_eq!(a.cell_at(a.text_x + 6.0, a.y0).1, 1);
    }

    #[test]
    fn clicks_above_the_text_clamp_to_the_first_line() {
        let a = area(0);
        assert_eq!(a.cell_at(a.text_x, 0.0), (0, 0));
        assert_eq!(a.cell_at(0.0, a.y0).1, 0, "clicking the gutter is column 0");
    }

    #[test]
    fn content_that_fits_gets_no_thumb() {
        assert_eq!(thumb(200.0, 0, 40, 40), None);
        assert_eq!(thumb(200.0, 0, 40, 12), None);
    }

    #[test]
    fn the_thumb_spans_the_track_from_top_to_bottom() {
        // The two ends are what a scrollbar is read for: flush at the top
        // means "nothing above", flush at the bottom means "nothing below".
        let (top, len) = thumb(200.0, 0, 25, 100).unwrap();
        assert_eq!(top, 0.0);
        let (bottom, _) = thumb(200.0, 75, 25, 100).unwrap();
        assert!((bottom + len - 200.0).abs() < 0.01, "{bottom} + {len} should reach 200");
    }

    #[test]
    fn the_thumb_length_tracks_how_much_is_on_screen() {
        let (_, quarter) = thumb(200.0, 0, 25, 100).unwrap();
        let (_, half) = thumb(200.0, 0, 50, 100).unwrap();
        assert!((quarter - 50.0).abs() < 0.01);
        assert!((half - 100.0).abs() < 0.01);
    }

    #[test]
    fn a_huge_file_still_leaves_something_visible() {
        // Proportionally this thumb is under a pixel.
        let (_, len) = thumb(400.0, 0, 40, 50_000).unwrap();
        assert!(len >= 18.0, "{len} would be invisible");
    }

    #[test]
    fn the_thumb_never_leaves_the_track() {
        // `first` can exceed the last scrollable line while a resize is still
        // settling, and a thumb drawn past the edge paints over the divider.
        for first in [0, 1, 40, 99, 500] {
            let (off, len) = thumb(120.0, first, 10, 100).unwrap();
            assert!(off >= 0.0, "{first}: {off}");
            assert!(off + len <= 120.01, "{first}: {off} + {len}");
        }
    }

    #[test]
    fn a_track_shorter_than_the_minimum_still_fits_inside_it() {
        let (off, len) = thumb(10.0, 50, 10, 100).unwrap();
        assert!(len <= 10.0);
        assert!(off + len <= 10.01);
    }

    #[test]
    fn the_gutter_widens_with_the_line_numbers() {
        // Otherwise scrolling from line 99 to 100 would shift the text sideways
        // by a column mid-scroll.
        assert!(area(1000).text_x > area(0).text_x);
    }
}
