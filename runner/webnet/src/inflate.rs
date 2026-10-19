//! DEFLATE decompression (RFC 1951) with gzip (RFC 1952) and zlib
//! (RFC 1950) wrappers, for HTTP Content-Encoding. Output is capped so a
//! small "zip bomb" can't use unbounded memory; checksums are verified.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateError {
    Corrupt(&'static str),
    TooLarge,
}

impl std::fmt::Display for InflateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InflateError::Corrupt(w) => write!(f, "corrupt compressed data ({w})"),
            InflateError::TooLarge => write!(f, "the decompressed page is too large"),
        }
    }
}

type R<T> = Result<T, InflateError>;

fn corrupt<T>(w: &'static str) -> R<T> {
    Err(InflateError::Corrupt(w))
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    bit: u32,
    nbits: u32,
}

impl<'a> Bits<'a> {
    fn need(&mut self, n: u32) -> R<u32> {
        while self.nbits < n {
            if self.pos >= self.d.len() {
                return corrupt("unexpected end");
            }
            self.bit |= (self.d[self.pos] as u32) << self.nbits;
            self.pos += 1;
            self.nbits += 8;
        }
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }
    fn align(&mut self) {
        self.bit = 0;
        self.nbits = 0;
    }
}

/// A canonical Huffman code: counts per length and symbols in code order.
struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huff {
    /// Builds a code from code lengths. Incomplete codes are allowed only
    /// where RFC 1951 allows them (a single distance code).
    fn new(lengths: &[u8]) -> R<Huff> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        let mut left: i32 = 1;
        for len in 1..16 {
            left <<= 1;
            left -= count[len] as i32;
            if left < 0 {
                return corrupt("over-subscribed code");
            }
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        count[0] = 0;
        Ok(Huff { count, symbol })
    }

    fn decode(&self, b: &mut Bits) -> R<u16> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            code |= b.need(1)? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return Ok(self.symbol[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        corrupt("bad code")
    }
}

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193,
    12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn codes(b: &mut Bits, out: &mut Vec<u8>, max: usize, lit: &Huff, dist: &Huff) -> R<()> {
    loop {
        let sym = lit.decode(b)?;
        if sym < 256 {
            if out.len() >= max {
                return Err(InflateError::TooLarge);
            }
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let s = (sym - 257) as usize;
            if s >= 29 {
                return corrupt("bad length code");
            }
            let len = LEN_BASE[s] as usize + b.need(LEN_EXTRA[s] as u32)? as usize;
            let ds = dist.decode(b)? as usize;
            if ds >= 30 {
                return corrupt("bad distance code");
            }
            let d = DIST_BASE[ds] as usize + b.need(DIST_EXTRA[ds] as u32)? as usize;
            if d > out.len() {
                return corrupt("distance too far back");
            }
            if out.len() + len > max {
                return Err(InflateError::TooLarge);
            }
            let start = out.len() - d;
            for i in 0..len {
                let c = out[start + i];
                out.push(c);
            }
        }
    }
}

/// Raw DEFLATE. Returns the output and how many input bytes were used.
pub fn inflate(data: &[u8], max: usize) -> R<(Vec<u8>, usize)> {
    let mut b = Bits { d: data, pos: 0, bit: 0, nbits: 0 };
    let mut out = Vec::new();
    loop {
        let last = b.need(1)?;
        let kind = b.need(2)?;
        match kind {
            0 => {
                b.align();
                if b.pos + 4 > data.len() {
                    return corrupt("unexpected end");
                }
                let len = u16::from_le_bytes([data[b.pos], data[b.pos + 1]]);
                let nlen = u16::from_le_bytes([data[b.pos + 2], data[b.pos + 3]]);
                if len != !nlen {
                    return corrupt("stored block length");
                }
                b.pos += 4;
                let len = len as usize;
                if b.pos + len > data.len() {
                    return corrupt("unexpected end");
                }
                if out.len() + len > max {
                    return Err(InflateError::TooLarge);
                }
                out.extend_from_slice(&data[b.pos..b.pos + len]);
                b.pos += len;
            }
            1 => {
                let mut l = [0u8; 288];
                l[..144].fill(8);
                l[144..256].fill(9);
                l[256..280].fill(7);
                l[280..].fill(8);
                let lit = Huff::new(&l)?;
                let dist = Huff::new(&[5u8; 30])?;
                codes(&mut b, &mut out, max, &lit, &dist)?;
            }
            2 => {
                let nlen = b.need(5)? as usize + 257;
                let ndist = b.need(5)? as usize + 1;
                let ncode = b.need(4)? as usize + 4;
                if nlen > 286 || ndist > 30 {
                    return corrupt("bad counts");
                }
                const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &o in ORDER.iter().take(ncode) {
                    cl[o] = b.need(3)? as u8;
                }
                let clh = Huff::new(&cl)?;
                let mut lengths = vec![0u8; nlen + ndist];
                let mut i = 0;
                while i < nlen + ndist {
                    let sym = clh.decode(&mut b)?;
                    if sym < 16 {
                        lengths[i] = sym as u8;
                        i += 1;
                    } else {
                        let (val, rep) = match sym {
                            16 => {
                                if i == 0 {
                                    return corrupt("repeat with no previous length");
                                }
                                (lengths[i - 1], 3 + b.need(2)? as usize)
                            }
                            17 => (0, 3 + b.need(3)? as usize),
                            _ => (0, 11 + b.need(7)? as usize),
                        };
                        if i + rep > nlen + ndist {
                            return corrupt("too many lengths");
                        }
                        lengths[i..i + rep].fill(val);
                        i += rep;
                    }
                }
                if lengths[256] == 0 {
                    return corrupt("no end-of-block code");
                }
                let lit = Huff::new(&lengths[..nlen])?;
                let dist = Huff::new(&lengths[nlen..])?;
                codes(&mut b, &mut out, max, &lit, &dist)?;
            }
            _ => return corrupt("bad block type"),
        }
        if last == 1 {
            break;
        }
    }
    Ok((out, b.pos))
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &x in data {
        c ^= x as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(c & 1));
        }
    }
    !c
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// A gzip stream (one member; trailing members are rejected).
pub fn gunzip(data: &[u8], max: usize) -> R<Vec<u8>> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return corrupt("not gzip");
    }
    let flags = data[3];
    if flags & 0xe0 != 0 {
        return corrupt("reserved gzip flags");
    }
    let mut p = 10;
    if flags & 4 != 0 {
        if p + 2 > data.len() {
            return corrupt("gzip header");
        }
        p += 2 + u16::from_le_bytes([data[p], data[p + 1]]) as usize;
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            while p < data.len() && data[p] != 0 {
                p += 1;
            }
            p += 1;
        }
    }
    if flags & 2 != 0 {
        p += 2;
    }
    if p > data.len() {
        return corrupt("gzip header");
    }
    let (out, used) = inflate(&data[p..], max)?;
    let t = p + used;
    if data.len() < t + 8 {
        return corrupt("gzip trailer");
    }
    let crc = u32::from_le_bytes([data[t], data[t + 1], data[t + 2], data[t + 3]]);
    let size = u32::from_le_bytes([data[t + 4], data[t + 5], data[t + 6], data[t + 7]]);
    if crc != crc32(&out) || size != out.len() as u32 {
        return corrupt("gzip checksum");
    }
    if data[t + 8..].iter().any(|&b| b != 0) {
        return corrupt("data after the gzip stream");
    }
    Ok(out)
}

