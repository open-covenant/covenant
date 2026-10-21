//! Client-sealed workspace files. Encryption and decryption run locally with a
//! caller-owned vault key. Files stream through private temporary ciphertext;
//! restoration publishes a new file only after every chunk and digest verifies.

use std::path::{Path, PathBuf};

use covenant_compute_protocol::workspace::*;
use covenant_compute_protocol::VaultKey;
use covenant_identity::LocalIdentity;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{BuyerConfig, BuyerError};

pub struct WorkspaceClient<'a> {
    http: &'a reqwest::Client,
    config: &'a BuyerConfig,
    identity: &'a LocalIdentity,
}

/// Keep this object while retrying an uncertain PUT or commit. Its random
/// reservation id and ciphertext stay identical; no re-encryption or second
/// version is needed after a lost HTTP response.
pub struct PreparedWorkspaceSnapshot {
    workspace: WorkspaceView,
    file: PathBuf,
    _temporary: Option<tempfile::NamedTempFile>,
    pub upload: WorkspaceUpload,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedManifest {
    version: u8,
    workspace: WorkspaceView,
    upload: WorkspaceUpload,
}

impl PreparedWorkspaceSnapshot {
    /// Preserve ciphertext and reservation before any remote write. This state
    /// contains no plaintext or encryption key. Keep it until commit is known.
    pub fn persist(mut self, directory: &Path) -> Result<Self, BuyerError> {
        let parent = directory
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if directory.exists() {
            return Err(protocol("snapshot pending directory already exists"));
        }
        // An incomplete copy is never authoritative retry state. A crash can
        // leave only an ignored private staging directory, never half a manifest.
        let staging = tempfile::Builder::new()
            .prefix(".workspace-preparing-")
            .tempdir_in(parent)
            .map_err(protocol)?;
        let destination = staging.path().join("ciphertext");
        if let Some(file) = self._temporary.take() {
            // Persist by copy permits a state directory on another filesystem.
            let mut out = tempfile::NamedTempFile::new_in(staging.path()).map_err(protocol)?;
            std::io::copy(&mut file.reopen().map_err(protocol)?, out.as_file_mut())
                .map_err(protocol)?;
            out.as_file_mut().sync_all().map_err(protocol)?;
            out.persist_noclobber(&destination).map_err(protocol)?;
        } else {
            return Err(protocol("snapshot is already persistent"));
        }
        let manifest = PreparedManifest {
            version: 1,
            workspace: self.workspace.clone(),
            upload: self.upload.clone(),
        };
        let mut file = tempfile::NamedTempFile::new_in(staging.path()).map_err(protocol)?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec(&manifest).map_err(protocol)?)
            .map_err(protocol)?;
        file.as_file_mut().sync_all().map_err(protocol)?;
        file.persist_noclobber(staging.path().join("manifest.json"))
            .map_err(protocol)?;
        std::fs::File::open(staging.path())
            .and_then(|f| f.sync_all())
            .map_err(protocol)?;
        std::fs::rename(staging.path(), directory).map_err(protocol)?;
        std::fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(protocol)?;
        self.file = directory.join("ciphertext");
        Ok(self)
    }

    /// Reload an interrupted operation without generating another version or
    /// nonce. Verify local bytes before sending any coordinator request.
    pub fn load(directory: &Path) -> Result<Self, BuyerError> {
        let manifest: PreparedManifest = serde_json::from_slice(
            &std::fs::read(directory.join("manifest.json")).map_err(protocol)?,
        )
        .map_err(protocol)?;
        if manifest.version != 1
            || manifest.workspace.version.checked_add(1).is_none()
            || manifest.upload.expected_version != manifest.workspace.version
            || manifest.upload.upload_id.is_nil()
            || !valid_digest(&manifest.upload.ciphertext_sha256)
            || ciphertext_len(manifest.upload.plaintext_bytes).map_err(protocol)?
                != manifest.upload.ciphertext_bytes
        {
            return Err(protocol("invalid prepared workspace manifest"));
        }
        let file = directory.join("ciphertext");
        let mut input = std::fs::File::open(&file).map_err(protocol)?;
        if input.metadata().map_err(protocol)?.len() != manifest.upload.ciphertext_bytes {
            return Err(protocol("prepared ciphertext length changed"));
        }
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; CHUNK_BYTES];
        loop {
            let n = input.read(&mut buffer).map_err(protocol)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        let digest = hash
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        if digest != manifest.upload.ciphertext_sha256 {
            return Err(protocol("prepared ciphertext digest changed"));
        }
        Ok(Self {
            workspace: manifest.workspace,
            upload: manifest.upload,
            file,
            _temporary: None,
        })
    }

