//! Public merchant identity. Every asset is embedded and fixed at build time;
//! these routes do not consult discovery, payment state, or an executor.
use crate::Gateway;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::sync::Arc;

const PAGE: &str = include_str!("branding/index.html");
const STYLE: &[u8] = include_bytes!("branding/style.css");
const ICON: &[u8] = include_bytes!("branding/covenant-icon-02026997.png");
pub(crate) const ICON_PATH: &str = "/brand/covenant-icon-02026997.png";
const FAVICON: &[u8] = include_bytes!("branding/favicon.ico");
const BANNER: &[u8] = include_bytes!("branding/covenant-banner-4b58cf6c.png");
const REVALIDATE: &str = "public, max-age=300";
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const CSP: &str = "default-src 'none'; img-src 'self'; style-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn headers(response: &mut Response, cache: &'static str) {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, cache.parse().unwrap());
    headers.insert(header::CONTENT_SECURITY_POLICY, CSP.parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
}

fn page(resource_url: &str) -> Response {
    // Resolve the origin exclusively from the configured payment resource.
    // Invalid configuration fails closed rather than reflecting Host or a URL
    // containing credentials, path data, query strings, or fragments into HTML.
    let resource = reqwest::Url::parse(resource_url).ok().filter(|resource| {
        resource.as_str().len() <= 512
            && resource.scheme() == "https"
            && resource.host_str().is_some()
            && resource.username().is_empty()
            && resource.password().is_none()
            && resource.path() == "/v1/chat/completions"
            && resource.query().is_none()
            && resource.fragment().is_none()
    });
    let Some(resource) = resource else {
        let mut response = (
            StatusCode::SERVICE_UNAVAILABLE,
            "Merchant metadata unavailable",
        )
            .into_response();
        headers(&mut response, "no-store");
        return response;
    };
    let origin = resource
        .origin()
        .ascii_serialization()
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&#39;");
    let body = PAGE.replace("{{ORIGIN}}", &origin);
    let mut response = ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response();
    headers(&mut response, REVALIDATE);
    response
}

fn asset(bytes: &'static [u8], content_type: &'static str, cache: &'static str) -> Response {
    let mut response = ([(header::CONTENT_TYPE, content_type)], bytes).into_response();
    headers(&mut response, cache);
    response
}

async fn page_http(State(gateway): State<Arc<Gateway>>) -> Response {
    page(gateway.rail.resource_url())
}

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route("/", get(page_http))
        .route(
            "/favicon.ico",
            get(|| async { asset(FAVICON, "image/x-icon", REVALIDATE) }),
        )
        .route(
            ICON_PATH,
            get(|| async { asset(ICON, "image/png", IMMUTABLE) }),
        )
        .route(
            "/brand/covenant-banner-4b58cf6c.png",
            get(|| async { asset(BANNER, "image/png", IMMUTABLE) }),
        )
        .route(
            "/brand/style.css",
            get(|| async { asset(STYLE, "text/css; charset=utf-8", REVALIDATE) }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn official_assets_are_bounded_and_keep_their_provenance() {
        for (bytes, maximum, sha) in [
            (
                ICON,
                56_000,
                "02026997226a417d29b16bd90b19893965217e0ed3d71b80daf7ab1cba10264a",
            ),
            (
                FAVICON,
                3_000,
                "2b5136747b64db4eba4bd7f43c41af6c970bdd4680f4d9c0af4854174c9e4d1a",
            ),
            (
                BANNER,
                162_000,
                "4b58cf6c68c0179b911953aef8dc1aac872860272de2ac0bd1db3104d7d11d4c",
            ),
        ] {
            assert!(bytes.len() <= maximum);
            let hash: String = Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            assert_eq!(hash, sha);
        }
        assert!(PAGE.len() < 4096);
        assert!(STYLE.len() < 4096);
        assert!(!PAGE.contains("<script"));
        assert!(!PAGE.contains("<form"));
    }

    #[tokio::test]
    async fn metadata_origin_is_canonical_and_invalid_configuration_is_private() {
        let response = page("https://compute.example:8443/v1/chat/completions");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let body = std::str::from_utf8(&bytes).unwrap();
        assert!(body.contains(
            "content=\"https://compute.example:8443/brand/covenant-banner-4b58cf6c.png\""
        ));
        assert!(body.contains("href=\"https://compute.example:8443/\""));
        assert!(!body.contains("{{ORIGIN}}"));
        for resource in [
            "http://compute.example/v1/chat/completions",
            "https://user:private@compute.example/v1/chat/completions",
            "https://compute.example/v1/chat/completions?private=value",
            "https://compute.example/v1/chat/completions#private",
            "https://compute.example/other",
        ] {
            let response = page(resource);
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let bytes = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            assert_eq!(bytes.as_ref(), b"Merchant metadata unavailable");
        }
        assert_eq!(
            page(&format!(
                "https://{}.example/v1/chat/completions",
                "x".repeat(512)
            ))
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
