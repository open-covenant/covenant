//! Live ping of the public zauth directory.
//!
//! Disabled unless `COVENANT_ZAUTH_LIVE=1`. Free, read-only,
//! unauthenticated. Confirms our serde structs still parse what zauth
//! actually returns; if this passes locally and later breaks, zauth
//! changed their wire format.
//!
//! Run with:
//!     COVENANT_ZAUTH_LIVE=1 cargo test -p covenant-zauth --test live_directory

use covenant_zauth::{DirectoryClient, ListQuery};

fn live() -> bool {
    std::env::var("COVENANT_ZAUTH_LIVE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

#[tokio::test(flavor = "current_thread")]
async fn live_directory_list_returns_parseable_page() {
    if !live() {
        eprintln!("skipping: set COVENANT_ZAUTH_LIVE=1 to enable");
        return;
    }
    let client = DirectoryClient::new(reqwest::Client::new());
    let page = client
        .list(ListQuery {
            verified: Some(true),
            network: None,
            limit: Some(1),
            offset: None,
        })
        .await
        .expect("live list");
    assert!(page.stats.total_endpoints > 0, "stats.total_endpoints");
    assert!(page.stats.verified > 0, "stats.verified");
    assert_eq!(page.endpoints.len(), 1, "limit=1 honored");
    let ep = &page.endpoints[0];
    assert!(ep.verified, "verified=true honored");
    assert!(!ep.url.is_empty(), "endpoint has url");
}