    pub fn workspace(&self) -> &WorkspaceView {
        &self.workspace
    }
}

fn protocol(error: impl std::fmt::Display) -> BuyerError {
    BuyerError::Protocol(error.to_string())
}
fn transport(action: &str) -> BuyerError {
    BuyerError::Coordinator(format!("workspace {action} failed; retry the same prepared snapshot if the upload outcome is unknown"))
}

impl<'a> WorkspaceClient<'a> {
    pub fn new(
        http: &'a reqwest::Client,
        config: &'a BuyerConfig,
        identity: &'a LocalIdentity,
    ) -> Self {
        Self {
            http,
            config,
            identity,
        }
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
    ) -> Result<T, BuyerError> {
        let stamp = crate::epoch_ms();
        let signature =
            sign_workspace(self.identity, method.as_str(), path, &body, stamp).map_err(protocol)?;
        let response = self
            .http
            .request(
                method,
                format!(
                    "{}{path}",
                    self.config.coordinator_url.trim_end_matches('/')
                ),
            )
            .header(WORKSPACE_SIGNED_AT_HEADER, stamp.to_string())
            .header(WORKSPACE_SIGNATURE_HEADER, signature)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| transport("request"))?;
        if !response.status().is_success() {
            let status = response.status();
            let reason = response.text().await.unwrap_or_default();
            return Err(BuyerError::Coordinator(format!(
                "workspace request returned {status}: {}",
                covenant_compute_protocol::coordinator_reason(&reason)
            )));
        }
        response
            .json()
            .await
            .map_err(|_| BuyerError::Coordinator("workspace response was malformed".into()))
    }

    fn path(&self, id: Option<Uuid>) -> String {
        let owner = self.identity.agent_id().pubkey_base58();
        match id {
            Some(id) => format!("/workspaces/{owner}/{id}"),
            None => format!("/workspaces/{owner}"),
        }
    }

    pub async fn create(
        &self,
        id: Uuid,
        name: impl Into<String>,
        trust_floor: WorkspaceTrustFloor,
    ) -> Result<WorkspaceView, BuyerError> {
        let request = CreateWorkspace {
            id,
            name: name.into(),
            trust_floor,
        };
        let view: WorkspaceView = self
            .request(
                reqwest::Method::POST,
                &self.path(None),
                serde_json::to_vec(&request).map_err(protocol)?,
            )
            .await?;
        if view.id != id
            || view.owner != self.identity.agent_id().pubkey_base58()
            || view.trust_floor != trust_floor
            || view.name != request.name
        {
            return Err(protocol(
                "coordinator returned different workspace identity or trust terms",
            ));
        }
        Ok(view)
    }

    pub async fn list(&self) -> Result<Vec<WorkspaceView>, BuyerError> {
        let views: Vec<WorkspaceView> = self
            .request(reqwest::Method::GET, &self.path(None), Vec::new())
            .await?;
        let mut ids = std::collections::BTreeSet::new();
        if views.iter().any(|view| {
            view.owner != self.identity.agent_id().pubkey_base58()
                || view.id.is_nil()
                || !ids.insert(view.id)
        }) {
            return Err(protocol(
                "workspace list changed owner or duplicates identities",
            ));
        }
        Ok(views)
    }

    pub async fn get(&self, id: Uuid) -> Result<WorkspaceView, BuyerError> {
        let view: WorkspaceView = self
            .request(reqwest::Method::GET, &self.path(Some(id)), Vec::new())
            .await?;
        if view.id != id || view.owner != self.identity.agent_id().pubkey_base58() {
            return Err(protocol("workspace identity mismatch"));
        }
        Ok(view)
    }

    pub async fn delete(&self, id: Uuid) -> Result<(), BuyerError> {
        let path = self.path(Some(id));
        let stamp = crate::epoch_ms();
        let signature =
            sign_workspace(self.identity, "DELETE", &path, &[], stamp).map_err(protocol)?;
        let response = self
            .http
            .delete(format!(
                "{}{}",
                self.config.coordinator_url.trim_end_matches('/'),
                path
            ))
            .header(WORKSPACE_SIGNED_AT_HEADER, stamp.to_string())
            .header(WORKSPACE_SIGNATURE_HEADER, signature)
            .send()
            .await
            .map_err(|_| transport("delete"))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(BuyerError::Coordinator(format!(
                "workspace delete returned {}",
                response.status()
            )))
        }
    }

    /// The supplied view is the caller's last known version. Store returned views
    /// locally when rollback detection across process restarts is required.
    pub async fn prepare_snapshot(
        &self,
        workspace: &WorkspaceView,
        key: &VaultKey,
        source: &Path,
    ) -> Result<PreparedWorkspaceSnapshot, BuyerError> {
        if workspace.owner != self.identity.agent_id().pubkey_base58() {
            return Err(protocol("workspace belongs to a different buyer"));
        }
        let workspace = workspace.clone();
        let key = key.clone();
        let source = source.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let input = std::fs::File::open(source).map_err(protocol)?;
            let length = input.metadata().map_err(protocol)?.len();
            let ciphertext_bytes = ciphertext_len(length).map_err(protocol)?;
            let version = workspace
                .version
                .checked_add(1)
                .ok_or_else(|| protocol("workspace version exhausted"))?;
            let context = SnapshotContext {
                owner: &workspace.owner,
                workspace_id: workspace.id,
                version,
                trust_floor: workspace.trust_floor,
                plaintext_bytes: length,
            };
            let mut file = tempfile::NamedTempFile::new().map_err(protocol)?;
            let ciphertext_sha256 =
                seal_snapshot(&key, &context, input, file.as_file_mut()).map_err(protocol)?;
            file.as_file_mut().sync_all().map_err(protocol)?;
            let upload = WorkspaceUpload {
                upload_id: Uuid::new_v4(),
                expected_version: workspace.version,
                plaintext_bytes: length,
                ciphertext_bytes,
                ciphertext_sha256,
            };
            Ok(PreparedWorkspaceSnapshot {
                workspace,
                file: file.path().to_path_buf(),
                _temporary: Some(file),
                upload,
            })
        })
        .await
        .map_err(protocol)?
    }

    pub async fn upload_snapshot(
        &self,
        prepared: &PreparedWorkspaceSnapshot,
    ) -> Result<WorkspaceView, BuyerError> {
        if prepared.workspace.owner != self.identity.agent_id().pubkey_base58() {
            return Err(protocol("prepared workspace belongs to another buyer"));
        }
        let path = self.path(Some(prepared.workspace.id));
        let grant: WorkspaceUploadGrant = self
            .request(
                reqwest::Method::POST,
                &format!("{path}/upload"),
                serde_json::to_vec(&prepared.upload).map_err(protocol)?,
            )
            .await?;
        if grant.upload_id != prepared.upload.upload_id
            || grant.version != prepared.workspace.version + 1
            || grant.grant.method != "PUT"
            || grant.grant.headers.get("if-none-match").map(String::as_str) != Some("*")
        {
            return Err(protocol(
                "coordinator returned a mismatched or mutable upload grant",
            ));
        }
        let storage = storage_client()?;
        validate_grant(&grant.grant)?;
        let input = tokio::fs::File::open(&prepared.file)
            .await
            .map_err(protocol)?;
        let stream = futures::stream::try_unfold(input, |mut file| async move {
            let mut bytes = vec![0u8; CHUNK_BYTES];
            let count = file.read(&mut bytes).await?;
            if count == 0 {
                Ok::<_, std::io::Error>(None)
            } else {
                bytes.truncate(count);
                Ok(Some((bytes, file)))
            }
        });
        let mut request = storage
            .put(&grant.grant.url)
            .body(reqwest::Body::wrap_stream(stream));
        for (key, value) in &grant.grant.headers {
            request = request.header(key, value);
        }
        let response = request
            .send()
            .await
            .map_err(|_| transport("ciphertext upload"))?;
        // A retry of a successful write-once PUT returns 412. Commit still
        // verifies S3's stored size and digest before accepting this version.
        if !response.status().is_success()
            && response.status() != reqwest::StatusCode::PRECONDITION_FAILED
        {
            return Err(BuyerError::Coordinator(format!(
                "workspace ciphertext upload returned {}",
                response.status()
            )));
        }
        let committed: WorkspaceView = self
            .request(
                reqwest::Method::POST,
                &format!("{path}/commit"),
                serde_json::to_vec(&CommitWorkspace {
                    upload_id: prepared.upload.upload_id,
                    expected_version: prepared.upload.expected_version,
                })
                .map_err(protocol)?,
            )
            .await?;
        let snapshot = committed
            .snapshot
            .as_ref()
            .ok_or_else(|| protocol("committed workspace has no snapshot"))?;
        if committed.id != prepared.workspace.id
            || committed.owner != prepared.workspace.owner
            || committed.trust_floor != prepared.workspace.trust_floor
            || committed.version != grant.version
            || snapshot.version != grant.version
            || snapshot.ciphertext_sha256 != prepared.upload.ciphertext_sha256
            || snapshot.ciphertext_bytes != prepared.upload.ciphertext_bytes
            || snapshot.plaintext_bytes != prepared.upload.plaintext_bytes
        {
            return Err(protocol(
                "committed snapshot does not match the ciphertext this buyer sealed",
            ));
        }
        Ok(committed)
    }

    pub async fn snapshot_file(
        &self,
        workspace: &WorkspaceView,
        key: &VaultKey,
        source: &Path,
    ) -> Result<WorkspaceView, BuyerError> {
        let prepared = self.prepare_snapshot(workspace, key, source).await?;
        self.upload_snapshot(&prepared).await
    }

    /// Restore a pinned snapshot into a live, buyer-signed Phala lease. Hardware
    /// verification gates both local restoration and the first plaintext
    /// upload. A second current lease read rejects replacement/closing while
    /// downloading. No private workspace key is sent to the guest.
    ///
    /// The destination is deterministic and never overwritten. An interrupted
    /// transfer may leave a partial remote file; reconcile that exact path
    /// before retrying, rather than creating another destination.
    pub async fn restore_to_lease(
        &self,
        workspace: &WorkspaceView,
        key: &VaultKey,
        envelope: &covenant_compute_protocol::SignedJobEnvelope,
        private_ssh_key: &Path,
    ) -> Result<String, BuyerError> {
        use crate::hardware::{scope_for_phala_lease, HardwareClient};
        use covenant_compute_attestation::vendor_types::TrustClass;
        if workspace.owner != self.identity.agent_id().pubkey_base58() {
            return Err(protocol("workspace belongs to a different buyer"));
        }
        let snapshot = workspace
            .snapshot
            .as_ref()
            .ok_or_else(|| protocol("workspace has no committed snapshot"))?;
        if snapshot.plaintext_bytes > crate::ssh::MAX_TRANSFER_BYTES {
            return Err(protocol(
                "lease workspace transfer exceeds the 64 MiB limit",
            ));
        }
        let floor = match workspace.trust_floor {
            WorkspaceTrustFloor::Open => TrustClass::Open,
            WorkspaceTrustFloor::Isolated => TrustClass::Isolated,
            WorkspaceTrustFloor::Attested => TrustClass::Attested,
            WorkspaceTrustFloor::Confidential => TrustClass::Confidential,
        };
        let hardware = HardwareClient::new(&self.config.coordinator_url, self.identity)?;
        let current = crate::lease_view(
            self.http,
            self.config,
            self.identity,
            envelope.payload.job_id,
        )
        .await?;
        let scope = scope_for_phala_lease(envelope, &current, &workspace.owner, crate::epoch_ms())?;
        if scope.profile
            == covenant_compute_attestation::binding::HardwareProfile::PhalaEphemeralDstack061
            && snapshot.plaintext_bytes > crate::relay::MAX_TRANSFER_BYTES
        {
            return Err(protocol(
                "ephemeral relay workspace transfer exceeds the 8 MiB payload limit",
            ));
        }
        hardware.verify_lease(&scope, floor).await?;
        let temporary = tempfile::tempdir().map_err(protocol)?;
        let plaintext = temporary.path().join("snapshot");
        self.restore_file(workspace, key, &plaintext, Some(scope.job_id))
            .await?;

        let current =
            crate::lease_view(self.http, self.config, self.identity, scope.job_id).await?;
        let latest =
            scope_for_phala_lease(envelope, &current, &workspace.owner, crate::epoch_ms())?;
        if latest != scope {
            return Err(protocol(
                "lease channel changed during workspace restoration",
            ));
        }
        let basename = format!("covenant-{}-v{}.snapshot", workspace.id, workspace.version);
        // upload_file freshly fetches and independently verifies the current
        // verdict again, then validates TLS and the single hardware-bound key.
        crate::ssh::SshClient::default()
            .upload_file(
                &hardware,
                &scope,
                floor,
                private_ssh_key,
                &plaintext,
                &basename,
            )
            .await?;
        Ok(format!("/workspace/{basename}"))
    }

    /// Restores the exact caller-pinned snapshot. `lease_job_id` asks the server
    /// to additionally authorize delivery into that live lease. The caller then
    /// transfers the resulting local file over its authenticated lease channel.
    /// Existing destination files are never overwritten.
    pub async fn restore_file(
        &self,
        workspace: &WorkspaceView,
        key: &VaultKey,
        destination: &Path,
        lease_job_id: Option<Uuid>,
    ) -> Result<(), BuyerError> {
        let expected = workspace
            .snapshot
            .clone()
            .ok_or_else(|| protocol("workspace has no committed snapshot"))?;
        if ciphertext_len(expected.plaintext_bytes).map_err(protocol)? != expected.ciphertext_bytes
            || !valid_digest(&expected.ciphertext_sha256)
        {
            return Err(protocol("snapshot metadata has an invalid size or digest"));
        }
        if workspace.owner != self.identity.agent_id().pubkey_base58()
            || expected.version != workspace.version
        {
            return Err(protocol("workspace owner or version mismatch"));
        }
        let grant: WorkspaceDownloadGrant = self
            .request(
                reqwest::Method::POST,
                &format!("{}/download", self.path(Some(workspace.id))),
                serde_json::to_vec(&DownloadWorkspace {
                    version: workspace.version,
                    lease_job_id,
                })
                .map_err(protocol)?,
            )
            .await?;
        if grant.workspace != *workspace || grant.grant.method != "GET" {
            return Err(protocol(
                "download grant changed the caller-pinned snapshot",
            ));
        }
        validate_grant(&grant.grant)?;
        let storage = storage_client()?;
        let mut request = storage.get(&grant.grant.url);
        for (key, value) in &grant.grant.headers {
            request = request.header(key, value);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| transport("ciphertext download"))?;
        if !response.status().is_success() {
            return Err(BuyerError::Coordinator(format!(
                "workspace ciphertext download returned {}",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|n| n != expected.ciphertext_bytes)
        {
            return Err(protocol("download length differs from committed snapshot"));
        }
        let file = tempfile::NamedTempFile::new().map_err(protocol)?;
        let mut out = tokio::fs::File::from_std(file.reopen().map_err(protocol)?);
        let mut count = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| transport("ciphertext download"))?
        {
            count = count
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| protocol("download length overflow"))?;
            if count > expected.ciphertext_bytes {
                return Err(protocol("download exceeds committed snapshot length"));
            }
            out.write_all(&chunk).await.map_err(protocol)?;
        }
        out.sync_all().await.map_err(protocol)?;
        drop(out);
        if count != expected.ciphertext_bytes {
            return Err(protocol("download was truncated"));
        }
        let key = key.clone();
        let workspace = workspace.clone();
        let destination: PathBuf = destination.into();
        tokio::task::spawn_blocking(move || {
            let context = SnapshotContext {
                owner: &workspace.owner,
                workspace_id: workspace.id,
                version: workspace.version,
                trust_floor: workspace.trust_floor,
                plaintext_bytes: expected.plaintext_bytes,
            };
            let parent = destination
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let mut plaintext = tempfile::NamedTempFile::new_in(parent).map_err(protocol)?;
            open_snapshot(
                &key,
                &context,
                &expected.ciphertext_sha256,
                file.reopen().map_err(protocol)?,
                plaintext.as_file_mut(),
            )
            .map_err(protocol)?;
            plaintext.as_file_mut().sync_all().map_err(protocol)?;
            plaintext
                .persist_noclobber(&destination)
                .map_err(protocol)?;
            std::fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(protocol)?;
            Ok(())
        })
        .await
        .map_err(protocol)?
    }
}

