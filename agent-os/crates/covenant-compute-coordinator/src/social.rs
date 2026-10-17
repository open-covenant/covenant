//! Automatic sharing metadata for public HTML, using the website's branded
//! card renderer. Never derives origins from request headers or request data.
use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{header, HeaderValue, Method},
    middleware::Next,
    response::Response,
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
};

const MAX_HTML_BYTES: usize = 1024 * 1024;
const ORIGIN: &str = "https://compute-api.opencovenant.org";
const IMAGE_ORIGIN: &str = "https://opencovenant.org/og/compute-api";

/// Only anonymous, successful HTML gets public sharing metadata. Financial,
/// authenticated and private responses keep their original representation.
pub async fn metadata(req: Request, next: Next) -> Response {
    let path = canonical_path(req.uri().path());
    let head = req.method() == Method::HEAD;
    let eligible = matches!(*req.method(), Method::GET | Method::HEAD)
        && !req.headers().contains_key(header::AUTHORIZATION)
        && !req.headers().contains_key(header::COOKIE)
        && path.as_ref().is_some_and(|path| {
            !["/admin", "/vault", "/federation"]
                .iter()
                .any(|prefix| path == prefix || path.starts_with(&format!("{prefix}/")))
        });
    let mut response = next.run(req).await;
    if !eligible || !response.status().is_success() || !public_html(&response) {
        return response;
    }
    if head {
        // HEAD handlers retain their own behavior. Omit representation fields
        // whose values depend on the transformed GET body, which is absent.
        invalidate_representation_headers(response.headers_mut());
        response.headers_mut().remove(header::CONTENT_LENGTH);
        return response;
    }
    let (mut parts, mut body) = response.into_parts();
    let mut frames = VecDeque::new();
    let mut bytes = Vec::new();
    // Retain original frames until parsing succeeds. An oversized, malformed,
    // non-UTF8 or interrupted response is passed through without truncation.
    while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        match frame {
            Ok(frame) => {
                if let Some(data) = frame.data_ref() {
                    if bytes.len().saturating_add(data.len()) > MAX_HTML_BYTES {
                        frames.push_back(Ok(frame));
                        return Response::from_parts(parts, Body::new(ReplayBody { frames, body }));
                    }
                    bytes.extend_from_slice(data);
                } else {
                    // Do not change integrity trailers or other streamed metadata.
                    frames.push_back(Ok(frame));
                    return Response::from_parts(parts, Body::new(ReplayBody { frames, body }));
                }
                frames.push_back(Ok(frame));
            }
            Err(error) => {
                frames.push_back(Err(error));
                return Response::from_parts(parts, Body::new(ReplayBody { frames, body }));
            }
        }
    }
    let updated = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|html| inject(html, path.as_deref().unwrap_or("/")));
    let Some(updated) = updated else {
        return Response::from_parts(parts, Body::new(ReplayBody { frames, body }));
    };
    // Validators and digests for the old bytes would be wrong. Preserve cache
    // policy and CSP, and advertise the length of the transformed document.
    invalidate_representation_headers(&mut parts.headers);
    parts
        .headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(updated.len()));
    Response::from_parts(parts, Body::from(updated))
}

fn invalidate_representation_headers(headers: &mut axum::http::HeaderMap) {
    for name in [
        "etag",
        "content-md5",
        "digest",
        "content-digest",
        "repr-digest",
        "accept-ranges",
    ] {
        headers.remove(name);
    }
}

fn public_html(response: &Response) -> bool {
    let headers = response.headers();
    let html = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("text/html")
        });
    let private = headers
        .get_all(header::CACHE_CONTROL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|v| {
            matches!(
                v.trim()
                    .split('=')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str(),
                "private" | "no-store"
            )
        });
    let personal = headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "*" | "cookie" | "authorization"
            )
        });
    html && !private
        && !personal
        && !headers.contains_key(header::SET_COOKIE)
        && !headers.contains_key(header::CONTENT_ENCODING)
        && !headers.contains_key(header::CONTENT_RANGE)
}

fn canonical_path(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return Some("/".into());
    }
    let segments = path[1..].split('/').collect::<Vec<_>>();
    if segments.len() > 8
        || segments.iter().any(|segment| {
            segment.is_empty()
                || segment.len() > 80
                || matches!(*segment, "." | "..")
                || !segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
    {
        return None;
    }
    Some(path.into())
}

struct ReplayBody {
    frames: VecDeque<Result<Frame<Bytes>, axum::Error>>,
    body: Body,
}
impl HttpBody for ReplayBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        if let Some(frame) = self.frames.pop_front() {
            return Poll::Ready(Some(frame));
        }
        Pin::new(&mut self.body).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.frames.is_empty() && self.body.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

struct Tag<'a> {
    start: usize,
    end: usize,
    name: String,
    closing: bool,
    attributes: Vec<(String, &'a str)>,
}
impl Tag<'_> {
    fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| *value)
    }
}

