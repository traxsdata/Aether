//! icon_data.rs - helpers for window icons, as in eframe. æther draws its
//! own window chrome, so icons are accepted and ignored.

use egui::IconData;

pub trait IconDataExt {
    /// Convert into [`image::RgbaImage`].
    fn to_image(&self) -> Result<image::RgbaImage, String>;

    /// Encode as PNG.
    fn to_png_bytes(&self) -> Result<Vec<u8>, String>;
}

/// Load the contents of .png file.
pub fn from_png_bytes(png_bytes: &[u8]) -> Result<IconData, image::ImageError> {
    let image = image::load_from_memory(png_bytes)?.into_rgba8();
    Ok(IconData { width: image.width(), height: image.height(), rgba: image.into_raw() })
}

impl IconDataExt for IconData {
    fn to_image(&self) -> Result<image::RgbaImage, String> {
        image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone()).ok_or_else(|| "Invalid IconData".to_owned())
    }

    fn to_png_bytes(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        self.to_image()?
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(out)
    }
}
