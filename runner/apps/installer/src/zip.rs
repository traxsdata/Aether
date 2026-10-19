//! zip.rs - just enough ZIP to read an æther package from an `.app.zip`.
//!
//! GitHub release uploads reject the `.app` extension, so the Craft release
//! workflow publishes the package inside a ZIP that contains one `.app`
//! file. This reader accepts stored and deflated entries, bounds every size,
//! and checks the entry's CRC-32. It is not a general archive extractor.

use aether_host::abi::APP_MAX_BYTES;
use webnet::inflate::{crc32, inflate};

const EOCD: u32 = 0x0605_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;
const MAX_ENTRY: usize = APP_MAX_BYTES;

pub fn looks_like_zip(data: &[u8]) -> bool {
    data.len() >= 4 && (le32(data) == LOCAL || le32(data) == EOCD)
}

/// The single `.app` entry in a ZIP.
pub fn extract_app(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 22 {
        return Err("the ZIP is too short".into());
    }
    // The end-of-central-directory record is at the end, after an optional
    // comment of at most 65535 bytes.
    let search_from = data.len().saturating_sub(22 + 65535);
    let mut eocd = None;
    for i in (search_from..=data.len() - 22).rev() {
        if u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) == EOCD {
            let comment = u16::from_le_bytes(data[i + 20..i + 22].try_into().unwrap()) as usize;
            if i + 22 + comment <= data.len() {
                eocd = Some(i);
                break;
            }
        }
    }
    let e = eocd.ok_or("the ZIP has no end record")?;
    let disk = u16::from_le_bytes(data[e + 4..e + 6].try_into().unwrap());
    let cd_disk = u16::from_le_bytes(data[e + 6..e + 8].try_into().unwrap());
    let entries = u16::from_le_bytes(data[e + 10..e + 12].try_into().unwrap()) as usize;
    if disk != 0 || cd_disk != 0 {
        return Err("multi-disk ZIPs aren't supported".into());
    }
    if entries > 4096 {
        return Err("the ZIP has too many entries".into());
    }
    let cd_size = u32::from_le_bytes(data[e + 12..e + 16].try_into().unwrap()) as usize;
    let cd_off = u32::from_le_bytes(data[e + 16..e + 20].try_into().unwrap()) as usize;
    if cd_off.checked_add(cd_size).is_none_or(|x| x > data.len()) {
        return Err("the ZIP central directory is outside the file".into());
    }
    let mut pos = cd_off;
    for _ in 0..entries {
        if pos + 46 > data.len() || le32(&data[pos..]) != CENTRAL {
            return Err("the ZIP central directory is damaged".into());
        }
        let method = u16::from_le_bytes(data[pos + 10..pos + 12].try_into().unwrap());
        let crc = u32::from_le_bytes(data[pos + 16..pos + 20].try_into().unwrap());
        let csize = u32::from_le_bytes(data[pos + 20..pos + 24].try_into().unwrap()) as usize;
        let usize_ = u32::from_le_bytes(data[pos + 24..pos + 28].try_into().unwrap()) as usize;
        let nlen = u16::from_le_bytes(data[pos + 28..pos + 30].try_into().unwrap()) as usize;
        let xlen = u16::from_le_bytes(data[pos + 30..pos + 32].try_into().unwrap()) as usize;
        let clen = u16::from_le_bytes(data[pos + 32..pos + 34].try_into().unwrap()) as usize;
        let local = u32::from_le_bytes(data[pos + 42..pos + 46].try_into().unwrap()) as usize;
        let name_end = pos + 46 + nlen;
        if name_end + xlen + clen > data.len() {
            return Err("the ZIP entry name is truncated".into());
        }
        let name = &data[pos + 46..name_end];
        pos = name_end + xlen + clen;
        if !name.ends_with(b".app") {
            continue;
        }
        if usize_ > MAX_ENTRY {
            return Err("the package in the ZIP is too large".into());
        }
        if csize > MAX_ENTRY {
            return Err("the compressed package is too large".into());
        }
        if local + 30 > data.len() || le32(&data[local..]) != LOCAL {
            return Err("the ZIP entry has no local header".into());
        }
        let ln = u16::from_le_bytes(data[local + 26..local + 28].try_into().unwrap()) as usize;
        let lx = u16::from_le_bytes(data[local + 28..local + 30].try_into().unwrap()) as usize;
        let start = local + 30 + ln + lx;
        if start.checked_add(csize).is_none_or(|x| x > data.len()) {
            return Err("the ZIP entry is truncated".into());
        }
        let raw = &data[start..start + csize];
        let bytes = match method {
            0 => raw.to_vec(),
            8 => inflate(raw, usize_.min(MAX_ENTRY)).map(|(v, _)| v).map_err(|e| format!("the ZIP entry is damaged ({e})"))?,
            m => return Err(format!("unsupported ZIP compression method {m}")),
        };
        if bytes.len() != usize_ {
            return Err("the ZIP entry has the wrong size".into());
        }
        if crc32(&bytes) != crc {
            return Err("the ZIP entry failed its CRC check".into());
        }
        return Ok(bytes);
    }
    Err("the ZIP contains no .app package".into())
}

fn le32(d: &[u8]) -> u32 {
    u32::from_le_bytes([d[0], d[1], d[2], d[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_zip_is_an_error() {
        assert!(extract_app(b"not a zip").is_err());
        assert!(!looks_like_zip(b"not a zip"));
    }

    #[test]
    fn extracts_an_app_package() {
        let zip = include_bytes!("../testdata/test.app.zip");
        let want = include_bytes!("../testdata/test.app.bytes");
        assert!(looks_like_zip(zip));
        assert_eq!(extract_app(zip).unwrap(), want);
    }
}
