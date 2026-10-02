//! OPDS 1.x (Atom) catalog feeds.

use std::io;

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use quick_xml::{
    Writer,
    events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event},
};

use crate::{state::AppState, store, util::now_unix};

use super::{
    PER_PAGE, PageQuery, base_url, page_for, percent_encode_path_segment, require_user, rfc3339,
    store_error,
};

const ATOM_NAMESPACE: &str = "http://www.w3.org/2005/Atom";
const OPDS_NAMESPACE: &str = "http://opds-spec.org/2010/catalog";
const OPENSEARCH_NAMESPACE: &str = "http://a9.com/-/spec/opensearch/1.1/";
const DCTERMS_NAMESPACE: &str = "http://purl.org/dc/terms/";

/// Media type for the OPDS 1.x acquisition feed.
const ACQUISITION_TYPE: &str = "application/atom+xml;profile=opds-catalog;kind=acquisition";
/// Media type for the OPDS 1.x navigation feed.
const NAVIGATION_TYPE: &str = "application/atom+xml;profile=opds-catalog;kind=navigation";
/// The taxonomy URI assigned to genre categories in Atom entries.
const GENRE_SCHEME: &str = "https://kosync-rs.invalid/genre";
/// The taxonomy URI assigned to series categories in Atom entries.
const SERIES_SCHEME: &str = "https://kosync-rs.invalid/series";

/// Render the OPDS 1.x root navigation feed.
pub(super) async fn nav_feed(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let _user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let base = base_url(&headers);
    let updated = rfc3339(now_unix());

    atom_result(root_navigation_xml(&base, &updated))
}

/// Render the OPDS 1.x all-publications acquisition feed.
pub(super) async fn publication_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Response {
    let _user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let base = base_url(&headers);
    let search = query.query.as_deref().filter(|q| !q.trim().is_empty());
    let total = match store::count_publications(&state.pool, search).await {
        Ok(total) => total,
        Err(err) => return store_error(&err),
    };
    let page = page_for(total, query.page);
    let publications =
        match store::list_publications_paged(&state.pool, PER_PAGE, (page - 1) * PER_PAGE, search)
            .await
        {
            Ok(publications) => publications,
            Err(err) => return store_error(&err),
        };

    atom_result(acquisition_xml(
        &base,
        "kosync-rs library",
        "/opds/publications",
        search,
        total,
        page,
        &publications,
    ))
}

/// Render the OPDS 1.x author navigation feed.
pub(super) async fn author_navigation_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let _user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let groups = match store::list_authors(&state.pool).await {
        Ok(groups) => groups,
        Err(err) => return store_error(&err),
    };
    atom_result(group_navigation_xml(
        &base_url(&headers),
        "Authors",
        "authors",
        &groups,
    ))
}

/// Render the OPDS 1.x genre navigation feed.
pub(super) async fn genre_navigation_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let _user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let groups = match store::list_genres(&state.pool).await {
        Ok(groups) => groups,
        Err(err) => return store_error(&err),
    };
    atom_result(group_navigation_xml(
        &base_url(&headers),
        "Genres",
        "genres",
        &groups,
    ))
}

/// Render the OPDS 1.x series navigation feed.
pub(super) async fn series_navigation_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let _user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let groups = match store::list_series(&state.pool).await {
        Ok(groups) => groups,
        Err(err) => return store_error(&err),
    };
    atom_result(group_navigation_xml(
        &base_url(&headers),
        "Series",
        "series",
        &groups,
    ))
}

/// Render the OPDS 1.x acquisition feed for a named author.
pub(super) async fn author_publication_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    grouped_publication_feed(
        state,
        headers,
        name,
        query,
        store::GroupKind::Author,
        "Author",
        "authors",
    )
    .await
}

/// Render the OPDS 1.x acquisition feed for a named genre.
pub(super) async fn genre_publication_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    grouped_publication_feed(
        state,
        headers,
        name,
        query,
        store::GroupKind::Genre,
        "Genre",
        "genres",
    )
    .await
}

/// Render the OPDS 1.x acquisition feed for a named series.
pub(super) async fn series_publication_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    grouped_publication_feed(
        state,
        headers,
        name,
        query,
        store::GroupKind::Series,
        "Series",
        "series",
    )
    .await
}

