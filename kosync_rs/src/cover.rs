//! Cover image processing: bounded decoding, thumbnail generation, and the
//! tint colour pages use behind a book's cover.

use std::error::Error;
use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat, ImageReader, Limits, RgbImage};

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
/// `JPEG` quality of covers re-encoded into a book.
const COVER_QUALITY: u8 = 92;

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
    let img = decode_bounded(data)?;

    let img = if img.width() > THUMBNAIL_WIDTH || img.height() > THUMBNAIL_HEIGHT {
        img.resize(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT, FilterType::Triangle)
    } else {
        img
    };

    let mut buffer = Vec::new();
    JpegEncoder::new_with_quality(&mut buffer, THUMBNAIL_QUALITY).encode_image(&on_white(&img))?;
    Ok(buffer)
}

/// Decode a picture of any supported format, refusing oversized ones.
///
/// # Errors
///
/// Returns an error if the picture cannot be decoded or exceeds the limits.
fn decode_bounded(data: &[u8]) -> Result<DynamicImage, Box<dyn Error + Send + Sync>> {
    let mut reader = ImageReader::new(Cursor::new(data)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_EDGE);
    limits.max_image_height = Some(MAX_SOURCE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    Ok(reader.decode()?)
}

/// The picture as `RGB`, with transparent areas drawn on white.
fn on_white(img: &DynamicImage) -> RgbImage {
    let rgba = img.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (out, pixel) in rgb.pixels_mut().zip(rgba.pixels()) {
        let alpha = u16::from(pixel[3]);
        for channel in 0..3 {
            let blended = (u16::from(pixel[channel]) * alpha + 255 * (255 - alpha)) / 255;
            out[channel] = u8::try_from(blended).unwrap_or(u8::MAX);
        }
    }
    rgb
}

/// Prepare an uploaded picture to stand in for a book's cover image of type
/// `target_mime`.
///
/// The book's manifest keeps naming the old image file and type, so the
/// picture must arrive in that format: one already in it is used as is (no
/// quality loss), any other is decoded and re-encoded. Its size and shape stay
/// as uploaded. Decoding is bounded like thumbnails.
///
/// # Errors
///
/// Returns an error if the upload is not a decodable picture within the
/// limits, `target_mime` is not a raster format, or encoding fails.
pub(crate) fn prepare_replacement(
    data: &[u8],
    target_mime: &str,
) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    let target = ImageFormat::from_mime_type(target_mime)
        .ok_or_else(|| format!("unsupported cover type {target_mime}"))?;
    let img = decode_bounded(data)?;
    if image::guess_format(data).ok() == Some(target) {
        return Ok(data.to_vec());
    }

    let mut buffer = Vec::new();
    if target == ImageFormat::Jpeg {
        JpegEncoder::new_with_quality(&mut buffer, COVER_QUALITY).encode_image(&on_white(&img))?;
    } else {
        DynamicImage::ImageRgba8(img.to_rgba8()).write_to(&mut Cursor::new(&mut buffer), target)?;
    }
    Ok(buffer)
}

/// Lightness band of cover tints: dark enough for light text on a generated
/// cover, light enough to stay a colour rather than near-black.
const TINT_LIGHTNESS: (f64, f64) = (0.26, 0.40);
/// Saturation band of cover tints, so loud covers calm down and dull ones
/// still read as a colour.
const TINT_SATURATION: (f64, f64) = (0.18, 0.55);
/// Below this average saturation a cover counts as black and white, and gets
/// a warm neutral tint instead of an arbitrary hue.
const GREYSCALE_SATURATION: f64 = 0.08;

/// The tint colour of a cover, as `#rrggbb`.
///
/// Pixels are averaged with more weight on saturated ones, so a white or grey
/// background does not drown out the artwork's colour. The result is clamped
/// into a calm band of saturation and lightness, so every tint works behind
/// light text and as a soft panel colour. Returns `None` when the picture
/// cannot be decoded.
pub(crate) fn tint(data: &[u8]) -> Option<String> {
    let img = image::load_from_memory(data).ok()?;
    let img = img.thumbnail(48, 48);
    let (mut red, mut green, mut blue, mut weights, mut saturation_sum, mut count) =
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (_, _, pixel) in img.pixels() {
        let [r, g, b, a] = pixel.0;
        if a < 128 {
            continue;
        }
        let (r, g, b) = (
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
        );
        let (_, saturation, _) = rgb_to_hsl(r, g, b);
        let weight = 0.15 + saturation;
        red += r * weight;
        green += g * weight;
        blue += b * weight;
        weights += weight;
        saturation_sum += saturation;
        count += 1.0;
    }
    if count == 0.0 {
        return None;
    }
    let (hue, saturation, lightness) = rgb_to_hsl(red / weights, green / weights, blue / weights);
    let (hue, saturation) = if saturation_sum / count < GREYSCALE_SATURATION {
        (35.0, 0.12)
    } else {
        (hue, saturation.clamp(TINT_SATURATION.0, TINT_SATURATION.1))
    };
    let lightness = lightness.clamp(TINT_LIGHTNESS.0, TINT_LIGHTNESS.1);
    let (r, g, b) = hsl_to_rgb(hue, saturation, lightness);
    Some(format!(
        "#{:02x}{:02x}{:02x}",
        channel(r),
        channel(g),
        channel(b)
    ))
}

