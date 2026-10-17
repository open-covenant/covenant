//! Durable aggregate alert transitions. This is an independent sidecar file,
//! never the financial journal. Payloads contain no account/job/RPC identities.
use super::*;
use std::{fs, io::Write, path::PathBuf};

const CONFIRMATIONS: u32 = 3;
const MAX_PENDING: usize = 64;
const MAX_STATE_BYTES: u64 = 128 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Track {
    consecutive: u32,
    clean: u32,
    active: bool,
    count: usize,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Event {
    pub event_id: Uuid,
    pub at_ms: u64,
    pub kind: FindingKind,
    pub transition: String,
    pub count: usize,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    schema_version: u32,
    deployment_binding: String,
    last_observed_at_ms: Option<u64>,
    last_delivery_at_ms: Option<u64>,
    tracks: BTreeMap<FindingKind, Track>,
    pending: Vec<Event>,
}

pub(super) struct AlertTracker {
    saved: Saved,
    path: Option<PathBuf>,
    delivery: Delivery,
    healthy: bool,
}

/// Configuration never derives Debug or Serialize: destinations can contain
/// credentials and email addresses do not belong in public status.
#[derive(Clone)]
pub(super) enum Delivery {
    Logs,
    Webhook(String),
    Email {
        key: String,
        account_id: String,
        from: String,
        to: String,
    },
}

impl Delivery {
    fn from_env() -> Result<Self, String> {
        let key = std::env::var("COVENANT_COMPUTE_OBSERVER_ALERT_CLOUDFLARE_API_TOKEN").ok();
        let account_id =
            std::env::var("COVENANT_COMPUTE_OBSERVER_ALERT_CLOUDFLARE_ACCOUNT_ID").ok();
        let from = std::env::var("COVENANT_COMPUTE_OBSERVER_ALERT_FROM").ok();
        let to = std::env::var("COVENANT_COMPUTE_OBSERVER_ALERT_TO").ok();
        let webhook = std::env::var("COVENANT_COMPUTE_OBSERVER_ALERT_WEBHOOK").ok();
        if key.is_some() || account_id.is_some() || from.is_some() || to.is_some() {
            if webhook.is_some() {
                return Err("choose exactly one observer alert transport".into());
            }
            let key = key
                .filter(|s| !s.trim().is_empty() && !s.contains(['\r', '\n']))
                .ok_or("observer email requires CLOUDFLARE_API_TOKEN")?;
            let address = |value: Option<String>| {
                value.filter(|s| {
                    s.len() <= 254 && s.contains('@') && !s.contains([' ', '\r', '\n', '<', '>'])
                })
            };
            let account_id = account_id
                .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or("observer email requires a 32-character hex Cloudflare account ID")?;
            return Ok(Self::Email {
                key,
                account_id,
                from: address(from).ok_or("observer email requires a plain FROM mailbox")?,
                to: address(to).ok_or("observer email requires a plain TO mailbox")?,
            });
        }
        if let Some(url) = webhook {
            let parsed =
                reqwest::Url::parse(&url).map_err(|_| "observer alert webhook URL is invalid")?;
            if parsed.scheme() != "https"
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.fragment().is_some()
            {
                return Err(
                    "observer alert webhook requires HTTPS without userinfo or fragment".into(),
                );
            }
            return Ok(Self::Webhook(url));
        }
        Ok(Self::Logs)
    }

    pub async fn send(&self, http: &reqwest::Client, event: &Event) -> bool {
        let request = match self {
            Self::Logs => return true,
            Self::Webhook(url) => http.post(url).json(&json!({"schema_version":1,
                "source":"covenant_compute_observer","mode":"alert_only","event":event})),
            Self::Email {
                key,
                account_id,
                from,
                to,
            } => {
                let kind = serde_json::to_value(event.kind)
                    .expect("enum")
                    .as_str()
                    .expect("kind")
                    .replace('_', " ");
                let subject = if event.transition == "resolved" {
                    "Covenant Compute: observer finding resolved"
                } else {
                    "Covenant Compute: observer finding confirmed"
                };
                let body = format!("{subject}\n\nFinding: {kind}\nLatest count: {}\nObserved at (Unix milliseconds): {}\nEvent ID: {}\n\nThis alert follows three observations. RPC or visibility failures are not evidence of missing funds. The observer is read-only and has not moved funds or changed settlement.\n\nStatus: https://opencovenant.org/compute-status\n", event.count, event.at_ms, event.event_id);
                http.post(format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/email/sending/send"))
                    .bearer_auth(key)
                    .json(&json!({"from":from,"to":to,"subject":subject,"text":body,"headers":{"X-Covenant-Event-ID":event.event_id.to_string()}}))
            }
        };
        let Ok(mut response) = request
            .timeout(Duration::from_secs(5))
            .header("Idempotency-Key", event.event_id.to_string())
            .send()
            .await
        else {
            return false;
        };
        if !response.status().is_success() {
            return false;
        }
        let Self::Email { to, .. } = self else {
            return true;
        };
        // HTTP 200 alone can include per-recipient bounces or suppressions.
        // Bound parsing and never log the provider response or token.
        if response.content_length().is_some_and(|n| n > 16_384) {
            return false;
        }
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if chunk.len() <= 16_384usize.saturating_sub(bytes.len()) => {
                    bytes.extend_from_slice(&chunk)
                }
                Ok(None) => break,
                _ => return false,
            }
        }
        serde_json::from_slice::<Value>(&bytes).is_ok_and(|value| cloudflare_accepted(&value, to))
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Logs => "structured_logs",
            Self::Webhook(_) => "webhook",
            Self::Email { .. } => "email",
        }
    }
}

