//! What browsers, crawlers and phones ask for besides the pages (R19): the
//! web app manifest, robots.txt, the sitemap, /favicon.ico, and the caching
//! headers every page goes out with.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::app::{AppError, AppState};
use crate::db::{self, Pool};
use crate::round::Round;

/// Installing the site to a home screen opens the global round, full screen.
pub async fn manifest() -> Response {
    let body = r##"{
  "name": "i guess low",
  "short_name": "i guess low",
  "description": "pick the lowest number nobody else picks. one guess a day.",
  "start_url": "/g/",
  "scope": "/",
  "display": "standalone",
  "background_color": "#0c0c0c",
  "theme_color": "#0c0c0c",
  "icons": [
    { "src": "/static/icon-192.png", "sizes": "192x192", "type": "image/png" },
    { "src": "/static/icon-512.png", "sizes": "512x512", "type": "image/png" },
    { "src": "/static/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable" },
    { "src": "/static/favicon.svg", "sizes": "any", "type": "image/svg+xml" }
  ]
}
"##;
    ([(header::CONTENT_TYPE, "application/manifest+json")], body).into_response()
}

pub async fn robots(State(state): State<AppState>) -> Response {
    let body = format!(
        "User-agent: *\nDisallow: /admin\nDisallow: /mod\nDisallow: /account\nDisallow: /api/\nDisallow: /verify\nDisallow: /reset\n\nSitemap: {}/sitemap.xml\n",
        state.cfg.public_url
    );
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

/// The public pages: the global round's pages and every closed global
/// round. The 42 round is for 42 students only, so it is not listed.
pub async fn sitemap(State(state): State<AppState>) -> Result<Response, AppError> {
    let base = &state.cfg.public_url;
    let open = Round::current(state.now()).key();
    let mut urls: Vec<String> = ["/g/", "/g/results", "/g/trends", "/login", "/signup", "/privacy", "/imprint"]
        .iter()
        .map(|p| format!("{base}{p}"))
        .collect();
    for r in db::closed_rounds(&state.db, Pool::Global, &open, i64::MAX).await? {
        urls.push(format!("{base}/g/day/{}", r.round_date));
    }
    let mut body = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    for u in urls {
        body.push_str(&format!("  <url><loc>{u}</loc></url>\n"));
    }
    body.push_str("</urlset>\n");
    Ok(([(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response())
}

/// Every page: never cached as-is, always revalidated, and answered with
/// 304 Not Modified when it has not changed since the browser's copy (an
/// ETag over the body). Pages are per-visitor, so the cache is private.
pub async fn page_caching(req: Request, next: Next) -> Response {
    let if_none_match = req.headers().get(header::IF_NONE_MATCH).cloned();
    let resp = next.run(req).await;
    let is_html = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/html"));
    if resp.status() != StatusCode::OK || !is_html {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let digest = Sha256::digest(&bytes);
    let tag: String = digest[..12].iter().map(|b| format!("{b:02x}")).collect();
    let etag = HeaderValue::from_str(&format!("W/\"{tag}\"")).expect("hex is a valid header");
    parts.headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    parts.headers.insert(header::ETAG, etag.clone());
    if if_none_match.is_some_and(|v| v == etag) {
        parts.status = StatusCode::NOT_MODIFIED;
        return Response::from_parts(parts, Body::empty());
    }
    Response::from_parts(parts, Body::from(bytes))
}
