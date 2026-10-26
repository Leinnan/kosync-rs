//! Package facts and cover detection that the `epub` crate misses.
//!
//! The `epub` crate is namespace-strict and checks a single cover declaration
//! per `EPUB` version. Calibre files that prefix every `OPF` element (`ns0:`)
//! lose their unprefixed `<meta name="cover">`, and many books declare their
//! cover only through the guide or a cover page. This re-reads the package by
//! local element name and falls back the way reading systems do.
//!
//! Ported from skryba's `skryba-epub` package and `OPF` readers.

use std::borrow::Cow;
use std::io::{Read, Seek};
use std::path::Path;

use epub::doc::EpubDoc;
use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
};

/// Image types the thumbnail generator can decode.
const RASTER_TYPES: [&str; 6] = [
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/tiff",
];

/// Most pictures inspected on a cover page before giving up.
const MAX_PAGE_IMAGES: usize = 8;

/// Package metadata read by local element name.
#[derive(Debug, Default)]
pub(super) struct Package {
    /// Manifest id named by `<meta name="cover">`.
    pub(super) cover_id: Option<String>,
    /// Decoded href of a `<guide>` reference of type `cover`.
    pub(super) guide_cover: Option<String>,
    /// `calibre:series` content.
    pub(super) series: Option<String>,
    /// Raw `calibre:series_index` content.
    pub(super) series_index: Option<String>,
}

/// Re-read the package document, ignoring element namespaces.
pub(super) fn read_package<R: Read + Seek>(doc: &mut EpubDoc<R>) -> Package {
    let root = doc.root_file.clone();
    let Some(bytes) = doc.get_resource_by_path(&root) else {
        return Package::default();
    };
    let xml = String::from_utf8_lossy(&bytes);

    let mut pkg = Package::default();
    for_each_element(&xml, |element| {
        match element.local_name().as_ref() {
            b"meta" => {
                let content = attribute(element, b"content");
                match attribute(element, b"name").as_deref() {
                    Some("cover") => set_once(&mut pkg.cover_id, content),
                    Some("calibre:series") => set_once(&mut pkg.series, content),
                    Some("calibre:series_index") => set_once(&mut pkg.series_index, content),
                    _ => {}
                }
            }
            b"reference" => {
                if matches!(
                    attribute(element, b"type").as_deref(),
                    Some("cover" | "cover-page")
                ) {
                    let href = attribute(element, b"href")
                        .map(|href| percent_decode(href.split('#').next().unwrap_or_default()));
                    set_once(&mut pkg.guide_cover, href);
                }
            }
            _ => {}
        }
        true
    });
    pkg
}

/// Find the cover picture: the declared cover image, then the
/// `<meta name="cover">` id, then the guide's cover page, then the first spine
/// page. A page resolves to the first raster picture it shows (Calibre wraps
/// covers in `<svg><image xlink:href>`).
pub(super) fn find_cover<R: Read + Seek>(
    doc: &mut EpubDoc<R>,
    pkg: &Package,
) -> Option<(Vec<u8>, String)> {
    let declared = doc
        .get_cover_id()
        .filter(|id| doc.resources.contains_key(id))
        .or_else(|| {
            pkg.cover_id.clone().filter(|id| {
                doc.resources
                    .get(id)
                    .is_some_and(|r| r.mime.starts_with("image/"))
            })
        });
    let guided = || {
        let root = decoded_path(&doc.root_file);
        let target = resolve_path(&root, pkg.guide_cover.as_deref()?)?;
        doc.resources
            .iter()
            .find_map(|(id, r)| (decoded_path(&r.path) == target).then(|| id.clone()))
    };
    let id = declared
        .or_else(guided)
        .or_else(|| doc.spine.first().map(|s| s.idref.clone()))?;

    let resource = doc.resources.get(&id)?;
    let raw = raw_path(&resource.path);
    let path = decoded_path(&resource.path);
    let mime = resource.mime.clone();
    if mime.starts_with("image/") {
        return load_raster(doc, &[&raw, &path], &mime);
    }

    let page = read_any(doc, &[&raw, &path])?;
    let page = String::from_utf8_lossy(&page);
    for src in image_sources(&page) {
        let Some(target) = resolve_path(&path, &percent_decode(&src)) else {
            continue;
        };
        let (raw, mime) = doc
            .resources
            .values()
            .find(|r| decoded_path(&r.path) == target)
            .map_or_else(
                || {
                    let guessed = mime_guess::from_path(&target).first_or_octet_stream();
                    (target.clone(), guessed.essence_str().to_owned())
                },
                |r| (raw_path(&r.path), r.mime.clone()),
            );
        if let Some(cover) = load_raster(doc, &[&raw, &target], &mime) {
            return Some(cover);
        }
    }
    None
}

