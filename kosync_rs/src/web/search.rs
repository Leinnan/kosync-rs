//! Global search backing the command palette (and its no-JS fallback page).

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use serde::{Deserialize, Serialize};
use tower_cookies::Cookies;

use crate::{state::AppState, store};

use super::{
    BookLinkView, base_context, book_link, cover_hue, current_user, format_timestamp, is_htmx,
    render,
};

/// Maximum hits returned per result group.
const LIMIT: i64 = 6;

/// Search query parameters.
#[derive(Debug, Deserialize)]
pub(super) struct SearchQuery {
    q: Option<String>,
}

/// A navigation shortcut offered by the palette.
#[derive(Debug, Serialize)]
struct ActionView {
    label: &'static str,
    href: &'static str,
    icon: &'static str,
}

/// A synced document matching the search.
#[derive(Debug, Serialize)]
struct DocumentHit {
    document_hash: String,
    title: String,
    subtitle: String,
    thumb_hash: Option<String>,
    hue: u16,
    percentage: f64,
    synced: String,
}

/// Navigation shortcuts available to `user`.
fn actions(user: &store::User) -> Vec<ActionView> {
    let mut actions = vec![
        ActionView {
            label: "Reading progress",
            href: "/",
            icon: "activity",
        },
        ActionView {
            label: "Library",
            href: "/books",
            icon: "library",
        },
    ];
    if user.may_upload() {
        actions.push(ActionView {
            label: "Upload books",
            href: "/books/upload",
            icon: "upload",
        });
    }
    if user.is_administrator {
        actions.push(ActionView {
            label: "Manage users",
            href: "/admin/users",
            icon: "users",
        });
    }
    actions.push(ActionView {
        label: "Settings",
        href: "/settings",
        icon: "settings",
    });
    actions.push(ActionView {
        label: "Connect an e-reader",
        href: "/help",
        icon: "help",
    });
    actions
}

/// Build a search hit for a synced document, preferring library metadata.
async fn document_hit(state: &AppState, doc: store::Document) -> DocumentHit {
    let book = store::find_publication_for_document(&state.pool, &doc)
        .await
        .unwrap_or(None)
        .map(|publication| book_link(&publication));
    let synced = format_timestamp(doc.timestamp);

    if let Some(BookLinkView {
        document_hash: book_hash,
        title,
        author,
        has_thumb,
        hue,
    }) = book
    {
        return DocumentHit {
            document_hash: doc.document_hash,
            title,
            subtitle: author,
            thumb_hash: has_thumb.then_some(book_hash),
            hue,
            percentage: doc.percentage,
            synced,
        };
    }

    let title = doc
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| doc.document_hash.clone());
    DocumentHit {
        hue: cover_hue(&title),
        title,
        subtitle: doc.authors.clone().unwrap_or_else(|| doc.device.clone()),
        document_hash: doc.document_hash,
        thumb_hash: None,
        percentage: doc.percentage,
        synced,
    }
}

/// Render search results for books, synced documents, and pages.
pub(super) async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    cookies: Cookies,
    Query(query): Query<SearchQuery>,
) -> Response {
    let Some(user) = current_user(&state, &cookies).await else {
        return Redirect::to("/login").into_response();
    };

    let q: String = query
        .q
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect();
    let needle = q.to_lowercase();

    let actions: Vec<ActionView> = actions(&user)
        .into_iter()
        .filter(|action| needle.is_empty() || action.label.to_lowercase().contains(&needle))
        .collect();

    let search = if q.is_empty() { None } else { Some(q.as_str()) };

    let books: Vec<BookLinkView> = if search.is_some() {
        let filter = store::PublicationFilter {
            search,
            user_id: user.id,
            ..store::PublicationFilter::default()
        };
        store::list_publications_filtered(&state.pool, &filter, LIMIT, 0)
            .await
            .unwrap_or_default()
            .iter()
            .map(book_link)
            .collect()
    } else {
        Vec::new()
    };

    let doc_filter = store::DocumentFilter {
        search,
        user_id: user.id,
        ..store::DocumentFilter::default()
    };
    let documents = store::list_documents_filtered(&state.pool, &doc_filter, LIMIT, 0)
        .await
        .unwrap_or_default();
    let mut document_hits = Vec::with_capacity(documents.len());
    for doc in documents {
        document_hits.push(document_hit(&state, doc).await);
    }

    let mut ctx = base_context(&user, "search");
    ctx.insert("q", &q);
    ctx.insert("actions", &actions);
    ctx.insert("books", &books);
    ctx.insert("documents", &document_hits);

    if is_htmx(&headers) {
        render(&state, "search_results.html", &ctx)
    } else {
        render(&state, "search.html", &ctx)
    }
}
