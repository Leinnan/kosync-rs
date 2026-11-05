//! Conversion of uploaded `MOBI` books to `EPUB`.
//!
//! The library only stores `EPUB` files, so a `MOBI` upload is converted once
//! and the original discarded. Export goes through `boko`'s normalizing
//! pipeline because its passthrough mode copies legacy `MOBI` markup as is,
//! which is rarely well-formed `XHTML`.

use std::fmt;
use std::io::{Cursor, Read, Write};

use boko::export::{EpubConfig, EpubExporter, Exporter};
use boko::{Book, Format};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

/// Offset of the Palm database type and creator fields.
const MAGIC_OFFSET: usize = 60;
/// Palm database type and creator of Mobipocket files.
const MAGIC: &[u8] = b"BOOKMOBI";
/// Offset of the first record's location in the Palm database header.
const FIRST_RECORD_OFFSET: usize = 78;
/// Offset of the format version within the first record.
const VERSION_OFFSET: usize = 36;
/// Format version of files holding only a `KF8` book.
const KF8_VERSION: u32 = 8;
/// Where `boko` writes the package document.
const OPF_PATH: &str = "OEBPS/content.opf";

/// Why a `MOBI` file could not be converted.
#[derive(Debug)]
pub(crate) enum ConvertError {
    /// The book is encrypted.
    DrmProtected,
    /// The file could not be read or written as a book.
    Invalid(boko::Error),
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DrmProtected => f.write_str("book is DRM-protected"),
            Self::Invalid(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ConvertError {}

impl From<boko::Error> for ConvertError {
    fn from(err: boko::Error) -> Self {
        match err {
            boko::Error::DrmProtected(_) => Self::DrmProtected,
            err => Self::Invalid(err),
        }
    }
}

/// Whether `data` is a Mobipocket file, judged by its content.
#[must_use]
pub(crate) fn is_mobi(data: &[u8]) -> bool {
    data.get(MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()) == Some(MAGIC)
}

/// The `boko` importer for `data`.
///
/// Hybrid files open as `MOBI` even though they also hold a `KF8` book:
/// `boko`'s `KF8` importer loses their images. Files holding only a `KF8` book
/// have no `MOBI` section to read.
fn source_format(data: &[u8]) -> Format {
    if read_u32(data, FIRST_RECORD_OFFSET)
        .and_then(|record| usize::try_from(record).ok()?.checked_add(VERSION_OFFSET))
        .and_then(|offset| read_u32(data, offset))
        == Some(KF8_VERSION)
    {
        Format::Azw3
    } else {
        Format::Mobi
    }
}

/// Read a big-endian `u32` at `offset`.
fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

/// Convert a Mobipocket file to `EPUB`.
///
/// # Errors
///
/// Returns an error if the book is encrypted or cannot be read.
pub(crate) fn to_epub(data: &[u8]) -> Result<Vec<u8>, ConvertError> {
    let book = Book::from_bytes(data, source_format(data))?;

    let config = EpubConfig {
        normalize: true,
        ..EpubConfig::default()
    };
    let mut output = Cursor::new(Vec::new());
    EpubExporter::new()
        .with_config(config)
        .export(&book, &mut output)?;

    Ok(restore_cover(&book, output.into_inner()))
}

/// Add the book's cover back to a normalized `EPUB`.
///
/// Normalizing keeps only the images a page shows, and a `MOBI` cover usually
/// lives outside the pages. A book that cannot get its cover back is still
/// readable, so failures only log a warning.
fn restore_cover(book: &Book, epub: Vec<u8>) -> Vec<u8> {
    let Some(path) = book.metadata().cover_image.clone() else {
        return epub;
    };
    let cover = match book.load_asset(&path) {
        Ok(cover) => cover,
        Err(err) => {
            tracing::warn!(error = %err, "could not read MOBI cover");
            return epub;
        }
    };
    let extension = path.rsplit('.').next().unwrap_or("jpg");
    let media_type = mime_guess::from_ext(extension).first_or_octet_stream();

    match add_cover(&epub, &cover, extension, media_type.essence_str()) {
        Ok(Some(with_cover)) => with_cover,
        Ok(None) => epub,
        Err(err) => {
            tracing::warn!(error = %err, "could not add cover to converted EPUB");
            epub
        }
    }
}

/// Copy `epub` with `cover` declared as its cover image.
///
/// Returns `None` if the package already declares a cover.
fn add_cover(
    epub: &[u8],
    cover: &[u8],
    extension: &str,
    media_type: &str,
) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
    let mut archive = ZipArchive::new(Cursor::new(epub))?;

    let mut opf = String::new();
    archive.by_name(OPF_PATH)?.read_to_string(&mut opf)?;
    if opf.contains("cover-image") {
        return Ok(None);
    }
    let href = format!("cover-image.{extension}");
    let opf = opf
        .replacen(
            "</metadata>",
            "  <meta name=\"cover\" content=\"cover-image\"/>\n  </metadata>",
            1,
        )
        .replacen(
            "</manifest>",
            &format!(
                "  <item id=\"cover-image\" href=\"{href}\" media-type=\"{media_type}\" properties=\"cover-image\"/>\n  </manifest>"
            ),
            1,
        );

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    // Entries keep their order, so `mimetype` stays first as `EPUB` requires.
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        if entry.name() == OPF_PATH {
            drop(entry);
            writer.start_file(OPF_PATH, deflated())?;
            writer.write_all(opf.as_bytes())?;
        } else {
            writer.raw_copy_file(entry)?;
        }
    }
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer.start_file(format!("OEBPS/{href}"), options)?;
    writer.write_all(cover)?;

