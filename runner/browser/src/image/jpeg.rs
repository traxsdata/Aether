//! jpeg.rs - JPEG (baseline and progressive): Huffman, 8x8 IDCT, YCbCr and
//! grayscale, 4:4:4 / 4:2:2 / 4:2:0. Written from the JPEG specification;
//! no third-party decoder.

use std::sync::OnceLock;

use super::{be16, check_size, Image, ImageError};

/// Zigzag position -> natural (row-major) coefficient index.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

#[derive(Clone)]
struct Huff {
    counts: [u8; 16],
    symbols: Vec<u8>,
}

impl Huff {
    fn decode(&self, bits: &mut Bits) -> Result<u8, ImageError> {
        let mut code = 0u32;
        let mut first = 0u32;
        let mut index = 0usize;
        for len in 0..16 {
            code = (code << 1) | bits.bit()?;
            let count = self.counts[len] as u32;
            if code < first + count {
                let i = index + (code - first) as usize;
                return self.symbols.get(i).copied().ok_or(ImageError::Corrupt("Huffman symbol"));
            }
            index += count as usize;
            first = (first + count) << 1;
        }
        Err(ImageError::Corrupt("Huffman code"))
    }
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u32,
    n: u8,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, pos: 0, buf: 0, n: 0 }
    }

    fn bit(&mut self) -> Result<u32, ImageError> {
        if self.n == 0 {
            if self.pos >= self.data.len() {
                return Err(ImageError::Corrupt("JPEG data ends early"));
            }
            self.buf = self.data[self.pos] as u32;
            self.pos += 1;
            self.n = 8;
        }
        self.n -= 1;
        Ok((self.buf >> self.n) & 1)
    }

    fn bits(&mut self, n: u8) -> Result<u32, ImageError> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }

    fn align(&mut self) {
        self.buf = 0;
        self.n = 0;
    }
}

#[derive(Clone)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    blocks_x: usize,
    blocks_y: usize,
    coefs: Vec<i16>,
    plane: Vec<u8>,
}

struct Frame {
    width: usize,
    height: usize,
    comps: Vec<Component>,
    max_h: usize,
    max_v: usize,
}

struct ScanComp {
    frame: usize,
    dc: usize,
    ac: usize,
}

struct Scan {
    comps: Vec<ScanComp>,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
    sequential: bool,
}

pub fn decode(data: &[u8]) -> Result<Image, ImageError> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(ImageError::Corrupt("not a JPEG"));
    }
    let mut qt: [Option<[u16; 64]>; 4] = [None, None, None, None];
    let mut dc: [Option<Huff>; 4] = [None, None, None, None];
    let mut ac: [Option<Huff>; 4] = [None, None, None, None];
    let mut frame: Option<Frame> = None;
    let mut restart = 0usize;
    let mut adobe: Option<u8> = None;
    let mut saw_scan = false;
    let mut pos = 2;
    while pos < data.len() {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        while pos < data.len() && data[pos] == 0xFF {
            pos += 1;
        }
        if pos >= data.len() {
            break;
        }
        let marker = data[pos];
        pos += 1;
        if marker == 0xD9 {
            break;
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if pos + 2 > data.len() {
            return Err(ImageError::Corrupt("short JPEG marker"));
        }
        let len = be16(&data[pos..pos + 2]) as usize;
        if len < 2 || pos + len > data.len() {
            return Err(ImageError::Corrupt("bad JPEG marker length"));
        }
        let seg = &data[pos + 2..pos + len];
        pos += len;
        match marker {
            0xDB => parse_dqt(seg, &mut qt)?,
            0xC0 | 0xC1 | 0xC2 => {
                if frame.is_some() {
                    return Err(ImageError::Corrupt("two JPEG frames"));
                }
                frame = Some(parse_sof(seg)?);
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err(ImageError::Unsupported("JPEG coding mode"))
            }
            0xC4 => parse_dht(seg, &mut dc, &mut ac)?,
            0xDD => {
                if seg.len() < 2 {
                    return Err(ImageError::Corrupt("bad restart interval"));
                }
                restart = be16(&seg[..2]) as usize;
            }
            0xEE => {
                if seg.len() >= 12 && seg.starts_with(b"Adobe") {
                    adobe = Some(seg[11]);
                }
            }
            0xDA => {
                let f = frame.as_mut().ok_or(ImageError::Corrupt("scan before frame"))?;
                let scan = parse_sos(seg, f)?;
                let end = entropy_end(data, pos);
                decode_scan(f, &scan, &data[pos..end], restart, &qt, &dc, &ac)?;
                saw_scan = true;
                pos = end;
            }
            _ => {}
        }
    }
    let f = frame.ok_or(ImageError::Corrupt("no JPEG frame"))?;
    if !saw_scan {
        return Err(ImageError::Corrupt("no JPEG scan"));
    }
    finish(f, &qt, adobe)
}

