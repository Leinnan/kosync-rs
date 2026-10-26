//! Cover image processing: bounded decoding and thumbnail generation.

use std::error::Error;
use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ImageReader, Limits, RgbImage};

/// Thumbnails fit this box, which keeps them sharp in e-reader grid views.
const THUMBNAIL_WIDTH: u32 = 300;
/// Height of the thumbnail box.
const THUMBNAIL_HEIGHT: u32 = 450;
/// `JPEG` quality of generated thumbnails.
const THUMBNAIL_QUALITY: u8 = 85;
/// Longest accepted source edge, in pixels; larger pictures are rejected.
const MAX_SOURCE_EDGE: u32 = 10_000;
/// Most bytes the decoder may allocate for one picture.
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// Read an image's pixel size from its header, without decoding it.
pub(crate) fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
        .filter(|&(width, height)| width > 0 && height > 0)
}

/// Generate a `JPEG` thumbnail from raw image bytes.
///
/// The picture is shrunk to fit the thumbnail box, preserving aspect ratio;
/// smaller pictures keep their size. Transparent areas are drawn on white,
/// since `JPEG` has no alpha channel. Decoding is bounded so oversized or
/// malicious images fail instead of exhausting memory.
///
/// # Errors
///
/// Returns an error if the image cannot be decoded, exceeds the decoding
/// limits, or the thumbnail cannot be encoded.
pub(crate) fn thumbnail(data: &[u8]) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    let mut reader = ImageReader::new(Cursor::new(data)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_EDGE);
    limits.max_image_height = Some(MAX_SOURCE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let img = reader.decode()?;

    let img = if img.width() > THUMBNAIL_WIDTH || img.height() > THUMBNAIL_HEIGHT {
        img.resize(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT, FilterType::Triangle)
    } else {
        img
    };

    let rgba = img.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (out, pixel) in rgb.pixels_mut().zip(rgba.pixels()) {
        let alpha = u16::from(pixel[3]);
        for channel in 0..3 {
            let blended = (u16::from(pixel[channel]) * alpha + 255 * (255 - alpha)) / 255;
            out[channel] = u8::try_from(blended).unwrap_or(u8::MAX);
        }
    }

    let mut buffer = Vec::new();
    JpegEncoder::new_with_quality(&mut buffer, THUMBNAIL_QUALITY).encode_image(&rgb)?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::Cursor;

    use image::{GenericImageView, ImageFormat, Rgba, RgbaImage};

    use super::{THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, dimensions, thumbnail};

    fn png(img: &RgbaImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn transparent_pixels_become_white() {
        let source = png(&RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 0])));
        let thumb = image::load_from_memory(&thumbnail(&source).unwrap()).unwrap();
        let pixel = thumb.get_pixel(0, 0);
        assert!(
            pixel[0] > 240 && pixel[1] > 240 && pixel[2] > 240,
            "{pixel:?}"
        );
    }

    #[test]
    fn large_pictures_fit_the_box_and_small_ones_keep_their_size() {
        let big = png(&RgbaImage::from_pixel(1200, 1600, Rgba([10, 20, 30, 255])));
        let thumb = image::load_from_memory(&thumbnail(&big).unwrap()).unwrap();
        assert!(thumb.width() <= THUMBNAIL_WIDTH && thumb.height() <= THUMBNAIL_HEIGHT);
        // 3:4 is wider than the 2:3 box, so the width is the limiting edge.
        assert_eq!(thumb.dimensions(), (THUMBNAIL_WIDTH, 400));

        let small = png(&RgbaImage::from_pixel(20, 30, Rgba([10, 20, 30, 255])));
        let thumb = image::load_from_memory(&thumbnail(&small).unwrap()).unwrap();
        assert_eq!(thumb.dimensions(), (20, 30));
    }

    #[test]
    fn oversized_pictures_are_rejected() {
        let huge = png(&RgbaImage::new(super::MAX_SOURCE_EDGE + 1, 1));
        assert!(thumbnail(&huge).is_err());
    }

    #[test]
    fn dimensions_come_from_the_header() {
        assert_eq!(dimensions(&png(&RgbaImage::new(30, 45))), Some((30, 45)));
        let thumb = thumbnail(&png(&RgbaImage::new(600, 900))).unwrap();
        assert_eq!(
            dimensions(&thumb),
            Some((THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT))
        );
        assert_eq!(dimensions(b"not an image"), None);
    }

    #[test]
    fn non_images_are_rejected() {
        assert!(thumbnail(b"not an image").is_err());
    }
}
