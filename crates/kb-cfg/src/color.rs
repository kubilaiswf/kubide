//! Color type and `#rrggbb` parsing.
//!
//! A type of its own because themes are hand-written, so the error has to be
//! useful: say what's wrong, not just "invalid color".

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Normalized components, the form the drawing layer wants.
    pub fn f32s(self) -> (f32, f32, f32, f32) {
        (
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        )
    }

    /// Rounds rather than truncating: `with_alpha(0.5)` should be 128, not
    /// 127. Truncation also makes a color fail to survive a write-then-read
    /// round trip through the config file.
    pub fn with_alpha(self, a: f32) -> Self {
        Self {
            a: (a.clamp(0.0, 1.0) * 255.0).round() as u8,
            ..self
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        let h = s
            .strip_prefix('#')
            .ok_or_else(|| format!("color must start with '#', got '{s}' (e.g. #1e1e2e)"))?;
        let bytes = |i: usize| -> Result<u8, String> {
            u8::from_str_radix(&h[i..i + 2], 16)
                .map_err(|_| format!("'{s}' is not valid hexadecimal"))
        };
        match h.len() {
            6 => Ok(Self {
                r: bytes(0)?,
                g: bytes(2)?,
                b: bytes(4)?,
                a: 255,
            }),
            8 => Ok(Self {
                r: bytes(0)?,
                g: bytes(2)?,
                b: bytes(4)?,
                a: bytes(6)?,
            }),
            n => Err(format!(
                "color needs 6 or 8 hex digits, '{s}' has {n}"
            )),
        }
    }
}

impl std::fmt::Display for Color {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.a == 255 {
            write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            write!(f, "#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_digits() {
        assert_eq!(Color::parse("#1e1e2e").unwrap(), Color::rgb(0x1e, 0x1e, 0x2e));
    }

    #[test]
    fn eight_digits_carry_alpha() {
        let c = Color::parse("#1e1e2e80").unwrap();
        assert_eq!(c.a, 0x80);
    }

    #[test]
    fn errors_say_what_is_wrong() {
        assert!(Color::parse("1e1e2e").unwrap_err().contains("start with '#'"));
        assert!(Color::parse("#abc").unwrap_err().contains("has 3"));
        assert!(Color::parse("#zzzzzz").unwrap_err().contains("hexadecimal"));
    }

    #[test]
    fn round_trip() {
        for s in ["#1e1e2e", "#1e1e2e80"] {
            assert_eq!(Color::parse(s).unwrap().to_string(), s);
        }
    }
}
