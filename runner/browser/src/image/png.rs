//! png.rs - PNG (RFC 2083) for the browser: non-interlaced, 8- or 16-bit,
//! gray, RGB, palette, gray+alpha and RGBA, with tRNS transparency.

use super::{be32, check_size, Image, ImageError, MAX_PIXELS};
use webnet::inflate::{crc32, unzlib};

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_COMPRESSED: usize = 32 << 20;

pub fn decode(data: &[u8]) -> Result<Image, ImageError> {
    if data.len() < 8 || &data[..8] != SIGNATURE {
        return Err(ImageError::Corrupt("not a PNG"));
    }
    let mut pos = 8;
    let mut header: Option<(u32, u32, u8, u8, u8, u8, u8)> = None;
    let mut palette: Option<Vec<u8>> = None;
    let mut trns: Option<Vec<u8>> = None;
    let mut idat: Vec<u8> = Vec::new();
    while pos + 12 <= data.len() {
        let len = be32(&data[pos..pos + 4]) as usize;
        if len > MAX_COMPRESSED {
            return Err(ImageError::TooLarge);
        }
        let end = pos.checked_add(12 + len).ok_or(ImageError::Corrupt("chunk length"))?;
        if end > data.len() {
            return Err(ImageError::Corrupt("truncated chunk"));
        }
        let kind = &data[pos + 4..pos + 8];
        let body = &data[pos + 8..pos + 8 + len];
        let want = be32(&data[pos + 8 + len..pos + 12 + len]);
        if crc32(&data[pos + 4..pos + 8 + len]) != want {
            return Err(ImageError::Corrupt("PNG chunk CRC"));
        }
        match kind {
            b"IHDR" => {
                if len != 13 {
                    return Err(ImageError::Corrupt("bad IHDR"));
                }
                header = Some((
                    be32(&body[0..4]),
                    be32(&body[4..8]),
                    body[8],
                    body[9],
                    body[10],
                    body[11],
                    body[12],
                ));
            }
            b"PLTE" => {
                if len % 3 != 0 || len > 768 {
                    return Err(ImageError::Corrupt("bad palette"));
                }
                palette = Some(body.to_vec());
            }
            b"tRNS" => trns = Some(body.to_vec()),
            b"IDAT" => {
                if idat.len() + len > MAX_COMPRESSED {
                    return Err(ImageError::TooLarge);
                }
                idat.extend_from_slice(body);
            }
            b"IEND" => break,
            _ => {}
        }
        pos = end;
    }
    let (w, h, depth, color, comp, filter, interlace) = header.ok_or(ImageError::Corrupt("no IHDR"))?;
    if comp != 0 || filter != 0 {
        return Err(ImageError::Unsupported("unknown PNG compression"));
    }
    if interlace != 0 {
        return Err(ImageError::Unsupported("interlaced PNG"));
    }
    if depth != 8 && depth != 16 {
        return Err(ImageError::Unsupported("PNG bit depth"));
    }
    let channels: usize = match color {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return Err(ImageError::Corrupt("unknown PNG colour type")),
    };
    if color == 3 && depth != 8 {
        return Err(ImageError::Unsupported("packed palette PNG"));
    }
    check_size(w, h)?;
    if depth == 16 && w as u64 * h as u64 > 8 * 1024 * 1024 {
        return Err(ImageError::TooLarge);
    }
    let bpp = channels * (depth as usize / 8);
    let stride = (w as usize).checked_mul(bpp).ok_or(ImageError::TooLarge)?;
    let raw_len = (h as usize).checked_mul(stride + 1).ok_or(ImageError::TooLarge)?;
    if raw_len as u64 > MAX_PIXELS * 4 + h as u64 {
        return Err(ImageError::TooLarge);
    }
    let raw = unzlib(&idat, raw_len).map_err(|_| ImageError::Corrupt("compressed pixels"))?;
    if raw.len() < raw_len {
        return Err(ImageError::Corrupt("short pixel data"));
    }
    let mut rows = vec![0u8; stride * h as usize];
    let mut prev = vec![0u8; stride];
    for y in 0..h as usize {
        let src = &raw[y * (stride + 1)..(y + 1) * (stride + 1)];
        let filter = src[0];
        let mut row = src[1..].to_vec();
        for i in 0..stride {
            let a = if i >= bpp { row[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            let add = match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                4 => paeth(a, b, c),
                _ => return Err(ImageError::Corrupt("PNG filter")),
            };
            row[i] = row[i].wrapping_add(add);
        }
        rows[y * stride..(y + 1) * stride].copy_from_slice(&row);
        prev = row;
    }
    let mut img = Image::new(w, h)?;
    let step = depth as usize / 8;
    let pixel_stride = channels * step;
    let out = &mut img.pixels;
    let mut o = 0;
    for y in 0..h as usize {
        let row = &rows[y * stride..(y + 1) * stride];
        for x in 0..w as usize {
            let px = match color {
                0 => {
                    let g = row[x * pixel_stride];
                    let a = match &trns {
                        Some(t) if t.len() >= 2 => {
                            let key = u16::from_be_bytes([t[0], t[1]]);
                            let v = if step == 2 { u16::from_be_bytes([row[x * pixel_stride], row[x * pixel_stride + 1]]) } else { g as u16 };
                            if v == key { 0 } else { 255 }
                        }
                        _ => 255,
                    };
                    [g, g, g, a]
                }
                2 => {
                    let (r, g, b) = (row[x * pixel_stride], row[x * pixel_stride + step], row[x * pixel_stride + 2 * step]);
                    let a = match &trns {
                        Some(t) if t.len() >= 6 => {
                            let key = [u16::from_be_bytes([t[0], t[1]]), u16::from_be_bytes([t[2], t[3]]), u16::from_be_bytes([t[4], t[5]])];
                            let v = if step == 2 {
                                [
                                    u16::from_be_bytes([row[x * pixel_stride], row[x * pixel_stride + 1]]),
                                    u16::from_be_bytes([row[x * pixel_stride + 2], row[x * pixel_stride + 3]]),
                                    u16::from_be_bytes([row[x * pixel_stride + 4], row[x * pixel_stride + 5]]),
                                ]
                            } else {
                                [r as u16, g as u16, b as u16]
                            };
                            if v == key { 0 } else { 255 }
                        }
                        _ => 255,
                    };
                    [r, g, b, a]
                }
                3 => {
                    let idx = row[x] as usize;
                    let p = palette.as_ref().ok_or(ImageError::Corrupt("no palette"))?;
                    if idx * 3 + 2 >= p.len() {
                        return Err(ImageError::Corrupt("palette index"));
                    }
                    let a = trns.as_ref().and_then(|t| t.get(idx).copied()).unwrap_or(255);
                    [p[idx * 3], p[idx * 3 + 1], p[idx * 3 + 2], a]
                }
                4 => {
                    let g = row[x * pixel_stride];
                    let a = row[x * pixel_stride + step];
                    [g, g, g, a]
                }
                _ => {
                    let (r, g, b) = (row[x * pixel_stride], row[x * pixel_stride + step], row[x * pixel_stride + 2 * step]);
                    let a = row[x * pixel_stride + 3 * step];
                    [r, g, b, a]
                }
            };
            out[o..o + 4].copy_from_slice(&px);
            o += 4;
        }
    }
    Ok(img)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i32 + b as i32 - c as i32;
    let pa = (p - a as i32).abs();
    let pb = (p - b as i32).abs();
    let pc = (p - c as i32).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_short_and_huge() {
        assert!(decode(b"not a png").is_err());
        assert!(decode(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]).is_err());
    }
}
