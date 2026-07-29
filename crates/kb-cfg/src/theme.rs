//! Theme — color roles and the ANSI palette.
//!
//! Every field here maps to something actually drawn. An option nobody reads
//! is a promise to the user that the code doesn't keep.
//!
//! UI roles are named by what they mean — "dim", "accent" — rather than by
//! where they appear, so a role can move without the config having to.

use crate::Color;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub fg: Color,
    /// Secondary text: pane labels, hints.
    pub dim: Color,
    /// Focus stripe and highlights.
    pub accent: Color,
    /// Attention, but not an error (scrollback badge).
    pub warning: Color,
    pub error: Color,
    /// Pane divider hairline.
    pub divider: Color,
    /// Backdrop of the overlay.
    ///
    /// Translucent, so the acrylic carries through it — an opaque panel over a
    /// blurred window reads as a dialog from another program. Dark enough that
    /// the code underneath is a texture rather than something you start
    /// reading: a search box you can read through is a search box you cannot
    /// read.
    ///
    /// That second sentence was the intent; `e0` was not living up to it. At
    /// 0.88 a light line of code behind the panel contributes about 30 of 255
    /// and the eye finds it — over a diff the picker had two texts in it at
    /// once. `f2` leaves a tint of what is behind rather than a legible ghost.
    pub overlay: Color,

    pub caption: Caption,
    pub terminal: TerminalColors,
    pub git: GitColors,
    pub syntax: SyntaxColors,
}

/// Syntax roles.
///
/// Deliberately short. tree-sitter's capture names go on forever
/// (`function.method.builtin`), and a theme with forty entries is one nobody
/// edits. Finer names fall back to these.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyntaxColors {
    pub keyword: Color,
    pub function: Color,
    /// `type` in the file; the trailing underscore is only a Rust keyword
    /// clash, and leaking that into the config would be silly.
    #[serde(rename = "type")]
    pub type_: Color,
    pub string: Color,
    pub number: Color,
    pub comment: Color,
    pub constant: Color,
    pub operator: Color,
    pub punctuation: Color,
    pub variable: Color,
    pub property: Color,
    pub attribute: Color,
}

impl Default for SyntaxColors {
    fn default() -> Self {
        Self {
            keyword: c(0xc7, 0xa0, 0xe8),
            function: c(0x8c, 0xb8, 0xf2),
            type_: c(0x7d, 0xd4, 0xc4),
            string: c(0x9c, 0xcf, 0x8e),
            number: c(0xe8, 0xa9, 0x7c),
            // Dim on purpose: a comment you have to read past is worse than
            // one you can skip over.
            comment: c(0x76, 0x7b, 0x86),
            constant: c(0xe8, 0xa9, 0x7c),
            operator: c(0xc2, 0xbf, 0xba),
            punctuation: c(0x9b, 0x97, 0x92),
            variable: c(0xed, 0xeb, 0xe6),
            property: c(0x93, 0xc9, 0xd8),
            attribute: c(0xd6, 0xb2, 0x7a),
        }
    }
}

/// File status in the explorer. Separate from the ANSI palette because these
/// carry meaning — green is "added", not "the third color".
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitColors {
    pub modified: Color,
    pub added: Color,
    pub deleted: Color,
    pub untracked: Color,
    pub conflicted: Color,
}

impl Default for GitColors {
    fn default() -> Self {
        Self {
            modified: c(0xe0, 0xb1, 0x5c),
            added: c(0x8f, 0xc8, 0x8a),
            deleted: c(0xd9, 0x7b, 0x7b),
            untracked: c(0x7d, 0x8b, 0xa6),
            conflicted: c(0xe0, 0x6c, 0xa8),
        }
    }
}

/// Title bar. Close is styled apart from the other buttons because Windows
/// does it and users expect it.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Caption {
    pub fg: Color,
    pub icon: Color,
    pub hover: Color,
    pub press: Color,
    pub close_hover: Color,
    pub close_press: Color,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalColors {
    /// What the shell considers "default background". Cells of this color are
    /// NOT painted, so the acrylic shows through. Changing it doesn't set a
    /// color — it picks which cells stay transparent.
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    /// Selection highlight. Must stay distinguishable whatever the content's
    /// colors are, so it's a role of its own rather than an ANSI slot.
    pub selection: Color,
    pub ansi: Ansi,
}

/// The 16 ANSI colors, read directly by the terminal layer.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ansi {
    pub black: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub white: Color,
    pub bright_black: Color,
    pub bright_red: Color,
    pub bright_green: Color,
    pub bright_yellow: Color,
    pub bright_blue: Color,
    pub bright_magenta: Color,
    pub bright_cyan: Color,
    pub bright_white: Color,
}

