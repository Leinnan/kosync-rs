//! Archive safety checks run on uploads before the `epub` crate reads them.
//!
//! The `epub` crate inflates entries into memory without limits, so a small
//! zip bomb could exhaust memory. Each entry is inflated here, counting the
//! bytes actually produced rather than trusting the sizes the archive
//! declares.
//!
//! Ported from skryba's `skryba-epub` preflight.

use std::fmt;
use std::io::{Cursor, Read};

/// Most entries an archive may contain.
const MAX_ENTRIES: usize = 10_000;
/// Largest inflated size, in bytes, of a single entry.
const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
/// Largest inflated size, in bytes, of all entries together.
const MAX_EXPANDED_BYTES: u64 = 1024 * 1024 * 1024;

/// Why an archive was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreflightError {
    /// The data is not a readable zip archive.
    NotZip,
    /// The archive has more than [`MAX_ENTRIES`] entries.
    TooManyEntries,
    /// An entry name is absolute or escapes the archive root.
    UnsafePath,
    /// An entry inflates past [`MAX_ENTRY_BYTES`].
    EntryTooLarge,
    /// All entries together inflate past [`MAX_EXPANDED_BYTES`].
    ExpandedTooLarge,
}

impl fmt::Display for PreflightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotZip => "not a valid EPUB archive",
            Self::TooManyEntries => "archive has too many entries",
            Self::UnsafePath => "archive contains an unsafe path",
            Self::EntryTooLarge => "archive entry is too large",
            Self::ExpandedTooLarge => "archive expands to too much data",
        })
    }
}

impl std::error::Error for PreflightError {}

/// Check an uploaded archive against the safety limits.
///
/// # Errors
///
/// Returns the first limit the archive breaks.
pub(crate) fn preflight(data: &[u8]) -> Result<(), PreflightError> {
    check(data, MAX_ENTRY_BYTES, MAX_EXPANDED_BYTES)
}

/// [`preflight`] with explicit size limits, so tests can use small ones.
fn check(data: &[u8], max_entry: u64, max_expanded: u64) -> Result<(), PreflightError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(data)).map_err(|_| PreflightError::NotZip)?;
    if zip.len() > MAX_ENTRIES {
        return Err(PreflightError::TooManyEntries);
    }

    let mut total: u64 = 0;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|_| PreflightError::NotZip)?;
        if entry.enclosed_name().is_none() {
            return Err(PreflightError::UnsafePath);
        }
        if entry.size() > max_entry {
            return Err(PreflightError::EntryTooLarge);
        }
        let inflated = std::io::copy(
            &mut entry.by_ref().take(max_entry + 1),
            &mut std::io::sink(),
        )
        .map_err(|_| PreflightError::NotZip)?;
        if inflated > max_entry {
            // The declared size lied about the inflated length.
            return Err(PreflightError::EntryTooLarge);
        }
        total += inflated;
        if total > max_expanded {
            return Err(PreflightError::ExpandedTooLarge);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::Write;

    use super::{PreflightError, check, preflight};

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(contents).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn ordinary_archives_pass() {
        assert_eq!(
            preflight(&archive(&[("mimetype", b"application/epub+zip")])),
            Ok(())
        );
    }

    #[test]
    fn non_archives_are_rejected() {
        assert_eq!(preflight(b"not a zip"), Err(PreflightError::NotZip));
    }

    #[test]
    fn highly_compressible_entries_are_measured_by_inflated_size() {
        let zeros = vec![0u8; 4096];
        let data = archive(&[("big.bin", &zeros)]);
        assert!(data.len() < 1024, "the entry compresses well");
        assert_eq!(
            check(&data, 1024, 1 << 20),
            Err(PreflightError::EntryTooLarge)
        );
    }

    #[test]
    fn total_expansion_is_bounded() {
        let chunk = vec![0u8; 600];
        let data = archive(&[("a.bin", &chunk), ("b.bin", &chunk)]);
        assert_eq!(
            check(&data, 1024, 1000),
            Err(PreflightError::ExpandedTooLarge)
        );
    }

    #[test]
    fn escaping_paths_are_rejected() {
        let data = archive(&[("../evil.xhtml", b"x")]);
        assert_eq!(preflight(&data), Err(PreflightError::UnsafePath));
    }
}
