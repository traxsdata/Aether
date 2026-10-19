//! bmp.rs - Windows BMP, uncompressed 8/24/32-bit, bottom-up or top-down.

use super::{check_size, le16, le32, Image, ImageError};

pub fn decode(data: &[u8]) -> Result<Image, ImageError> {
    if data.len() < 26 || &data[..2] != b"BM" {
        return Err(ImageError::Corrupt("not a BMP"));
    }
    let offset = le32(&data[10..14]) as usize;
    let dib = le32(&data[14..18]) as usize;
    if dib < 40 || 14 + dib > data.len() {
        return Err(ImageError::Unsupported("BMP header"));
    }
    let w = le32(&data[18..22]) as i32;
    let h = le32(&data[22..26]) as i32;
    if w <= 0 || h == 0 {
        return Err(ImageError::Corrupt("BMP size"));
    }
    let width = w as u32;
    let height = h.unsigned_abs();
    check_size(width, height)?;
    let planes = le16(&data[26..28]);
    let bpp = le16(&data[28..30]);
    let compression = le32(&data[30..34]);
    if planes != 1 || compression != 0 {
        return Err(ImageError::Unsupported("compressed BMP"));
    }
    if bpp != 8 && bpp != 24 && bpp != 32 {
        return Err(ImageError::Unsupported("BMP bit depth"));
    }
    let top_down = h < 0;
    let palette = if bpp == 8 {
        let colors = le32(&data[46..50]) as usize;
        let n = if colors == 0 { 256 } else { colors.min(256) };
        let start = 14 + dib;
        let end = start + n * 4;
        if end > data.len() {
            return Err(ImageError::Corrupt("short BMP palette"));
        }
        Some(data[start..end].to_vec())
    } else {
        None
    };
    let row_bytes = (width as usize * bpp as usize + 31) / 32 * 4;
    let need = offset.checked_add(row_bytes * height as usize).ok_or(ImageError::TooLarge)?;
    if need > data.len() {
        return Err(ImageError::Corrupt("short BMP pixels"));
    }
    let mut img = Image::new(width, height)?;
    let mut any_alpha = false;
    for y in 0..height as usize {
        let sy = if top_down { y } else { height as usize - 1 - y };
        let row = &data[offset + sy * row_bytes..offset + (sy + 1) * row_bytes];
        for x in 0..width as usize {
            let px = match bpp {
                8 => {
                    let idx = row[x] as usize;
                    let p = palette.as_ref().ok_or(ImageError::Corrupt("no palette"))?;
                    if idx * 4 + 2 >= p.len() {
                        return Err(ImageError::Corrupt("BMP palette index"));
                    }
                    [p[idx * 4 + 2], p[idx * 4 + 1], p[idx * 4], 255]
                }
                24 => {
                    let p = &row[x * 3..x * 3 + 3];
                    [p[2], p[1], p[0], 255]
                }
                _ => {
                    let p = &row[x * 4..x * 4 + 4];
                    any_alpha |= p[3] != 0;
                    [p[2], p[1], p[0], p[3]]
                }
            };
            let o = (y * width as usize + x) * 4;
            img.pixels[o..o + 4].copy_from_slice(&px);
        }
    }
    // Many 32-bit BMPs leave alpha at zero; treat that as opaque.
    if bpp == 32 && !any_alpha {
        for px in img.pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_headers() {
        assert!(decode(b"BM").is_err());
        assert!(decode(b"not a bmp").is_err());
    }
}
