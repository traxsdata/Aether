//! image - decoders for the pictures pages actually use.
//!
//! These are small, strict, safe-Rust decoders written for æther: PNG
//! (non-interlaced, 8- or 16-bit), baseline JPEG, GIF (first frame) and BMP.
//! Every decoder checks dimensions and output size before allocating, and
//! returns an error instead of guessing when the input is malformed. There is
//! no `unsafe` and no external crate.

pub mod bmp;
pub mod gif;
pub mod jpeg;
pub mod png;

/// Largest image side accepted (in pixels).
pub const MAX_DIM: u32 = 8192;
/// Largest image accepted, in pixels. At RGBA8888 this is 64 MiB.
pub const MAX_PIXELS: u64 = 16 * 1024 * 1024;

/// A decoded image, always RGBA8888, row-major, alpha unpremultiplied.
#[derive(Clone, Debug)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Result<Image, ImageError> {
        if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
            return Err(ImageError::TooLarge);
        }
        let pixels = width as u64 * height as u64;
        if pixels > MAX_PIXELS {
            return Err(ImageError::TooLarge);
        }
        Ok(Image { width, height, pixels: vec![0; (pixels * 4) as usize] })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    Unsupported(&'static str),
    Corrupt(&'static str),
    TooLarge,
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::Unsupported(w) => write!(f, "this image format isn't supported ({w})"),
            ImageError::Corrupt(w) => write!(f, "the image is damaged ({w})"),
            ImageError::TooLarge => write!(f, "the image is too large"),
        }
    }
}

pub fn check_size(width: u32, height: u32) -> Result<(), ImageError> {
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        return Err(ImageError::TooLarge);
    }
    if width as u64 * height as u64 > MAX_PIXELS {
        return Err(ImageError::TooLarge);
    }
    Ok(())
}

/// Decodes by content sniffing, with the response's MIME type as a hint.
pub fn decode(data: &[u8], mime: Option<&str>) -> Result<Image, ImageError> {
    if data.len() >= 8 && data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return png::decode(data);
    }
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return jpeg::decode(data);
    }
    if data.len() >= 6 && (data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
        return gif::decode(data);
    }
    if data.len() >= 2 && data.starts_with(b"BM") {
        return bmp::decode(data);
    }
    match mime.map(|m| m.split(';').next().unwrap_or("").trim().to_ascii_lowercase()) {
        Some(m) if m == "image/png" => png::decode(data),
        Some(m) if m == "image/jpeg" || m == "image/jpg" => jpeg::decode(data),
        Some(m) if m == "image/gif" => gif::decode(data),
        Some(m) if m == "image/bmp" || m == "image/x-ms-bmp" => bmp::decode(data),
        Some(m) if m.starts_with("image/") => Err(ImageError::Unsupported("this picture type")),
        _ => Err(ImageError::Unsupported("not a picture")),
    }
}

/// Big-endian helpers shared by the decoders.
pub(crate) fn be16(d: &[u8]) -> u16 {
    (d[0] as u16) << 8 | d[1] as u16
}

pub(crate) fn be32(d: &[u8]) -> u32 {
    (d[0] as u32) << 24 | (d[1] as u32) << 16 | (d[2] as u32) << 8 | d[3] as u32
}

pub(crate) fn le16(d: &[u8]) -> u16 {
    d[0] as u16 | (d[1] as u16) << 8
}

pub(crate) fn le32(d: &[u8]) -> u32 {
    d[0] as u32 | (d[1] as u32) << 8 | (d[2] as u32) << 16 | (d[3] as u32) << 24
}