/// A `0.0..=1.0` colour channel as a byte.
fn channel(value: f64) -> u8 {
    // The clamp keeps the rounded value inside 0..=255.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    byte
}

/// Convert `0.0..=1.0` RGB to hue in degrees, saturation and lightness.
fn rgb_to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = f64::midpoint(max, min);
    let delta = max - min;
    if delta < f64::EPSILON {
        return (0.0, 0.0, lightness);
    }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if (max - r).abs() < f64::EPSILON {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f64::EPSILON {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    (hue, saturation.min(1.0), lightness)
}

/// Convert hue in degrees, saturation and lightness to `0.0..=1.0` RGB.
fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> (f64, f64, f64) {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let second = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = match sector {
        s if s < 1.0 => (chroma, second, 0.0),
        s if s < 2.0 => (second, chroma, 0.0),
        s if s < 3.0 => (0.0, chroma, second),
        s if s < 4.0 => (0.0, second, chroma),
        s if s < 5.0 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let base = lightness - chroma / 2.0;
    (red + base, green + base, blue + base)
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
    fn tints_are_clamped_hex_colours() {
        let red = png(&RgbaImage::from_pixel(10, 10, Rgba([255, 0, 0, 255])));
        let tint = super::tint(&red).unwrap();
        assert_eq!(tint.len(), 7);
        assert!(tint.starts_with('#'));
        let (r, g, b) = hex(&tint);
        assert!(r > g && r > b, "a red cover keeps a red tint: {tint}");
        assert!(r < 180, "the tint is darkened into the band: {tint}");
        assert_eq!(super::tint(&red), Some(tint), "the tint is deterministic");
    }

    #[test]
    fn white_covers_get_a_warm_neutral_tint() {
        let white = png(&RgbaImage::from_pixel(10, 10, Rgba([255, 255, 255, 255])));
        let (r, g, b) = hex(&super::tint(&white).unwrap());
        assert!(r >= g && g >= b, "warm neutral");
        assert!(r < 140, "dark enough for light text");
    }

    #[test]
    fn colour_beats_a_white_background() {
        let mut img = RgbaImage::from_pixel(10, 10, Rgba([255, 255, 255, 255]));
        for x in 0..10 {
            for y in 0..3 {
                img.put_pixel(x, y, Rgba([20, 60, 200, 255]));
            }
        }
        let (r, _, b) = hex(&super::tint(&png(&img)).unwrap());
        assert!(b > r, "the blue band sets the hue");
    }

    #[test]
    fn tints_need_a_picture() {
        assert_eq!(super::tint(b"not an image"), None);
    }

    fn hex(colour: &str) -> (u8, u8, u8) {
        let byte = |i: usize| u8::from_str_radix(&colour[i..i + 2], 16).unwrap();
        (byte(1), byte(3), byte(5))
    }

    #[test]
    fn replacements_take_the_format_of_the_cover_they_replace() {
        let source = png(&RgbaImage::from_pixel(40, 70, Rgba([200, 30, 30, 128])));

        let jpeg = super::prepare_replacement(&source, "image/jpeg").unwrap();
        assert_eq!(image::guess_format(&jpeg).unwrap(), ImageFormat::Jpeg);
        assert_eq!(dimensions(&jpeg), Some((40, 70)), "size and shape are kept");

        let gif = super::prepare_replacement(&source, "image/gif").unwrap();
        assert_eq!(image::guess_format(&gif).unwrap(), ImageFormat::Gif);
    }

    #[test]
    fn replacements_already_in_the_right_format_are_kept_byte_for_byte() {
        let source = png(&RgbaImage::from_pixel(30, 45, Rgba([1, 2, 3, 255])));
        assert_eq!(
            super::prepare_replacement(&source, "image/png").unwrap(),
            source
        );
    }

    #[test]
    fn replacements_must_be_pictures_of_a_raster_type() {
        assert!(super::prepare_replacement(b"not an image", "image/png").is_err());
        let source = png(&RgbaImage::new(10, 10));
        assert!(super::prepare_replacement(&source, "image/svg+xml").is_err());
        let huge = png(&RgbaImage::new(super::MAX_SOURCE_EDGE + 1, 1));
        assert!(super::prepare_replacement(&huge, "image/png").is_err());
    }

    #[test]
    fn non_images_are_rejected() {
        assert!(thumbnail(b"not an image").is_err());
    }
}
