//! Tool provider abstraction for Covenant.
//!
//! Three implementations of [`SearchProvider`]: [`MockSearch`] for
//! tests, [`BraveSearch`] for `api.search.brave.com`, and
//! [`SerpApiSearch`] for `serpapi.com`. [`SearchConfig`] parses the
//! `[search]` section of `~/.covenant/secrets.toml`, and
//! [`pick_search`] returns the configured provider, falling back to
//! [`MockSearch`] when nothing is configured.

#![deny(unsafe_code)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use tracing::debug;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("search returned no hits")]
    Empty,
    #[error("search error ({status}): {body}")]
    Status { status: u16, body: String },
    #[error("missing api key for {0}")]
    MissingKey(&'static str),
}

#[async_trait]
pub trait SearchProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError>;
}

// ---------- Mock ----------

pub struct MockSearch {
    pub canned: Vec<SearchHit>,
}

impl MockSearch {
    pub fn new(canned: Vec<SearchHit>) -> Self {
        Self { canned }
    }

    pub fn stub() -> Self {
        Self {
            canned: vec![SearchHit {
                title: "stub result".into(),
                url: "stub://no-real-search".into(),
                snippet:
                    "covenant-tools mock — configure a real provider in ~/.covenant/secrets.toml"
                        .into(),
            }],
        }
    }
}

#[async_trait]
impl SearchProvider for MockSearch {
    fn name(&self) -> &'static str {
        "mock"
    }
    async fn search(&self, _query: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError> {
        let mut out = self.canned.clone();
        out.truncate(limit);
        Ok(out)
    }
}

// ---------- Brave ----------

pub struct BraveSearch {
    pub api_key: String,
    client: reqwest::Client,
}

impl BraveSearch {
    pub fn new(api_key: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("reqwest client");
        Self {
            api_key: api_key.into(),
            client,
        }
    }
}

#[derive(Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    description: Option<String>,
}

#[async_trait]
impl SearchProvider for BraveSearch {
    fn name(&self) -> &'static str {
        "brave"
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError> {
        if self.api_key.is_empty() {
            return Err(SearchError::MissingKey("brave"));
        }
        let resp = self
            .client
            .get("https://api.search.brave.com/res/v1/web/search")
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .query(&[("q", query), ("count", &limit.to_string())])
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SearchError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: BraveResponse = resp.json().await?;
        let hits = parsed
            .web
            .map(|w| w.results)
            .unwrap_or_default()
            .into_iter()
            .map(|r| SearchHit {
                title: r.title,
                url: r.url,
                snippet: r.description.unwrap_or_default(),
            })
            .take(limit)
            .collect::<Vec<_>>();
        if hits.is_empty() {
            return Err(SearchError::Empty);
        }
        Ok(hits)
    }
}

// ---------- SerpAPI ----------

pub struct SerpApiSearch {
    pub api_key: String,
    client: reqwest::Client,
}

impl SerpApiSearch {
    pub fn new(api_key: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("reqwest client");
        Self {
            api_key: api_key.into(),
            client,
        }
    }
}

#[derive(Deserialize)]
struct SerpApiResponse {
    organic_results: Option<Vec<SerpApiResult>>,
}

#[derive(Deserialize)]
struct SerpApiResult {
    title: String,
    link: String,
    snippet: Option<String>,
}

#[async_trait]
impl SearchProvider for SerpApiSearch {
    fn name(&self) -> &'static str {
        "serpapi"
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError> {
        if self.api_key.is_empty() {
            return Err(SearchError::MissingKey("serpapi"));
        }
        let resp = self
            .client
            .get("https://serpapi.com/search")
            .query(&[
                ("engine", "google"),
                ("q", query),
                ("api_key", &self.api_key),
                ("num", &limit.to_string()),
            ])
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SearchError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: SerpApiResponse = resp.json().await?;
        let hits = parsed
            .organic_results
            .unwrap_or_default()
            .into_iter()
            .map(|r| SearchHit {
                title: r.title,
                url: r.link,
                snippet: r.snippet.unwrap_or_default(),
            })
            .take(limit)
            .collect::<Vec<_>>();
        if hits.is_empty() {
            return Err(SearchError::Empty);
        }
        Ok(hits)
    }
}

// ---------- secrets.toml + auto pick ----------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchConfig {
    #[serde(default)]
    pub search: Option<SearchSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchSection {
    pub provider: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

impl SearchConfig {
    pub fn from_path(p: &Path) -> Result<Self, SearchError> {
        if !p.exists() {
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(p)?;
        Ok(toml::from_str(&s)?)
    }
}

pub fn search_from_config(cfg: &SearchConfig) -> Option<Box<dyn SearchProvider>> {
    let s = cfg.search.as_ref()?;
    match s.provider.as_str() {
        "mock" => Some(Box::new(MockSearch::stub())),
        "brave" => {
            let key = s.api_key.clone().unwrap_or_default();
            Some(Box::new(BraveSearch::new(key)))
        }
        "serpapi" => {
            let key = s.api_key.clone().unwrap_or_default();
            Some(Box::new(SerpApiSearch::new(key)))
        }
        other => {
            debug!(provider = %other, "unknown search provider in config");
            None
        }
    }
}

pub fn pick_search(secrets_path: &Path) -> Box<dyn SearchProvider> {
    if let Ok(cfg) = SearchConfig::from_path(secrets_path) {
        if let Some(p) = search_from_config(&cfg) {
            return p;
        }
    }
    Box::new(MockSearch::stub())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_search_returns_canned_hits() {
        let s = MockSearch::stub();
        let r = s.search("anything", 10).await.unwrap();
        assert!(!r.is_empty());
        assert!(r[0].url.starts_with("stub://"));
    }

    #[tokio::test]
    async fn mock_search_respects_limit() {
        let s = MockSearch::new(vec![
            SearchHit {
                title: "a".into(),
                url: "u://a".into(),
                snippet: "".into(),
            },
            SearchHit {
                title: "b".into(),
                url: "u://b".into(),
                snippet: "".into(),
            },
            SearchHit {
                title: "c".into(),
                url: "u://c".into(),
                snippet: "".into(),
            },
        ]);
        assert_eq!(s.search("x", 2).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn brave_without_key_returns_missing_key() {
        let s = BraveSearch::new("");
        assert!(matches!(
            s.search("x", 5).await,
            Err(SearchError::MissingKey("brave"))
        ));
    }

    #[tokio::test]
    async fn serpapi_without_key_returns_missing_key() {
        let s = SerpApiSearch::new("");
        assert!(matches!(
            s.search("x", 5).await,
            Err(SearchError::MissingKey("serpapi"))
        ));
    }

    #[test]
    fn search_config_parses_brave_block() {
        let toml_src = r#"
[search]
provider = "brave"
api_key = "BSA-test"
"#;
        let cfg: SearchConfig = toml::from_str(toml_src).unwrap();
        let p = search_from_config(&cfg).unwrap();
        assert_eq!(p.name(), "brave");
    }

    #[test]
    fn search_config_unknown_provider_returns_none() {
        let toml_src = r#"
[search]
provider = "made-up"
"#;
        let cfg: SearchConfig = toml::from_str(toml_src).unwrap();
        assert!(search_from_config(&cfg).is_none());
    }

    #[test]
    fn pick_search_falls_back_to_mock_when_no_file() {
        let dir = std::env::temp_dir();
        let nope = dir.join("covenant-no-search.toml");
        let _ = std::fs::remove_file(&nope);
        let p = pick_search(&nope);
        assert_eq!(p.name(), "mock");
    }
}