fn parse_dqt(seg: &[u8], qt: &mut [Option<[u16; 64]>; 4]) -> Result<(), ImageError> {
    let mut i = 0;
    while i < seg.len() {
        let pq_tq = seg[i];
        i += 1;
        let pq = pq_tq >> 4;
        let tq = (pq_tq & 15) as usize;
        if tq > 3 || pq > 1 {
            return Err(ImageError::Corrupt("bad quantisation table"));
        }
        let n = if pq == 0 { 64 } else { 128 };
        if i + n > seg.len() {
            return Err(ImageError::Corrupt("short quantisation table"));
        }
        let mut q = [0u16; 64];
        for k in 0..64 {
            q[ZIGZAG[k]] = if pq == 0 {
                let v = seg[i] as u16;
                i += 1;
                v
            } else {
                let v = be16(&seg[i..i + 2]);
                i += 2;
                v
            };
        }
        qt[tq] = Some(q);
    }
    Ok(())
}

fn parse_dht(seg: &[u8], dc: &mut [Option<Huff>; 4], ac: &mut [Option<Huff>; 4]) -> Result<(), ImageError> {
    let mut i = 0;
    while i < seg.len() {
        let tc_th = seg[i];
        i += 1;
        let tc = tc_th >> 4;
        let th = (tc_th & 15) as usize;
        if th > 3 || i + 16 > seg.len() {
            return Err(ImageError::Corrupt("bad Huffman table"));
        }
        let mut counts = [0u8; 16];
        counts.copy_from_slice(&seg[i..i + 16]);
        i += 16;
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if total == 0 || i + total > seg.len() {
            return Err(ImageError::Corrupt("short Huffman table"));
        }
        let h = Huff { counts, symbols: seg[i..i + total].to_vec() };
        i += total;
        match tc {
            0 => dc[th] = Some(h),
            1 => ac[th] = Some(h),
            _ => return Err(ImageError::Corrupt("Huffman table class")),
        }
    }
    Ok(())
}

fn parse_sof(seg: &[u8]) -> Result<Frame, ImageError> {
    if seg.len() < 6 {
        return Err(ImageError::Corrupt("short SOF"));
    }
    if seg[0] != 8 {
        return Err(ImageError::Unsupported("JPEG sample precision"));
    }
    let height = be16(&seg[1..3]) as usize;
    let width = be16(&seg[3..5]) as usize;
    check_size(width as u32, height as u32)?;
    let n = seg[5] as usize;
    if n == 0 || n > 4 || seg.len() < 6 + 3 * n {
        return Err(ImageError::Corrupt("bad SOF components"));
    }
    let mut comps = Vec::with_capacity(n);
    let (mut max_h, mut max_v) = (1, 1);
    for i in 0..n {
        let id = seg[6 + i * 3];
        let hv = seg[7 + i * 3];
        let tq = seg[8 + i * 3] as usize;
        let h = (hv >> 4) as usize;
        let v = (hv & 15) as usize;
        if h == 0 || v == 0 || h > 4 || v > 4 || tq > 3 {
            return Err(ImageError::Corrupt("bad JPEG component"));
        }
        max_h = max_h.max(h);
        max_v = max_v.max(v);
        comps.push(Component { id, h, v, tq, blocks_x: 0, blocks_y: 0, coefs: Vec::new(), plane: Vec::new() });
    }
    Ok(Frame { width, height, comps, max_h, max_v })
}

