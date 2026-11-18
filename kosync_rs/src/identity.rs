//! How readers name a library book, so every identifier they send for it
//! resolves to the same publication.
//!
//! `KOReader`-compatible readers identify a document either by a partial `MD5`
//! of its bytes or, in "filename" mode, by `md5(basename)`. The basename is
//! whatever the reader saved the book as, which for an OPDS download is built
//! from the feed's title and author, not the name the book was uploaded with.
//! [`name_hashes`] predicts those names so the server can recognise them.

use crate::db::md5_hex;

/// `CrossPoint` caps a sanitized download name at this many bytes, before the
/// `.epub` extension.
const CROSSPOINT_MAX_BYTES: usize = 100;

/// Port of `CrossPoint`'s `StringUtils::sanitizeFilename`.
///
/// Leading spaces and dots are skipped, `/\:*?"<>|` become `_`, other control
/// characters are dropped, the result is cut to `max_bytes` on a character
/// boundary, trailing spaces and dots are trimmed, and an empty result becomes
/// `book`.
pub(crate) fn crosspoint_sanitize(name: &str, max_bytes: usize) -> String {
    let mut result = String::with_capacity(name.len().min(max_bytes));
    for c in name.trim_start_matches([' ', '.']).chars() {
        let replacement = match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if u32::from(c) >= 128 || (' '..='~').contains(&c) => c,
            _ => continue,
        };
        if result.len() + replacement.len_utf8() > max_bytes {
            break;
        }
        result.push(replacement);
    }
    let kept = result.trim_end_matches([' ', '.']).len();
    result.truncate(kept);
    if result.is_empty() {
        "book".to_owned()
    } else {
        result
    }
}

/// The file names readers are likely to have saved a book as.
///
/// Covers the uploaded `file_name`, the server's own download name
/// (`Title.epub`), and `CrossPoint`'s three OPDS formats (`Author - Title`,
/// `Title - Author`, `Title`), which also cover `KOReader`'s
/// `Author - Title` default. `CrossPoint` keeps the last `<author>` of a feed
/// entry, so each author is tried alone as well as all of them joined.
pub(crate) fn candidate_names(
    title: &str,
    authors: &[String],
    file_name: Option<&str>,
) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(file_name) = file_name.filter(|n| !n.is_empty()) {
        names.push(file_name.to_owned());
    }

    let title = title.trim();
    if !title.is_empty() {
        names.push(format!("{}.epub", crate::library::sanitize_filename(title)));

        let mut author_forms: Vec<String> = authors
            .iter()
            .map(|a| a.trim().to_owned())
            .filter(|a| !a.is_empty())
            .collect();
        if author_forms.len() > 1 {
            author_forms.push(author_forms.join(", "));
        }

        let sanitized =
            |base: &str| format!("{}.epub", crosspoint_sanitize(base, CROSSPOINT_MAX_BYTES));
        names.push(sanitized(title));
        for author in &author_forms {
            names.push(sanitized(&format!("{author} - {title}")));
            names.push(sanitized(&format!("{title} - {author}")));
        }
    }

    names.sort();
    names.dedup();
    names
}

/// `MD5` digests of [`candidate_names`], the identifiers a reader in
/// "filename" mode would send.
pub(crate) fn name_hashes(title: &str, authors_json: &str, file_name: Option<&str>) -> Vec<String> {
    let authors: Vec<String> = serde_json::from_str(authors_json).unwrap_or_default();
    let mut hashes: Vec<String> = candidate_names(title, &authors, file_name)
        .iter()
        .map(|name| md5_hex(name))
        .collect();
    hashes.sort();
    hashes.dedup();
    hashes
}

/// Fold a title or author for loose comparison: lowercase, with runs of
/// whitespace collapsed to one space.
pub(crate) fn fold(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether a reader's free-text author string (`"A, B"`, `"A & B"`, `"A"`)
/// names at least one of a publication's authors. An empty side matches,
/// since many readers send no authors at all.
pub(crate) fn authors_overlap(reader_authors: Option<&str>, authors: &[String]) -> bool {
    let Some(reader) = reader_authors.map(fold).filter(|a| !a.is_empty()) else {
        return true;
    };
    if authors.is_empty() {
        return true;
    }
    authors
        .iter()
        .map(|a| fold(a))
        .filter(|a| !a.is_empty())
        .any(|author| reader.contains(&author) || author.contains(&reader))
}

#[cfg(test)]
mod tests {
    use super::{authors_overlap, candidate_names, crosspoint_sanitize, fold, name_hashes};

    #[test]
    fn sanitize_replaces_illegal_characters_and_trims() {
        assert_eq!(
            crosspoint_sanitize(" ..a/b\\c:d*e?f\"g<h>i|j. ", 100),
            "a_b_c_d_e_f_g_h_i_j"
        );
        assert_eq!(crosspoint_sanitize("tab\there\u{7f}", 100), "tabhere");
        assert_eq!(crosspoint_sanitize(" . ", 100), "book");
        assert_eq!(crosspoint_sanitize("", 100), "book");
    }

    #[test]
    fn sanitize_caps_on_a_character_boundary() {
        // "ż" is two bytes; a 5-byte cap cannot fit the third one.
        assert_eq!(crosspoint_sanitize("żżż", 5), "żż");
        assert_eq!(crosspoint_sanitize(&"a".repeat(150), 100).len(), 100);
        // Trailing spaces left by the cut are trimmed too.
        assert_eq!(crosspoint_sanitize("abc   def", 5), "abc");
    }

    #[test]
    fn crosspoint_download_name_of_gra_o_tron_is_predicted() {
        let hashes = name_hashes(
            "Gra o tron 1",
            r#"["George R. R. Martin"]"#,
            Some("01 Gra o tron 1 - George R. R. Martin.epub"),
        );
        // md5("George R. R. Martin - Gra o tron 1.epub"), what CrossPoint sent.
        assert!(hashes.contains(&"fa7a8ce9d48dfb352737deeea8c20743".to_owned()));
        // md5 of the uploaded name, the old `file_name_hash`.
        assert!(hashes.contains(&"af0aff3f8f944fd2565874bd5b6a0013".to_owned()));
    }

    #[test]
    fn several_authors_are_tried_alone_and_joined() {
        let names = candidate_names("T", &["A".to_owned(), "B".to_owned()], None);
        for expected in [
            "A - T.epub",
            "B - T.epub",
            "A, B - T.epub",
            "T - B.epub",
            "T.epub",
        ] {
            assert!(names.contains(&expected.to_owned()), "missing {expected}");
        }
    }

    #[test]
    fn untitled_books_only_predict_their_file_name() {
        assert_eq!(
            candidate_names("  ", &["A".to_owned()], Some("x.epub")),
            vec!["x.epub"]
        );
    }

    #[test]
    fn author_overlap_is_loose() {
        let authors = ["George R. R. Martin".to_owned()];
        assert!(authors_overlap(Some("george r. r.  martin"), &authors));
        assert!(authors_overlap(
            Some("George R. R. Martin, Someone"),
            &authors
        ));
        assert!(authors_overlap(None, &authors));
        assert!(!authors_overlap(Some("Terry Pratchett"), &authors));
        assert_eq!(fold("  Gra  O Tron "), "gra o tron");
    }
}
