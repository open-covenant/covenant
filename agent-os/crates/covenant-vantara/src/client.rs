//! Read-only client for the Vantara explorer.
//!
//! Both feeds are public GETs with `limit`/`offset` paging. [`jobs`] and
//! [`payouts`] each return one typed page; [`find_job`] walks the job feed to
//! resolve a single record by predicate — a job id or an output hash — with a
//! hard scan bound so a large feed can never spin the caller forever.
//!
//! [`jobs`]: VantaraClient::jobs
//! [`payouts`]: VantaraClient::payouts
//! [`find_job`]: VantaraClient::find_job

use crate::types::{Job, JobsPage, MppDoc, PayoutsPage, SigningBlock};
use crate::{Result, VantaraError};

/// Server default page size for the job feed. The explorer caps a page at
/// 50, so we page in 50s when scanning.
const PAGE: u32 = 50;
/// Most records `find_job` will scan before giving up. Bounds worst-case
/// work on a growing feed; a truncated scan is reported, never silent.
const MAX_SCAN: u32 = 5_000;

#[derive(Debug, Clone)]
pub struct VantaraClient {
    http: reqwest::Client,
    base_url: String,
    pinned_key: Option<String>,
}

/// Outcome of a bounded feed scan.
pub enum Lookup {
    /// The record, plus the cluster, hash algorithm, and signing block of the
    /// page it was found on — the envelope fields an attestation needs.
    Found {
        job: Job,
        cluster: String,
        hash_algorithm: String,
        signing: Option<SigningBlock>,
    },
    /// Not present in the records scanned. `scanned` records were read of
    /// `total` in the feed; `truncated` is true when the scan bound was hit
    /// before reaching the end.
    Missing {
        scanned: u32,
        total: u64,
        truncated: bool,
    },
}

