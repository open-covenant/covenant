//! Public, read-only presentation of the price-history API.
use crate::CoordinatorState;
use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, Redirect},
};

/// Preserve shared links while keeping the public experience on the main site.
pub async fn redirect(
    State(state): State<CoordinatorState>,
) -> Result<Redirect, (StatusCode, &'static str)> {
    if !state.config().public_price_history {
        return Err((StatusCode::NOT_FOUND, "public price history is disabled"));
    }
    Ok(Redirect::permanent(
        "https://opencovenant.org/compute-prices",
    ))
}

/// Public source document consumed by the main site's same-origin page proxy.
pub async fn page(
    State(state): State<CoordinatorState>,
) -> Result<(HeaderMap, Html<&'static str>), (StatusCode, &'static str)> {
    if !state.config().public_price_history {
        return Err((StatusCode::NOT_FOUND, "public price history is disabled"));
    }
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; font-src data:; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
    ));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    Ok((headers, Html(include_str!("prices.html"))))
}