fn storage_client() -> Result<reqwest::Client, BuyerError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(900))
        .build()
        .map_err(protocol)
}

fn validate_grant(grant: &ObjectGrant) -> Result<(), BuyerError> {
    let url = reqwest::Url::parse(&grant.url).map_err(|_| protocol("invalid storage grant URL"))?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || grant.expires_at_ms <= crate::epoch_ms()
    {
        return Err(protocol("storage grant is expired or does not use HTTPS"));
    }
    Ok(())
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use std::io::Write;

    fn prepared() -> PreparedWorkspaceSnapshot {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let bytes = vec![7; ciphertext_len(0).unwrap() as usize];
        file.write_all(&bytes).unwrap();
        PreparedWorkspaceSnapshot {
            workspace: WorkspaceView {
                id: Uuid::new_v4(),
                owner: "owner".into(),
                name: "test".into(),
                trust_floor: WorkspaceTrustFloor::Open,
                version: 0,
                snapshot: None,
            },
            file: file.path().to_path_buf(),
            _temporary: Some(file),
            upload: WorkspaceUpload {
                upload_id: Uuid::new_v4(),
                expected_version: 0,
                plaintext_bytes: 0,
                ciphertext_bytes: bytes.len() as u64,
                ciphertext_sha256: sha256_hex(&bytes),
            },
        }
    }

    #[test]
    fn interrupted_preparation_never_publishes_unloadable_pending_state() {
        let directory = tempfile::tempdir().unwrap();
        let pending = directory.path().join("test.pending");
        let broken = prepared();
        // Fail after staging-directory creation, at the ciphertext copy step.
        std::fs::remove_file(&broken.file).unwrap();
        assert!(broken.persist(&pending).is_err());
        assert!(!pending.exists());
        // A process killed before publication may leave an ignored stage.
        let abandoned = directory.path().join(".workspace-preparing-interrupted");
        std::fs::create_dir(&abandoned).unwrap();
        std::fs::write(abandoned.join("ciphertext"), b"partial").unwrap();
        let original = prepared();
        let id = original.upload.upload_id;
        original.persist(&pending).unwrap();
        assert_eq!(
            PreparedWorkspaceSnapshot::load(&pending)
                .unwrap()
                .upload
                .upload_id,
            id
        );
        assert!(prepared().persist(&pending).is_err());
        assert_eq!(
            PreparedWorkspaceSnapshot::load(&pending)
                .unwrap()
                .upload
                .upload_id,
            id
        );
    }
}