fn parse_sos(seg: &[u8], f: &Frame) -> Result<Scan, ImageError> {
    if seg.is_empty() {
        return Err(ImageError::Corrupt("short SOS"));
    }
    let n = seg[0] as usize;
    if n == 0 || n > 4 || seg.len() < 1 + 2 * n + 3 {
        return Err(ImageError::Corrupt("bad SOS components"));
    }
    let mut comps = Vec::with_capacity(n);
    for i in 0..n {
        let id = seg[1 + i * 2];
        let tables = seg[2 + i * 2];
        let frame = f.comps.iter().position(|c| c.id == id).ok_or(ImageError::Corrupt("scan component"))?;
        comps.push(ScanComp { frame, dc: (tables >> 4) as usize, ac: (tables & 15) as usize });
    }
    let ss = seg[1 + 2 * n] as usize;
    let se = seg[2 + 2 * n] as usize;
    let ahal = seg[3 + 2 * n];
    let ah = (ahal >> 4) as u32;
    let al = (ahal & 15) as u32;
    if ss > 63 || se > 63 || ss > se || al > 13 || ah > 13 {
        return Err(ImageError::Corrupt("bad progressive scan"));
    }
    let sequential = ss == 0 && se == 63 && ah == 0 && al == 0;
    if !sequential {
        if ss == 0 {
            if se != 0 {
                return Err(ImageError::Corrupt("DC scan has AC range"));
            }
        } else if ss > se {
            return Err(ImageError::Corrupt("AC scan range"));
        }
    }
    Ok(Scan { comps, ss, se, ah, al, sequential })
}

/// Where the entropy-coded data ends: the next real marker.
fn entropy_end(data: &[u8], start: usize) -> usize {
    let mut i = start;
    while i + 1 < data.len() {
        if data[i] == 0xFF {
            let m = data[i + 1];
            if m == 0x00 || (0xD0..=0xD7).contains(&m) {
                i += 2;
                continue;
            }
            return i;
        }
        i += 1;
    }
    data.len()
}

fn decode_scan(
    f: &mut Frame,
    scan: &Scan,
    entropy: &[u8],
    restart: usize,
    qt: &[Option<[u16; 64]>; 4],
    dc: &[Option<Huff>; 4],
    ac: &[Option<Huff>; 4],
) -> Result<(), ImageError> {
    let _ = qt;
    // Remove byte stuffing and note where each restart marker starts.
    let mut bytes = Vec::with_capacity(entropy.len());
    let mut restarts = Vec::new();
    let mut i = 0;
    while i < entropy.len() {
        if entropy[i] == 0xFF {
            if i + 1 >= entropy.len() {
                break;
            }
            let m = entropy[i + 1];
            if m == 0x00 {
                bytes.push(0xFF);
                i += 2;
                continue;
            }
            if (0xD0..=0xD7).contains(&m) {
                restarts.push(bytes.len());
                i += 2;
                continue;
            }
            break;
        }
        bytes.push(entropy[i]);
        i += 1;
    }
    let mcus_x = (f.width + 8 * f.max_h - 1) / (8 * f.max_h);
    let mcus_y = (f.height + 8 * f.max_v - 1) / (8 * f.max_v);
    if f.comps.iter().any(|c| c.coefs.is_empty()) {
        for c in &mut f.comps {
            c.blocks_x = mcus_x * c.h;
            c.blocks_y = mcus_y * c.v;
            c.coefs = vec![0i16; c.blocks_x * c.blocks_y * 64];
        }
    }
    let mut pred = vec![0i32; f.comps.len()];
    let mut eobrun = 0u32;
    let mut bits = Bits::new(&bytes);
    let mut restart_i = 0usize;
    if scan.comps.len() == 1 {
        // A non-interleaved scan is one block per MCU, in the component's
        // raster order (not the interleaved 2x2 order of a full image MCU).
        let sc = &scan.comps[0];
        let (blocks_x, blocks_y) = {
            let c = &f.comps[sc.frame];
            (c.blocks_x, c.blocks_y)
        };
        for idx in 0..blocks_x * blocks_y {
            if restart > 0 && idx > 0 && idx % restart == 0 {
                do_restart(&mut bits, &restarts, &mut restart_i, &mut pred, &mut eobrun)?;
            }
            decode_scan_block(f, scan, sc, idx, &mut bits, dc, ac, &mut pred[sc.frame], &mut eobrun)?;
        }
    } else {
        let mut mcu = 0usize;
        for my in 0..mcus_y {
            for mx in 0..mcus_x {
                if restart > 0 && mcu > 0 && mcu % restart == 0 {
                    do_restart(&mut bits, &restarts, &mut restart_i, &mut pred, &mut eobrun)?;
                }
                for sc in &scan.comps {
                    let (h, v, blocks_x) = {
                        let c = &f.comps[sc.frame];
                        (c.h, c.v, c.blocks_x)
                    };
                    for by in 0..v {
                        for bx in 0..h {
                            let idx = (my * v + by) * blocks_x + (mx * h + bx);
                            decode_scan_block(f, scan, sc, idx, &mut bits, dc, ac, &mut pred[sc.frame], &mut eobrun)?;
                        }
                    }
                }
                mcu += 1;
            }
        }
    }
    Ok(())
}

