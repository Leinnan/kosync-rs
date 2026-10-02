//! `EPUB` parsing: metadata extraction, cover detection, and the `KOReader`
//! partial `MD5` document digest used to match uploads to sync records.

use std::error::Error;
use std::io::{Read, Seek};

use epub::doc::EpubDoc;
use md5::{Digest, Md5};

/// Size, in bytes, of each sample read by the partial digest.
const SAMPLE_SIZE: usize = 1024;
/// Number of samples read by the partial digest (`i = -1 ..= 10`).
const SAMPLE_COUNT: usize = 12;

/// Metadata and cover extracted from an `EPUB` file.
#[derive(Debug)]
pub(crate) struct ParsedEpub {
    /// The document title.
    pub(crate) title: Option<String>,
    /// The list of authors (creators).
    pub(crate) authors: Vec<String>,
    /// The list of subjects (genres), deduplicated.
    pub(crate) subjects: Vec<String>,
    /// A normalized ISBN extracted from the identifiers, if present.
    pub(crate) isbn: Option<String>,
    /// The publication language code.
    pub(crate) language: Option<String>,
    /// A persistent identifier (ISBN or similar).
    pub(crate) identifier: Option<String>,
    /// The publisher name.
    pub(crate) publisher: Option<String>,
    /// The publication date.
    pub(crate) published: Option<String>,
    /// A free-text description.
    pub(crate) description: Option<String>,
    /// The series name (calibre `calibre:series` or `EPUB` 3 collection), if present.
    pub(crate) series: Option<String>,
    /// The position within the series, if present.
    pub(crate) series_index: Option<f64>,
    /// The cover image bytes and MIME type, if present.
    pub(crate) cover: Option<(Vec<u8>, String)>,
}

/// Compute the `KOReader` partial `MD5` document digest of `data`.
///
/// This replicates [`util.partialMD5`] from the `KOReader` source: a `1024`-byte
/// sample is read at offsets `256, 1024, 4096, 16384, 65536, 262144, 1048576,
/// 4194304, 16777216, 67108864, 268435456, 1073741824` (each `4x` the previous),
/// stopping at the first offset where no bytes are available. The samples are
/// concatenated and hashed with `MD5`, yielding a lowercase hex digest.
///
/// [`util.partialMD5`]: https://github.com/koreader/koreader/blob/master/frontend/util.lua
#[must_use]
pub(crate) fn partial_md5_bytes(data: &[u8]) -> String {
    let mut hasher = Md5::new();
    let mut offset: usize = 256; // 1024 << (2 * -1)

    for _ in 0..SAMPLE_COUNT {
        if offset >= data.len() {
            break;
        }
        let end = (offset + SAMPLE_SIZE).min(data.len());
        hasher.update(&data[offset..end]);
        offset = offset.saturating_mul(4);
    }

    format!("{:x}", hasher.finalize())
}

fn meta<R: Read + Seek>(doc: &EpubDoc<R>, property: &str) -> Option<String> {
    doc.mdata(property).map(|item| item.value.clone())
}

/// Collect all values of a metadata property, trimmed, non-empty, and
/// deduplicated case-insensitively (first occurrence wins).
fn meta_all<R: Read + Seek>(doc: &EpubDoc<R>, property: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    doc.metadata
        .iter()
        .filter(|item| item.property == property)
        .map(|item| item.value.trim())
        .filter(|value| !value.is_empty())
        .filter(|value| seen.insert(value.to_lowercase()))
        .map(str::to_owned)
        .collect()
}