/// Read a picture if its type is one the thumbnail generator decodes.
fn load_raster<R: Read + Seek>(
    doc: &mut EpubDoc<R>,
    paths: &[&str],
    mime: &str,
) -> Option<(Vec<u8>, String)> {
    if !RASTER_TYPES.contains(&mime) {
        return None;
    }
    read_any(doc, paths).map(|bytes| (bytes, mime.to_owned()))
}

/// Read the first archive entry that exists among `paths`.
fn read_any<R: Read + Seek>(doc: &mut EpubDoc<R>, paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|path| doc.get_resource_by_path(path))
}

/// Picture references on an `XHTML` page, in document order.
fn image_sources(xml: &str) -> Vec<String> {
    let mut sources = Vec::new();
    for_each_element(xml, |element| {
        let source = match element.local_name().as_ref() {
            b"img" => attribute(element, b"src"),
            b"image" => attribute(element, b"href"),
            _ => None,
        };
        sources.extend(source.filter(|s| !s.is_empty()));
        sources.len() < MAX_PAGE_IMAGES
    });
    sources
}

/// Visit every start or empty element until `visit` returns `false`, the
/// document ends, or it stops parsing. Mismatched end tags are tolerated.
fn for_each_element(xml: &str, mut visit: impl FnMut(&BytesStart<'_>) -> bool) {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                if !visit(&element) {
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// An attribute's trimmed value, matched by local name (so `xlink:href` is
/// `href`). Values with unknown entities are kept as written.
fn attribute(element: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    element
        .html_attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .map(|a| {
            a.normalized_value(XmlVersion::Implicit1_0).map_or_else(
                |_| String::from_utf8_lossy(&a.value).into_owned(),
                Cow::into_owned,
            )
        })
        .map(|value| value.trim().to_owned())
}

/// Store `value` in `slot` unless it already holds one; empty values are skipped.
fn set_once(slot: &mut Option<String>, value: Option<String>) {
    if slot.is_none() {
        *slot = value.filter(|v| !v.is_empty());
    }
}

/// An archive path as stored by the `epub` crate, with forward slashes.
fn raw_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// An archive path with `%XX` escapes decoded, for comparing hrefs.
fn decoded_path(path: &Path) -> String {
    percent_decode(&raw_path(path))
}

/// Decode `%XX` escapes as in URLs and package paths; malformed escapes stay
/// as written.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    if !bytes.contains(&b'%') {
        return s.to_owned();
    }
    let hex = |b: u8| {
        char::from(b)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escape = (bytes[i] == b'%')
            .then(|| Some(hex(*bytes.get(i + 1)?)? * 16 + hex(*bytes.get(i + 2)?)?))
            .flatten();
        if let Some(value) = escape {
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve `relative` against the directory of `base`. Absolute paths, URLs,
/// and paths climbing above the archive root are rejected.
fn resolve_path(base: &str, relative: &str) -> Option<String> {
    if relative.contains(':') || relative.starts_with('/') {
        return None;
    }
    let dir = base.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut parts = Vec::new();
    for part in dir.split('/').chain(relative.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::{image_sources, percent_decode, resolve_path};

    #[test]
    fn percent_escapes_decode_and_malformed_ones_stay() {
        assert_eq!(percent_decode("a%20b.jpg"), "a b.jpg");
        assert_eq!(percent_decode("100%.xhtml"), "100%.xhtml");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%C4%85"), "ą");
    }

    #[test]
    fn paths_resolve_against_the_page_directory() {
        assert_eq!(
            resolve_path("OEBPS/text/cover.xhtml", "../images/c.jpg").as_deref(),
            Some("OEBPS/images/c.jpg")
        );
        assert_eq!(
            resolve_path("content.opf", "cover.jpeg").as_deref(),
            Some("cover.jpeg")
        );
        assert_eq!(resolve_path("a.xhtml", "../../c.jpg"), None);
        assert_eq!(resolve_path("a.xhtml", "/c.jpg"), None);
        assert_eq!(resolve_path("a.xhtml", "http://x/c.jpg"), None);
    }

    #[test]
    fn page_images_include_svg_wrapped_covers_despite_entities() {
        let page = r#"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:xlink="http://www.w3.org/1999/xlink"><body><p>&nbsp;</p>
<svg xmlns="http://www.w3.org/2000/svg"><image width="1" height="1" xlink:href="cover.jpeg"/></svg>
<img src="logo.png"/></body></html>"#;
        assert_eq!(image_sources(page), ["cover.jpeg", "logo.png"]);
    }
}
