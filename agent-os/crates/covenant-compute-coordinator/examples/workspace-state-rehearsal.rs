//! Offline replay of a copied workspace journal. The input is read only; actual
//! WorkspaceService replay/compaction runs on private temporary copies. No
//! provider, financial, cleanup or background worker is constructed or started.
use anyhow::{anyhow, ensure};
use async_trait::async_trait;
use covenant_compute_coordinator::{
    workspace::{ObjectMetadata, WorkspaceError, WorkspaceObjects, WorkspaceService},
    CoordinatorConfig, CoordinatorState, MockPayout, NoReputation,
};
use covenant_compute_protocol::workspace::{sha256_hex, ObjectGrant, WorkspaceUpload};
use covenant_identity::LocalIdentity;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io::Write, path::Path, sync::Arc};

struct NoObjects;

#[async_trait]
impl WorkspaceObjects for NoObjects {
    fn put_grant(
        &self,
        _: &str,
        _: &WorkspaceUpload,
        _: u64,
    ) -> Result<ObjectGrant, WorkspaceError> {
        Err(WorkspaceError::Config(
            "offline rehearsal forbids object actions".into(),
        ))
    }
    fn get_grant(&self, _: &str, _: u64, _: u64) -> Result<ObjectGrant, WorkspaceError> {
        Err(WorkspaceError::Config(
            "offline rehearsal forbids object actions".into(),
        ))
    }
    async fn head(&self, _: &str, _: u64) -> Result<ObjectMetadata, WorkspaceError> {
        Err(WorkspaceError::Config(
            "offline rehearsal forbids object actions".into(),
        ))
    }
    async fn delete(&self, _: &str, _: u64) -> Result<(), WorkspaceError> {
        Err(WorkspaceError::Config(
            "offline rehearsal forbids object actions".into(),
        ))
    }
    async fn fence(&self, _: &str, _: u64) -> Result<(), WorkspaceError> {
        Err(WorkspaceError::Config(
            "offline rehearsal forbids object actions".into(),
        ))
    }
}

type Rows = BTreeMap<(String, String), Value>;

fn latest_rows(bytes: &[u8]) -> anyhow::Result<Rows> {
    let complete = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let mut rows = BTreeMap::new();
    for line in bytes[..complete]
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
    {
        let mut row: Value = serde_json::from_slice(line)
            .map_err(|_| anyhow!("invalid complete workspace journal record"))?;
        let object = row
            .as_object_mut()
            .ok_or_else(|| anyhow!("journal record must be an object"))?;
        object.entry("schema_version").or_insert(json!(1));
        object.entry("usage_day").or_insert(json!(0));
        object.entry("object_actions").or_insert(json!(0));
        object.entry("fenced_objects").or_insert(json!(0));
        let owner = row["view"]["owner"]
            .as_str()
            .ok_or_else(|| anyhow!("missing owner"))?
            .to_owned();
        let id = row["view"]["id"]
            .as_str()
            .ok_or_else(|| anyhow!("missing workspace ID"))?
            .to_owned();
        rows.insert((owner, id), row);
    }
    Ok(rows)
}

fn replay(path: &Path) -> anyhow::Result<Rows> {
    let coordinator = CoordinatorState::new(
        LocalIdentity::generate("offline-workspace-rehearsal"),
        CoordinatorConfig::default(),
        Arc::new(NoReputation),
        Arc::new(MockPayout::new()),
        Arc::new(covenant_audit::InMemoryAuditLog::new()),
    );
    let service = WorkspaceService::open(path, Arc::new(NoObjects), coordinator, 1024)
        .map_err(|_| anyhow!("workspace journal replay refused the copy"))?
        .with_writes_enabled(false);
    drop(service);
    latest_rows(&fs::read(path)?)
}

fn number(value: &Value) -> anyhow::Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| anyhow!("invalid workspace counter"))
}

fn add(total: &mut u64, value: u64) -> anyhow::Result<()> {
    *total = total
        .checked_add(value)
        .ok_or_else(|| anyhow!("workspace counter overflow"))?;
    Ok(())
}

fn summary(rows: &Rows) -> anyhow::Result<Value> {
    let latest_day = rows
        .values()
        .map(|r| number(&r["usage_day"]))
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .max()
        .unwrap_or(0);
    let mut committed = 0;
    let mut pending = 0;
    let mut garbage = 0;
    let mut fences = 0;
    let mut current_bytes = 0;
    let mut pending_bytes = 0;
    let mut garbage_bytes = 0;
    let mut actions = 0;
    let mut latest_day_actions = 0;
    let mut tombstones = 0;
    let mut versions = 0;
    let mut owners = std::collections::BTreeSet::new();
    for ((owner, _), row) in rows {
        owners.insert(owner);
        add(&mut fences, number(&row["fenced_objects"])?)?;
        add(&mut versions, number(&row["view"]["version"])?)?;
        add(&mut actions, number(&row["object_actions"])?)?;
        if number(&row["usage_day"])? == latest_day {
            add(&mut latest_day_actions, number(&row["object_actions"])?)?;
        }
        if row["deleted"]
            .as_bool()
            .ok_or_else(|| anyhow!("invalid tombstone flag"))?
        {
            tombstones += 1;
        }
        if !row["current_key"].is_null() {
            committed += 1;
            add(
                &mut current_bytes,
                number(&row["view"]["snapshot"]["ciphertext_bytes"])?,
            )?;
        }
        if !row["pending"].is_null() {
            pending += 1;
            add(
                &mut pending_bytes,
                number(&row["pending"]["request"]["ciphertext_bytes"])?,
            )?;
        }
        for item in row["garbage"]
            .as_array()
            .ok_or_else(|| anyhow!("invalid cleanup obligations"))?
        {
            garbage += 1;
            add(&mut garbage_bytes, number(&item["bytes"])?)?;
        }
    }
    let mut reserved_bytes = current_bytes;
    add(&mut reserved_bytes, pending_bytes)?;
    add(&mut reserved_bytes, garbage_bytes)?;
    Ok(json!({
        "owners":owners.len(),"rows":rows.len(),"tombstones":tombstones,
        "committed_snapshots":committed,"committed_version_total":versions,
        "pending_uploads":pending,"garbage_objects":garbage,
        "permanent_empty_fences":fences,"allocated_objects":committed+pending+garbage+fences,
        "current_ciphertext_bytes":current_bytes,"pending_ciphertext_bytes":pending_bytes,
        "garbage_ciphertext_bytes":garbage_bytes,"reserved_ciphertext_bytes":reserved_bytes,
        "object_actions_all_retained_days":actions,"latest_usage_day":latest_day,
        "object_actions_latest_day":latest_day_actions
    }))
}

