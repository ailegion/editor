//! Read-only raster previews decoded entirely in Rust, off the UI thread.
use iced::widget::{column, container, image, row, text, Space};
use iced::{Element, Length};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone)]
pub struct Decoded {
    handle: image::Handle,
    width: u32,
    height: u32,
}

pub struct Preview {
    pub request: u64,
    result: Option<Result<Decoded, String>>,
}

pub fn supported(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| {
        matches!(extension.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "ico")
    })
}

impl Preview {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self { request: NEXT.fetch_add(1, Ordering::Relaxed), result: None }
    }

    pub fn receive(&mut self, request: u64, result: Result<Decoded, String>) -> bool {
        if self.request != request { return false; }
        self.result = Some(result);
        true
    }


}

pub fn load(path: &Path) -> Result<Decoded, String> {
    let reader = ::image::ImageReader::open(path).map_err(|error| error.to_string())?;
    decode(reader.with_guessed_format().map_err(|error| error.to_string())?)
}

fn decode<R: std::io::BufRead + std::io::Seek>(mut reader: ::image::ImageReader<R>) -> Result<Decoded, String> {
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|error| error.to_string())?.into_rgba8();
    let (width, height) = image.dimensions();
    Ok(Decoded { handle: image::Handle::from_rgba(width, height, image.into_raw()), width, height })
}

pub fn view<Message: 'static>(preview: &Preview) -> Element<'_, Message> {
    match &preview.result {
        Some(Ok(decoded)) => column![
            container(image::viewer(decoded.handle.clone()).width(Length::Fill).height(Length::Fill)
                .content_fit(iced::ContentFit::Contain).padding(16)).height(Length::Fill),
            container(row![text(format!("{} × {} px", decoded.width, decoded.height)).size(12),
                Space::new().width(Length::Fill), text("Scroll to zoom · Drag to pan").size(12)
                    .style(iced::widget::text::secondary),
            ].spacing(12)).padding([6, 12]).style(crate::chrome_style),
        ].height(Length::Fill).into(),
        result => {
            let message = match result {
                None => "Loading image…".into(),
                Some(Err(error)) => format!("Could not preview this image\n{error}"),
                _ => unreachable!(),
            };
            container(text(message).size(13)).padding(24).center_x(Length::Fill).center_y(Length::Fill).into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn formats_decode_without_system_codecs() {
        for (extension, format) in [("png", ::image::ImageFormat::Png), ("jpg", ::image::ImageFormat::Jpeg),
            ("webp", ::image::ImageFormat::WebP), ("gif", ::image::ImageFormat::Gif),
            ("bmp", ::image::ImageFormat::Bmp), ("ico", ::image::ImageFormat::Ico)] {
            let source = ::image::DynamicImage::ImageRgb8(::image::RgbImage::from_pixel(3, 2, ::image::Rgb([40, 80, 120])));
            let mut bytes = Cursor::new(Vec::new());
            let source = if format == ::image::ImageFormat::Jpeg { source } else { ::image::DynamicImage::ImageRgba8(source.into_rgba8()) };
            source.write_to(&mut bytes, format).unwrap();
            bytes.set_position(0);
            let decoded = decode(::image::ImageReader::new(bytes).with_guessed_format().unwrap()).unwrap();
            assert_eq!((decoded.width, decoded.height), (3, 2), "{extension}");
            assert!(supported(Path::new(&format!("picture.{}", extension.to_uppercase()))));
        }
        assert!(supported(Path::new("photo.jpeg")));
        for path in ["main.rs", "notes.md", "picture.svg", "no-extension"] { assert!(!supported(Path::new(path))); }
    }

    #[test]
    fn loading_a_file_preserves_its_bytes_and_pixels() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.PNG");
        let image = ::image::DynamicImage::ImageRgba8(::image::RgbaImage::from_pixel(2, 1, ::image::Rgba([10, 20, 30, 40])));
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, ::image::ImageFormat::Png).unwrap();
        std::fs::write(&path, encoded.get_ref()).unwrap();
        let decoded = load(&path).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1));
        match decoded.handle {
            image::Handle::Rgba { pixels, .. } => assert_eq!(pixels.as_ref(), &[10, 20, 30, 40, 10, 20, 30, 40]),
            _ => panic!("expected decoded pixels"),
        }
        assert_eq!(std::fs::read(path).unwrap(), encoded.into_inner());
    }

    #[test]
    fn corrupt_images_and_missing_files_return_errors() {
        let reader = ::image::ImageReader::with_format(Cursor::new(b"invalid image"), ::image::ImageFormat::Png);
        assert!(decode(reader).is_err());
        let directory = tempfile::tempdir().unwrap();
        assert!(load(&directory.path().join("missing.png")).is_err());
    }

    #[test]
    fn stale_results_cannot_replace_a_new_preview() {
        let old = Preview::new();
        let mut current = Preview::new();
        assert!(!current.receive(old.request, Err("old result".into())));
        assert!(current.result.is_none());
        assert!(current.receive(current.request, Err("current error".into())));
        assert!(matches!(current.result, Some(Err(_))));
    }
}