#[cfg(test)]
mod lease_gate_tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse, Json, Router};
    use base64::Engine as _;
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{
        lease_input, phala,
        ssh::{SshAccess, SshHostKey, SshTransport},
        CapabilityRequirement, JobEnvelopePayload, JobKind, LeaseAccess, LeaseTerms, LeaseView,
        SignedJobEnvelope,
    };
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn restore_to_lease_checks_hardware_before_requesting_or_decrypting_snapshot() {
        rejected_restore(false, WorkspaceTrustFloor::Open, 4, "409").await;
        rejected_restore(true, WorkspaceTrustFloor::Open, 4, "409").await;
    }

    #[tokio::test]
    async fn ephemeral_floor_and_payload_limit_refuse_before_verdict_or_plaintext() {
        for floor in [
            WorkspaceTrustFloor::Isolated,
            WorkspaceTrustFloor::Attested,
            WorkspaceTrustFloor::Confidential,
        ] {
            rejected_restore(true, floor, 4, "only Open").await;
        }
        rejected_restore(
            true,
            WorkspaceTrustFloor::Open,
            crate::relay::MAX_TRANSFER_BYTES + 1,
            "8 MiB",
        )
        .await;
    }

    async fn rejected_restore(
        ephemeral: bool,
        floor: WorkspaceTrustFloor,
        bytes: u64,
        expected_error: &str,
    ) {
        let buyer = LocalIdentity::generate("workspace-ssh@buyer");
        let operator = LocalIdentity::generate("workspace-ssh@operator");
        let now = crate::epoch_ms();
        let key = SshHostKey {
            algorithm: "ssh-ed25519".into(),
            key_base64: base64::engine::general_purpose::STANDARD
                .encode([b"\0\0\0\x0bssh-ed25519\0\0\0\x20".as_slice(), &[0x9e; 32]].concat()),
        };
        let mut job = JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: buyer.agent_id().clone(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                inference_tariff: None,
                gpu_class: Some("cpu".into()),
                min_vram_gb: None,
                model_id: Some(
                    if ephemeral {
                        phala::PHALA_EPHEMERAL_CPU_MODEL
                    } else {
                        phala::PHALA_CPU_MODEL
                    }
                    .into(),
                ),
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![lease_input(LeaseTerms {
                max_duration_secs: 600,
                rate_micro_usdc_per_sec: 1,
                client_public_key: Some(format!("{} {}", key.algorithm, key.key_base64)),
            })
            .unwrap()],
            price_micro_usdc: 600,
            deadline_ms: 900_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "workspace-gate"),
            issued_at_ms: now - 1000,
            referral_code: None,
            stream: true,
        };
        let generation = if ephemeral {
            let relay = phala::PhalaRelayConfig {
                url: "https://relay.example.com".into(),
                session_id: job.job_id.to_string(),
            };
            let (_, spec) = phala::phala_ephemeral_workload(
                &job,
                &operator.agent_id().pubkey_base58(),
                &phala::reviewed_phala_ephemeral_wrapper(),
                &format!("sha256:{}", "ab".repeat(32)),
                &relay,
            )
            .unwrap();
            let generation = spec.lease_generation.clone();
            job.input
                .push(phala::phala_ephemeral_lease_input(spec).unwrap());
            generation
        } else {
            let (_, spec) = phala::phala_workload(
                &job,
                &operator.agent_id().pubkey_base58(),
                &phala::reviewed_phala_wrapper(),
                &format!("sha256:{}", "ab".repeat(32)),
            )
            .unwrap();
            let generation = spec.lease_generation.clone();
            job.input.push(phala::phala_lease_input(spec).unwrap());
            generation
        };
        let envelope = SignedJobEnvelope::sign(job, &buyer).unwrap();
        let lease = LeaseView {
            job_id: envelope.payload.job_id,
            status: "accepted".into(),
            access: Some(LeaseAccess {
                job_id: envelope.payload.job_id,
                endpoint: "structured TLS SSH".into(),
                ready_at_ms: now,
                note: None,
                source_price: None,
                ssh: Some(SshAccess {
                    host: if ephemeral {
                        "relay.example.com".into()
                    } else {
                        format!("{}-1022.{}", "a".repeat(40), phala::PHALA_GATEWAY)
                    },
                    port: 443,
                    username: "covenant".into(),
                    lease_generation: generation,
                    transport: Some(if ephemeral {
                        SshTransport::OutboundRelay
                    } else {
                        SshTransport::Tls
                    }),
                    observed_host_keys: vec![key],
                }),
            }),
            rate_micro_usdc_per_sec: 1,
            max_duration_secs: 600,
            elapsed_ms: 0,
            charged_micro_usdc: 0,
            close_requested: false,
        };
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = calls.clone();
        let lease_path = format!("/federation/jobs/{}/lease", envelope.payload.job_id);
        let view_path = lease_path.clone();
        let router = Router::new().fallback(move |request: axum::extract::Request| {
            let path = request.uri().path().to_owned();
            seen.lock().unwrap().push(path.clone());
            let lease = lease.clone();
            let is_view = path == view_path;
            async move {
                if is_view {
                    Json(lease).into_response()
                } else {
                    StatusCode::CONFLICT.into_response()
                }
            }
        });
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let config = BuyerConfig {
            coordinator_url: format!("http://{}", listener.local_addr().unwrap()),
            poll_interval: std::time::Duration::from_millis(10),
            referral_code: None,
            rpc_url: None,
        };
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let http = crate::http_client();
        let client = WorkspaceClient::new(&http, &config, &buyer);
        let workspace = WorkspaceView {
            id: Uuid::new_v4(),
            owner: buyer.agent_id().pubkey_base58(),
            name: "gated".into(),
            trust_floor: floor,
            version: 1,
            snapshot: Some(WorkspaceSnapshot {
                version: 1,
                plaintext_bytes: bytes,
                ciphertext_bytes: ciphertext_len(bytes).unwrap(),
                ciphertext_sha256: "a".repeat(64),
                created_at_ms: now,
            }),
        };
        let error = client
            .restore_to_lease(
                &workspace,
                &VaultKey::random(),
                &envelope,
                Path::new("/missing-private-ssh-key"),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected_error), "{error}");
        let mut expected = vec![lease_path];
        if expected_error == "409" {
            expected.push(format!(
                "/federation/hardware/{}/verdict",
                envelope.payload.job_id
            ));
        }
        assert_eq!(*calls.lock().unwrap(), expected);
        // No workspace download grant, object fetch, decryption, or SSH request
        // occurred. A server-selected scope cannot bypass this gate.
        server.abort();
    }
}
