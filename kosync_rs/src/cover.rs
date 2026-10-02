//! Cover image processing: decoding and thumbnail generation.

use std::error::Error;
use std::io::Cursor;

use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat, ImageReader};

/// Maximum dimension, in pixels, of a generated cover thumbnail.
const THUMBNAIL_MAX: u32 = 300;

/// Generate a `JPEG` thumbnail from raw image bytes, preserving aspect ratio.
///
/// The longest edge is scaled to [`THUMBNAIL_MAX`] pixels.
///
/// # Errors
///
/// Returns an error if the image cannot be decoded or the thumbnail cannot be
/// encoded.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub(crate) fn thumbnail(data: &[u8]) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    let img = ImageReader::new(Cursor::new(data))
        .with_guessed_format()?
        .decode()?;

    let (width, height) = img.dimensions();
    let longest = width.max(height);
    let scale = f64::from(THUMBNAIL_MAX) / f64::from(longest);
    let new_width = ((f64::from(width) * scale).round().max(1.0)) as u32;
    let new_height = ((f64::from(height) * scale).round().max(1.0)) as u32;

    let resized = img.resize(new_width, new_height, FilterType::Triangle);

    let mut buffer = Vec::new();
    resized.write_to(&mut Cursor::new(&mut buffer), ImageFormat::Jpeg)?;

    Ok(buffer)
}
