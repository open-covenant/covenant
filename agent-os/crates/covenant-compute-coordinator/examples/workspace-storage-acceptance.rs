//! Opt-in acceptance against the configured private S3 provider. Writes three
//! tiny, uniquely named synthetic objects, then removes them after known writes.
//! Unknown PUT outcomes retain cleanup keys; expiry alone cannot retire them.
//! Does not provision a bucket, make a Compute purchase, or print signed URLs.
use covenant_compute_coordinator::workspace::{
    S3WorkspaceConfig, S3WorkspaceObjects, WorkspaceObjects,
};
use covenant_compute_protocol::workspace::{sha256_hex, ObjectGrant, WorkspaceUpload};
use std::{collections::BTreeMap, sync::Arc};

// Send the beginning of a real HTTP PUT while withholding the remainder until
// the coordinator has installed its empty fence. No signed URL is printed.
struct PausedBody {
    first: Option<axum::body::Bytes>,
    rest: Option<axum::body::Bytes>,
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: tokio::sync::oneshot::Receiver<()>,
    length: u64,
}
impl http_body::Body for PausedBody {
    type Data = axum::body::Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use std::{future::Future, task::Poll};
        if let Some(bytes) = self.first.take() {
            self.length -= bytes.len() as u64;
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            return Poll::Ready(Some(Ok(http_body::Frame::data(bytes))));
        }
        if self.rest.is_none() {
            return Poll::Ready(None);
        }
        match std::pin::Pin::new(&mut self.release).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(_)) => {
                Poll::Ready(Some(Err(std::io::Error::other("acceptance cancelled"))))
            }
            Poll::Ready(Ok(())) => {
                self.length = 0;
                Poll::Ready(
                    self.rest
                        .take()
                        .map(|bytes| Ok(http_body::Frame::data(bytes))),
                )
            }
        }
    }
    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.length)
    }
    fn is_end_stream(&self) -> bool {
        self.first.is_none() && self.rest.is_none()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn required(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| anyhow::anyhow!("{name} is required"))
}
async fn request(
    client: &reqwest::Client,
    grant: ObjectGrant,
    body: Option<Vec<u8>>,
) -> anyhow::Result<reqwest::Response> {
    let mut request = client.request(
        reqwest::Method::from_bytes(grant.method.as_bytes())?,
        &grant.url,
    );
    for (key, value) in grant.headers {
        request = request.header(key, value);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("object-store request outcome unknown"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    anyhow::ensure!(std::env::args().any(|a| a == "--allow-synthetic-storage-writes"),
        "pass --allow-synthetic-storage-writes after authorizing this configured provider acceptance");
    let objects = Arc::new(S3WorkspaceObjects::new(S3WorkspaceConfig {
        endpoint: required("COVENANT_COMPUTE_WORKSPACE_S3_ENDPOINT")?,
        region: required("COVENANT_COMPUTE_WORKSPACE_S3_REGION")?,
        bucket: required("COVENANT_COMPUTE_WORKSPACE_S3_BUCKET")?,
        prefix: std::env::var("COVENANT_COMPUTE_WORKSPACE_S3_PREFIX")
            .unwrap_or_else(|_| "covenant-workspaces".into()),
        access_key: required("COVENANT_COMPUTE_WORKSPACE_S3_ACCESS_KEY")?,
        secret_key: required("COVENANT_COMPUTE_WORKSPACE_S3_SECRET_KEY")?,
        session_token: std::env::var("COVENANT_COMPUTE_WORKSPACE_S3_SESSION_TOKEN").ok(),
    })?);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let run = uuid::Uuid::new_v4();
    let first = format!("acceptance/{run}/valid");
    let wrong = format!("acceptance/{run}/bad-checksum");
    let slow = format!("acceptance/{run}/inflight-fence");
    let mut checks = BTreeMap::new();
    let mut unknown_put = false;
    let mut cleanup_not_before_ms = 0u64;
    let outcome: anyhow::Result<()> = async {
        let bytes = b"Covenant synthetic encrypted workspace provider acceptance".to_vec();
        let upload = WorkspaceUpload {
            upload_id: run,
            expected_version: 0,
            plaintext_bytes: 0,
            ciphertext_bytes: bytes.len() as u64,
            ciphertext_sha256: sha256_hex(&bytes),
        };
        let put = objects.put_grant(&first, &upload, now())?;
        cleanup_not_before_ms = put.expires_at_ms.saturating_add(120_000);
        let status = request(&client, put.clone(), Some(bytes.clone()))
            .await
            .inspect_err(|_| {
                unknown_put = true;
            })?
            .status();
        if status.is_server_error() {
            unknown_put = true;
        }
        anyhow::ensure!(status.is_success(), "initial PUT failed with {status}");
        checks.insert("put", true);
        let repeated = request(&client, put, Some(bytes.clone()))
            .await
            .inspect_err(|_| {
                unknown_put = true;
            })?
            .status();
        if repeated.is_server_error() {
            unknown_put = true;
        }
        anyhow::ensure!(
            repeated == reqwest::StatusCode::PRECONDITION_FAILED,
            "duplicate conditional PUT must return412"
        );
        checks.insert("immutable_put", true);
        let mut bad = bytes.clone();
        bad[0] ^= 1;
        let bad_grant = objects.put_grant(&wrong, &upload, now())?;
        cleanup_not_before_ms =
            cleanup_not_before_ms.max(bad_grant.expires_at_ms.saturating_add(120_000));
        let bad_status = request(&client, bad_grant, Some(bad))
            .await
            .inspect_err(|_| {
                unknown_put = true;
            })?
            .status();
        if bad_status.is_server_error() {
            unknown_put = true;
        }
        anyhow::ensure!(
            bad_status == reqwest::StatusCode::BAD_REQUEST,
            "provider must reject a mismatched SHA256 with400"
        );
        checks.insert("checksum_rejection", true);
        let metadata = objects.head(&first, now()).await?;
        anyhow::ensure!(
            metadata.ciphertext_bytes == upload.ciphertext_bytes
                && metadata.ciphertext_sha256 == upload.ciphertext_sha256,
            "provider HEAD must return the verified SHA256 and size"
        );
        checks.insert("head_verified_checksum", true);
        let grant = objects.get_grant(&first, now(), 300_000)?;
        let mut anonymous = reqwest::Url::parse(&grant.url)?;
        anonymous.set_query(None);
        let unauth = client
            .get(anonymous)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("anonymous read failed"))?
            .status();
        anyhow::ensure!(!unauth.is_success(), "private object unexpectedly public");
        checks.insert("private_object", true);
        let returned = request(&client, grant, None)
            .await?
            .bytes()
            .await
            .map_err(|_| anyhow::anyhow!("GET body failed"))?;
        anyhow::ensure!(returned.as_ref() == bytes, "download changed bytes");
        checks.insert("get_exact_bytes", true);
        let expired = objects.get_grant(&first, now(), 1000)?;
        tokio::time::sleep(std::time::Duration::from_millis(2200)).await;
        let expired_status = request(&client, expired, None).await?.status();
        anyhow::ensure!(
            !expired_status.is_success(),
            "expired grant unexpectedly accepted"
        );
        checks.insert("grant_expiry", true);
        let slow_grant = objects.put_grant(&slow, &upload, now())?;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let body = PausedBody {
            first: Some(bytes[..8].to_vec().into()),
            rest: Some(bytes[8..].to_vec().into()),
            entered: Some(entered_tx),
            release: release_rx,
            length: bytes.len() as u64,
        };
        let mut slow_request = client.put(&slow_grant.url).body(reqwest::Body::wrap(body));
        for (key, value) in slow_grant.headers {
            slow_request = slow_request.header(key, value);
        }
        unknown_put = true;
        let inflight = tokio::spawn(async move {
            slow_request
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("delayed PUT outcome unknown"))
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), entered_rx).await??;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        anyhow::ensure!(
            !inflight.is_finished(),
            "PUT did not remain in flight before fencing"
        );
        objects.fence(&slow, now()).await?;
        checks.insert("empty_fence_verified", true);
        release_tx
            .send(())
            .map_err(|_| anyhow::anyhow!("delayed body receiver disappeared"))?;
        let late_status = inflight.await??.status();
        unknown_put = late_status.is_server_error();
        anyhow::ensure!(
            late_status == reqwest::StatusCode::PRECONDITION_FAILED,
            "in-flight conditional PUT after fence must return412; got {late_status}"
        );
        let fenced = objects.head(&slow, now()).await?;
        anyhow::ensure!(
            fenced.ciphertext_bytes == 0 && fenced.ciphertext_sha256 == sha256_hex(b""),
            "delayed PUT resurrected ciphertext after fence"
        );
        checks.insert("inflight_put_rejected_after_fence", true);
        checks.insert("no_ciphertext_resurrection", true);
        Ok(())
    }
    .await;
    let mut cleanup_ok = !unknown_put;
    if !unknown_put {
        // Synthetic requests are now definitively complete; only this isolated
        // acceptance may remove its fence. Production never removes fences.
        for key in [&first, &wrong, &slow] {
            cleanup_ok &= objects.delete(key, now()).await.is_ok();
        }
    }
    checks.insert("cleanup", cleanup_ok);
    let report = serde_json::json!({"schema":"covenant.workspace.storage-acceptance.v2","run_id":run,"checked_at_ms":now(),"accepted":outcome.is_ok() && cleanup_ok,"checks":checks,"error":outcome.as_ref().err().map(ToString::to_string),"cleanup_keys":if cleanup_ok {vec![]} else {vec![first,wrong,slow]},"unknown_put":unknown_put,"earliest_grant_expiry_ms":cleanup_not_before_ms,"cleanup_requires_fence_or_definitive_put_outcome":unknown_put});
    println!("{}", serde_json::to_string_pretty(&report)?);
    anyhow::ensure!(
        outcome.is_ok() && cleanup_ok,
        "workspace provider acceptance failed; inspect the verdict above"
    );
    Ok(())
}
