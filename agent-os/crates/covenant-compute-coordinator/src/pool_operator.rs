//! Explicit public pilot publications, isolated from private buyer/job APIs.
//! Publication authority conveys an operator assertion, never a settlement proof.
use crate::http::ApiError;
use axum::{
    extract::Path,
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use covenant_compute_protocol::sha256_hex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path as FsPath, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
const MAX: usize = 512 * 1024;
const DIR: &str = "COVENANT_POOL_OPERATOR_DIR";
const TOKEN: &str = "COVENANT_POOL_OPERATOR_PUBLISH_TOKEN";
fn bad() -> ApiError {
    ApiError::BadRequest("Invalid public pilot publication".into())
}
fn unavailable() -> ApiError {
    ApiError::ServiceUnavailable("Public pilot data is unavailable".into())
}
fn missing() -> ApiError {
    ApiError::NotFound("Published pilot artifact not found".into())
}
fn need(ok: bool) -> Result<(), ApiError> {
    if ok {
        Ok(())
    } else {
        Err(bad())
    }
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn amount(s: &str) -> bool {
    s == "0"
        || (!s.is_empty()
            && s.len() <= 20
            && !s.starts_with('0')
            && s.bytes().all(|b| b.is_ascii_digit()))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn time(t: u64) -> bool {
    t > 0 && t <= 9_007_199_254_740_991
}
fn root() -> Result<PathBuf, ApiError> {
    let p = PathBuf::from(std::env::var(DIR).map_err(|_| unavailable())?);
    need(p.is_absolute())?;
    for ancestor in p.ancestors() {
        if fs::symlink_metadata(ancestor)
            .map_err(|_| unavailable())?
            .file_type()
            .is_symlink()
        {
            return Err(unavailable());
        }
    }
    need(p.is_dir())?;
    Ok(p)
}
fn auth(headers: &HeaderMap) -> Result<(), ApiError> {
    let secret =
        std::env::var(TOKEN).map_err(|_| ApiError::Unauthorized("Publication disabled".into()))?;
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if secret.len() < 32 || !bool::from(secret.as_bytes().ct_eq(supplied.as_bytes())) {
        return Err(ApiError::Unauthorized("Publication denied".into()));
    }
    Ok(())
}
fn canonical(value: &Value) -> Vec<u8> {
    fn ordered(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut entries: Vec<_> = m.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                let mut out = serde_json::Map::new();
                for (k, v) in entries {
                    out.insert(k.clone(), ordered(v));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(ordered).collect()),
            _ => v.clone(),
        }
    }
    serde_json::to_vec(&ordered(value)).expect("JSON serializable")
}
fn read(path: &FsPath) -> Result<Vec<u8>, ApiError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| missing())?;
    need(f.metadata().map_err(|_| unavailable())?.is_file())?;
    let mut b = Vec::new();
    (&mut f)
        .take(MAX as u64 + 1)
        .read_to_end(&mut b)
        .map_err(|_| unavailable())?;
    need(b.len() <= MAX)?;
    Ok(b)
}
fn write(path: &FsPath, bytes: &[u8], immutable: bool) -> Result<(), ApiError> {
    use std::os::unix::fs::OpenOptionsExt;
    need(bytes.len() <= MAX)?;
    // Never expose the destination name until all bytes are durable. A crash
    // before publication can leave only an unreachable temporary file; retrying
    // the same content still works without replacing immutable evidence.
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|_| unavailable())?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .map_err(|_| unavailable())?;
        if immutable {
            match fs::hard_link(&temp, path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    need(read(path)? == bytes)?;
                }
                Err(_) => return Err(unavailable()),
            }
        } else {
            fs::rename(&temp, path).map_err(|_| unavailable())?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&temp);
    result?;
    fs::File::open(path.parent().ok_or_else(unavailable)?)
        .and_then(|d| d.sync_all())
        .map_err(|_| unavailable())?;
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    pub id: String,
    pub status: String,
    pub checked_at_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quotes {
    pub observed_at_ms: u64,
    pub completed_at_ms: u64,
    pub status: String,
    pub available_quotes: u8,
    pub observation_sha256: String,
    pub records_url: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Analyst {
    pub status: String,
    pub evidence_manifest_url: Option<String>,
    pub source_sha256: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Costs {
    pub reserved_usdc_micros: String,
    pub coordinator_reported_charge_usdc_micros: Option<String>,
    pub settlement: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: u8,
    pub generated_at_ms: u64,
    pub pool_funded: bool,
    pub stage: String,
    pub quote_observation: Option<Quotes>,
    pub readiness: Vec<Gate>,
    pub analyst: Analyst,
    pub costs: Costs,
}
fn link_hash(link: &str, kind: &str) -> Option<String> {
    let path = link
        .strip_prefix("https://compute-api.opencovenant.org")
        .unwrap_or(link);
    let h = path
        .strip_prefix(&format!("/pool-operator/{kind}/"))?
        .strip_suffix(".json")?;
    hash(h).then(|| h.to_string())
}
impl Snapshot {
    fn validate(&self) -> Result<(), ApiError> {
        need(
            self.schema_version == 1
                && time(self.generated_at_ms)
                && !self.pool_funded
                && matches!(
                    self.stage.as_str(),
                    "preparing" | "observing" | "awaiting_liquidity"
                ),
        )?;
        let expected: HashSet<_> = [
            "analyst",
            "pool_rehearsal",
            "conversion_route",
            "operator_approval",
            "shadow_trial",
        ]
        .into_iter()
        .collect();
        need(
            self.readiness.len() == 5
                && self
                    .readiness
                    .iter()
                    .map(|g| g.id.as_str())
                    .collect::<HashSet<_>>()
                    == expected,
        )?;
        for g in &self.readiness {
            need(
                matches!(g.status.as_str(), "pending" | "passed" | "failed")
                    && g.checked_at_ms
                        .is_none_or(|t| time(t) && t <= self.generated_at_ms)
                    && (g.status == "pending" || g.checked_at_ms.is_some()),
            )?;
        }
        if let Some(q) = &self.quote_observation {
            need(
                time(q.observed_at_ms)
                    && q.observed_at_ms <= q.completed_at_ms
                    && q.completed_at_ms <= self.generated_at_ms
                    && q.available_quotes <= 6
                    && hash(&q.observation_sha256),
            )?;
            need(match q.status.as_str() {
                "available" => q.available_quotes == 6,
                "partial" => (1..6).contains(&q.available_quotes),
                "unavailable" => q.available_quotes == 0,
                _ => false,
            })?;
            if let Some(u) = &q.records_url {
                need(
                    link_hash(u, "observations").as_deref() == Some(q.observation_sha256.as_str()),
                )?;
            }
        }
        need(matches!(
            self.analyst.status.as_str(),
            "pending" | "accepted" | "failed"
        ))?;
        if self.analyst.status == "accepted" {
            need(
                self.analyst.source_sha256.as_deref().is_some_and(hash)
                    && self
                        .analyst
                        .evidence_manifest_url
                        .as_deref()
                        .and_then(|u| link_hash(u, "manifests"))
                        .is_some(),
            )?;
        } else {
            need(
                self.analyst.source_sha256.is_none()
                    && self.analyst.evidence_manifest_url.is_none(),
            )?;
        }
        let analyst_pass = self
            .readiness
            .iter()
            .any(|g| g.id == "analyst" && g.status == "passed");
        need(analyst_pass == (self.analyst.status == "accepted"))?;
        need(
            amount(&self.costs.reserved_usdc_micros)
                && self
                    .costs
                    .reserved_usdc_micros
                    .parse::<u64>()
                    .is_ok_and(|v| v <= 20_000_000),
        )?;
        need(
            self.costs
                .coordinator_reported_charge_usdc_micros
                .as_deref()
                .is_none_or(|s| amount(s) && s.parse::<u64>().is_ok_and(|v| v <= 20_000_000)),
        )?;
        // This pre-funding projection has no chain-settlement verifier.
        need(self.costs.settlement == "unresolved")?;
        let all = self.readiness.iter().all(|g| g.status == "passed");
        need((self.stage == "awaiting_liquidity") == all)?;
        if all {
            need(self.quote_observation.as_ref().is_some_and(|q| {
                q.status == "available" && self.generated_at_ms - q.observed_at_ms <= 180_000
            }))?;
        }
        if self.stage == "observing" {
            need(analyst_pass)?;
        }
        Ok(())
    }
}
fn response(bytes: Vec<u8>) -> Response {
    let mut r = bytes.into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    r
}
pub async fn snapshot() -> Result<Response, ApiError> {
    let bytes = read(&root()?.join("snapshot.json"))?;
    let s: Snapshot = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    s.validate()?;
    Ok(response(canonical(
        &serde_json::to_value(s).map_err(|_| unavailable())?,
    )))
}
fn artifact_path(root: &FsPath, kind: &str, h: &str) -> Result<PathBuf, ApiError> {
    need(matches!(kind, "observations" | "manifests" | "artifacts") && hash(h))?;
    Ok(root.join(format!("{kind}-{h}.json")))
}
pub async fn artifact(Path((kind, name)): Path<(String, String)>) -> Result<Response, ApiError> {
    let h = name.strip_suffix(".json").ok_or_else(missing)?;
    let bytes = read(&artifact_path(&root()?, &kind, h)?)?;
    need(sha256_hex(&bytes) == h)?;
    Ok(response(bytes))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    snapshot: Snapshot,
    observation: Option<Value>,
}
pub async fn publish(headers: HeaderMap, Json(p): Json<Publication>) -> Result<Response, ApiError> {
    auth(&headers)?;
    p.snapshot.validate()?;
    need(p.snapshot.generated_at_ms <= now() + 30_000)?;
    let root = root()?;
    // Serialize publishers across the monotonic latest-pointer check and write.
    use std::os::unix::fs::OpenOptionsExt;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(".publish.lock"))
        .map_err(|_| unavailable())?;
    lock.lock().map_err(|_| unavailable())?;
    if fs::symlink_metadata(root.join("snapshot.json")).is_ok() {
        let old = read(&root.join("snapshot.json"))?;
        let old: Snapshot = serde_json::from_slice(&old).map_err(|_| unavailable())?;
        need(p.snapshot.generated_at_ms >= old.generated_at_ms)?;
        if p.snapshot.generated_at_ms == old.generated_at_ms {
            need(
                canonical(&serde_json::to_value(&p.snapshot).map_err(|_| bad())?)
                    == canonical(&serde_json::to_value(old).map_err(|_| bad())?),
            )?;
        }
    }
    if let Some(q) = &p.snapshot.quote_observation {
        if q.records_url.is_some() {
            let v = p.observation.as_ref().ok_or_else(bad)?;
            validate_public_json(v, 0)?;
            need(
                v.get("kind") == Some(&json!("external_quote_observation"))
                    && v.get("schema_version") == Some(&json!(1))
                    && v.get("observed_at_ms") == Some(&json!(q.observed_at_ms))
                    && v.get("completed_at_ms") == Some(&json!(q.completed_at_ms))
                    && v.get("status") == Some(&json!(q.status)),
            )?;
            let rows = v.get("quotes").and_then(Value::as_array).ok_or_else(bad)?;
            need(
                rows.len() <= 6
                    && rows
                        .iter()
                        .filter(|r| r.get("status") == Some(&json!("available")))
                        .count()
                        == q.available_quotes as usize,
            )?;
            let bytes = canonical(v);
            need(sha256_hex(&bytes) == q.observation_sha256)?;
            write(
                &artifact_path(&root, "observations", &q.observation_sha256)?,
                &bytes,
                true,
            )?;
        }
    } else {
        need(p.observation.is_none())?;
    }
    if let Some(u) = &p.snapshot.analyst.evidence_manifest_url {
        let h = link_hash(u, "manifests").ok_or_else(bad)?;
        let b = read(&artifact_path(&root, "manifests", &h)?)?;
        need(sha256_hex(&b) == h)?;
        let manifest: Value = serde_json::from_slice(&b).map_err(|_| bad())?;
        need(
            manifest.get("source_sha256").and_then(Value::as_str)
                == p.snapshot.analyst.source_sha256.as_deref(),
        )?;
        need(manifest.get("authorship").and_then(Value::as_str) == Some("covenant_paid_agent"))?;
    }
    let bytes = canonical(&serde_json::to_value(&p.snapshot).map_err(|_| bad())?);
    let h = sha256_hex(&bytes);
    write(&root.join(format!("snapshot-{h}.json")), &bytes, true)?;
    write(&root.join("snapshot.json"), &bytes, false)?;
    Ok(response(canonical(&json!({"published_sha256":h}))))
}
fn validate_public_json(v: &Value, depth: usize) -> Result<(), ApiError> {
    need(depth <= 24)?;
    match v {
        Value::Object(m) => {
            need(m.len() <= 100)?;
            for (k, v) in m {
                let lower = k.to_ascii_lowercase();
                need(
                    ![
                        "authorization",
                        "private_key",
                        "secret_key",
                        "password",
                        "access_token",
                        "refresh_token",
                        "bearer_token",
                        "operator_session",
                        "cookie",
                        "set-cookie",
                        "buyer_key",
                        "api_key",
                    ]
                    .contains(&lower.as_str()),
                )?;
                validate_public_json(v, depth + 1)?;
            }
        }
        Value::Array(a) => {
            need(a.len() <= 2000)?;
            for v in a {
                validate_public_json(v, depth + 1)?;
            }
        }
        Value::String(s) => {
            need(s.len() <= 256 * 1024 && !s.contains("-----BEGIN PRIVATE KEY-----"))?;
        }
        Value::Number(n) => need(n.is_i64() || n.is_u64())?,
        _ => {}
    };
    Ok(())
}
pub async fn publish_artifact(
    Path((kind, name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Result<Response, ApiError> {
    auth(&headers)?;
    need(matches!(kind.as_str(), "artifacts" | "manifests"))?;
    let h = name.strip_suffix(".json").ok_or_else(bad)?;
    need(hash(h))?;
    // Publishing arbitrary evidence is an explicit operator review, never an
    // automatic projection of a private job response.
    need(
        headers
            .get("x-publication-review-sha256")
            .and_then(|v| v.to_str().ok())
            == Some(h),
    )?;
    validate_public_json(&value, 0)?;
    let bytes = canonical(&value);
    need(sha256_hex(&bytes) == h)?;
    if kind == "manifests" {
        need(
            value.get("schema_version") == Some(&json!(1))
                && value.get("kind") == Some(&json!("pool_operator_evidence_manifest")),
        )?;
    }
    write(&artifact_path(&root()?, &kind, h)?, &bytes, true)?;
    Ok(response(canonical(&json!({"published_sha256":h}))))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Snapshot {
        serde_json::from_value(json!({"schema_version":1,"generated_at_ms":1000,"pool_funded":false,"stage":"preparing","quote_observation":null,"readiness":[{"id":"analyst","status":"pending","checked_at_ms":null},{"id":"pool_rehearsal","status":"pending","checked_at_ms":null},{"id":"conversion_route","status":"pending","checked_at_ms":null},{"id":"operator_approval","status":"pending","checked_at_ms":null},{"id":"shadow_trial","status":"pending","checked_at_ms":null}],"analyst":{"status":"pending","evidence_manifest_url":null,"source_sha256":null},"costs":{"reserved_usdc_micros":"1500000","coordinator_reported_charge_usdc_micros":"0","settlement":"unresolved"}})).unwrap()
    }
    #[test]
    fn readiness_cannot_hide_pending_or_missing_gates() {
        let mut s = valid();
        assert!(s.validate().is_ok());
        s.stage = "awaiting_liquidity".into();
        assert!(s.validate().is_err());
        s = valid();
        s.readiness.pop();
        assert!(s.validate().is_err());
        s = valid();
        s.pool_funded = true;
        assert!(s.validate().is_err());
    }
    #[test]
    fn accepted_requires_manifest_source_and_gate() {
        let mut s = valid();
        s.analyst.status = "accepted".into();
        assert!(s.validate().is_err());
        s.analyst.source_sha256 = Some("a".repeat(64));
        s.analyst.evidence_manifest_url =
            Some(format!("/pool-operator/manifests/{}.json", "b".repeat(64)));
        s.readiness[0].status = "passed".into();
        s.readiness[0].checked_at_ms = Some(999);
        assert!(s.validate().is_ok());
    }
    #[test]
    fn reject_private_fields_and_nonlocal_links() {
        assert!(validate_public_json(&json!({"child":{"operator_session":"secret"}}), 0).is_err());
        assert!(link_hash(
            "https://evil.test/pool-operator/manifests/a.json",
            "manifests"
        )
        .is_none());
        let mut v = serde_json::to_value(valid()).unwrap();
        v["private_key"] = json!("secret");
        assert!(serde_json::from_value::<Snapshot>(v).is_err());
    }
    #[test]
    fn canonical_is_recursive_and_hash_stable() {
        let v: Value = serde_json::from_str("{\"z\":[{\"y\":2,\"a\":1}],\"a\":0}").unwrap();
        assert_eq!(
            String::from_utf8(canonical(&v)).unwrap(),
            "{\"a\":0,\"z\":[{\"a\":1,\"y\":2}]}"
        );
    }
    #[test]
    fn quotes_require_hash_counts_and_chronology() {
        let mut s = valid();
        s.quote_observation = Some(Quotes {
            observed_at_ms: 900,
            completed_at_ms: 950,
            status: "available".into(),
            available_quotes: 6,
            observation_sha256: "c".repeat(64),
            records_url: Some(format!(
                "/pool-operator/observations/{}.json",
                "c".repeat(64)
            )),
        });
        assert!(s.validate().is_ok());
        s.quote_observation.as_mut().unwrap().available_quotes = 5;
        assert!(s.validate().is_err());
    }
    #[tokio::test]
    async fn publication_authentication_integrity_monotonicity_and_public_reads() {
        // These environment names belong only to this test/module. No other
        // test mutates them, and the entire filesystem lifecycle is sequential.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        std::env::set_var(DIR, &path);
        std::env::set_var(TOKEN, "test-only-publish-token-with-32-characters");
        let mut headers = HeaderMap::new();
        let p = || Publication {
            snapshot: valid(),
            observation: None,
        };
        assert!(matches!(
            publish(headers.clone(), Json(p())).await,
            Err(ApiError::Unauthorized(_))
        ));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong"),
        );
        assert!(matches!(
            publish(headers.clone(), Json(p())).await,
            Err(ApiError::Unauthorized(_))
        ));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer test-only-publish-token-with-32-characters"),
        );
        assert!(publish(headers.clone(), Json(p())).await.is_ok());
        assert!(publish(headers.clone(), Json(p())).await.is_ok());
        let response = snapshot().await.unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = axum::body::to_bytes(response.into_body(), MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            serde_json::to_value(valid()).unwrap()
        );
        let mut changed = p();
        changed.snapshot.costs.reserved_usdc_micros = "1".into();
        assert!(publish(headers.clone(), Json(changed)).await.is_err());
        let mut rollback = p();
        rollback.snapshot.generated_at_ms -= 1;
        assert!(publish(headers.clone(), Json(rollback)).await.is_err());

        let observation = json!({"schema_version":1,"kind":"external_quote_observation","observed_at_ms":1001,"completed_at_ms":1002,"status":"unavailable","quotes":[]});
        let h = sha256_hex(&canonical(&observation));
        let mut next = valid();
        next.generated_at_ms = 1003;
        next.quote_observation = Some(Quotes {
            observed_at_ms: 1001,
            completed_at_ms: 1002,
            status: "unavailable".into(),
            available_quotes: 0,
            observation_sha256: h.clone(),
            records_url: Some(format!("/pool-operator/observations/{h}.json")),
        });
        let mut tampered = observation.clone();
        tampered["extra"] = json!(true);
        assert!(publish(
            headers.clone(),
            Json(Publication {
                snapshot: next.clone(),
                observation: Some(tampered)
            })
        )
        .await
        .is_err());
        assert!(publish(
            headers.clone(),
            Json(Publication {
                snapshot: next.clone(),
                observation: Some(observation)
            })
        )
        .await
        .is_ok());
        assert!(artifact(Path(("observations".into(), format!("{h}.json"))))
            .await
            .is_ok());
        assert!(artifact(Path(("../private".into(), format!("{h}.json"))))
            .await
            .is_err());

        let manifest = json!({"schema_version":1,"kind":"pool_operator_evidence_manifest","authorship":"covenant_paid_agent","source_sha256":"a".repeat(64)});
        let mh = sha256_hex(&canonical(&manifest));
        let name = ("manifests".to_string(), format!("{mh}.json"));
        assert!(
            publish_artifact(Path(name.clone()), headers.clone(), Json(manifest.clone()))
                .await
                .is_err()
        );
        headers.insert(
            "x-publication-review-sha256",
            HeaderValue::from_str(&mh).unwrap(),
        );
        assert!(
            publish_artifact(Path(name.clone()), headers.clone(), Json(manifest.clone()))
                .await
                .is_ok()
        );
        assert!(
            publish_artifact(Path(name.clone()), headers.clone(), Json(manifest.clone()))
                .await
                .is_ok()
        );
        let mut wrong = manifest;
        wrong["source_sha256"] = json!("b".repeat(64));
        assert!(publish_artifact(Path(name), headers.clone(), Json(wrong))
            .await
            .is_err());
        next.generated_at_ms += 1;
        next.quote_observation = None;
        next.analyst.status = "accepted".into();
        next.analyst.source_sha256 = Some("b".repeat(64));
        next.analyst.evidence_manifest_url = Some(format!("/pool-operator/manifests/{mh}.json"));
        next.readiness[0].status = "passed".into();
        next.readiness[0].checked_at_ms = Some(1003);
        assert!(publish(
            headers.clone(),
            Json(Publication {
                snapshot: next.clone(),
                observation: None
            })
        )
        .await
        .is_err());
        next.analyst.source_sha256 = Some("a".repeat(64));
        assert!(publish(
            headers.clone(),
            Json(Publication {
                snapshot: next,
                observation: None
            })
        )
        .await
        .is_ok());
        let private = json!({"nested":{"api_key":"do-not-publish"}});
        let ph = sha256_hex(&canonical(&private));
        headers.insert(
            "x-publication-review-sha256",
            HeaderValue::from_str(&ph).unwrap(),
        );
        assert!(publish_artifact(
            Path(("artifacts".into(), format!("{ph}.json"))),
            headers,
            Json(private)
        )
        .await
        .is_err());
        assert!(!path.join(format!("artifacts-{ph}.json")).exists());
        std::env::remove_var(DIR);
        std::env::remove_var(TOKEN);
    }
    #[test]
    fn immutable_and_symlink_storage_refuses_replacement_or_readthrough() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("evidence.json");
        write(&p, b"one", true).unwrap();
        write(&p, b"one", true).unwrap();
        assert!(write(&p, b"two", true).is_err());
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(read(&link).is_err());
        assert!(write(&link, b"one", true).is_err());
    }
    #[test]
    fn interrupted_staging_does_not_poison_an_immutable_retry() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("artifacts-complete.json");
        // Simulate process death during staging, before the atomic link.
        let abandoned = destination.with_extension("tmp-interrupted");
        fs::write(&abandoned, b"partial").unwrap();
        assert!(!destination.exists());
        write(&destination, b"complete", true).unwrap();
        assert_eq!(read(&destination).unwrap(), b"complete");
        write(&destination, b"complete", true).unwrap();
        assert_eq!(fs::read(abandoned).unwrap(), b"partial");
    }
}