// A deliberately small head tokenizer, not a general HTML rewriter. Quoted
// attributes, comments and raw script/style/title text cannot create fake tags.
// Malformed markup returns None, leaving the original document untouched.
fn next_tag(html: &str, mut from: usize) -> Option<Tag<'_>> {
    let bytes = html.as_bytes();
    loop {
        let start = from + html.get(from..)?.find('<')?;
        if html[start..].starts_with("<!--") {
            from = start + 4 + html[start + 4..].find("-->")? + 3;
            continue;
        }
        let mut pos = start + 1;
        let closing = bytes.get(pos) == Some(&b'/');
        if closing {
            pos += 1;
        }
        let name_start = pos;
        while bytes
            .get(pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'!')
        {
            pos += 1;
        }
        if pos == name_start {
            return None;
        }
        let name = html[name_start..pos].to_ascii_lowercase();
        let mut attributes = Vec::new();
        loop {
            while bytes.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
                pos += 1;
            }
            if bytes.get(pos) == Some(&b'/') {
                pos += 1;
            }
            if bytes.get(pos) == Some(&b'>') {
                return Some(Tag {
                    start,
                    end: pos + 1,
                    name,
                    closing,
                    attributes,
                });
            }
            let key_start = pos;
            while bytes
                .get(pos)
                .is_some_and(|b| b.is_ascii_alphanumeric() || b"-_:".contains(b))
            {
                pos += 1;
            }
            if pos == key_start {
                return None;
            }
            let key = html[key_start..pos].to_ascii_lowercase();
            if attributes.iter().any(|(k, _)| *k == key) {
                return None;
            }
            while bytes.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
                pos += 1;
            }
            let mut value = "";
            if bytes.get(pos) == Some(&b'=') {
                pos += 1;
                while bytes.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
                    pos += 1;
                }
                if let Some(quote @ (b'\'' | b'"')) = bytes.get(pos).copied() {
                    pos += 1;
                    let value_start = pos;
                    while *bytes.get(pos)? != quote {
                        pos += 1;
                    }
                    value = &html[value_start..pos];
                    pos += 1;
                } else {
                    let value_start = pos;
                    while bytes
                        .get(pos)
                        .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'>')
                    {
                        pos += 1;
                    }
                    if pos == value_start {
                        return None;
                    }
                    value = &html[value_start..pos];
                }
            }
            attributes.push((key, value));
        }
    }
}

fn raw_close<'a>(html: &'a str, lower: &str, tag: &Tag<'_>) -> Option<Tag<'a>> {
    let pattern = format!("</{}", tag.name);
    let mut from = tag.end;
    loop {
        let at = from + lower.get(from..)?.find(&pattern)?;
        let closing = next_tag(html, at)?;
        if closing.closing && closing.name == tag.name {
            return Some(closing);
        }
        from = closing.end;
    }
}

