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

    #[test]
    fn search_hit_serde_pins_three_required_fields() {
        // SearchHit is the wire form every search-tool result flows
        // through, from BraveSearch / SerpApiSearch JSON deserialisation
        // up into the MCP ToolCallResult content array and finally into
        // the LLM agent's context. Three strictly required String fields:
        // title, url, snippet. No #[serde(default)] or
        // #[serde(skip_serializing_if)] attributes — the wire form must
        // always carry the three keys. A refactor that flipped one to
        // optional would silently let a malformed provider response
        // decode with an empty-string default and the agent would see a
        // half-populated result with no signal that the integration
        // dropped a field.
        let hit = SearchHit {
            title: "t".into(),
            url: "u".into(),
            snippet: "s".into(),
        };
        let wire = serde_json::to_value(&hit).unwrap();
        let obj = wire
            .as_object()
            .expect("SearchHit serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["snippet", "title", "url"],
            "SearchHit wire form must be exactly three keys; a skip_serializing_if on any field would silently shrink the wire form when a provider returned an empty value",
        );

        let back: SearchHit = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, hit,
            "SearchHit must round-trip through serde_json verbatim — the PartialEq + Eq derive is the contract every MCP ToolCallResult consumer leans on",
        );

        for required in ["title", "url", "snippet"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<SearchHit>(serde_json::Value::Object(missing)).is_err(),
                "SearchHit wire form must reject a payload missing {required:?}; a stray #[serde(default)] would silently let a provider response decode with an empty-string default and the agent would see a half-populated result",
            );
        }

        let empty_snippet = SearchHit {
            title: "t".into(),
            url: "u".into(),
            snippet: String::new(),
        };
        let wire = serde_json::to_value(&empty_snippet).unwrap();
        assert_eq!(
            wire.as_object().unwrap().len(),
            3,
            "empty-snippet SearchHit must still surface all three keys on the wire — pinning no skip_serializing_if = String::is_empty regression",
        );
        let back: SearchHit = serde_json::from_value(wire).unwrap();
        assert_eq!(
            back, empty_snippet,
            "empty snippet must round-trip verbatim — a String::is_empty skip would silently drop the key and produce a two-key decode",
        );
    }

    #[tokio::test]
    async fn mock_search_returns_canned_hits() {
        let s = MockSearch::stub();
        let r = s.search("anything", 10).await.unwrap();
        assert!(!r.is_empty());
        assert!(r[0].url.starts_with("stub://"));
    }

    #[test]
    fn mock_search_stub_pins_single_hit_with_operator_facing_secrets_path_hint() {
        // MockSearch::stub (line 60-70) is the no-config fallback the
        // daemon returns from pick_search and search_from_config when
        // no real search provider is configured. Its single canned
        // SearchHit is the operator-facing surface that tells
        // operators where to configure a real provider — title,
        // url, and snippet together form an inline 'why is search
        // broken and how do I fix it' breadcrumb.
        //
        // mock_search_returns_canned_hits (above) asserts !r.is_empty()
        // and r[0].url.starts_with('stub://') but never pins the exact
        // shape, count, or operator-facing snippet content. This pin
        // closes the help-text contract so a refactor that strips the
        // secrets.toml path or switches to multiple hits surfaces as
        // a parse-time test failure rather than a silent operator-UX
        // degradation.
        let s = MockSearch::stub();
        assert_eq!(
            s.canned.len(),
            1,
            "MockSearch::stub must produce exactly one hit — a refactor \
             that switched to multiple canned hits under a 'demonstrate \
             the search result shape' rationale would silently change \
             the response shape so downstream consumers expecting one \
             hit on the no-config path (e.g., a CLI banner that prints \
             'using stub fallback' iff results.len() == 1) would \
             surface as multi-hit anomalies",
        );
        assert_eq!(
            s.canned[0].title, "stub result",
            "title must be the exact string 'stub result' so operator-\
             facing dashboards and support tickets can grep for it as \
             the canonical no-config marker",
        );
        assert_eq!(
            s.canned[0].url, "stub://no-real-search",
            "url must be the exact string 'stub://no-real-search' — \
             a refactor that swapped the scheme to 'mock://' under an \
             'align with the provider name' rationale would silently \
             break every operator-facing dashboard that grep'd for the \
             exact url, even though the existing starts_with('stub://') \
             test would still pass",
        );
        assert!(
            s.canned[0].snippet.contains("~/.covenant/secrets.toml"),
            "snippet must contain the secrets.toml path so an operator \
             seeing the stub result has the exact path to update — a \
             refactor that 'cleaned up' the snippet by removing the \
             internal config path would silently strip the breadcrumb \
             that converts a 'why is search broken' question into a \
             self-service fix; got snippet: {snippet:?}",
            snippet = s.canned[0].snippet,
        );
        assert!(
            s.canned[0].snippet.contains("covenant-tools mock"),
            "snippet must contain the 'covenant-tools mock' identifier \
             so operators can grep for the exact string in support \
             tickets and unambiguously identify the no-config fallback; \
             got snippet: {snippet:?}",
            snippet = s.canned[0].snippet,
        );
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
    async fn mock_search_truncation_arms_pin_zero_equal_and_oversized_limits_and_query_independence(
    ) {
        // MockSearch::search (line 78-82) clones self.canned and calls
        // Vec::truncate(limit), returning the truncated owned Vec. The
        // existing mock_search_respects_limit test only pins the strict
        // limit < canned.len() arm (limit=2 against canned.len()=3).
        // Unpinned arms: limit=0 (returns empty), limit==canned.len()
        // (returns all without truncation), limit>canned.len()
        // (Vec::truncate is a no-op). The _query argument is intentionally
        // ignored — pick_search's no-config fallback to MockSearch::stub
        // must surface deterministic canned hits regardless of operator
        // query.
        //
        // A refactor that swapped Vec::truncate for slice indexing
        // (e.g., self.canned[..limit].to_vec()) would panic at runtime
        // when limit > canned.len(); a refactor that began filtering by
        // query would break the stub determinism every dashboard and
        // agent context relies on; a refactor that returned a slice
        // borrow would change the SearchProvider trait contract from
        // owned Vec<SearchHit> to a borrow.
        let canned = vec![
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
        ];
        let s = MockSearch::new(canned.clone());

        let zero = s.search("any-query", 0).await.unwrap();
        assert!(
            zero.is_empty(),
            "limit=0 must surface an empty Vec — a refactor that swapped \
             Vec::truncate for explicit slice indexing (canned[..0]) would \
             still produce an empty slice, but a refactor that special-cased \
             limit=0 to mean 'unbounded' would silently exfiltrate the full \
             canned set to callers that intended to suppress results"
        );

        let exact = s.search("any-query", canned.len()).await.unwrap();
        assert_eq!(
            exact, canned,
            "limit==canned.len()=3 must surface all three canned hits in \
             insertion order — Vec::truncate(canned.len()) is a no-op, and \
             a refactor that pre-checked `limit < canned.len()` before \
             truncating would still pass this arm; a refactor that flipped \
             the comparison to `<=` (e.g., to drop the boundary case as a \
             defensive guard) would silently shrink the result by one when \
             callers asked for exactly canned.len() hits"
        );

        let oversized = s.search("any-query", canned.len() + 7).await.unwrap();
        assert_eq!(
            oversized, canned,
            "limit>canned.len() must surface all canned hits without \
             panicking — Vec::truncate saturates when limit > Vec::len. A \
             refactor that swapped Vec::truncate for explicit slice indexing \
             (self.canned[..limit].to_vec()) would panic with \
             'slice index out of range' on every MCP web_search call where \
             the requested limit exceeded canned.len() — operators raising \
             the request limit above three would see the daemon crash on \
             every mock-mode call with no parse-time signal"
        );

        let first_query = s.search("alpha", 10).await.unwrap();
        let second_query = s.search("beta-with-different-shape", 10).await.unwrap();
        assert_eq!(
            first_query, second_query,
            "two distinct queries must surface identical canned results — \
             the _query argument is intentionally underscore-prefixed and \
             ignored so pick_search's no-config fallback stays deterministic. \
             A refactor that began filtering canned by query (e.g., to make \
             MockSearch 'more realistic' by matching the query string against \
             titles/urls/snippets) would silently break stub determinism — \
             agents issuing varied queries would receive partial or empty \
             results when canned hits did not contain the query, masking \
             real-provider misconfiguration as success"
        );
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
    fn search_error_display_messages_pin_three_string_variant_format_strings() {
        // SearchError (lib.rs lines 26-41) has seven variants parallel
        // to covenant_llm::ProviderError. Four wrap external errors via
        // #[from]. The three string-literal variants emit operator-
        // facing format strings that no existing test inspects.
        // brave_without_key_returns_missing_key (line 553) and
        // serpapi_without_key_returns_missing_key (line 562) assert
        // MissingKey via `matches!` which ignores the Display rendering;
        // Empty and Status have no test at all. The two error catalogs
        // (covenant-tools SearchError and covenant-llm ProviderError)
        // share parallel variant names but use INTENTIONALLY distinct
        // phrasing — pinning anchors the intentional asymmetry so a
        // 'unify cross-crate error wording' refactor surfaces here.

        let empty = SearchError::Empty;
        assert_eq!(
            format!("{empty}"),
            "search returned no hits",
            "SearchError::Empty Display must remain 'search returned no \
             hits' — intentionally distinct from ProviderError::Empty \
             ('provider returned no content'). Operators read the \
             search log separately from the LLM log; merging the \
             phrasings under a 'unify error wording' pass would shift \
             dashboards that grep specifically for 'no hits'"
        );

        let status = SearchError::Status {
            status: 429,
            body: "Too Many Requests".into(),
        };
        let status_message = format!("{status}");
        assert!(
            status_message.contains("(429)"),
            "SearchError::Status Display must parenthesize the status \
             code — operator dashboards grep for '\\(429\\)' to track \
             rate-limit incidents; a swap to 'search error: 429, Too \
             Many Requests' would silently break the convention: \
             {status_message}"
        );
        assert!(
            status_message.contains("Too Many Requests"),
            "SearchError::Status must surface the body string — the \
             body carries the upstream search provider's error reason: \
             {status_message}"
        );
        assert!(
            status_message.contains("search error"),
            "SearchError::Status must keep the 'search error' prefix \
             — intentionally distinct from ProviderError::Status's \
             'provider error' prefix so dashboards distinguish search \
             from chat failures: {status_message}"
        );
        assert!(
            !status_message.contains("(Too Many Requests)"),
            "SearchError::Status body must NOT appear in the \
             parenthesized slot — a #[error] format swap binding {{body}} \
             to the parens and {{status}} to the suffix would emit \
             'search error (Too Many Requests): 429'. Pinning that the \
             body does NOT appear in parens anchors the slot ordering: \
             {status_message}"
        );

        let missing_brave = SearchError::MissingKey("brave");
        assert_eq!(
            format!("{missing_brave}"),
            "missing api key for brave",
            "SearchError::MissingKey Display must remain 'missing api \
             key for <slug>' — INTENTIONALLY LACKS the 'for provider' \
             qualifier that ProviderError::MissingKey uses ('missing \
             api key for provider <slug>'). Search tools are not \
             'providers' in the LLM-protocol sense; the asymmetry is \
             documented. A refactor that added 'for provider' under a \
             'match ProviderError phrasing for cross-crate consistency' \
             rationale would silently shift dashboards that distinguish \
             search-tool secrets from LLM-provider secrets"
        );

        let missing_serpapi = SearchError::MissingKey("serpapi");
        assert_eq!(
            format!("{missing_serpapi}"),
            "missing api key for serpapi",
            "SearchError::MissingKey must bind the slug verbatim \
             without case transformation — the slug is the operator's \
             actionable hint for which [search] api_key to populate \
             in secrets.toml"
        );
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
    fn search_from_config_pins_mock_and_serpapi_discriminator_mapping() {
        // search_from_config matches SearchSection.provider against
        // three exact slugs (mock, brave, serpapi) and falls through
        // to None for any other value. The existing tests pin the
        // brave arm (search_config_parses_brave_block) and the
        // unknown-provider rejection arm
        // (search_config_unknown_provider_returns_none); the mock and
        // serpapi arms are not pinned. Mirrors the just-integrated
        // covenant_llm::provider_from_config_pins_mock_openai_and_deepseek_discriminator_mapping
        // and embedder_from_config_pins_mock_arm_and_unknown_provider_rejection
        // coverage shape.
        //
        // A refactor that renamed the 'serpapi' slug would silently
        // send paid-SerpAPI deployments through the unknown-provider
        // arm; pick_search would degrade to MockSearch::stub() and
        // operators expecting paid Google search results via SerpAPI
        // would silently receive mock stub results with no parse-time
        // signal. A refactor that renamed the 'mock' slug would
        // silently send mock-configured deployments through the same
        // fallback path — same end-state outcome but via an
        // unintentional fallback that masks misconfiguration as
        // success.
        let mock_toml = r#"
[search]
provider = "mock"
"#;
        let cfg: SearchConfig = toml::from_str(mock_toml).unwrap();
        let s = search_from_config(&cfg).expect(
            "the mock arm must dispatch to MockSearch; a slug rename \
             (e.g., to 'stub' or 'fake') would silently fall through \
             into the auto-detect ladder and pick_search would land on \
             MockSearch::stub() — same outcome as the configured intent \
             but via an unintentional fallback path that masks the \
             slug-rename regression as success",
        );
        assert_eq!(
            s.name(),
            "mock",
            "mock search provider must surface name='mock' so search \
             dashboards can identify deterministic test-fixture \
             deployments at a glance",
        );

        let serpapi_toml = r#"
[search]
provider = "serpapi"
api_key = "serpapi-test"
"#;
        let cfg: SearchConfig = toml::from_str(serpapi_toml).unwrap();
        let s = search_from_config(&cfg).expect(
            "the serpapi arm must dispatch to SerpApiSearch; a slug \
             rename (e.g., to 'serpapi.com' or 'google-serpapi') would \
             silently fall through search_from_config's unknown-provider \
             arm and pick_search would degrade paid SerpAPI search to \
             MockSearch::stub() with no parse-time signal — operators \
             expecting paid Google search results would silently \
             receive mock stub responses",
        );
        assert_eq!(
            s.name(),
            "serpapi",
            "serpapi-configured provider must surface name='serpapi' so \
             search dashboards can distinguish paid Google search from \
             the brave and mock paths",
        );
    }

    #[test]
    fn search_config_serde_pins_optional_search_section_default() {
        // SearchConfig is the top-level secrets.toml decoder for the
        // [search] section. The struct carries a single field — search:
        // Option<SearchSection> — with field-level #[serde(default)] and
        // a derived Default impl. SearchConfig::from_path returns
        // Self::default() when the file is missing, and pick_search
        // treats search.is_none() as the fall-through to
        // MockSearch::stub().
        //
        // SearchSection itself is pinned by
        // search_section_serde_pins_required_provider_and_api_key_default
        // but no test pins the wrapper's field name, the None default
        // on an empty TOML payload, or the parse contract when [search]
        // is omitted but other root keys exist. A refactor that renamed
        // SearchConfig::search would silently make every operator's
        // secrets.toml decode into SearchConfig::default(), pick_search
        // would fall through to MockSearch::stub() with no parse signal,
        // and configured Brave/SerpApi deployments would silently lose
        // their provider — agents would think they're searching the web
        // but be looking at the canned stub on every call.
        let empty: SearchConfig = toml::from_str("").unwrap();
        assert!(
            empty.search.is_none(),
            "SearchConfig must decode an empty TOML payload as \
             SearchConfig::default() with search == None — \
             SearchConfig::from_path relies on this path for missing \
             secrets.toml, and pick_search relies on search.is_none() \
             to fall through to MockSearch::stub()"
        );

        // No [search] section but other unknown root keys present — the
        // top-level deserializer must still produce search == None
        // rather than rejecting the unknown key. Operators co-locate
        // [llm]/[embed] blocks in the same secrets.toml and the wrapper
        // does not carry #[serde(deny_unknown_fields)].
        let other_root = r#"
[llm]
provider = "anthropic"
api_key = "sk"
"#;
        let cfg: SearchConfig = toml::from_str(other_root).unwrap();
        assert!(
            cfg.search.is_none(),
            "SearchConfig must tolerate unknown root sections without \
             refusing to parse — operators co-locate [llm]/[embed] \
             blocks in the same secrets.toml and a refactor that added \
             #[serde(deny_unknown_fields)] would break every co-resident \
             secrets.toml at daemon start"
        );

        // [search] fully populated — wrapper round-trips into the
        // SearchSection. Pin every inner field so a refactor that broke
        // the wrapper's deserialization path would fail this assertion
        // rather than landing on a misleading inner-field error.
        let full = r#"
[search]
provider = "brave"
api_key = "BSA-test"
"#;
        let cfg: SearchConfig = toml::from_str(full).unwrap();
        let section = cfg
            .search
            .as_ref()
            .expect("[search] block must surface through SearchConfig.search");
        assert_eq!(section.provider, "brave");
        assert_eq!(section.api_key.as_deref(), Some("BSA-test"));

        // [search] with only the strictly-required provider field — the
        // inner section's Option default must surface through the
        // wrapper so partial secrets.toml stays forward-compatible.
        let minimal = r#"
[search]
provider = "mock"
"#;
        let cfg: SearchConfig = toml::from_str(minimal).unwrap();
        let section = cfg.search.as_ref().unwrap();
        assert_eq!(section.provider, "mock");
        assert!(section.api_key.is_none());

        // SearchConfig::default() must match the empty-TOML decode —
        // pin this so a refactor that diverged the derived Default impl
        // from the empty-TOML path would fail loud.
        let default_cfg = SearchConfig::default();
        assert!(
            default_cfg.search.is_none(),
            "SearchConfig::default() must have search == None to match \
             the empty-TOML decode path that SearchConfig::from_path \
             relies on for missing secrets.toml"
        );
    }

    #[test]
    fn search_section_serde_pins_required_provider_and_api_key_default() {
        // SearchSection is the [search] block operators write to
        // secrets.toml to pick the search-tool provider. Mirrors
        // LlmSection's asymmetric contract: provider is strictly
        // required (no serde default) while api_key carries
        // #[serde(default)] so an unkeyed provider (mock) stays valid.
        //
        // A refactor that added #[serde(default)] to provider would
        // silently parse blocks with no provider as provider='',
        // search_from_config would return None, pick_search would fall
        // back to MockSearch::stub, and the operator-facing web_search
        // tool would silently return the canned stub result on every
        // call — agents would think they're searching the web but be
        // looking at hard-coded fixtures.
        let full = r#"
[search]
provider = "brave"
api_key = "BSA-test"
"#;
        let cfg: SearchConfig = toml::from_str(full).unwrap();
        let section = cfg
            .search
            .as_ref()
            .expect("[search] block must surface as SearchSection");
        assert_eq!(section.provider, "brave");
        assert_eq!(section.api_key.as_deref(), Some("BSA-test"));

        let minimal = r#"
[search]
provider = "mock"
"#;
        let cfg: SearchConfig = toml::from_str(minimal).unwrap();
        let section = cfg.search.as_ref().unwrap();
        assert_eq!(section.provider, "mock");
        assert!(
            section.api_key.is_none(),
            "SearchSection::api_key must decode as None when omitted; a \
             refactor that dropped #[serde(default)] would silently fail \
             parse for operator deployments running on the keyless mock \
             provider"
        );

        // Provider is the only strictly-required field; omitting it must
        // fail parse so a future #[serde(default)] regression on
        // provider does not silently fall back to MockSearch::stub and
        // hide every operator misconfiguration.
        let no_provider = r#"
[search]
api_key = "BSA-test"
"#;
        assert!(
            toml::from_str::<SearchConfig>(no_provider).is_err(),
            "SearchSection::provider must remain strictly required; a \
             #[serde(default)] regression would silently parse blocks \
             with no provider as provider='' and pick_search would fall \
             back to MockSearch::stub — agents would think they're \
             searching the web but be looking at the canned stub on every \
             call"
        );
    }

    #[test]
    fn mock_search_stub_pins_canonical_canned_title_url_and_snippet() {
        // MockSearch::stub (line 60-70) hardcodes a one-element
        // Vec<SearchHit> that surfaces in two places: pick_search's
        // no-config fallback (line 298) and search_from_config's mock
        // arm (line 276). The snippet is an operator-facing breadcrumb
        // — it lands in the LLM agent's reasoning context whenever the
        // web_search tool runs against the mock fallback, and in any
        // dashboard that renders search results. The breadcrumb tells
        // operators exactly which file to edit (~/.covenant/secrets.toml)
        // and exactly which section to add ([search] with a real
        // provider) so unconfigured deployments degrade with a
        // self-diagnostic instead of silently returning empty or
        // generic stub text.
        //
        // Existing tests partially cover this surface:
        // mock_search_returns_canned_hits only asserts the result is
        // non-empty and the first URL starts with "stub://";
        // mock_search_respects_limit observes the limit-truncation
        // behavior, not the source canned length;
        // search_from_config_pins_mock_and_serpapi_discriminator_mapping
        // and pick_search_falls_back_to_mock_when_no_file observe
        // name=="mock" through the trait object but not the canned
        // content. None of these tests pin the exact title, exact
        // full url, exact snippet text, or single-element length, so a
        // refactor that rewrote the snippet to a generic "stub" value,
        // expanded the stub to a multi-element fixture, or changed the
        // url scheme would silently degrade the operator signal with
        // no parse-time or compile-time error.
        let stub = MockSearch::stub();
        assert_eq!(
            stub.canned.len(),
            1,
            "MockSearch::stub must return exactly one canned hit — \
             a refactor that expanded the stub to a multi-element \
             fixture (for richer LLM-agent multi-result coverage) \
             would silently shift mock_search_respects_limit's \
             truncate semantics and the documented single-hit \
             contract that surfaces nowhere outside the source; \
             got len={}",
            stub.canned.len(),
        );
        let hit = &stub.canned[0];
        assert_eq!(
            hit.title, "stub result",
            "MockSearch::stub title must be \"stub result\" — pinning \
             the canonical breadcrumb title that operator dashboards \
             use to identify the mock-mode fallback at a glance",
        );
        assert_eq!(
            hit.url, "stub://no-real-search",
            "MockSearch::stub url must be exactly \"stub://no-real-search\" \
             — the existing mock_search_returns_canned_hits prefix \
             check (url.starts_with(\"stub://\")) accepts any path \
             after the scheme but the canonical full URL is the \
             durable diagnostic; a refactor that changed the url to \
             a real https:// URL pointing at a project landing page \
             or to a placeholder example.com URL would silently \
             collapse the stub:// scheme discriminator that downstream \
             consumers (including the LLM agent if it inspects URL \
             scheme) rely on to detect mock-mode without inspecting \
             content",
        );
        assert_eq!(
            hit.snippet,
            "covenant-tools mock — configure a real provider in ~/.covenant/secrets.toml",
            "MockSearch::stub snippet must contain the verbatim \
             operator breadcrumb pointing at ~/.covenant/secrets.toml; \
             a refactor that emptied or rewrote the snippet would \
             silently degrade the diagnostic that tells operators \
             which file to edit and which section to add — agents \
             that surface search results in their reasoning context \
             lose the actionable guidance, and operators have to \
             read the source to figure out why search returns are \
             uninformative",
        );
        assert_eq!(
            stub.name(),
            "mock",
            "MockSearch must surface name=='mock' — cross-binds \
             search_from_config_pins_mock_and_serpapi_discriminator_mapping \
             and pick_search_falls_back_to_mock_when_no_file pins \
             that observe this identity contract through the trait \
             object",
        );
    }

    #[test]
    fn pick_search_falls_back_to_mock_when_no_file() {
        let dir = std::env::temp_dir();
        let nope = dir.join("covenant-no-search.toml");
        let _ = std::fs::remove_file(&nope);
        let p = pick_search(&nope);
        assert_eq!(p.name(), "mock");
    }

    #[test]
    fn pick_search_falls_back_to_mock_for_malformed_toml_and_unknown_provider() {
        // covenant_tools::pick_search (line 292-299) has THREE distinct
        // fallback paths to MockSearch::stub():
        //
        //   pub fn pick_search(secrets_path: &Path) -> Box<dyn SearchProvider> {
        //       if let Ok(cfg) = SearchConfig::from_path(secrets_path) {
        //           if let Some(p) = search_from_config(&cfg) {
        //               return p;
        //           }
        //       }
        //       Box::new(MockSearch::stub())
        //   }
        //
        //   A. secrets.toml missing       -> from_path returns Self::default with
        //                                    search=None -> search_from_config None
        //                                    -> outer fallback fires
        //   B. secrets.toml malformed     -> from_path returns Err(Toml(_)) -> the
        //                                    `if let Ok` branch fails -> outer fallback
        //                                    fires WITHOUT entering search_from_config
        //   C. secrets.toml has unknown   -> from_path returns Ok -> search_from_config
        //      provider slug                 returns None on the catch-all match arm
        //                                    -> outer fallback fires
        //
        // pick_search_falls_back_to_mock_when_no_file (line 748) pins
        // path A. Path B is NOT pinned by any test
        // (search_config_unknown_provider_returns_none and
        // search_from_config_pins_mock_and_serpapi_discriminator_mapping
        // pin the helper search_from_config, never the boundary error-
        // swallowing semantics of pick_search itself). Path C is pinned
        // at the helper level via search_config_unknown_provider_returns_none
        // but NOT at the pick_search boundary.
        //
        // A refactor that swapped `if let Ok(cfg) = ...` for `.unwrap()`
        // or `.expect("secrets.toml must parse")` under a 'surface config
        // errors loudly' rationale would silently turn malformed
        // secrets.toml into a daemon panic at search-tool resolution;
        // pick_search_falls_back_to_mock_when_no_file passes (file is
        // missing, not malformed) while every operator with a typo in
        // secrets.toml hits a covenantd boot panic.
        //
        // A refactor that changed the unknown-provider catch-all in
        // search_from_config from None to e.g.
        // `Some(Box::new(BraveSearch::new("")))` as a "safer
        // placeholder" would land between the helper pin and the
        // missing-file pin — operators with a typo'd provider slug
        // would silently route through Brave with an empty API key and
        // every search would return MissingKey("brave").

        // Path B: malformed TOML. SearchConfig::from_path returns
        // Err(SearchError::Toml(_)); pick_search must swallow it and
        // return MockSearch, not panic.
        let dir = std::env::temp_dir();
        let malformed_path = dir.join("covenant-malformed-search-7c9f.toml");
        std::fs::write(&malformed_path, "this is not = valid toml ::: garbage\n[[")
            .expect("temp dir is writable in tests");
        let p = pick_search(&malformed_path);
        assert_eq!(
            p.name(),
            "mock",
            "pick_search MUST swallow the Err arm of SearchConfig::from_path \
             and degrade to MockSearch — a refactor that swapped the `if let \
             Ok(cfg)` branch for .unwrap() or .expect() would silently turn \
             a malformed secrets.toml into a covenantd boot panic at search-\
             tool resolution time, taking down the entire daemon before the \
             operator could see the parse error; a refactor that bubbled \
             the error via Result<Box<dyn SearchProvider>, SearchError> \
             would shift error-handling onto every caller and break the \
             documented infallible-fallback contract",
        );
        let _ = std::fs::remove_file(&malformed_path);

        // Path C: valid TOML but unknown provider slug.
        // SearchConfig::from_path returns Ok; search_from_config returns
        // None on the catch-all arm; pick_search must enter the outer
        // fallback.
        let unknown_path = dir.join("covenant-unknown-provider-search-7c9f.toml");
        std::fs::write(
            &unknown_path,
            "[search]\nprovider = \"bogus-future-provider\"\napi_key = \"x\"\n",
        )
        .expect("temp dir is writable in tests");
        let p = pick_search(&unknown_path);
        assert_eq!(
            p.name(),
            "mock",
            "pick_search MUST fall back to MockSearch when search_from_config \
             returns None on the unknown-provider catch-all — pinning this \
             at the pick_search boundary (not just at search_from_config) \
             catches a refactor that promoted the unknown-provider arm \
             from None to e.g. Some(Box::new(BraveSearch::new(\"\"))) as a \
             'safer placeholder', which would land between the existing \
             helper-level pin (search_config_unknown_provider_returns_none) \
             and the missing-file pin (pick_search_falls_back_to_mock_when_no_file) \
             — operators with a typo'd provider slug would silently route \
             through Brave with an empty API key and every search call would \
             fail with SearchError::MissingKey(\"brave\")",
        );
        let _ = std::fs::remove_file(&unknown_path);
    }

    #[test]
    fn search_error_from_wrappers_display_messages_pin_prefixes_and_external_source_display_delegation() {
        // Pins the three directly-constructible #[from] wrappers (Io,
        // Serde, Toml). SearchError::Http wraps reqwest::Error which
        // has no public constructor (same constraint as covenant-llm
        // ProviderError::Http; see commit a093047). Cross-crate parity
        // note: SearchError uses intentionally distinct phrasing from
        // ProviderError for the three string surfaces; this pin
        // anchors the asymmetry from the wrapper side too.

        let io_err = SearchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "tools.toml missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "SearchError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish web-search IO faults from HTTP transport, JSON-decode, and TOML-parse faults (dropped-prefix regression class): {io_message}"
        );
        assert!(
            io_message.contains("tools.toml missing"),
            "SearchError::Io must surface the inner std::io::Error Display rendering after the colon (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "SearchError::Io must NOT surface the std::io::Error Debug rendering (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );

        let serde_source =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let serde_err = SearchError::Serde(serde_source);
        let serde_message = format!("{serde_err}");
        assert!(
            serde_message.starts_with("serde: "),
            "SearchError::Serde must surface the literal 'serde: ' bootstrap-stage prefix (dropped-prefix regression class): {serde_message}"
        );
        assert!(
            serde_message.contains("expected"),
            "SearchError::Serde must surface the inner serde_json::Error Display rendering after the colon (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );
        assert!(
            !serde_message.contains("Error("),
            "SearchError::Serde must NOT surface the serde_json::Error Debug rendering (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );

        let toml_source = toml::from_str::<toml::Value>("= invalid =").expect_err("parse must fail");
        let toml_err = SearchError::Toml(toml_source);
        let toml_message = format!("{toml_err}");
        assert!(
            toml_message.starts_with("toml: "),
            "SearchError::Toml must surface the literal 'toml: ' bootstrap-stage prefix so audit-log filters can distinguish tools-config-toml-parse faults from HTTP, IO, and JSON-decode faults (dropped-prefix regression class): {toml_message}"
        );
        assert!(
            toml_message.len() > "toml: ".len(),
            "SearchError::Toml must surface the inner toml::de::Error Display rendering after the colon (dropped-source-rendering regression class on the {{0}} interpolation): {toml_message}"
        );

        assert_ne!(
            io_message, serde_message,
            "SearchError::Io and SearchError::Serde Display must not converge (prefix-convergence regression class): io={io_message} serde={serde_message}"
        );
        assert_ne!(
            io_message, toml_message,
            "SearchError::Io and SearchError::Toml Display must not converge (prefix-convergence regression class): io={io_message} toml={toml_message}"
        );
        assert_ne!(
            serde_message, toml_message,
            "SearchError::Serde and SearchError::Toml Display must not converge (prefix-convergence regression class): serde={serde_message} toml={toml_message}"
        );
        for (name, message) in [
            ("Io", io_message.as_str()),
            ("Serde", serde_message.as_str()),
            ("Toml", toml_message.as_str()),
        ] {
            assert!(
                !message.starts_with("http: "),
                "SearchError::{name} must not start with 'http: '; a sibling-prefix swap toward the Http wrapper would silently mis-route incident triage (sibling-prefix-swap regression class): {message}"
            );
            assert!(
                !message.starts_with("search returned no hits")
                    && !message.starts_with("search error (")
                    && !message.starts_with("missing api key for "),
                "SearchError::{name} must not converge with the three pinned string-variant surfaces (Empty, Status, MissingKey); a wrapper Display refactor must not collapse onto the operator-facing literal strings (string-surface-convergence regression class): {message}"
            );
            assert!(
                !message.starts_with("provider returned no content")
                    && !message.starts_with("provider error (")
                    && !message.starts_with("missing api key for provider"),
                "SearchError::{name} must not converge with the sibling covenant_llm::ProviderError string surfaces ('provider returned no content', 'provider error ({{status}}): {{body}}', 'missing api key for provider {{0}}'); the cross-crate intentional asymmetry (search-error vocabulary vs provider-error vocabulary) must hold from the wrapper side too (cross-crate-convergence regression class): {message}"
            );
        }
    }
}