/// Render an OPDS 1.x acquisition feed for one named group.
async fn grouped_publication_feed(
    state: AppState,
    headers: HeaderMap,
    name: String,
    query: PageQuery,
    kind: store::GroupKind,
    label: &str,
    segment: &str,
) -> Response {
    let _user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if name.trim().is_empty() {
        return super::not_found();
    }

    let total = match store::count_publications_in_group(&state.pool, kind, &name).await {
        Ok(total) => total,
        Err(err) => return store_error(&err),
    };
    if total == 0 {
        return super::not_found();
    }

    let page = page_for(total, query.page);
    let publications = match store::list_publications_in_group(
        &state.pool,
        kind,
        &name,
        PER_PAGE,
        (page - 1) * PER_PAGE,
    )
    .await
    {
        Ok(publications) => publications,
        Err(err) => return store_error(&err),
    };
    let base = base_url(&headers);
    let endpoint = format!("/opds/{segment}/{}", percent_encode_path_segment(&name));
    atom_result(acquisition_xml(
        &base,
        &format!("{label}: {name}"),
        &endpoint,
        None,
        total,
        page,
        &publications,
    ))
}

/// Build the root OPDS navigation feed.
fn root_navigation_xml(base: &str, updated: &str) -> io::Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::new());
    let id = format!("{base}/opds/");
    feed_head(
        &mut writer,
        &id,
        "kosync-rs library",
        NAVIGATION_TYPE,
        updated,
    )?;
    navigation_entry(
        &mut writer,
        "All books",
        &format!("{base}/opds/publications"),
        ACQUISITION_TYPE,
        updated,
        None,
    )?;
    navigation_entry(
        &mut writer,
        "Authors",
        &format!("{base}/opds/authors"),
        NAVIGATION_TYPE,
        updated,
        None,
    )?;
    navigation_entry(
        &mut writer,
        "Genres",
        &format!("{base}/opds/genres"),
        NAVIGATION_TYPE,
        updated,
        None,
    )?;
    navigation_entry(
        &mut writer,
        "Series",
        &format!("{base}/opds/series"),
        NAVIGATION_TYPE,
        updated,
        None,
    )?;
    end(&mut writer, "feed")?;
    Ok(writer.into_inner())
}

/// Build one author, genre, or series navigation feed.
fn group_navigation_xml(
    base: &str,
    title: &str,
    segment: &str,
    groups: &[store::GroupCount],
) -> io::Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::new());
    let updated = rfc3339(now_unix());
    let id = format!("{base}/opds/{segment}");
    feed_head(
        &mut writer,
        &id,
        &format!("kosync-rs library: {title}"),
        NAVIGATION_TYPE,
        &updated,
    )?;
    for group in groups {
        let href = format!(
            "{base}/opds/{segment}/{}",
            percent_encode_path_segment(&group.name)
        );
        navigation_entry(
            &mut writer,
            &group.name,
            &href,
            ACQUISITION_TYPE,
            &updated,
            Some(group.count),
        )?;
    }
    end(&mut writer, "feed")?;
    Ok(writer.into_inner())
}

/// Build a paginated OPDS Atom acquisition feed.
fn acquisition_xml(
    base: &str,
    title: &str,
    endpoint: &str,
    search: Option<&str>,
    total: i64,
    page: i64,
    publications: &[store::Publication],
) -> io::Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::new());
    let updated = rfc3339(now_unix());
    let id = format!("{base}{endpoint}");
    feed_head(&mut writer, &id, title, ACQUISITION_TYPE, &updated)?;

    link(
        &mut writer,
        "search",
        &format!("{base}/opds/publications?query={{searchTerms}}"),
        Some("application/atom+xml"),
    )?;
    text(&mut writer, "opensearch:totalResults", &total.to_string())?;
    text(
        &mut writer,
        "opensearch:itemsPerPage",
        &PER_PAGE.to_string(),
    )?;

    let total_pages = ((total + PER_PAGE - 1) / PER_PAGE).max(1);
    if page > 1 {
        link(
            &mut writer,
            "previous",
            &atom_page_href(base, endpoint, page - 1, search),
            Some(ACQUISITION_TYPE),
        )?;
    }
    if page < total_pages {
        link(
            &mut writer,
            "next",
            &atom_page_href(base, endpoint, page + 1, search),
            Some(ACQUISITION_TYPE),
        )?;
    }

    for publication in publications {
        publication_entry(&mut writer, base, publication)?;
    }

    end(&mut writer, "feed")?;
    Ok(writer.into_inner())
}

/// Write the opening `<feed>` element and common metadata.
fn feed_head(
    writer: &mut Writer<Vec<u8>>,
    id: &str,
    title: &str,
    kind: &str,
    updated: &str,
) -> io::Result<()> {
    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
    let mut feed = BytesStart::new("feed");
    feed.push_attribute(("xmlns", ATOM_NAMESPACE));
    feed.push_attribute(("xmlns:opds", OPDS_NAMESPACE));
    feed.push_attribute(("xmlns:opensearch", OPENSEARCH_NAMESPACE));
    feed.push_attribute(("xmlns:dcterms", DCTERMS_NAMESPACE));
    writer.write_event(Event::Start(feed))?;
    text(writer, "id", id)?;
    text(writer, "title", title)?;
    text(writer, "updated", updated)?;
    link(writer, "self", id, Some(kind))
}