/// Normalize a raw identifier string to an ISBN, if it looks like one.
///
/// Accepts common prefixes (`urn:isbn:`, `isbn:`, `isbn-10:`, `isbn-13:`),
/// ignores hyphens and spaces, and validates the check digit. `ISBN`-10s are
/// uppercased so a trailing `x` becomes `X`.
#[must_use]
pub(crate) fn normalize_isbn(raw: &str) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    let stripped = lower
        .strip_prefix("urn:isbn:")
        .or_else(|| lower.strip_prefix("isbn-13:"))
        .or_else(|| lower.strip_prefix("isbn-10:"))
        .or_else(|| lower.strip_prefix("isbn:"))
        .unwrap_or(&lower);

    let candidate: String = stripped
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .collect();

    if candidate.len() == 13
        && candidate.bytes().all(|b| b.is_ascii_digit())
        && candidate.starts_with("97")
        && valid_isbn13(&candidate)
    {
        return Some(candidate);
    }

    if candidate.len() == 10 && valid_isbn10(&candidate) {
        return Some(candidate.to_uppercase());
    }

    None
}

/// Validate an `ISBN`-13 check digit.
fn valid_isbn13(isbn: &str) -> bool {
    let mut sum = 0u32;
    for (i, b) in isbn.bytes().enumerate() {
        let Some(digit) = (b as char).to_digit(10) else {
            return false;
        };
        sum += if i % 2 == 0 { digit } else { digit * 3 };
    }
    sum.is_multiple_of(10)
}

/// Validate an `ISBN`-10 check digit (a trailing `x`/`X` counts as 10).
fn valid_isbn10(isbn: &str) -> bool {
    const WEIGHTS: [u32; 10] = [10, 9, 8, 7, 6, 5, 4, 3, 2, 1];

    let mut sum = 0u32;
    for (i, c) in isbn.chars().enumerate() {
        let digit = if i == 9 && (c == 'x' || c == 'X') {
            10
        } else {
            let Some(digit) = c.to_digit(10) else {
                return false;
            };
            digit
        };
        sum += digit * WEIGHTS[i];
    }
    sum.is_multiple_of(11)
}