fn audit(input: &Path) -> anyhow::Result<Value> {
    ensure!(
        fs::metadata(input)?.len() <= 256 * 1024 * 1024,
        "copied journal exceeds the 256 MiB rehearsal limit"
    );
    let original = fs::read(input)?;
    let expected = latest_rows(&original)?;
    let private = tempfile::tempdir()?;
    let copy = private.path().join("workspaces.jsonl");
    fs::write(&copy, &original)?;
    let first = replay(&copy)?;
    ensure!(
        first == expected,
        "first replay changed a complete workspace record"
    );
    let second = replay(&copy)?;
    ensure!(second == first, "second replay changed workspace state");
    let torn = private.path().join("torn.jsonl");
    fs::copy(&copy, &torn)?;
    fs::OpenOptions::new()
        .append(true)
        .open(&torn)?
        .write_all(b"{\"schema_version\":1,\"view\":")?;
    let recovered = replay(&torn)?;
    ensure!(
        recovered == first,
        "torn-tail recovery changed complete workspace state"
    );
    ensure!(
        fs::read(input)? == original,
        "original copied journal changed during rehearsal"
    );
    Ok(json!({
        "schema":"covenant.workspace.state-rehearsal.v1","accepted":true,
        "source_sha256":sha256_hex(&original),"source_bytes":original.len(),
        "checks":{"first_replay_matches":true,"second_replay_matches":true,
        "torn_tail_recovery_matches":true,"input_unchanged":true,"offline_no_workers":true},
        "counts":summary(&first)?
    }))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    ensure!(
        args.len() == 3 && args[1] == "--copied-journal",
        "usage: workspace-state-rehearsal --copied-journal /private/copy/workspaces.jsonl"
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&audit(Path::new(&args[2]))?)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        json!({"schema_version":1,"usage_day":20000,"object_actions":7,
            "view":{"id":"da62db35-3240-43a4-8c88-9d8000000001","owner":"synthetic-owner","name":"private-name",
                "trust_floor":"open","version":1,"snapshot":{"version":1,"plaintext_bytes":0,
                "ciphertext_bytes":60,"ciphertext_sha256":"ab".repeat(32),"created_at_ms":10}},
            "current_key":"private-current-key","committed_upload_id":"da62db35-3240-43a4-8c88-9d8000000002",
            "pending":{"request":{"upload_id":"da62db35-3240-43a4-8c88-9d8000000003","expected_version":1,
                "plaintext_bytes":0,"ciphertext_bytes":60,"ciphertext_sha256":"cd".repeat(32)},
                "key":"private-pending-key","expires_at_ms":300000},
            "garbage":[{"key":"private-garbage-key","after_ms":300000,"bytes":60}],"deleted":false})
    }

    #[test]
    fn copied_state_retains_obligations_and_reports_only_aggregates() {
        let private = tempfile::tempdir().unwrap();
        let input = private.path().join("copy.jsonl");
        let mut old = fixture();
        old["object_actions"] = json!(6);
        let mut deleted = fixture();
        deleted["view"]["id"] = json!("da62db35-3240-43a4-8c88-9d8000000004");
        deleted["current_key"] = Value::Null;
        deleted["pending"] = Value::Null;
        deleted["deleted"] = json!(true);
        deleted["object_actions"] = json!(2);
        let content = format!("{old}\n{}\n{deleted}\n{{\"torn\":", fixture());
        fs::write(&input, &content).unwrap();
        let report = audit(&input).unwrap();
        assert_eq!(report["counts"]["rows"], 2);
        assert_eq!(report["counts"]["committed_snapshots"], 1);
        assert_eq!(report["counts"]["pending_uploads"], 1);
        assert_eq!(report["counts"]["garbage_objects"], 2);
        assert_eq!(report["counts"]["tombstones"], 1);
        assert_eq!(report["counts"]["reserved_ciphertext_bytes"], 240);
        assert_eq!(report["counts"]["object_actions_latest_day"], 9);
        assert_eq!(fs::read_to_string(input).unwrap(), content);
        let public = report.to_string();
        for secret in [
            "synthetic-owner",
            "private-name",
            "private-current-key",
            "private-pending-key",
            "private-garbage-key",
        ] {
            assert!(!public.contains(secret));
        }
    }

    #[test]
    fn malformed_complete_records_and_future_schema_fail_without_mutating_input() {
        let private = tempfile::tempdir().unwrap();
        let input = private.path().join("copy.jsonl");
        let mut future = fixture();
        future["schema_version"] = json!(2);
        for content in ["{invalid}\n".to_owned(), format!("{future}\n")] {
            fs::write(&input, &content).unwrap();
            assert!(audit(&input).is_err());
            assert_eq!(fs::read_to_string(&input).unwrap(), content);
        }
    }
}