fn cloudflare_accepted(value: &Value, recipient: &str) -> bool {
    if value["success"] != true
        || !value["errors"]
            .as_array()
            .is_some_and(|errors| errors.is_empty())
    {
        return false;
    }
    let list = |field| {
        value["result"][field]
            .as_array()
            .filter(|items| items.iter().all(Value::is_string))
    };
    let (Some(delivered), Some(queued), Some(bounces)) =
        (list("delivered"), list("queued"), list("permanent_bounces"))
    else {
        return false;
    };
    let matches = |item: &Value| item.as_str() == Some(recipient);
    if bounces.iter().any(matches) {
        return false;
    }
    if let Some(suppressed) = value["result"].get("suppressed_recipients") {
        if !suppressed
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string) && !items.iter().any(matches))
        {
            return false;
        }
    }
    delivered.iter().any(matches) || queued.iter().any(matches)
}

impl AlertTracker {
    pub fn memory() -> Self {
        Self {
            saved: Saved {
                schema_version: 1,
                deployment_binding: String::new(),
                last_observed_at_ms: None,
                last_delivery_at_ms: None,
                tracks: BTreeMap::new(),
                pending: Vec::new(),
            },
            path: None,
            delivery: Delivery::Logs,
            healthy: true,
        }
    }

    pub fn open(path: PathBuf, binding: &str) -> Result<Self, String> {
        let mut tracker = Self::memory();
        tracker.saved.deployment_binding = binding.to_owned();
        match fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_STATE_BYTES => {
                return Err("observer alert state exceeds limit".into())
            }
            Ok(_) => {
                let bytes = fs::read(&path).map_err(|_| "observer alert state cannot be read")?;
                tracker.saved = serde_json::from_slice(&bytes)
                    .map_err(|_| "observer alert state is invalid")?;
                if tracker.saved.deployment_binding != binding {
                    return Err("observer alert state deployment binding differs".into());
                }
                if tracker.saved.schema_version != 1
                    || tracker.saved.pending.len() > MAX_PENDING
                    || tracker.saved.tracks.len() > 32
                    || tracker
                        .saved
                        .pending
                        .iter()
                        .any(|e| !matches!(e.transition.as_str(), "firing" | "resolved"))
                {
                    return Err("observer alert state has unsupported schema or limits".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("observer alert state cannot be inspected".into()),
        }
        tracker.path = Some(path);
        tracker.delivery = Delivery::from_env()?;
        tracker
            .persist(&tracker.saved)
            .map_err(|_| "observer alert state cannot be persisted")?;
        Ok(tracker)
    }

    fn persist(&self, next: &Saved) -> Result<(), ()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(next).map_err(|_| ())?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(());
        }
        let temporary = path.with_extension("json.pending");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|_| ())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| ())?;
        fs::rename(&temporary, path).map_err(|_| ())?;
        fs::File::open(path.parent().ok_or(())?)
            .and_then(|f| f.sync_all())
            .map_err(|_| ())?;
        Ok(())
    }

    pub fn record(&mut self, report: &ObserverReport, interval: Duration) {
        let mut next = self.saved.clone();
        // After downtime, require fresh repeated evidence before a new alert
        // or recovery. Previously active findings remain active until proven clear.
        if next.last_observed_at_ms.is_some_and(|last| {
            report.observed_at_ms.saturating_sub(last)
                > (interval.as_millis() as u64).saturating_mul(3)
                || report.observed_at_ms < last
        }) {
            for track in next.tracks.values_mut() {
                track.consecutive = 0;
                track.clean = 0;
            }
        }
        next.last_observed_at_ms = Some(report.observed_at_ms);
        let mut counts = BTreeMap::new();
        for finding in &report.findings {
            *counts.entry(finding.kind).or_insert(0usize) += 1;
        }
        for kind in counts.keys() {
            next.tracks.entry(*kind).or_default();
        }
        let complete = report.scan_complete && !report.findings.iter().any(|f| f.kind.blind());
        for (kind, track) in &mut next.tracks {
            if let Some(count) = counts.get(kind) {
                track.consecutive = track.consecutive.saturating_add(1);
                track.clean = 0;
                track.count = *count;
                if !track.active && track.consecutive >= CONFIRMATIONS {
                    track.active = true;
                    next.pending.push(Event {
                        event_id: Uuid::new_v4(),
                        at_ms: report.observed_at_ms,
                        kind: *kind,
                        transition: "firing".into(),
                        count: *count,
                    });
                }
            } else if complete {
                track.consecutive = 0;
                track.clean = track.clean.saturating_add(1);
                if track.active && track.clean >= CONFIRMATIONS {
                    track.active = false;
                    track.count = 0;
                    next.pending.push(Event {
                        event_id: Uuid::new_v4(),
                        at_ms: report.observed_at_ms,
                        kind: *kind,
                        transition: "resolved".into(),
                        count: 0,
                    });
                }
            } else {
                // A partial scan cannot prove an old finding disappeared.
                track.consecutive = 0;
                track.clean = 0;
            }
        }
        if next.pending.len() > MAX_PENDING || self.persist(&next).is_err() {
            self.healthy = false;
            tracing::error!(
                "observer alert state persistence failed; no alert transition acknowledged"
            );
            return;
        }
        self.saved = next;
        self.healthy = true;
    }

    pub fn next(&self) -> Option<(Event, Delivery)> {
        // These events were already persisted. A failed new transition or full
        // queue must not prevent their delivery and permanently block recovery.
        self.saved
            .pending
            .first()
            .cloned()
            .map(|event| (event, self.delivery.clone()))
    }

    pub fn delivered(&mut self, id: Uuid, now: u64) {
        let mut next = self.saved.clone();
        next.pending.retain(|event| event.event_id != id);
        next.last_delivery_at_ms = Some(now);
        if self.persist(&next).is_ok() {
            self.saved = next;
            self.healthy = true;
        } else {
            self.healthy = false;
        }
    }

    pub fn public_view(&self) -> Value {
        json!({"confirmation_observations":CONFIRMATIONS,
            "active_count":self.saved.tracks.values().filter(|t| t.active).count(),
            "pending_delivery_count":self.saved.pending.len(),
            "delivery":self.delivery.name(),
            "persistence_healthy":self.healthy,
            "last_delivery_at_ms":self.saved.last_delivery_at_ms})
    }

    pub fn metrics(&self, out: &mut String) {
        use std::fmt::Write;
        for (name, value) in [
            (
                "compute_observer_alerts_active",
                self.saved.tracks.values().filter(|t| t.active).count(),
            ),
            (
                "compute_observer_alerts_pending_delivery",
                self.saved.pending.len(),
            ),
            (
                "compute_observer_alert_persistence_healthy",
                usize::from(self.healthy),
            ),
        ] {
            let _ = writeln!(out, "# TYPE {name} gauge\n{name} {value}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report(time: u64, kind: Option<FindingKind>) -> ObserverReport {
        ObserverReport {
            schema_version: 1,
            observed_at_ms: time,
            scan_complete: true,
            l1_slot: Some(5),
            scanned_leases: 1,
            findings: kind
                .into_iter()
                .map(|kind| Finding {
                    kind,
                    account: None,
                    job_id: None,
                })
                .collect(),
        }
    }
    #[test]
    fn repeated_findings_deduplicate_survive_restart_and_require_clean_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alerts.json");
        let mut tracker = AlertTracker::open(path.clone(), "test-deployment").unwrap();
        for n in 1..=3 {
            tracker.record(
                &report(n * 60_000, Some(FindingKind::OrphanedFundedLease)),
                Duration::from_secs(60),
            );
        }
        assert_eq!(tracker.saved.pending.len(), 1);
        let event = tracker.saved.pending[0].event_id;
        drop(tracker);
        let mut tracker = AlertTracker::open(path, "test-deployment").unwrap();
        tracker.record(
            &report(240_000, Some(FindingKind::OrphanedFundedLease)),
            Duration::from_secs(60),
        );
        assert_eq!(tracker.saved.pending[0].event_id, event);
        assert_eq!(tracker.saved.pending.len(), 1);
        tracker.delivered(event, 240_001);
        for n in 5..=7 {
            tracker.record(
                &report(n * 60_000, Some(FindingKind::RpcUnavailable)),
                Duration::from_secs(60),
            );
        }
        assert!(tracker.saved.tracks[&FindingKind::OrphanedFundedLease].active);
        for n in 8..=10 {
            tracker.record(&report(n * 60_000, None), Duration::from_secs(60));
        }
        assert!(!tracker.saved.tracks[&FindingKind::OrphanedFundedLease].active);
        assert_eq!(
            tracker
                .saved
                .pending
                .iter()
                .filter(|e| e.transition == "resolved")
                .count(),
            2
        );
    }
    #[test]
    fn transient_finding_and_interrupted_confirmation_do_not_page() {
        let mut tracker = AlertTracker::memory();
        tracker.record(
            &report(1, Some(FindingKind::TermsMismatch)),
            Duration::from_secs(60),
        );
        tracker.record(&report(60_001, None), Duration::from_secs(60));
        tracker.record(
            &report(120_001, Some(FindingKind::TermsMismatch)),
            Duration::from_secs(60),
        );
        tracker.record(
            &report(600_001, Some(FindingKind::TermsMismatch)),
            Duration::from_secs(60),
        );
        assert!(tracker.saved.pending.is_empty());
    }
    #[test]
    fn corrupt_alert_state_is_not_silently_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alerts.json");
        fs::write(&path, "{broken").unwrap();
        assert!(AlertTracker::open(path, "test-deployment").is_err());
    }
    #[test]
    fn full_queue_can_drain_and_resume_a_previously_unrecorded_transition() {
        let mut tracker = AlertTracker::memory();
        for n in 0..MAX_PENDING {
            tracker.saved.pending.push(Event {
                event_id: Uuid::new_v4(),
                at_ms: n as u64,
                kind: FindingKind::RpcUnavailable,
                transition: "firing".into(),
                count: 1,
            });
        }
        for n in 1..=3 {
            tracker.record(
                &report(n * 60_000, Some(FindingKind::TermsMismatch)),
                Duration::from_secs(60),
            );
        }
        assert!(!tracker.healthy);
        assert!(!tracker.saved.tracks[&FindingKind::TermsMismatch].active);
        let (event, _) = tracker
            .next()
            .expect("already persisted events must still drain");
        tracker.delivered(event.event_id, 180_001);
        tracker.record(
            &report(240_000, Some(FindingKind::TermsMismatch)),
            Duration::from_secs(60),
        );
        assert!(tracker.healthy);
        assert!(tracker.saved.tracks[&FindingKind::TermsMismatch].active);
        assert_eq!(tracker.saved.pending.len(), MAX_PENDING);
    }

    #[test]
    fn persisted_alerts_cannot_be_replayed_under_different_chain_pins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alerts.json");
        AlertTracker::open(path.clone(), "market-a").unwrap();
        assert!(AlertTracker::open(path, "market-b").is_err());
    }
    #[tokio::test]
    async fn failed_notification_keeps_the_same_durable_event_for_retry() {
        use axum::{
            http::{HeaderMap, StatusCode},
            routing::post,
            Router,
        };
        let requests = Arc::new(parking_lot::Mutex::new(Vec::<(String, Value)>::new()));
        let captured = requests.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/",
                    post(move |headers: HeaderMap, Json(body): Json<Value>| {
                        let captured = captured.clone();
                        async move {
                            let mut rows = captured.lock();
                            rows.push((
                                headers["idempotency-key"].to_str().unwrap().to_owned(),
                                body,
                            ));
                            if rows.len() == 1 {
                                StatusCode::SERVICE_UNAVAILABLE
                            } else {
                                StatusCode::OK
                            }
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alerts.json");
        let mut tracker = AlertTracker::open(path.clone(), "retry-test").unwrap();
        for n in 1..=3 {
            tracker.record(
                &report(n * 60_000, Some(FindingKind::RpcUnavailable)),
                Duration::from_secs(60),
            );
        }
        let (first, _) = tracker.next().unwrap();
        let transport = Delivery::Webhook(url);
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        assert!(!transport.send(&http, &first).await);
        drop(tracker);
        let mut tracker = AlertTracker::open(path, "retry-test").unwrap();
        let (retry, _) = tracker.next().unwrap();
        assert_eq!(retry.event_id, first.event_id);
        assert!(transport.send(&http, &retry).await);
        tracker.delivered(retry.event_id, 200_000);
        assert!(tracker.next().is_none());
        let requests = requests.lock();
        assert_eq!(requests[0], requests[1]);
        assert!(requests[0].1["event"].get("account").is_none());
        assert!(requests[0].1["event"].get("job_id").is_none());
        server.abort();
    }

    #[test]
    fn cloudflare_http_success_is_not_enough_without_recipient_acceptance() {
        let good = json!({"success":true,"errors":[],"result":{"delivered":[],"queued":["alert@example.com"],"permanent_bounces":[],"suppressed_recipients":[]}});
        assert!(cloudflare_accepted(&good, "alert@example.com"));
        assert!(!cloudflare_accepted(&good, "different@example.com"));
        let mut bounced = good.clone();
        bounced["result"]["permanent_bounces"] = json!(["alert@example.com"]);
        assert!(!cloudflare_accepted(&bounced, "alert@example.com"));
        let mut suppressed = good.clone();
        suppressed["result"]["suppressed_recipients"] = json!(["alert@example.com"]);
        assert!(!cloudflare_accepted(&suppressed, "alert@example.com"));
        let mut failed = good.clone();
        failed["success"] = json!(false);
        assert!(!cloudflare_accepted(&failed, "alert@example.com"));
        assert!(!cloudflare_accepted(
            &json!({"success":true}),
            "alert@example.com"
        ));
    }
}