impl VantaraClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_client(reqwest::Client::new(), base_url)
    }

    pub fn with_client(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            pinned_key: None,
        }
    }

    /// Client that pins the provider signing key (base58): verification then
    /// requires the feed's signing key to equal `key`.
    pub fn with_pinned_key(base_url: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            pinned_key: Some(key.into()),
            ..Self::new(base_url)
        }
    }

    /// The pinned provider key, if any.
    pub fn pinned_key(&self) -> Option<&str> {
        self.pinned_key.as_deref()
    }

    /// Resolve the provider signing key from the MPP discovery doc at
    /// `/.well-known/mpp` (`providerCallback.publicKey`). This is the
    /// out-of-band anchor: a different endpoint than the feed, so the feed
    /// cannot assert its own signing key unchecked.
    pub async fn provider_key_from_mpp(&self) -> Result<String> {
        let url = format!("{}/.well-known/mpp", self.base_url);
        let resp = self
            .http
            .get(url)
            .header("accept", "application/json")
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(VantaraError::UnexpectedStatus(status.as_u16()));
        }
        let text = resp.text().await?;
        let doc: MppDoc = serde_json::from_str(&text)
            .map_err(|e| VantaraError::Decode(format!("{e}: body={text}")))?;
        Ok(doc.provider_callback.public_key)
    }

    /// One page of the job feed.
    pub async fn jobs(&self, limit: u32, offset: u32) -> Result<JobsPage> {
        self.get_page("/explorer/jobs", limit, offset).await
    }

    /// One page of the payouts feed.
    pub async fn payouts(&self, limit: u32, offset: u32) -> Result<PayoutsPage> {
        self.get_page("/explorer/payouts", limit, offset).await
    }

    async fn get_page<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        limit: u32,
        offset: u32,
    ) -> Result<T> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .http
            .get(url)
            .query(&[("limit", limit.to_string()), ("offset", offset.to_string())])
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(VantaraError::UnexpectedStatus(status.as_u16()));
        }
        let text = resp.text().await?;
        serde_json::from_str(&text).map_err(|e| VantaraError::Decode(format!("{e}: body={text}")))
    }

    /// Walk the job feed newest-first until `pred` matches or the feed is
    /// exhausted, paging in server-sized batches.
    pub async fn find_job(&self, pred: impl Fn(&Job) -> bool) -> Result<Lookup> {
        self.scan_jobs(pred, PAGE).await
    }

    /// Page the job feed in `page`-sized batches looking for `pred`. The scan
    /// ends on a short page (fewer than `page` records returned), not on
    /// `pagination.total`: a lagging total counter on a live feed must never
    /// cut the scan short and report a job that exists as missing. Bounded by
    /// `MAX_SCAN` so a misbehaving feed can't spin forever.
    async fn scan_jobs(&self, pred: impl Fn(&Job) -> bool, page: u32) -> Result<Lookup> {
        let mut offset = 0u32;
        let mut scanned = 0u32;
        let mut total;
        loop {
            let batch = self.jobs(page, offset).await?;
            total = batch.pagination.total;
            if let Some(job) = batch.jobs.iter().find(|j| pred(j)) {
                return Ok(Lookup::Found {
                    job: job.clone(),
                    cluster: batch.cluster.clone(),
                    hash_algorithm: batch.hash_algorithm.clone(),
                    signing: batch.signing.clone(),
                });
            }
            let got = batch.jobs.len() as u32;
            scanned = scanned.saturating_add(got);
            offset = offset.saturating_add(got);
            let exhausted = got < page;
            let capped = offset >= MAX_SCAN;
            if exhausted || capped {
                return Ok(Lookup::Missing {
                    scanned,
                    total,
                    truncated: capped && !exhausted,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn feed(total: u64, jobs: &str) -> String {
        format!(
            r#"{{"cluster":"mainnet-beta","hashAlgorithm":"sha256","note":"",
                "pagination":{{"limit":50,"offset":0,"total":{total}}},"jobs":[{jobs}]}}"#
        )
    }

    fn job_json(id: &str, hash: &str) -> String {
        format!(
            r#"{{"jobId":"{id}","nodeId":null,"model":"claude","outputHash":"{hash}",
                "proofSignature":null,"completedAt":"2026-07-01T18:04:12.311Z"}}"#
        )
    }

    #[tokio::test]
    async fn jobs_passes_paging_params() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .and(query_param("limit", "10"))
            .and(query_param("offset", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_string(feed(0, "")))
            .mount(&server)
            .await;

        let client = VantaraClient::new(server.uri());
        let page = client.jobs(10, 20).await.expect("jobs");
        assert_eq!(page.pagination.total, 0);
        assert!(page.jobs.is_empty());
    }

    #[test]
    fn with_pinned_key_sets_the_pin() {
        assert_eq!(
            VantaraClient::with_pinned_key("http://x", "KEY").pinned_key(),
            Some("KEY")
        );
        assert_eq!(VantaraClient::new("http://x").pinned_key(), None);
    }

    #[tokio::test]
    async fn provider_key_from_mpp_reads_the_callback_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/mpp"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"providerCallback":{"scheme":"ed25519","publicKey":"DBkSpBFu5oUNmPuJwB1J2gBLfRtVHmZHXaynaX3hAs71","encoding":"base58"}}"#,
            ))
            .mount(&server)
            .await;

        let key = VantaraClient::new(server.uri())
            .provider_key_from_mpp()
            .await
            .expect("mpp key");
        assert_eq!(key, "DBkSpBFu5oUNmPuJwB1J2gBLfRtVHmZHXaynaX3hAs71");
    }

    #[tokio::test]
    async fn unexpected_status_surfaces() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = VantaraClient::new(server.uri())
            .jobs(50, 0)
            .await
            .unwrap_err();
        assert!(matches!(err, VantaraError::UnexpectedStatus(503)));
    }

    #[tokio::test]
    async fn decode_error_carries_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not-json"))
            .mount(&server)
            .await;

        let err = VantaraClient::new(server.uri())
            .jobs(50, 0)
            .await
            .unwrap_err();
        assert!(matches!(err, VantaraError::Decode(m) if m.contains("body=")));
    }

    #[tokio::test]
    async fn scan_jobs_resolves_across_full_pages() {
        let hash = "45b682f486d5bc8d835f79865591c79d034b1ad0e281e74ab3aadf9e268fffa6";
        let server = MockServer::start().await;
        // Page 0 (limit 2) is full, so the scan continues; the target sits on
        // the next page.
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .and(query_param("limit", "2"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_string(feed(
                3,
                &format!("{},{}", job_json("a", "00"), job_json("b", "11")),
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .and(query_param("limit", "2"))
            .and(query_param("offset", "2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(feed(3, &job_json("target", hash))),
            )
            .mount(&server)
            .await;

        let client = VantaraClient::new(server.uri());
        match client
            .scan_jobs(|j| j.output_hash == hash, 2)
            .await
            .expect("scan")
        {
            Lookup::Found { job, cluster, .. } => {
                assert_eq!(job.job_id, "target");
                assert_eq!(cluster, "mainnet-beta");
            }
            Lookup::Missing { .. } => panic!("should have found the target"),
        }
    }

    #[tokio::test]
    async fn scan_survives_a_lagging_total_counter() {
        // Server under-reports total=1 while holding a full first page. The
        // old `offset >= total` termination would stop after page 0 and miss
        // the target on page 1; short-page termination must not.
        let hash = "45b682f486d5bc8d835f79865591c79d034b1ad0e281e74ab3aadf9e268fffa6";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_string(feed(
                1,
                &format!("{},{}", job_json("a", "00"), job_json("b", "11")),
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .and(query_param("offset", "2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(feed(1, &job_json("target", hash))),
            )
            .mount(&server)
            .await;

        let client = VantaraClient::new(server.uri());
        match client
            .scan_jobs(|j| j.output_hash == hash, 2)
            .await
            .expect("scan")
        {
            Lookup::Found { job, .. } => assert_eq!(job.job_id, "target"),
            Lookup::Missing { .. } => panic!("lagging total must not end the scan early"),
        }
    }

    #[tokio::test]
    async fn find_job_reports_missing_with_scan_count() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_string(feed(1, &job_json("a", "00"))))
            .mount(&server)
            .await;

        let client = VantaraClient::new(server.uri());
        match client.find_job(|j| j.job_id == "nope").await.expect("scan") {
            Lookup::Missing {
                scanned,
                total,
                truncated,
            } => {
                assert_eq!(scanned, 1);
                assert_eq!(total, 1);
                assert!(!truncated);
            }
            Lookup::Found { .. } => panic!("nothing should match"),
        }
    }
}
