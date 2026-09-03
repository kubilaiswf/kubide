//! Reading a Windows `.cur` file on a platform that never learned the format.
//!
//! Windows loads these with one call; Linux has to read them. The format is
//! the icon container with a hotspot in each directory entry, and each frame
//! is either a PNG or a bottom-up DIB followed by a 1-bit AND mask. Only
//! 32-bit DIBs and PNGs are taken — every cursor pack made this century is
//! one of those — and anything else, `.ani` included, is "no", which the
//! caller turns into the drawn shape. A mis-decoded pointer is worse than a
//! substituted one.

use std::path::Path;

pub(crate) struct CursorFile {
    /// Straight-alpha RGBA, top-down.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub hot: (u16, u16),
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))
}

pub(crate) fn load(path: &Path) -> Option<CursorFile> {
    let bytes = std::fs::read(path).ok()?;
    // ICONDIR: reserved, type (1 icon, 2 cursor), count.
    if u16_at(&bytes, 0)? != 0 || !matches!(u16_at(&bytes, 2)?, 1 | 2) {
        return None;
    }
    let count = u16_at(&bytes, 4)? as usize;
    // The largest frame: a pack usually carries one size per file, but
    // when it carries several the big one scales down better than a small
    // one scales up.
    let mut best: Option<(u32, usize)> = None;
    for i in 0..count {
        let e = 6 + i * 16;
        let w = match *bytes.get(e)? {
            0 => 256,
            n => n as u32,
        };
        if best.is_none_or(|(bw, _)| w > bw) {
            best = Some((w, e));
        }
    }
    let (_, e) = best?;
    let hot = (u16_at(&bytes, e + 4)?, u16_at(&bytes, e + 6)?);
    let size = u32_at(&bytes, e + 8)? as usize;
    let offset = u32_at(&bytes, e + 12)? as usize;
    let frame = bytes.get(offset..offset.checked_add(size)?)?;

    let (rgba, width, height) = if frame.starts_with(b"\x89PNG") {
        png_frame(frame)?
    } else {
        dib_frame(frame)?
    };
    let hot = (hot.0.min(width.saturating_sub(1) as u16), hot.1.min(height.saturating_sub(1) as u16));
    Some(CursorFile { rgba, width, height, hot })
}

fn png_frame(frame: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(frame));
    // Whatever the file's depth and palette, hand back 8-bit channels.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((rgba, info.width, info.height))
}

/// A BITMAPINFOHEADER frame: the header says twice the height, because the
/// AND mask counts as rows of the same image.
fn dib_frame(frame: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    if u32_at(frame, 0)? != 40 {
        return None;
    }
    let width = u32_at(frame, 4)? as i32;
    let double = u32_at(frame, 8)? as i32;
    let bits = u16_at(frame, 14)?;
    if width <= 0 || double <= 0 || bits != 32 || u32_at(frame, 16)? != 0 {
        return None;
    }
    let (w, h) = (width as usize, (double / 2) as usize);
    let xor = frame.get(40..40 + w * h * 4)?;
    // Some 32-bit cursors leave the alpha bytes at zero and mean the AND
    // mask; if nothing in the image is opaque, the mask is the alpha.
    let alpha_used = xor.chunks(4).any(|p| p[3] != 0);
    let mask_stride = (w).div_ceil(32) * 4;
    let mask = frame.get(40 + w * h * 4..);

    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        // Bottom-up rows.
        let src_row = h - 1 - y;
        for x in 0..w {
            let s = (src_row * w + x) * 4;
            let d = (y * w + x) * 4;
            let (b, g, r, a) = (xor[s], xor[s + 1], xor[s + 2], xor[s + 3]);
            let a = if alpha_used {
                a
            } else {
                let bit = mask
                    .and_then(|m| m.get(src_row * mask_stride + x / 8))
                    .map(|byte| byte >> (7 - (x % 8)) & 1)
                    .unwrap_or(0);
                if bit == 0 { 255 } else { 0 }
            };
            rgba[d..d + 4].copy_from_slice(&[r, g, b, a]);
        }
    }
    Some((rgba, w as u32, h as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 32-bit cursor with a hotspot, built byte by byte: the reader
    /// has to flip the rows and swap the channels, and both mistakes are
    /// invisible until a real pointer comes out mirrored.
    #[test]
    fn a_dib_cursor_is_read_top_down_as_rgba() {
        let mut f = Vec::new();
        f.extend_from_slice(&0u16.to_le_bytes()); // reserved
        f.extend_from_slice(&2u16.to_le_bytes()); // cursor
        f.extend_from_slice(&1u16.to_le_bytes()); // one frame
        f.extend_from_slice(&[2, 2, 0, 0]); // 2x2, no palette
        f.extend_from_slice(&1u16.to_le_bytes()); // hotspot x
        f.extend_from_slice(&0u16.to_le_bytes()); // hotspot y
        let header_len = 40usize;
        let size = header_len + 2 * 2 * 4 + 2 * 4;
        f.extend_from_slice(&(size as u32).to_le_bytes());
        f.extend_from_slice(&22u32.to_le_bytes()); // offset
        // BITMAPINFOHEADER
        f.extend_from_slice(&40u32.to_le_bytes());
        f.extend_from_slice(&2i32.to_le_bytes());
        f.extend_from_slice(&4i32.to_le_bytes()); // twice the height
        f.extend_from_slice(&1u16.to_le_bytes());
        f.extend_from_slice(&32u16.to_le_bytes());
        f.extend_from_slice(&[0u8; 24]);
        // XOR rows, bottom-up, BGRA: bottom row red+green, top row blue+opaque white.
        f.extend_from_slice(&[0, 0, 255, 255, 0, 255, 0, 255]);
        f.extend_from_slice(&[255, 0, 0, 255, 255, 255, 255, 128]);
        // AND mask, two rows of 4 bytes.
        f.extend_from_slice(&[0u8; 8]);

        let dir = std::env::temp_dir().join("kubide-cur-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("two.cur");
        std::fs::write(&path, &f).unwrap();

        let c = load(&path).expect("a valid cursor");
        assert_eq!((c.width, c.height, c.hot), (2, 2, (1, 0)));
        assert_eq!(&c.rgba[0..4], &[0, 0, 255, 255], "top-left is blue");
        assert_eq!(&c.rgba[4..8], &[255, 255, 255, 128], "top-right keeps its alpha");
        assert_eq!(&c.rgba[8..12], &[255, 0, 0, 255], "bottom-left is red");
    }
}