impl Ansi {
    /// Palette index → color. xterm order: 0-7 normal, 8-15 bright.
    pub fn get(&self, i: u8) -> Color {
        match i {
            0 => self.black,
            1 => self.red,
            2 => self.green,
            3 => self.yellow,
            4 => self.blue,
            5 => self.magenta,
            6 => self.cyan,
            7 => self.white,
            8 => self.bright_black,
            9 => self.bright_red,
            10 => self.bright_green,
            11 => self.bright_yellow,
            12 => self.bright_blue,
            13 => self.bright_magenta,
            14 => self.bright_cyan,
            _ => self.bright_white,
        }
    }
}

const fn c(r: u8, g: u8, b: u8) -> Color {
    Color::rgb(r, g, b)
}

/// Defaults copy the previously hardcoded values exactly. Adding a config
/// system must not look like a redesign to someone with no config file.
impl Default for Theme {
    fn default() -> Self {
        Self {
            fg: c(0xed, 0xeb, 0xe6),
            dim: c(0xad, 0xa8, 0xa3),
            accent: c(0x8c, 0xb8, 0xf2),
            warning: c(0xf2, 0xcc, 0x73),
            error: c(0xf2, 0x8c, 0x8c),
            divider: Color::rgb(0xff, 0xff, 0xff).with_alpha(0.10),
            overlay: Color::rgb(0x14, 0x14, 0x1c).with_alpha(0.95),
            caption: Caption::default(),
            terminal: TerminalColors::default(),
            git: GitColors::default(),
            syntax: SyntaxColors::default(),
        }
    }
}

impl Default for Caption {
    fn default() -> Self {
        Self {
            fg: c(0xb3, 0xb0, 0xad),
            icon: c(0xf2, 0xf0, 0xed),
            hover: Color::rgb(0xff, 0xff, 0xff).with_alpha(0.09),
            press: Color::rgb(0xff, 0xff, 0xff).with_alpha(0.05),
            close_hover: Color::rgb(0xe6, 0x40, 0x47).with_alpha(0.85),
            close_press: Color::rgb(0xbf, 0x33, 0x3a).with_alpha(0.90),
        }
    }
}

impl Default for TerminalColors {
    fn default() -> Self {
        Self {
            background: c(0x11, 0x11, 0x1b),
            foreground: c(0xcd, 0xd6, 0xf4),
            cursor: Color::rgb(0xcc, 0xd9, 0xf2).with_alpha(0.75),
            selection: c(0x3a, 0x4a, 0x6e),
            ansi: Ansi::default(),
        }
    }
}

impl Default for Ansi {
    fn default() -> Self {
        Self {
            black: c(0x1a, 0x18, 0x22),
            red: c(0xf3, 0x8b, 0xa8),
            green: c(0xa6, 0xe3, 0xa1),
            yellow: c(0xf9, 0xe2, 0xaf),
            blue: c(0x89, 0xb4, 0xfa),
            magenta: c(0xcb, 0xa6, 0xf7),
            cyan: c(0x94, 0xe2, 0xd5),
            white: c(0xcd, 0xd6, 0xf4),
            bright_black: c(0x58, 0x5b, 0x70),
            bright_red: c(0xf5, 0x9f, 0xba),
            bright_green: c(0xb8, 0xe9, 0xb4),
            bright_yellow: c(0xfa, 0xe8, 0xc0),
            bright_blue: c(0x9c, 0xc0, 0xfb),
            bright_magenta: c(0xd5, 0xb8, 0xf9),
            bright_cyan: c(0xa8, 0xe8, 0xdd),
            bright_white: c(0xe6, 0xed, 0xf7),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_index_follows_xterm_order() {
        let a = Ansi::default();
        assert_eq!(a.get(0), a.black);
        assert_eq!(a.get(7), a.white);
        assert_eq!(a.get(8), a.bright_black);
        assert_eq!(a.get(15), a.bright_white);
    }

    #[test]
    fn partial_theme_keeps_defaults() {
        // Changing one color must not require writing all thirty.
        let t: Theme = toml::from_str("accent = \"#ff0000\"").unwrap();
        assert_eq!(t.accent, Color::rgb(0xff, 0, 0));
        assert_eq!(t.fg, Theme::default().fg);
        assert_eq!(t.terminal.ansi.red, Ansi::default().red);
    }

    #[test]
    fn unknown_field_is_an_error() {
        // Ignoring it silently is the worst outcome: the user thinks the
        // setting applied, nothing happens, and there's nothing to debug.
        let e = toml::from_str::<Theme>("acccent = \"#ff0000\"").unwrap_err();
        assert!(e.to_string().contains("acccent"), "{e}");
    }

    #[test]
    fn round_trip_is_lossless() {
        let t = Theme::default();
        let s = toml::to_string(&t).unwrap();
        assert_eq!(toml::from_str::<Theme>(&s).unwrap(), t);
    }
}
