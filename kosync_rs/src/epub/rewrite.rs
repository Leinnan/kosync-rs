//! Rewriting one entry of an `EPUB` archive while leaving the rest untouched.
//!
//! Reading positions (`KOReader` xpointers) name spine documents and the
//! elements inside them, so a rewrite that copies every other entry byte for
//! byte keeps every stored position valid.

use std::fmt;
use std::io::{Cursor, Write};

use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::package::percent_decode;

/// Why an archive entry could not be replaced.
#[derive(Debug)]
pub(crate) enum RewriteError {
    /// The archive has no entry by that name.
    MissingEntry(String),
    /// The archive could not be read or written.
    Zip(ZipError),
}

impl fmt::Display for RewriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEntry(name) => write!(f, "the archive has no entry {name:?}"),
            Self::Zip(err) => write!(f, "archive error: {err}"),
        }
    }
}

impl std::error::Error for RewriteError {}

impl From<ZipError> for RewriteError {
    fn from(err: ZipError) -> Self {
        Self::Zip(err)
    }
}

impl From<std::io::Error> for RewriteError {
    fn from(err: std::io::Error) -> Self {
        Self::Zip(ZipError::Io(err))
    }
}

/// Copy `data` with the entry `name` holding `bytes` instead.
///
/// `name` may carry `%XX` escapes, as package hrefs do; the decoded name is
/// tried when the literal one is missing. Every other entry is copied raw, in
/// its original order (so `mimetype` stays first and stored), and the new
/// entry is stored uncompressed, as images do not compress further.
///
/// # Errors
///
/// Returns [`RewriteError::MissingEntry`] if no entry matches `name`, or
/// [`RewriteError::Zip`] if the archive cannot be read or written.
pub(crate) fn replace_entry(
    data: &[u8],
    name: &str,
    bytes: &[u8],
) -> Result<Vec<u8>, RewriteError> {
    let mut archive = ZipArchive::new(Cursor::new(data))?;
    let target = if archive.index_for_name(name).is_some() {
        name.to_owned()
    } else {
        let decoded = percent_decode(name);
        if archive.index_for_name(&decoded).is_none() {
            return Err(RewriteError::MissingEntry(name.to_owned()));
        }
        decoded
    };

    let mut writer = ZipWriter::new(Cursor::new(Vec::with_capacity(data.len())));
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        if entry.name() == target {
            drop(entry);
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            writer.start_file(target.as_str(), options)?;
            writer.write_all(bytes)?;
        } else {
            writer.raw_copy_file(entry)?;
        }
    }
    Ok(writer.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::{Cursor, Read, Write};

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipArchive, ZipWriter};

    use super::{RewriteError, replace_entry};

    fn archive(entries: &[(&str, &[u8], CompressionMethod)]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data, method) in entries {
            let options = SimpleFileOptions::default().compression_method(*method);
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn entries(data: &[u8]) -> Vec<(String, Vec<u8>, CompressionMethod)> {
        let mut archive = ZipArchive::new(Cursor::new(data)).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut entry = archive.by_index(i).unwrap();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                (entry.name().to_owned(), bytes, entry.compression())
            })
            .collect()
    }

    fn raw_entry(data: &[u8], name: &str) -> Vec<u8> {
        let mut archive = ZipArchive::new(Cursor::new(data)).unwrap();
        let index = archive.index_for_name(name).unwrap();
        let mut entry = archive.by_index_raw(index).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn replaces_only_the_named_entry_and_keeps_order() {
        let text = "<html>chapter</html>".repeat(50);
        let source = archive(&[
            (
                "mimetype",
                b"application/epub+zip",
                CompressionMethod::Stored,
            ),
            (
                "OEBPS/content.opf",
                b"<package/>",
                CompressionMethod::Deflated,
            ),
            ("OEBPS/cover.jpg", b"old cover", CompressionMethod::Deflated),
            (
                "OEBPS/ch1.xhtml",
                text.as_bytes(),
                CompressionMethod::Deflated,
            ),
        ]);

        let out = replace_entry(&source, "OEBPS/cover.jpg", b"new cover").unwrap();
        let after = entries(&out);

        let names: Vec<&str> = after.iter().map(|(name, ..)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "mimetype",
                "OEBPS/content.opf",
                "OEBPS/cover.jpg",
                "OEBPS/ch1.xhtml"
            ]
        );
        assert_eq!(
            after[0].2,
            CompressionMethod::Stored,
            "mimetype stays stored"
        );
        assert_eq!(after[2].1, b"new cover");
        assert_eq!(after[2].2, CompressionMethod::Stored);
        for name in ["mimetype", "OEBPS/content.opf", "OEBPS/ch1.xhtml"] {
            assert_eq!(
                raw_entry(&out, name),
                raw_entry(&source, name),
                "{name} is copied raw"
            );
        }
    }

    #[test]
    fn escaped_names_find_the_decoded_entry() {
        let source = archive(&[("OEBPS/my cover.png", b"old", CompressionMethod::Stored)]);
        let out = replace_entry(&source, "OEBPS/my%20cover.png", b"new").unwrap();
        assert_eq!(entries(&out)[0].1, b"new");
    }

    #[test]
    fn missing_entries_are_reported() {
        let source = archive(&[(
            "mimetype",
            b"application/epub+zip",
            CompressionMethod::Stored,
        )]);
        assert!(matches!(
            replace_entry(&source, "cover.jpg", b"new"),
            Err(RewriteError::MissingEntry(_))
        ));
    }
}
