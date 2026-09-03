//! The drawing surface: a translucent frame, a handful of shapes, and text.
//!
//! draw.rs speaks this API and nothing under it. On Windows the [`Canvas`] is
//! a Direct2D device context on a DirectComposition swapchain — the only way
//! a window can be translucent there; see `win.rs` for the chain. On Linux it
//! is a tiny-skia pixmap presented through shared memory, with the
//! compositor doing whatever blur it does behind a window with alpha.
//!
//! Every colour is straight alpha at this boundary; each backend
//! premultiplies for its own surface, so the edge-darkening that comes from
//! mixing the two conventions cannot start in draw.rs.

/// Straight-alpha colour, 0..1 per channel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// Builds a colour. The one constructor, so a swapped channel is a compile
/// error rather than a tint.
pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color { r, g, b, a }
}

/// Axis-aligned, in pixels, edges rather than size — what every clip and
/// fill is expressed as.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RoundedRect {
    pub rect: Rect,
    pub radius_x: f32,
    pub radius_y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// Why a frame could not be drawn or presented. A sentence, because the one
/// thing that ever reads it is a log line or the status bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<kb_text::Error> for Error {
    fn from(e: kb_text::Error) -> Self {
        Error(e.0)
    }
}

impl From<kb_win::Error> for Error {
    fn from(e: kb_win::Error) -> Self {
        Error(e.0)
    }
}

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
pub use win::{Brush, Canvas, Renderer};

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
mod wayland;
#[cfg(not(windows))]
pub use unix::{Brush, Canvas, Renderer};
