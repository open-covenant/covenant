//! Durable local checkpoint state for the workspace CLI. Keys never enter an
//! API request or normal output. A process lock covers pin and pending changes.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use covenant_compute_buyer::workspace::{PreparedWorkspaceSnapshot, WorkspaceClient};
use covenant_compute_protocol::workspace::{sha256_hex, WorkspaceTrustFloor, WorkspaceView};
use covenant_compute_protocol::VaultKey;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{positional, prepared_home, take_flag_value, Ctx};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalWorkspace {
    format_version: u8,
    endpoint: String,
    workspace: WorkspaceView,
    key_b58: String,
    #[serde(default)]
    deleted: bool,
}

struct LocalStore {
    path: PathBuf,
    endpoint: String,
    owner: String,
    _lock: std::fs::File,
}

impl LocalStore {
    fn open(ctx: &Ctx) -> Result<Self> {
        let endpoint = ctx.config.coordinator_url.trim_end_matches('/').to_owned();
        let owner = ctx.identity.agent_id().pubkey_base58();
        let path = prepared_home()?.join("workspaces").join(sha256_hex(
            serde_json::to_string(&(&endpoint, &owner))?.as_bytes(),
        ));
        std::fs::create_dir_all(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(path.join("state.lock"))?;
        lock.try_lock()
            .context("another workspace command is using this buyer and endpoint")?;
        // Under the writer lock, these reserved private names can only be
        // unfinished local preparation (never dispatched) or already-retired
        // committed state. They are not authoritative retry directories.
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type()?.is_dir()
                && (name.starts_with(".workspace-preparing-")
                    || name.starts_with(".workspace-retired-"))
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(Self {
            path,
            endpoint,
            owner,
            _lock: lock,
        })
    }

    fn pin_path(&self, id: Uuid) -> PathBuf {
        self.path.join(format!("{id}.json"))
    }
    fn pending_path(&self, id: Uuid) -> PathBuf {
        self.path.join(format!("{id}.pending"))
    }

    fn load(&self, id: Uuid) -> Result<LocalWorkspace> {
        let value: LocalWorkspace =
            serde_json::from_slice(&std::fs::read(self.pin_path(id)).context(
                "no local workspace pin; restore the matching private workspace backup first",
            )?)?;
        anyhow::ensure!(
            value.format_version == 1
                && value.endpoint == self.endpoint
                && value.workspace.owner == self.owner
                && value.workspace.id == id,
            "local workspace belongs to another endpoint or buyer, or uses an unsupported format"
        );
        VaultKey::from_b58(&value.key_b58).context("local workspace key is invalid")?;
        Ok(value)
    }

    fn save(&self, value: &LocalWorkspace) -> Result<()> {
        let mut temporary = tempfile::NamedTempFile::new_in(&self.path)?;
        temporary.write_all(&serde_json::to_vec(value)?)?;
        temporary.as_file_mut().sync_all()?;
        temporary.persist(self.pin_path(value.workspace.id))?;
        std::fs::File::open(&self.path)?.sync_all()?;
        Ok(())
    }

    fn finish(&self, mut local: LocalWorkspace, committed: WorkspaceView) -> Result<WorkspaceView> {
        anyhow::ensure!(
            committed.id == local.workspace.id
                && committed.owner == self.owner
                && committed.name == local.workspace.name
                && committed.trust_floor == local.workspace.trust_floor
                && committed.version >= local.workspace.version,
            "committed workspace changed identity, trust terms or rolled back"
        );
        local.workspace = committed.clone();
        self.save(&local)?;
        // The verified pin is durable before pending ciphertext can be removed.
        let pending = self.pending_path(committed.id);
        if pending.exists() {
            // Retire the whole authoritative state atomically before recursive
            // deletion. Interrupted cleanup cannot turn a valid retry into a
            // pending directory with missing ciphertext or manifest.
            let retired = retire_pending(&pending, &self.path)?;
            std::fs::remove_dir_all(retired)?;
        }
        std::fs::File::open(&self.path)?.sync_all()?;
        Ok(committed)
    }
}

fn retire_pending(pending: &Path, parent: &Path) -> Result<PathBuf> {
    let retired = parent.join(format!(".workspace-retired-{}", Uuid::new_v4()));
    std::fs::rename(pending, &retired)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(retired)
}

pub async fn run(ctx: &Ctx, args: &mut Vec<String>, json_out: bool) -> Result<()> {
    let action = positional(
        args,
        0,
        "workspace action (create|list|status|save|retry|restore|delete|backup|import)",
    )?;
    args.remove(0);
    let client = WorkspaceClient::new(&ctx.http, &ctx.config, &ctx.identity);
    let store = LocalStore::open(ctx)?;
    let output = match action.as_str() {
        "create" => {
            let id = take_flag_value(args, "--id")
                .map(|v| v.parse::<Uuid>())
                .transpose()?
                .unwrap_or_else(Uuid::new_v4);
            let floor = match take_flag_value(args, "--trust")
                .as_deref()
                .unwrap_or("open")
            {
                "open" => WorkspaceTrustFloor::Open,
                "isolated" => WorkspaceTrustFloor::Isolated,
                "attested" => WorkspaceTrustFloor::Attested,
                "confidential" => WorkspaceTrustFloor::Confidential,
                _ => anyhow::bail!("--trust must be open, isolated, attested or confidential"),
            };
            anyhow::ensure!(
                args.len() == 1,
                "workspace create <name> [--id UUID] [--trust open]"
            );
            let name = &args[0];
            anyhow::ensure!(
                !id.is_nil()
                    && !name.trim().is_empty()
                    && name.len() <= 64
                    && !name.chars().any(char::is_control),
                "workspace needs a nonnil UUID and a 1..64 byte name without control characters"
            );
            let local = if store.pin_path(id).exists() {
                let local = store.load(id)?;
                anyhow::ensure!(
                    !local.deleted
                        && local.workspace.name == *name
                        && local.workspace.trust_floor == floor,
                    "workspace id already has different local terms or is deleted"
                );
                local
            } else {
                LocalWorkspace {
                    format_version: 1,
                    endpoint: store.endpoint.clone(),
                    workspace: WorkspaceView {
                        id,
                        owner: store.owner.clone(),
                        name: name.clone(),
                        trust_floor: floor,
                        version: 0,
                        snapshot: None,
                    },
                    key_b58: VaultKey::random().to_b58(),
                    deleted: false,
                }
            };
            store.save(&local)?;
            eprintln!("Workspace id: {id}. If interrupted, retry create with --id {id}.");
            let view = client.create(id, name, floor).await?;
            anyhow::ensure!(
                view == local.workspace,
                "remote workspace differs from the pinned local version; use status to inspect"
            );
            serde_json::to_value(view)?
        }
        "list" => {
            anyhow::ensure!(args.is_empty(), "workspace list takes no arguments");
            serde_json::to_value(client.list().await?)?
        }
        "status" => {
            let id = parse_id(args, 1)?;
            let local = store.load(id)?;
            let remote = client.get(id).await?;
            anyhow::ensure!(
                remote.version >= local.workspace.version,
                "coordinator rolled back below the locally pinned version"
            );
            serde_json::json!({ "pinned": local.workspace, "remote": remote, "pending": store.pending_path(id).exists(), "deleted_locally": local.deleted })
        }
        "save" => {
            let id = parse_id(args, 2)?;
            let local = store.load(id)?;
            anyhow::ensure!(!local.deleted, "workspace is deleted");
            let pending = store.pending_path(id);
            anyhow::ensure!(!pending.exists(), "a snapshot is pending; use workspace retry {id} to reconcile it before saving again");
            let key = VaultKey::from_b58(&local.key_b58)?;
            let prepared = client
                .prepare_snapshot(&local.workspace, &key, Path::new(&args[1]))
                .await?
                .persist(&pending)?;
            let committed = client.upload_snapshot(&prepared).await.with_context(|| {
                format!("snapshot remains saved locally; retry with workspace retry {id}")
            })?;
            serde_json::to_value(store.finish(local, committed)?)?
        }
        "retry" => {
            let id = parse_id(args, 1)?;
            let local = store.load(id)?;
            anyhow::ensure!(!local.deleted, "workspace is deleted");
            let prepared = PreparedWorkspaceSnapshot::load(&store.pending_path(id))?;
            anyhow::ensure!(
                prepared.workspace().id == id
                    && prepared.workspace().owner == store.owner
                    && prepared.workspace().trust_floor == local.workspace.trust_floor
                    && (prepared.workspace().version == local.workspace.version
                        || prepared.workspace().version.checked_add(1)
                            == Some(local.workspace.version)),
                "pending snapshot no longer matches the pinned workspace"
            );
            let committed = client.upload_snapshot(&prepared).await?;
            serde_json::to_value(store.finish(local, committed)?)?
        }
        "restore" => {
            let lease = take_flag_value(args, "--lease")
                .map(|v| v.parse::<Uuid>())
                .transpose()?;
            let id = parse_id(args, 2)?;
            let local = store.load(id)?;
            anyhow::ensure!(!local.deleted, "workspace is deleted");
            client
                .restore_file(
                    &local.workspace,
                    &VaultKey::from_b58(&local.key_b58)?,
                    Path::new(&args[1]),
                    lease,
                )
                .await?;
            serde_json::json!({ "workspace_id": id, "version": local.workspace.version, "restored": args[1], "lease_job_id": lease })
        }
        "delete" => {
            let id = parse_id(args, 1)?;
            let mut local = store.load(id)?;
            client.delete(id).await?;
            local.deleted = true;
            store.save(&local)?;
            serde_json::json!({"workspace_id":id,"deleted":true,"cleanup":"scheduled after outstanding grants expire"})
        }
        "backup" => {
            let id = parse_id(args, 2)?;
            anyhow::ensure!(
                !store.pending_path(id).exists(),
                "resolve the pending snapshot before exporting a pinned backup"
            );
            let local = store.load(id)?;
            let destination = Path::new(&args[1]);
            let parent = destination
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(&serde_json::to_vec(&local)?)?;
            temporary.as_file_mut().sync_all()?;
            temporary.persist_noclobber(destination)?;
            std::fs::File::open(parent)?.sync_all()?;
            serde_json::json!({"workspace_id":id,"private_backup":args[1],"contains_encryption_key":true})
        }
        "import" => {
            anyhow::ensure!(args.len() == 1, "workspace import <private-backup.json>");
            let backup: LocalWorkspace = serde_json::from_slice(&std::fs::read(&args[0])?)?;
            anyhow::ensure!(
                backup.format_version == 1
                    && backup.endpoint == store.endpoint
                    && backup.workspace.owner == store.owner
                    && !backup.deleted,
                "backup must be active and match this buyer identity and coordinator endpoint"
            );
            VaultKey::from_b58(&backup.key_b58)?;
            if store.pin_path(backup.workspace.id).exists() {
                let prior = store.load(backup.workspace.id)?;
                anyhow::ensure!(
                    !prior.deleted
                        && prior.key_b58 == backup.key_b58
                        && prior.workspace.trust_floor == backup.workspace.trust_floor
                        && prior.workspace.version <= backup.workspace.version,
                    "backup would replace key, trust terms, a tombstone or a newer pin"
                );
                if prior.workspace.version == backup.workspace.version {
                    anyhow::ensure!(
                        prior.workspace == backup.workspace,
                        "backup conflicts at the same version"
                    );
                }
            }
            store.save(&backup)?;
            serde_json::to_value(backup.workspace)?
        }
        _ => anyhow::bail!("unknown workspace action {action:?}"),
    };
    if json_out {
        println!("{}", serde_json::to_string(&output)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&output)?);
    }
    Ok(())
}

fn parse_id(args: &[String], expected: usize) -> Result<Uuid> {
    anyhow::ensure!(
        args.len() == expected && !args.iter().any(|a| a.starts_with("--")),
        "wrong workspace arguments; run --help for usage"
    );
    Ok(positional(args, 0, "workspace UUID")?.parse()?)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn interrupted_retired_directory_removal_cannot_corrupt_authoritative_pending() {
        let home = tempfile::tempdir().unwrap();
        let pending = home.path().join("checkpoint.pending");
        std::fs::create_dir(&pending).unwrap();
        std::fs::write(pending.join("manifest.json"), b"manifest").unwrap();
        std::fs::write(pending.join("ciphertext"), b"ciphertext").unwrap();
        let retired = retire_pending(&pending, home.path()).unwrap();
        // Model process interruption halfway through recursive removal.
        std::fs::remove_file(retired.join("manifest.json")).unwrap();
        assert!(!pending.exists());
        std::fs::create_dir(&pending).unwrap();
        std::fs::write(pending.join("manifest.json"), b"new manifest").unwrap();
        std::fs::remove_dir_all(retired).unwrap();
        assert_eq!(
            std::fs::read(pending.join("manifest.json")).unwrap(),
            b"new manifest"
        );
    }
}
