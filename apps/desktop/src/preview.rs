//! Bounded shell thumbnails/icons, extracted off the GPUI thread.
use anyhow::{Result, ensure};
use gpui::RenderImage;
use image::{Frame, RgbaImage};
use std::{mem::size_of, path::Path, sync::Arc};
use windows::{
    Win32::{Foundation::SIZE, Graphics::Gdi::*, UI::Shell::*},
    core::HSTRING,
};

pub const MAX_PREVIEWS: usize = 4;
const PREVIEW_PIXELS: i32 = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Image,
    Text,
    Word,
    Excel,
    PowerPoint,
    Pdf,
    Archive,
    Audio,
    Video,
    Other,
}
impl FileKind {
    pub fn for_path(path: &Path) -> Self {
        match path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase()
            .as_str()
        {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "heif"
            | "avif" | "ico" | "svg" => Self::Image,
            "txt" | "text" | "md" | "log" | "json" | "xml" | "yaml" | "yml" | "ini" | "toml"
            | "rs" | "py" | "js" | "ts" | "html" | "css" => Self::Text,
            "doc" | "docx" | "docm" | "dot" | "dotx" | "odt" | "rtf" => Self::Word,
            "xls" | "xlsx" | "xlsm" | "xlsb" | "xlt" | "xltx" | "csv" | "tsv" | "ods" => {
                Self::Excel
            }
            "ppt" | "pptx" | "pptm" | "pps" | "ppsx" | "pot" | "potx" | "odp" => Self::PowerPoint,
            "pdf" => Self::Pdf,
            "zip" | "7z" | "rar" | "tar" | "gz" => Self::Archive,
            "mp3" | "wav" | "flac" | "m4a" | "aac" | "ogg" => Self::Audio,
            "mp4" | "mov" | "mkv" | "avi" | "webm" | "wmv" => Self::Video,
            _ => Self::Other,
        }
    }
    pub fn badge(self, language: crate::i18n::Language) -> (&'static str, u32) {
        let text = language.strings();
        match self {
            Self::Image => ("IMG", 0x4186be),
            Self::Text => ("TXT", 0x65778b),
            Self::Word => ("W", 0x2564b6),
            Self::Excel => ("X", 0x217346),
            Self::PowerPoint => ("P", 0xc45131),
            Self::Pdf => ("PDF", 0xbd4145),
            Self::Archive => ("ZIP", 0x9e762c),
            Self::Audio => (text.badge_audio, 0x9959aa),
            Self::Video => (text.badge_video, 0x7462b7),
            Self::Other => ("FILE", 0x65778b),
        }
    }
}

pub fn load(path: &Path) -> Result<Arc<RenderImage>> {
    let _apartment = crate::picker::Apartment::new()?;
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
        let size = SIZE {
            cx: PREVIEW_PIXELS,
            cy: PREVIEW_PIXELS,
        };
        let bitmap = if FileKind::for_path(path) == FileKind::Image {
            factory
                .GetImage(size, SIIGBF_THUMBNAILONLY)
                .or_else(|_| factory.GetImage(size, SIIGBF_ICONONLY))?
        } else {
            // Documents show their associated app/type icon, not page content.
            factory.GetImage(size, SIIGBF_ICONONLY)?
        };
        bitmap_image(&Bitmap(bitmap))
    }
}

struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}
struct MemoryDc(HDC);
impl Drop for MemoryDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

fn bitmap_image(bitmap: &Bitmap) -> Result<Arc<RenderImage>> {
    unsafe {
        let mut description = BITMAP::default();
        ensure!(
            GetObjectW(
                bitmap.0.into(),
                size_of::<BITMAP>() as i32,
                Some(&mut description as *mut _ as _)
            ) != 0,
            "无法读取文件预览"
        );
        let (width, height) = (description.bmWidth, description.bmHeight.unsigned_abs());
        ensure!(
            (1..=1024).contains(&width) && (1..=1024).contains(&height),
            "预览尺寸无效"
        );
        let dc = MemoryDc(CreateCompatibleDC(None));
        ensure!(!dc.0.is_invalid(), "无法创建预览画布");
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -(height as i32), // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        ensure!(
            GetDIBits(
                dc.0,
                bitmap.0,
                0,
                height,
                Some(pixels.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS
            ) == height as i32,
            "无法读取预览像素"
        );
        normalize_alpha(&mut pixels, description.bmBitsPixel == 32);
        // The shell returns BGRA; GPUI RenderImage also expects BGRA, despite
        // using image::RgbaImage as its underlying buffer container.
        let buffer = RgbaImage::from_raw(width as u32, height, pixels).unwrap();
        Ok(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
    }
}

fn normalize_alpha(pixels: &mut [u8], has_alpha: bool) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        if !has_alpha {
            pixel[3] = 255;
        }
        let alpha = pixel[3] as u32;
        if alpha > 0 && alpha < 255 {
            // Shell bitmaps are premultiplied; GPUI's image shader expects
            // straight alpha. Preserve fully transparent image backgrounds.
            for color in &mut pixel[..3] {
                *color = ((*color as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_shell_extracts_a_real_thumbnail_and_document_icon() {
        let directory = tempfile::tempdir().unwrap();
        let photo = directory.path().join("缩略图.PNG");
        RgbaImage::from_pixel(32, 20, image::Rgba([235, 70, 20, 255]))
            .save_with_format(&photo, image::ImageFormat::Png)
            .unwrap();
        let thumbnail = load(&photo).unwrap();
        let pixels = thumbnail.as_bytes(0).unwrap();
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] < 30 && (60..80).contains(&p[1]) && p[2] > 225 && p[3] == 255),
            "thumbnail must preserve the photo's colors in BGRA order"
        );
        let document = directory.path().join("notes.txt");
        std::fs::write(&document, "Omdrop file icon test").unwrap();
        let icon = load(&document).unwrap();
        assert!(
            icon.as_bytes(0)
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] > 0)
        );
    }
    #[test]
    fn document_types_and_uppercase_image_extensions_have_distinct_fallbacks() {
        for (name, kind) in [
            ("照片.PNG", FileKind::Image),
            ("notes.TEXT", FileKind::Text),
            ("report.docx", FileKind::Word),
            ("table.XLSX", FileKind::Excel),
            ("talk.pptx", FileKind::PowerPoint),
            ("README", FileKind::Other),
        ] {
            assert_eq!(FileKind::for_path(Path::new(name)), kind);
        }
    }
    #[test]
    fn shell_alpha_is_converted_without_losing_transparency_or_channel_order() {
        let mut pixels = [10, 20, 40, 128, 0, 0, 0, 0, 20, 30, 40, 255];
        normalize_alpha(&mut pixels, true);
        assert_eq!(pixels, [20, 40, 80, 128, 0, 0, 0, 0, 20, 30, 40, 255]);
        normalize_alpha(&mut pixels, false);
        assert_eq!(pixels[7], 255);
    }
}
