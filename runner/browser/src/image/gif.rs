//! gif.rs - GIF87a/89a, first frame only. LZW, local/global palettes,
//! interlacing and transparency are supported; animation is not.

use super::{check_size, le16, Image, ImageError};

const MAX_CODES: usize = 4096;

pub fn decode(data: &[u8]) -> Result<Image, ImageError> {
    if data.len() < 13 || !(data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
        return Err(ImageError::Corrupt("not a GIF"));
    }
    let width = le16(&data[6..8]) as u32;
    let height = le16(&data[8..10]) as u32;
    check_size(width, height)?;
    let packed = data[10];
    let mut pos = 13;
    let global = if packed & 0x80 != 0 {
        let n = 2usize << (packed & 7);
        let end = pos + n * 3;
        if end > data.len() {
            return Err(ImageError::Corrupt("short global palette"));
        }
        let p = data[pos..end].to_vec();
        pos = end;
        Some(p)
    } else {
        None
    };
    let mut transparent: Option<u8> = None;
    loop {
        if pos >= data.len() {
            return Err(ImageError::Corrupt("no image"));
        }
        match data[pos] {
            0x21 => {
                if pos + 2 > data.len() {
                    return Err(ImageError::Corrupt("short extension"));
                }
                let label = data[pos + 1];
                pos += 2;
                let mut blocks: Vec<u8> = Vec::new();
                loop {
                    if pos >= data.len() {
                        return Err(ImageError::Corrupt("short extension block"));
                    }
                    let n = data[pos] as usize;
                    pos += 1;
                    if n == 0 {
                        break;
                    }
                    if pos + n > data.len() {
                        return Err(ImageError::Corrupt("short extension data"));
                    }
                    blocks.extend_from_slice(&data[pos..pos + n]);
                    pos += n;
                }
                if label == 0xF9 && blocks.len() >= 4 {
                    let flags = blocks[0];
                    if flags & 1 != 0 {
                        transparent = Some(blocks[3]);
                    }
                }
            }
            0x2C => {
                if pos + 10 > data.len() {
                    return Err(ImageError::Corrupt("short image descriptor"));
                }
                let left = le16(&data[pos + 1..pos + 3]) as usize;
                let top = le16(&data[pos + 3..pos + 5]) as usize;
                let iw = le16(&data[pos + 5..pos + 7]) as usize;
                let ih = le16(&data[pos + 7..pos + 9]) as usize;
                let ipacked = data[pos + 9];
                pos += 10;
                if iw == 0 || ih == 0 {
                    return Err(ImageError::Corrupt("empty image"));
                }
                if iw as u64 * ih as u64 > super::MAX_PIXELS {
                    return Err(ImageError::TooLarge);
                }
                if left.checked_add(iw).is_none_or(|x| x > width as usize)
                    || top.checked_add(ih).is_none_or(|y| y > height as usize)
                {
                    return Err(ImageError::Corrupt("GIF frame outside the screen"));
                }
                let palette = if ipacked & 0x80 != 0 {
                    let n = 2usize << (ipacked & 7);
                    let end = pos + n * 3;
                    if end > data.len() {
                        return Err(ImageError::Corrupt("short local palette"));
                    }
                    let p = data[pos..end].to_vec();
                    pos = end;
                    Some(p)
                } else {
                    global.clone()
                };
                let palette = palette.ok_or(ImageError::Corrupt("no palette"))?;
                let interlaced = ipacked & 0x40 != 0;
                if pos >= data.len() {
                    return Err(ImageError::Corrupt("missing LZW code size"));
                }
                let min = data[pos];
                pos += 1;
                if min > 11 {
                    return Err(ImageError::Corrupt("bad LZW code size"));
                }
                let mut lzw: Vec<u8> = Vec::new();
                loop {
                    if pos >= data.len() {
                        return Err(ImageError::Corrupt("short image data"));
                    }
                    let n = data[pos] as usize;
                    pos += 1;
                    if n == 0 {
                        break;
                    }
                    if pos + n > data.len() {
                        return Err(ImageError::Corrupt("short image data"));
                    }
                    lzw.extend_from_slice(&data[pos..pos + n]);
                    pos += n;
                }
                let indices = lzw_decode(&lzw, min as u16, iw * ih)?;
                let mut img = Image::new(width, height)?;
                let mut row = 0usize;
                let mut order = Vec::with_capacity(ih);
                if interlaced {
                    for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
                        let mut y = start;
                        while y < ih {
                            order.push(y);
                            y += step;
                        }
                    }
                } else {
                    order.extend(0..ih);
                }
                let mut k = 0usize;
                for &sy in &order {
                    row += 1;
                    let y = top + sy;
                    if y >= height as usize {
                        k += iw;
                        continue;
                    }
                    for x in 0..iw {
                        let cx = left + x;
                        let idx = indices[k] as usize;
                        k += 1;
                        if cx >= width as usize {
                            continue;
                        }
                        let mut px = [0u8, 0, 0, 255];
                        if idx * 3 + 2 < palette.len() {
                            px[0] = palette[idx * 3];
                            px[1] = palette[idx * 3 + 1];
                            px[2] = palette[idx * 3 + 2];
                        }
                        if transparent == Some(idx as u8) {
                            px[3] = 0;
                        }
                        let o = (y * width as usize + cx) * 4;
                        img.pixels[o..o + 4].copy_from_slice(&px);
                    }
                }
                let _ = row;
                return Ok(img);
            }
            0x3B => return Err(ImageError::Corrupt("GIF has no frame")),
            _ => return Err(ImageError::Corrupt("unknown GIF block")),
        }
    }
}