fn do_restart(
    bits: &mut Bits,
    restarts: &[usize],
    restart_i: &mut usize,
    pred: &mut [i32],
    eobrun: &mut u32,
) -> Result<(), ImageError> {
    if *restart_i >= restarts.len() {
        return Err(ImageError::Corrupt("missing restart marker"));
    }
    bits.align();
    bits.pos = restarts[*restart_i];
    bits.buf = 0;
    bits.n = 0;
    *restart_i += 1;
    for p in pred {
        *p = 0;
    }
    *eobrun = 0;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_scan_block(
    f: &mut Frame,
    scan: &Scan,
    sc: &ScanComp,
    idx: usize,
    bits: &mut Bits,
    dc: &[Option<Huff>; 4],
    ac: &[Option<Huff>; 4],
    pred: &mut i32,
    eobrun: &mut u32,
) -> Result<(), ImageError> {
    let dct = if scan.ss == 0 || scan.sequential {
        dc.get(sc.dc).and_then(|t| t.as_ref()).ok_or(ImageError::Corrupt("DC table"))?
    } else {
        &EMPTY_HUFF
    };
    let act = if scan.se > 0 || scan.sequential {
        ac.get(sc.ac).and_then(|t| t.as_ref()).ok_or(ImageError::Corrupt("AC table"))?
    } else {
        &EMPTY_HUFF
    };
    let c = &mut f.comps[sc.frame];
    let coefs = &mut c.coefs[idx * 64..idx * 64 + 64];
    if scan.sequential {
        decode_block_seq(bits, dct, act, pred, coefs)
    } else if scan.ss == 0 {
        decode_block_dc(bits, dct, pred, coefs, scan.ah, scan.al)
    } else {
        decode_block_ac(bits, act, coefs, scan.ss, scan.se, scan.ah, scan.al, eobrun)
    }
}

fn extend(v: u32, n: u32) -> i32 {
    if n == 0 {
        0
    } else if v < (1 << (n - 1)) {
        v as i32 - ((1 << n) - 1)
    } else {
        v as i32
    }
}

fn i16_check(v: i32) -> Result<i16, ImageError> {
    if (-32768..=32767).contains(&v) {
        Ok(v as i16)
    } else {
        Err(ImageError::Corrupt("JPEG coefficient overflow"))
    }
}

fn decode_block_seq(bits: &mut Bits, dc: &Huff, ac: &Huff, pred: &mut i32, coefs: &mut [i16]) -> Result<(), ImageError> {
    coefs.fill(0);
    let t = dc.decode(bits)? as u32;
    if t > 11 {
        return Err(ImageError::Corrupt("DC category"));
    }
    let diff = if t == 0 { 0 } else { extend(bits.bits(t as u8)?, t) };
    *pred = pred.wrapping_add(diff);
    coefs[0] = i16_check(*pred)?;
    let mut k = 1usize;
    while k < 64 {
        let rs = ac.decode(bits)?;
        let run = (rs >> 4) as usize;
        let size = (rs & 15) as u32;
        if size == 0 {
            if run == 15 {
                k += 16;
                continue;
            }
            break;
        }
        if size > 10 {
            return Err(ImageError::Corrupt("AC category"));
        }
        k += run;
        if k >= 64 {
            return Err(ImageError::Corrupt("AC run past block"));
        }
        coefs[ZIGZAG[k]] = i16_check(extend(bits.bits(size as u8)?, size))?;
        k += 1;
    }
    Ok(())
}

fn decode_block_dc(bits: &mut Bits, dc: &Huff, pred: &mut i32, coefs: &mut [i16], ah: u32, al: u32) -> Result<(), ImageError> {
    if ah == 0 {
        let t = dc.decode(bits)? as u32;
        if t > 11 {
            return Err(ImageError::Corrupt("DC category"));
        }
        let diff = if t == 0 { 0 } else { extend(bits.bits(t as u8)?, t) };
        *pred = pred.wrapping_add(diff);
        coefs[0] = i16_check(*pred << al)?;
    } else if bits.bit()? != 0 {
        coefs[0] = i16_check(coefs[0] as i32 + (1 << al))?;
    }
    Ok(())
}

fn decode_block_ac(
    bits: &mut Bits,
    ac: &Huff,
    coefs: &mut [i16],
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
    eobrun: &mut u32,
) -> Result<(), ImageError> {
    if ah == 0 {
        if *eobrun > 0 {
            *eobrun -= 1;
            return Ok(());
        }
        let mut k = ss;
        while k <= se {
            let rs = ac.decode(bits)?;
            let r = (rs >> 4) as usize;
            let s = (rs & 15) as u32;
            if s == 0 {
                if r < 15 {
                    let mut run = 1u32 << r;
                    if r > 0 {
                        run += bits.bits(r as u8)?;
                    }
                    *eobrun = run - 1;
                    break;
                }
                k += 16;
            } else {
                if s > 10 {
                    return Err(ImageError::Corrupt("AC category"));
                }
                k += r;
                if k > se {
                    return Err(ImageError::Corrupt("AC run past scan"));
                }
                coefs[ZIGZAG[k]] = i16_check(extend(bits.bits(s as u8)?, s) << al)?;
                k += 1;
            }
        }
    } else {
        let bit = 1i32 << al;
        if *eobrun > 0 {
            *eobrun -= 1;
            for k in ss..=se {
                let p = &mut coefs[ZIGZAG[k]];
                if *p != 0 && bits.bit()? != 0 && (*p as i32 & bit) == 0 {
                    *p = i16_check(*p as i32 + if *p > 0 { bit } else { -bit })?;
                }
            }
            return Ok(());
        }
        let mut k = ss;
        while k <= se {
            let rs = ac.decode(bits)?;
            let mut r = (rs >> 4) as usize;
            let s = (rs & 15) as u32;
            let mut newval = 0i16;
            if s == 0 {
                if r < 15 {
                    let mut run = (1u32 << r) - 1;
                    if r > 0 {
                        run += bits.bits(r as u8)?;
                    }
                    *eobrun = run;
                    r = 64;
                }
            } else {
                if s != 1 {
                    return Err(ImageError::Corrupt("AC refinement category"));
                }
                newval = if bits.bit()? != 0 { bit as i16 } else { -(bit as i16) };
            }
            while k <= se {
                let p = &mut coefs[ZIGZAG[k]];
                k += 1;
                if *p != 0 {
                    if bits.bit()? != 0 && (*p as i32 & bit) == 0 {
                        *p = i16_check(*p as i32 + if *p > 0 { bit } else { -bit })?;
                    }
                } else if r == 0 {
                    *p = newval;
                    break;
                } else {
                    r -= 1;
                }
            }
        }
    }
    Ok(())
}

static EMPTY_HUFF: Huff = Huff { counts: [0; 16], symbols: Vec::new() };

fn finish(f: Frame, qt: &[Option<[u16; 64]>; 4], adobe: Option<u8>) -> Result<Image, ImageError> {
    if f.comps.is_empty() || f.comps.len() == 2 || f.comps.len() > 3 {
        return Err(ImageError::Unsupported("JPEG colour components"));
    }
    let mut comps = f.comps;
    for c in &mut comps {
        let q = qt.get(c.tq).and_then(|t| t.as_ref()).ok_or(ImageError::Corrupt("quantisation table"))?;
        let stride = c.blocks_x * 8;
        let height = c.blocks_y * 8;
        let mut plane = vec![0u8; stride * height];
        for by in 0..c.blocks_y {
            for bx in 0..c.blocks_x {
                let coef = &c.coefs[(by * c.blocks_x + bx) * 64..(by * c.blocks_x + bx) * 64 + 64];
                idct_into(coef, q, &mut plane, stride, bx, by);
            }
        }
        c.plane = plane;
    }
    let rgb = adobe == Some(0) && comps.len() == 3;
    to_image(&comps, f.width, f.height, f.max_h, f.max_v, rgb)
}

fn cos_table() -> &'static [[f32; 8]; 8] {
    static T: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [[0f32; 8]; 8];
        for x in 0..8 {
            for u in 0..8 {
                let c = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
                t[x][u] = 0.5 * c * (((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI) / 16.0).cos();
            }
        }
        t
    })
}

