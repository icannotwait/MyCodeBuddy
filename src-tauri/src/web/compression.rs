//! Response compression for the web/server mode router.
//!
//! Large JSON payloads (a long conversation's `get_folder_conversation`
//! response reaches tens of MB) were previously sent uncompressed. A
//! [`CompressionLayer`] with a content-type ALLOWLIST predicate fixes that
//! without regressing the binary paths:
//!
//! - File/zip/backup downloads set an explicit `Content-Length` the remote
//!   proxy uses for transfer progress (`remote_proxy.rs` reads
//!   `response.content_length()`); compressing them would strip it, waste CPU
//!   re-compressing already-compressed archives, and break progress display.
//!   An allowlist keeps every `application/octet-stream` / `application/zip`
//!   response untouched by construction.
//! - `text/event-stream` (the office-watch SSE proxy) must never be
//!   compressed: a custom `compress_when` REPLACES tower-http's default
//!   predicate — which is what normally excludes SSE — so the exclusion has
//!   to be restated here explicitly or the encoder would buffer events.

use tower_http::compression::predicate::{Predicate, SizeAbove};
use tower_http::compression::CompressionLayer;

/// Compress only when the response body is at least this many bytes.
/// Mirrors tower-http's default threshold: tiny bodies fit one MTU anyway
/// and gzip/br framing would just add overhead.
const MIN_COMPRESS_BYTES: u16 = 32;

/// Content-type allowlist: text-ish payloads that compress well and are never
/// consumed as raw byte streams with progress accounting.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompressibleContentType;

impl Predicate for CompressibleContentType {
    fn should_compress<B>(&self, response: &http::Response<B>) -> bool
    where
        B: http_body::Body,
    {
        let mime = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if mime == "text/event-stream" {
            return false;
        }
        mime == "application/json"
            || mime.starts_with("text/")
            || mime == "application/javascript"
            || mime == "image/svg+xml"
            || mime == "application/wasm"
    }
}

/// The compression layer installed at the top of the router stack (covers
/// `/api` and the static `ServeDir`; the WebSocket route is an upgrade with
/// no response body and is unaffected).
pub fn compression_layer() -> CompressionLayer<impl Predicate> {
    CompressionLayer::new()
        .compress_when(SizeAbove::new(MIN_COMPRESS_BYTES).and(CompressibleContentType))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_with_content_type(ct: &str) -> http::Response<axum::body::Body> {
        http::Response::builder()
            .header(http::header::CONTENT_TYPE, ct)
            .body(axum::body::Body::empty())
            .unwrap()
    }

    #[test]
    fn allowlist_compresses_text_like_types() {
        let p = CompressibleContentType;
        for ct in [
            "application/json",
            "application/json; charset=utf-8",
            "text/html",
            "text/css",
            "text/javascript",
            "application/javascript",
            "image/svg+xml",
            "application/wasm",
        ] {
            assert!(p.should_compress(&response_with_content_type(ct)), "{ct}");
        }
    }

    #[test]
    fn allowlist_skips_binary_downloads_and_sse() {
        let p = CompressibleContentType;
        for ct in [
            "application/octet-stream",
            "application/zip",
            "image/png",
            "font/woff2",
            "text/event-stream",
            "",
        ] {
            assert!(!p.should_compress(&response_with_content_type(ct)), "{ct}");
        }
        assert!(!p.should_compress(&http::Response::new(axum::body::Body::empty())));
    }
}