struct Bits<'a> {
    data: &'a [u8],
    bit: usize,
}

impl Bits<'_> {
    fn get(&mut self, n: u32) -> Option<u16> {
        if n == 0 {
            return Some(0);
        }
        if self.bit + n as usize > self.data.len() * 8 {
            return None;
        }
        let mut v = 0u16;
        for i in 0..n {
            let p = self.bit + i as usize;
            let b = (self.data[p / 8] >> (p % 8)) & 1;
            v |= (b as u16) << i;
        }
        self.bit += n as usize;
        Some(v)
    }
}

fn lzw_decode(data: &[u8], min: u16, max_pixels: usize) -> Result<Vec<u8>, ImageError> {
    if min < 2 || min > 11 {
        return Err(ImageError::Corrupt("bad LZW minimum"));
    }
    let clear = 1u16 << min;
    let end = clear + 1;
    let mut bits = Bits { data, bit: 0 };
    let mut dict: Vec<Vec<u8>> = (0..clear).map(|i| vec![i as u8]).collect();
    dict.resize(MAX_CODES, Vec::new());
    let mut code_size = min + 1;
    let mut next = end + 1;
    let mut prev: Option<Vec<u8>> = None;
    let mut out = Vec::with_capacity(max_pixels.min(1 << 20));
    loop {
        let Some(code) = bits.get(code_size as u32) else { break };
        if code == clear {
            code_size = min + 1;
            next = end + 1;
            prev = None;
            continue;
        }
        if code == end {
            break;
        }
        let entry = if (code as usize) < next as usize && !dict[code as usize].is_empty() {
            dict[code as usize].clone()
        } else if code == next {
            let Some(p) = &prev else { return Err(ImageError::Corrupt("LZW code")) };
            let mut e = p.clone();
            e.push(p[0]);
            e
        } else {
            return Err(ImageError::Corrupt("LZW code out of range"));
        };
        if out.len() + entry.len() > max_pixels {
            return Err(ImageError::TooLarge);
        }
        out.extend_from_slice(&entry);
        if let Some(p) = &prev {
            if next < MAX_CODES as u16 {
                let mut e = p.clone();
                e.push(entry[0]);
                dict[next as usize] = e;
                next += 1;
                if next == (1u16 << code_size) && code_size < 12 {
                    code_size += 1;
                }
            }
        }
        prev = Some(entry);
    }
    if out.len() < max_pixels {
        return Err(ImageError::Corrupt("short GIF image"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_headers() {
        assert!(decode(b"GIF89a").is_err());
        assert!(decode(b"not a gif").is_err());
    }
}