/// Extract the series position from a raw calibre `series_index` value.
fn parse_series_index(raw: &str) -> Option<f64> {
    let value: f64 = raw.trim().parse().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

/// Parse an `EPUB` document from a reader, extracting metadata and cover image.
///
/// # Errors
///
/// Returns an error if the document is not a valid `EPUB`.
pub(crate) fn parse<R: Read + Seek>(reader: R) -> Result<ParsedEpub, Box<dyn Error + Send + Sync>> {
    let mut doc = EpubDoc::from_reader(reader)?;

    let authors = doc
        .metadata
        .iter()
        .filter(|item| item.property == "creator")
        .map(|item| item.value.clone())
        .collect();

    let identifier = meta(&doc, "identifier");
    let isbn = doc
        .metadata
        .iter()
        .filter(|item| item.property == "identifier")
        .find_map(|item| normalize_isbn(&item.value));

    let series = meta(&doc, "calibre:series").or_else(|| meta(&doc, "belongs-to-collection"));
    let series_index = meta(&doc, "calibre:series_index").and_then(|raw| parse_series_index(&raw));

    let cover = doc.get_cover();

    Ok(ParsedEpub {
        title: doc.get_title(),
        authors,
        subjects: meta_all(&doc, "subject"),
        isbn,
        language: meta(&doc, "language"),
        identifier,
        publisher: meta(&doc, "publisher"),
        published: meta(&doc, "date"),
        description: meta(&doc, "description"),
        series,
        series_index,
        cover,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use md5::Digest;

    use super::{normalize_isbn, parse, partial_md5_bytes};

    /// Build a minimal in-memory `EPUB` whose OPF metadata section is `metadata`.
    fn epub_with_metadata(metadata: &str) -> Vec<u8> {
        use std::io::Write;

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file("mimetype", options).unwrap();
        zip.write_all(b"application/epub+zip").unwrap();

        zip.start_file("META-INF/container.xml", options).unwrap();
        zip.write_all(
            br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#,
        )
        .unwrap();

        let opf = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
{metadata}
  </metadata>
  <manifest>
    <item id="c1" href="chap1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="c1"/>
  </spine>
</package>"#
        );
        zip.start_file("OEBPS/content.opf", options).unwrap();
        zip.write_all(opf.as_bytes()).unwrap();

        zip.start_file("OEBPS/chap1.xhtml", options).unwrap();
        zip.write_all(br#"<html xmlns="http://www.w3.org/1999/xhtml"><body/></html>"#)
            .unwrap();

        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_subjects_deduplicated_case_insensitively() {
        let data = epub_with_metadata(
            r#"    <dc:identifier id="uid">urn:uuid:test-1234</dc:identifier>
    <dc:title>Test Book</dc:title>
    <dc:subject>Fantasy</dc:subject>
    <dc:subject> fantasy </dc:subject>
    <dc:subject>Epic Fantasy</dc:subject>
    <dc:subject></dc:subject>"#,
        );
        let parsed = parse(std::io::Cursor::new(data)).unwrap();
        assert_eq!(parsed.subjects, vec!["Fantasy", "Epic Fantasy"]);
    }

    #[test]
    fn extracts_isbn_from_urn_prefixed_identifier() {
        let data = epub_with_metadata(
            r#"    <dc:identifier id="uid">urn:uuid:test-1234</dc:identifier>
    <dc:identifier>urn:isbn:978-0-13-468599-1</dc:identifier>
    <dc:title>Test Book</dc:title>"#,
        );
        let parsed = parse(std::io::Cursor::new(data)).unwrap();
        assert_eq!(parsed.isbn.as_deref(), Some("9780134685991"));
    }

    #[test]
    fn extracts_calibre_series_metadata() {
        let data = epub_with_metadata(
            r#"    <dc:identifier id="uid">urn:uuid:test-1234</dc:identifier>
    <dc:title>Test Book</dc:title>
    <meta name="calibre:series" content="The Expanse"/>
    <meta name="calibre:series_index" content="2"/>"#,
        );
        let parsed = parse(std::io::Cursor::new(data)).unwrap();
        assert_eq!(parsed.series.as_deref(), Some("The Expanse"));
        assert_eq!(parsed.series_index, Some(2.0));
    }

    #[test]
    fn normalizes_isbn13_with_hyphens() {
        assert_eq!(
            normalize_isbn("978-0-13-468599-1").as_deref(),
            Some("9780134685991")
        );
    }

    #[test]
    fn normalizes_prefixed_isbn13() {
        assert_eq!(
            normalize_isbn("urn:isbn:9780134685991").as_deref(),
            Some("9780134685991")
        );
        assert_eq!(
            normalize_isbn("ISBN-13: 9780134685991").as_deref(),
            Some("9780134685991")
        );
    }

    #[test]
    fn normalizes_isbn10_with_x_check_digit() {
        assert_eq!(
            normalize_isbn("0-8044-2957-x").as_deref(),
            Some("080442957X")
        );
    }

    #[test]
    fn rejects_invalid_isbns() {
        assert_eq!(normalize_isbn("9780134685992"), None); // bad check digit
        assert_eq!(normalize_isbn("urn:uuid:test-1234"), None);
        assert_eq!(normalize_isbn("12345"), None);
        assert_eq!(normalize_isbn("0134685992"), None); // bad ISBN-10 check digit
    }

    #[test]
    fn empty_input_yields_md5_of_empty() {
        assert_eq!(partial_md5_bytes(&[]), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn input_smaller_than_first_offset_yields_md5_of_empty() {
        let data = vec![0xAB; 200];
        assert_eq!(partial_md5_bytes(&data), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn single_sample_when_data_fits_within_first_window() {
        let data = vec![0x41; 300];
        let mut hasher = md5::Md5::new();
        hasher.update(&data[256..300]);
        let expected = format!("{:x}", hasher.finalize());
        assert_eq!(partial_md5_bytes(&data), expected);
    }

    #[test]
    fn reads_multiple_samples_with_geometric_offsets() {
        let data = vec![0x42; 2048];
        let mut hasher = md5::Md5::new();
        hasher.update(&data[256..1280]);
        hasher.update(&data[1024..2048]);
        let expected = format!("{:x}", hasher.finalize());
        assert_eq!(partial_md5_bytes(&data), expected);
    }
}