fn inject(html: &str, path: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut pos = 0;
    let head = loop {
        let tag = next_tag(html, pos)?;
        if tag.name == "head" && !tag.closing {
            break tag;
        }
        if !matches!(tag.name.as_str(), "!doctype" | "html") {
            return None;
        }
        pos = tag.end;
    };
    pos = head.end;
    let mut title = None;
    let mut description = None;
    let mut remove = Vec::new();
    let close = loop {
        let tag = next_tag(html, pos)?;
        if tag.name == "head" && tag.closing {
            break tag;
        }
        if matches!(tag.name.as_str(), "body" | "html" | "head") {
            return None;
        }
        pos = tag.end;
        if !tag.closing && matches!(tag.name.as_str(), "script" | "style" | "title") {
            let close = raw_close(html, &lower, &tag)?;
            if tag.name == "title" && title.is_none() {
                title = Some(clean_text(&html[tag.end..close.start], 200));
            }
            pos = close.end;
        } else if !tag.closing && tag.name == "meta" {
            let key = tag
                .attribute("property")
                .or_else(|| tag.attribute("name"))
                .unwrap_or("")
                .to_ascii_lowercase();
            if key == "description" && description.is_none() {
                description = tag.attribute("content").map(|v| clean_text(v, 400));
            }
            if key.starts_with("og:") || key.starts_with("twitter:") {
                remove.push((tag.start, tag.end));
            }
        } else if !tag.closing
            && tag.name == "link"
            && tag.attribute("rel").is_some_and(|v| {
                v.split_ascii_whitespace()
                    .any(|v| v.eq_ignore_ascii_case("canonical"))
            })
        {
            remove.push((tag.start, tag.end));
        }
    };
    let title = escape(
        title
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or("Covenant Compute"),
    );
    let description = escape(
        description
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or("Explore the public Covenant Compute network."),
    );
    let (canonical, image) = if path == "/prices/page" {
        (
            "https://opencovenant.org/compute-prices".into(),
            "https://opencovenant.org/og/compute-prices.png".into(),
        )
    } else {
        let image_path = if path == "/" { "/index" } else { path };
        (
            format!("{ORIGIN}{path}"),
            format!("{IMAGE_ORIGIN}{image_path}.png"),
        )
    };
    let tags = format!(
        r#"
<link rel="canonical" href="{canonical}">
<meta property="og:type" content="website">
<meta property="og:site_name" content="Covenant Compute">
<meta property="og:title" content="{title}">
<meta property="og:description" content="{description}">
<meta property="og:url" content="{canonical}">
<meta property="og:image" content="{image}">
<meta property="og:image:secure_url" content="{image}">
<meta property="og:image:type" content="image/png">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta property="og:image:alt" content="{title}">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:site" content="@OpenCovenant">
<meta name="twitter:title" content="{title}">
<meta name="twitter:description" content="{description}">
<meta name="twitter:image" content="{image}">
<meta name="twitter:image:alt" content="{title}">
"#
    );
    let mut result = String::with_capacity(html.len() + tags.len());
    let mut copied = 0;
    for (start, end) in remove {
        result.push_str(&html[copied..start]);
        copied = end;
    }
    result.push_str(&html[copied..close.start]);
    result.push_str(&tags);
    result.push_str(&html[close.start..]);
    Some(result)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn clean_text(value: &str, max_chars: usize) -> String {
    let mut decoded = String::new();
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        decoded.push_str(&rest[..index]);
        rest = &rest[index..];
        let entity = rest
            .find(';')
            .filter(|end| *end <= 12)
            .map(|end| (&rest[1..end], end));
        let character = entity.and_then(|(name, _)| match name {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "nbsp" => Some(' '),
            "middot" => Some('·'),
            "ndash" => Some('–'),
            "mdash" => Some('—'),
            "hellip" => Some('…'),
            _ if name.starts_with("#x") || name.starts_with("#X") => {
                u32::from_str_radix(&name[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if name.starts_with('#') => name[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        });
        if let (Some(character), Some((_, end))) = (character, entity) {
            decoded.push(character);
            rest = &rest[end + 1..];
        } else {
            decoded.push('&');
            rest = &rest[1..];
        }
    }
    decoded.push_str(rest);
    decoded
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::StatusCode,
        response::{Html, IntoResponse},
        routing::get,
        Router,
    };
    const PAGE: &str = "<!doctype html><html><head><title>GPU &amp; price &quot;history&quot;</title><meta name='description' content='Quotes &amp; rates'><meta property='og:image' content='https://old.invalid/x.png'><meta name='twitter:card' content='summary'><link rel='canonical' href='https://old.invalid/'></head><body>Public page</body></html>";

    async fn serve(router: Router) -> (reqwest::Client, String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            format!("http://{address}"),
            task,
        )
    }
    fn layer(router: Router) -> Router {
        router.layer(axum::middleware::from_fn(metadata))
    }

    #[tokio::test]
    async fn new_html_route_inherits_cards_and_ignores_host_and_query() {
        let app = layer(Router::new().route(
            "/future/report",
            get(|| async {
                (
                    [
                        ("cache-control", "public, max-age=60"),
                        ("etag", "old"),
                        ("digest", "old"),
                        ("content-security-policy", "default-src 'none'"),
                    ],
                    Html(PAGE),
                )
            }),
        ));
        let (client, base, task) = serve(app).await;
        let response = client
            .get(format!(
                "{base}/future/report?title=%22%3Eevil&url=https://evil.invalid"
            ))
            .header("host", "evil.invalid")
            .header("x-forwarded-host", "evil.invalid")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "public, max-age=60");
        assert_eq!(
            response.headers()["content-security-policy"],
            "default-src 'none'"
        );
        assert!(!response.headers().contains_key("etag"));
        assert!(!response.headers().contains_key("digest"));
        let length = response.content_length().unwrap();
        let html = response.text().await.unwrap();
        assert_eq!(length as usize, html.len());
        assert!(html.contains("https://compute-api.opencovenant.org/future/report"));
        assert!(html.contains("https://opencovenant.org/og/compute-api/future/report.png"));
        assert!(html.contains("GPU &amp; price &quot;history&quot;"));
        assert!(html.contains("Quotes &amp; rates"));
        assert!(html.contains("summary_large_image"));
        assert!(html.contains("@OpenCovenant"));
        assert!(html.contains("content=\"1200\""));
        assert!(html.contains("content=\"630\""));
        assert_eq!(html.matches("property=\"og:image\"").count(), 1);
        assert_eq!(html.matches("name=\"twitter:card\"").count(), 1);
        assert_eq!(html.matches("rel=\"canonical\"").count(), 1);
        assert!(!html.contains("evil.invalid"));
        assert!(!html.contains("old.invalid"));
        let head = client
            .head(format!("{base}/future/report"))
            .send()
            .await
            .unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert!(head.bytes().await.unwrap().is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn production_price_router_emits_cards_and_keeps_api_and_disabled_page_unchanged() {
        use crate::{CoordinatorConfig, CoordinatorState, MockPayout, NoReputation};
        use covenant_audit::InMemoryAuditLog;
        use covenant_identity::LocalIdentity;
        use std::sync::Arc;
        for enabled in [false, true] {
            let state = CoordinatorState::new(
                LocalIdentity::generate("social@test"),
                CoordinatorConfig {
                    public_price_history: enabled,
                    ..Default::default()
                },
                Arc::new(NoReputation),
                Arc::new(MockPayout::new()),
                Arc::new(InMemoryAuditLog::new()),
            );
            let (client, base, task) = serve(crate::router(state)).await;
            let response = client
                .get(format!("{base}/prices/page"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if enabled {
                    StatusCode::OK
                } else {
                    StatusCode::NOT_FOUND
                }
            );
            let html = response.text().await.unwrap();
            assert_eq!(html.contains("property=\"og:image\""), enabled);
            if enabled {
                assert!(html.contains("GPU price history · Covenant Compute"));
                assert!(html.contains("https://opencovenant.org/og/compute-prices.png"));
                assert!(html.contains(
                    "rel=\"canonical\" href=\"https://opencovenant.org/compute-prices\""
                ));
            }
            let redirect = client.get(format!("{base}/prices")).send().await.unwrap();
            assert_eq!(
                redirect.status(),
                if enabled {
                    StatusCode::PERMANENT_REDIRECT
                } else {
                    StatusCode::NOT_FOUND
                }
            );
            if enabled {
                assert_eq!(
                    redirect.headers()["location"],
                    "https://opencovenant.org/compute-prices"
                );
            }
            let json = client.get(format!("{base}/health")).send().await.unwrap();
            assert_eq!(json.headers()[header::CONTENT_TYPE], "application/json");
            assert!(json
                .json::<serde_json::Value>()
                .await
                .unwrap()
                .get("status")
                .is_some());
            task.abort();
        }
    }

    #[tokio::test]
    async fn private_errors_json_post_and_oversized_responses_are_unchanged() {
        let oversized = format!(
            "<html><head><title>Large</title></head><body>{}</body></html>",
            "x".repeat(MAX_HTML_BYTES)
        );
        let app = layer(
            Router::new()
                .route(
                    "/public",
                    get(|| async { Html(PAGE) }).post(|| async { Html(PAGE) }),
                )
                .route(
                    "/private",
                    get(|| async { ([("cache-control", "private, max-age=60")], Html(PAGE)) }),
                )
                .route(
                    "/no-store",
                    get(|| async { ([("cache-control", "no-store")], Html(PAGE)) }),
                )
                .route(
                    "/set-cookie",
                    get(|| async { ([("set-cookie", "session=abc")], Html(PAGE)) }),
                )
                .route(
                    "/vary",
                    get(|| async { ([("vary", "Accept, Authorization")], Html(PAGE)) }),
                )
                .route("/admin/page", get(|| async { Html(PAGE) }))
                .route(
                    "/error",
                    get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, Html(PAGE)) }),
                )
                .route(
                    "/json",
                    get(|| async { axum::Json(serde_json::json!({"html": PAGE})) }),
                )
                .route(
                    "/malformed",
                    get(|| async { Html("<html><head><title>Missing end") }),
                )
                .route(
                    "/large",
                    get({
                        let oversized = oversized.clone();
                        move || async move { ([("etag", "unchanged")], Html(oversized)) }
                    }),
                ),
        );
        let (client, base, task) = serve(app).await;
        for route in [
            "private",
            "no-store",
            "set-cookie",
            "vary",
            "admin/page",
            "error",
        ] {
            assert_eq!(
                client
                    .get(format!("{base}/{route}"))
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
                PAGE,
                "{route}"
            );
        }
        for (name, value) in [
            ("authorization", "Bearer private"),
            ("cookie", "session=private"),
        ] {
            assert_eq!(
                client
                    .get(format!("{base}/public"))
                    .header(name, value)
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
                PAGE
            );
        }
        assert_eq!(
            client
                .post(format!("{base}/public"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            PAGE
        );
        let json = client
            .get(format!("{base}/json"))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(json["html"], PAGE);
        assert_eq!(
            client
                .get(format!("{base}/malformed"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "<html><head><title>Missing end"
        );
        let large = client.get(format!("{base}/large")).send().await.unwrap();
        assert_eq!(large.headers()["etag"], "unchanged");
        assert_eq!(large.text().await.unwrap(), oversized);
        task.abort();
    }

    #[test]
    fn head_tokenizer_escapes_titles_skips_comments_scripts_and_replaces_duplicates() {
        let html = "<HTML><HEAD><!-- <meta property='og:image' content='comment'> --><script>const x = \"<meta name='description' content='evil'><title>evil</title>\";</script><STYLE>/* <title>evil</title> */</STYLE><TITLE>A &#x22;&gt;&lt;script&gt; &amp; B</TITLE><META NAME=description CONTENT='One &quot;quote&quot;'><meta property='og:title' content='old'><meta name='twitter:image' content='old'></HEAD><BODY>x</BODY></HTML>";
        let changed = inject(html, "/prices").unwrap();
        assert!(changed
            .contains("property=\"og:title\" content=\"A &quot;&gt;&lt;script&gt; &amp; B\""));
        assert!(changed.contains("property=\"og:description\" content=\"One &quot;quote&quot;\""));
        assert!(changed.contains("const x ="));
        assert!(!changed.contains("content='old'"));
        assert_eq!(
            inject(&changed, "/prices")
                .unwrap()
                .matches("property=\"og:title\"")
                .count(),
            1
        );
        for malformed in [
            "<html><head><title>x</title>",
            "<html><head><meta name='description content=x></head>",
            "<html><head><!-- </head>",
            "<html><head><script>missing</head>",
            "<html><head><meta name=x name=y></head>",
        ] {
            assert!(inject(malformed, "/prices").is_none(), "{malformed}");
        }
    }

    #[test]
    fn safe_paths_match_card_renderer_contract() {
        for path in ["/prices", "/a-b/c_d/v1.2", "/a/b/c/d/e/f/g/h"] {
            assert_eq!(canonical_path(path).as_deref(), Some(path));
        }
        assert_eq!(canonical_path("/prices/").as_deref(), Some("/prices"));
        assert_eq!(canonical_path("/").as_deref(), Some("/"));
        for path in [
            "//evil.invalid/prices",
            "/a//b",
            "/a/../b",
            "/./a",
            "/%22",
            "/a\\b",
            "/a/b/c/d/e/f/g/h/i",
            "/a?b",
            "/ümlaut",
        ] {
            assert!(canonical_path(path).is_none(), "{path}");
        }
        assert!(canonical_path(&format!("/{}", "x".repeat(81))).is_none());
        assert!(inject(PAGE, "/")
            .unwrap()
            .contains("https://opencovenant.org/og/compute-api/index.png"));
    }

    #[tokio::test]
    async fn invalid_utf8_passes_through_without_truncation() {
        let app = layer(Router::new().route(
            "/invalid",
            get(|| async {
                let mut response = (
                    [("content-type", "text/html")],
                    Bytes::from_static(b"<head>\xff</head>"),
                )
                    .into_response();
                response
                    .headers_mut()
                    .insert("etag", HeaderValue::from_static("original"));
                response
            }),
        ));
        let (client, base, task) = serve(app).await;
        let response = client.get(format!("{base}/invalid")).send().await.unwrap();
        assert_eq!(response.headers()["etag"], "original");
        assert_eq!(response.bytes().await.unwrap(), &b"<head>\xff</head>"[..]);
        task.abort();
    }
}
