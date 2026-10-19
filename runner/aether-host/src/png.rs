//! png.rs - a tiny PNG writer: 8-bit RGB, zlib "stored" blocks (no
//! compression), so it needs no deflate. Big files, but trivially correct.

use std::vec::Vec;

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[n] = c;
        n += 1;
    }
    t
}
static CRC: [u32; 256] = crc_table();

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for p in parts {
        for &b in *p {
            c = CRC[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    c ^ 0xFFFF_FFFF
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// Encodes 0x00RRGGBB pixels (`pitch` per row) as a PNG file.
pub fn encode_xrgb(pixels: &[u32], width: u32, height: u32, pitch: u32) -> Vec<u8> {
    let (w, h, pitch) = (width as usize, height as usize, pitch as usize);
    // Raw scanlines: filter byte 0, then RGB.
    let mut raw = Vec::with_capacity(h * (1 + 3 * w));
    for y in 0..h {
        raw.push(0);
        for &p in &pixels[y * pitch..y * pitch + w] {
            raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    // zlib: header, stored blocks of up to 65535 bytes, Adler-32.
    let mut z = Vec::with_capacity(raw.len() + raw.len() / 65535 * 5 + 16);
    z.extend_from_slice(&[0x78, 0x01]);
    let mut blocks = raw.chunks(65535).peekable();
    if blocks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(b) = blocks.next() {
        let last = blocks.peek().is_none();
        let n = b.len() as u16;
        z.push(last as u8);
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in raw.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8; // bit depth
    ihdr[9] = 2; // colour type: RGB
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc_matches_reference() {
        // CRC-32 of "IEND" as found in every PNG file.
        assert_eq!(super::crc32(&[b"IEND"]), 0xAE42_6082);
    }
}