/// A zlib stream (what HTTP calls "deflate").
pub fn unzlib(data: &[u8], max: usize) -> R<Vec<u8>> {
    if data.len() < 6 || data[0] & 0x0f != 8 || !((data[0] as u16) << 8 | data[1] as u16).is_multiple_of(31) || data[1] & 0x20 != 0 {
        return corrupt("not zlib");
    }
    let (out, used) = inflate(&data[2..], max)?;
    let t = 2 + used;
    if data.len() < t + 4 {
        return corrupt("zlib trailer");
    }
    let a = u32::from_be_bytes([data[t], data[t + 1], data[t + 2], data[t + 3]]);
    if a != adler32(&out) {
        return corrupt("zlib checksum");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc() {
        assert_eq!(crc32(b"123456789"), 0xcbf43926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e60398);
    }

    #[test]
    fn stored_and_fixed() {
        // A stored block "hi".
        let (o, _) = inflate(&[0x01, 0x02, 0x00, 0xfd, 0xff, b'h', b'i'], 100).unwrap();
        assert_eq!(o, b"hi");
        // Fixed Huffman "a" (from zlib: 4b 04 00).
        let (o, _) = inflate(&[0x4b, 0x04, 0x00], 100).unwrap();
        assert_eq!(o, b"a");
        assert!(inflate(&[0x07], 100).is_err());
    }

    include!("inflate_vectors.rs");

    fn sample_text() -> Vec<u8> {
        let w: Vec<&str> = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega".split(' ').collect();
        let mut x: u64 = 1;
        let mut out = Vec::new();
        for _ in 0..3000 {
            x = (x * 1103515245 + 12345) % (1 << 31);
            out.push(w[(x % w.len() as u64) as usize]);
        }
        out.join(" ").into_bytes()
    }

    #[test]
    fn python_vectors() {
        // "hello hello hello hello\n", gzip level 9.
        let gz = [
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40,
            0x27, 0xb9, 0x00, 0x00, 0x88, 0x59, 0x0b, 0x18, 0x00, 0x00, 0x00,
        ];
        assert_eq!(gunzip(&gz, 1000).unwrap(), b"hello hello hello hello\n");
        assert_eq!(gunzip(&gz, 10), Err(InflateError::TooLarge));
        let mut bad = gz;
        bad[24] ^= 1;
        assert!(gunzip(&bad, 1000).is_err());

        let t = sample_text();
        assert_eq!(t.len(), SAMPLE_LEN);
        assert_eq!(crc32(&t), SAMPLE_CRC);
        assert_eq!(unzlib(SAMPLE_ZLIB, 1 << 20).unwrap(), t);
        assert_eq!(gunzip(SAMPLE_GZIP, 1 << 20).unwrap(), t);
        assert_eq!(unzlib(SAMPLE_ZLIB, 1000), Err(InflateError::TooLarge));
        for i in [3, 50, SAMPLE_ZLIB.len() - 3] {
            let mut bad = SAMPLE_ZLIB.to_vec();
            bad[i] ^= 0x10;
            assert!(unzlib(&bad, 1 << 20).is_err());
        }
    }
}