/// Write one navigation entry and its optional publication count.
fn navigation_entry(
    writer: &mut Writer<Vec<u8>>,
    title: &str,
    href: &str,
    typ: &str,
    updated: &str,
    count: Option<i64>,
) -> io::Result<()> {
    start(writer, "entry")?;
    text(writer, "title", title)?;
    text(writer, "id", href)?;
    text(writer, "updated", updated)?;
    if let Some(count) = count {
        text(writer, "summary", &format!("{count} books"))?;
    }
    link(writer, "subsection", href, Some(typ))?;
    end(writer, "entry")
}

/// Write an Atom acquisition entry for a publication.
fn publication_entry(
    writer: &mut Writer<Vec<u8>>,
    base: &str,
    publication: &store::Publication,
) -> io::Result<()> {
    start(writer, "entry")?;
    text(writer, "title", &publication.title)?;
    text(
        writer,
        "id",
        &format!("urn:kosync:{}", publication.document_hash),
    )?;
    text(writer, "updated", &rfc3339(publication.updated_at))?;
    if let Some(language) = &publication.language {
        text(writer, "dcterms:language", language)?;
    }
    for author in super::parse_authors(&publication.authors) {
        start(writer, "author")?;
        text(writer, "name", &author.name)?;
        end(writer, "author")?;
    }
    for subject in super::parse_subjects(&publication.genres) {
        category(writer, &subject.name, GENRE_SCHEME)?;
    }
    if let Some(series) = &publication.series {
        category(writer, series, SERIES_SCHEME)?;
    }
    if let Some(description) = &publication.description {
        text(writer, "summary", description)?;
    }
    if publication.cover_path.is_some() {
        link(
            writer,
            "http://opds-spec.org/image",
            &format!(
                "{base}/opds/v2/publications/{}/cover",
                publication.document_hash
            ),
            publication.cover_media_type.as_deref(),
        )?;
    }
    if publication.thumb_path.is_some() {
        link(
            writer,
            "http://opds-spec.org/image/thumbnail",
            &format!(
                "{base}/opds/v2/publications/{}/thumbnail",
                publication.document_hash
            ),
            publication.thumb_media_type.as_deref(),
        )?;
    }
    link(
        writer,
        "http://opds-spec.org/acquisition",
        &format!(
            "{base}/opds/v2/publications/{}/file",
            publication.document_hash
        ),
        Some("application/epub+zip"),
    )?;
    end(writer, "entry")
}

/// Write an Atom `<category>` for a genre or series.
fn category(writer: &mut Writer<Vec<u8>>, value: &str, scheme: &str) -> io::Result<()> {
    let mut category = BytesStart::new("category");
    category.push_attribute(("term", value));
    category.push_attribute(("label", value));
    category.push_attribute(("scheme", scheme));
    writer.write_event(Event::Empty(category))
}

/// Write an Atom `<link>` element.
fn link(writer: &mut Writer<Vec<u8>>, rel: &str, href: &str, typ: Option<&str>) -> io::Result<()> {
    let mut link = BytesStart::new("link");
    link.push_attribute(("rel", rel));
    link.push_attribute(("href", href));
    if let Some(typ) = typ {
        link.push_attribute(("type", typ));
    }
    writer.write_event(Event::Empty(link))
}

/// Write one text-only XML element.
fn text(writer: &mut Writer<Vec<u8>>, name: &str, value: &str) -> io::Result<()> {
    writer
        .create_element(name)
        .write_text_content(BytesText::new(value))
        .map(|_| ())
}

/// Write an opening XML element without attributes.
fn start(writer: &mut Writer<Vec<u8>>, name: &str) -> io::Result<()> {
    writer.write_event(Event::Start(BytesStart::new(name)))
}

/// Write a closing XML element.
fn end(writer: &mut Writer<Vec<u8>>, name: &str) -> io::Result<()> {
    writer.write_event(Event::End(BytesEnd::new(name)))
}

/// Build an absolute paginated Atom feed URL.
fn atom_page_href(base: &str, endpoint: &str, page: i64, search: Option<&str>) -> String {
    match search {
        Some(query) => format!(
            "{base}{endpoint}?page={page}&query={}",
            percent_encode_path_segment(query)
        ),
        None => format!("{base}{endpoint}?page={page}"),
    }
}

/// Convert XML-writing failures into an HTTP response.
fn atom_result(result: io::Result<Vec<u8>>) -> Response {
    match result {
        Ok(xml) => atom_response(xml),
        Err(err) => {
            tracing::error!(error = %err, "failed to write Atom feed");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to build Atom feed",
            )
                .into_response()
        }
    }
}

/// Wrap an XML document in an `application/atom+xml` response.
fn atom_response(xml: Vec<u8>) -> Response {
    (
        [("content-type", "application/atom+xml; charset=utf-8")],
        xml,
    )
        .into_response()
}