    Ok(Some(writer.finish()?.into_inner()))
}

/// Zip options for compressed entries.
fn deflated() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::{Cursor, Read, Write};

    use zip::{ZipArchive, ZipWriter};

    use super::{FIRST_RECORD_OFFSET, KF8_VERSION, OPF_PATH, VERSION_OFFSET, add_cover, deflated};
    use super::{Format, is_mobi, source_format};

    fn palm_file(version: u32) -> Vec<u8> {
        let mut data = vec![0; 100];
        data[60..68].copy_from_slice(b"BOOKMOBI");
        data[FIRST_RECORD_OFFSET..FIRST_RECORD_OFFSET + 4].copy_from_slice(&90u32.to_be_bytes());
        data.resize(90 + VERSION_OFFSET + 4, 0);
        data[90 + VERSION_OFFSET..].copy_from_slice(&version.to_be_bytes());
        data
    }

    fn epub(opf: &str) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("mimetype", deflated()).unwrap();
        zip.write_all(b"application/epub+zip").unwrap();
        zip.start_file(OPF_PATH, deflated()).unwrap();
        zip.write_all(opf.as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn recognizes_mobi_by_content() {
        assert!(is_mobi(&palm_file(6)));
        assert!(!is_mobi(b"BOOKMOBI"));
        assert!(!is_mobi(&[0; 200]));
    }

    #[test]
    fn only_pure_kf8_files_use_the_kf8_importer() {
        assert!(matches!(source_format(&palm_file(6)), Format::Mobi));
        assert!(matches!(
            source_format(&palm_file(KF8_VERSION)),
            Format::Azw3
        ));
        assert!(matches!(source_format(&[0; 10]), Format::Mobi));
    }

    #[test]
    fn adds_cover_after_keeping_entry_order() {
        let source = epub("<package><metadata>\n</metadata><manifest>\n</manifest></package>");
        let output = add_cover(&source, b"picture", "jpg", "image/jpeg")
            .unwrap()
            .unwrap();

        let mut archive = ZipArchive::new(Cursor::new(output)).unwrap();
        assert_eq!(archive.by_index(0).unwrap().name(), "mimetype");

        let mut opf = String::new();
        archive
            .by_name(OPF_PATH)
            .unwrap()
            .read_to_string(&mut opf)
            .unwrap();
        assert!(opf.contains(r#"<meta name="cover" content="cover-image"/>"#));
        assert!(opf.contains(r#"href="cover-image.jpg" media-type="image/jpeg""#));

        let mut cover = Vec::new();
        archive
            .by_name("OEBPS/cover-image.jpg")
            .unwrap()
            .read_to_end(&mut cover)
            .unwrap();
        assert_eq!(cover, b"picture");
    }

    #[test]
    fn keeps_a_declared_cover() {
        let source = epub(r#"<package><item properties="cover-image"/></package>"#);
        assert!(
            add_cover(&source, b"picture", "jpg", "image/jpeg")
                .unwrap()
                .is_none()
        );
    }
}