fn idct_into(coef: &[i16], q: &[u16; 64], plane: &mut [u8], stride: usize, bx: usize, by: usize) {
    let cos = cos_table();
    let mut f = [0f32; 64];
    for i in 0..64 {
        f[i] = coef[i] as f32 * q[i] as f32;
    }
    let mut tmp = [0f32; 64];
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0.0;
            for u in 0..8 {
                s += cos[x][u] * f[y * 8 + u];
            }
            tmp[y * 8 + x] = s;
        }
    }
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0.0;
            for v in 0..8 {
                s += cos[y][v] * tmp[v * 8 + x];
            }
            let val = (s + 128.0).round().clamp(0.0, 255.0) as u8;
            let o = (by * 8 + y) * stride + bx * 8 + x;
            if o < plane.len() {
                plane[o] = val;
            }
        }
    }
}

fn to_image(
    comps: &[Component],
    width: usize,
    height: usize,
    max_h: usize,
    max_v: usize,
    rgb: bool,
) -> Result<Image, ImageError> {
    let mut img = Image::new(width as u32, height as u32)?;
    let sample = |ci: usize, x: usize, y: usize| -> f32 {
        let c = &comps[ci];
        let sx = x * c.h / max_h;
        let sy = y * c.v / max_v;
        let stride = c.blocks_x * 8;
        c.plane.get(sy * stride + sx).copied().unwrap_or(0) as f32
    };
    for y in 0..height {
        for x in 0..width {
            let px = if comps.len() == 1 {
                let g = sample(0, x, y).clamp(0.0, 255.0) as u8;
                [g, g, g, 255]
            } else if rgb {
                [sample(0, x, y).clamp(0.0, 255.0) as u8, sample(1, x, y).clamp(0.0, 255.0) as u8, sample(2, x, y).clamp(0.0, 255.0) as u8, 255]
            } else {
                let yy = sample(0, x, y);
                let cb = sample(1, x, y) - 128.0;
                let cr = sample(2, x, y) - 128.0;
                let r = yy + 1.402 * cr;
                let g = yy - 0.344136 * cb - 0.714136 * cr;
                let b = yy + 1.772 * cb;
                [r.clamp(0.0, 255.0) as u8, g.clamp(0.0, 255.0) as u8, b.clamp(0.0, 255.0) as u8, 255]
            };
            let o = (y * width + x) * 4;
            img.pixels[o..o + 4].copy_from_slice(&px);
        }
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_headers() {
        assert!(decode(b"not a jpeg").is_err());
        assert!(decode(&[0xFF, 0xD8, 0xFF]).is_err());
    }
}
