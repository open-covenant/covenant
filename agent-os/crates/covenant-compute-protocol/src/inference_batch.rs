//! Ordered independent inference and RFC6962 inclusion proofs. The batch request
//! is purchased once; its child receipts retain their normal operator signatures.
use crate::{merkle, ProtocolError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const MAX_BATCH_ITEMS: usize = 32;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceBatchSpec {
    pub version: u8,
    pub requests: Vec<Value>,
}
impl InferenceBatchSpec {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != 1
            || self.requests.is_empty()
            || self.requests.len() > MAX_BATCH_ITEMS
            || self
                .requests
                .iter()
                .any(|v| !v.is_object() || v.get("requests").is_some())
        {
            return Err(ProtocolError::Invalid(
                "batch requires version 1 and 1..=32 independent chat requests".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BatchLeaf {
    pub version: u8,
    pub batch_id: Uuid,
    pub request_sha256: String,
    pub index: u32,
    pub job_id: Uuid,
    pub result_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchItem {
    pub leaf: BatchLeaf,
    pub output: Value,
    pub audit_path: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceBatchResult {
    pub version: u8,
    pub batch_id: Uuid,
    pub request_sha256: String,
    pub tree_size: u32,
    pub merkle_root: String,
    pub items: Vec<BatchItem>,
}
fn invalid(message: &str) -> ProtocolError {
    ProtocolError::Invalid(message.into())
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(value: &Value) -> Result<String, ProtocolError> {
    Ok(hex(&Sha256::digest(
        serde_jcs::to_vec(value).map_err(|e| invalid(&e.to_string()))?,
    )))
}
fn decode(value: &str) -> Result<[u8; 32], ProtocolError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid batch digest"));
    }
    let mut out = [0; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b =
            u8::from_str_radix(&value[2 * i..2 * i + 2], 16).map_err(|_| invalid("invalid hex"))?;
    }
    Ok(out)
}
fn leaf_hash(leaf: &BatchLeaf) -> Result<[u8; 32], ProtocolError> {
    let mut bytes = b"covenant.compute.inference-batch.leaf.v1\n".to_vec();
    bytes.extend(serde_jcs::to_vec(leaf).map_err(|e| invalid(&e.to_string()))?);
    Ok(merkle::hash_leaf(&bytes))
}
impl InferenceBatchResult {
    pub fn build(
        batch_id: Uuid,
        request_sha256: String,
        results: Vec<(Uuid, Value)>,
    ) -> Result<Self, ProtocolError> {
        if batch_id.is_nil() || results.is_empty() || results.len() > MAX_BATCH_ITEMS {
            return Err(invalid("invalid batch size/id"));
        }
        decode(&request_sha256)?;
        let mut items = vec![];
        let mut hashes = vec![];
        for (index, (job_id, output)) in results.into_iter().enumerate() {
            if job_id.is_nil() || items.iter().any(|i: &BatchItem| i.leaf.job_id == job_id) {
                return Err(invalid("duplicate or nil child id"));
            }
            let leaf = BatchLeaf {
                version: 1,
                batch_id,
                request_sha256: request_sha256.clone(),
                index: index as u32,
                job_id,
                result_sha256: digest(&output)?,
            };
            hashes.push(leaf_hash(&leaf)?);
            items.push(BatchItem {
                leaf,
                output,
                audit_path: vec![],
            });
        }
        for (index, item) in items.iter_mut().enumerate() {
            item.audit_path = merkle::audit_path(index, &hashes)
                .unwrap()
                .iter()
                .map(|v| hex(v))
                .collect();
        }
        Ok(Self {
            version: 1,
            batch_id,
            request_sha256,
            tree_size: items.len() as u32,
            merkle_root: hex(&merkle::root(&hashes)),
            items,
        })
    }
    pub fn verify_item(&self, item: &BatchItem) -> Result<(), ProtocolError> {
        decode(&self.request_sha256)?;
        if self.version != 1
            || self.batch_id.is_nil()
            || item.leaf.job_id.is_nil()
            || item.audit_path.len() > 5
            || self.tree_size == 0
            || self.tree_size as usize > MAX_BATCH_ITEMS
            || item.leaf.version != 1
            || item.leaf.batch_id != self.batch_id
            || item.leaf.request_sha256 != self.request_sha256
            || item.leaf.result_sha256 != digest(&item.output)?
        {
            return Err(invalid("batch item binding mismatch"));
        }
        let path = item
            .audit_path
            .iter()
            .map(|h| decode(h))
            .collect::<Result<Vec<_>, _>>()?;
        if merkle::root_from_path(
            leaf_hash(&item.leaf)?,
            item.leaf.index as usize,
            self.tree_size as usize,
            &path,
        ) != Some(decode(&self.merkle_root)?)
        {
            return Err(invalid("invalid batch inclusion proof"));
        }
        Ok(())
    }
    pub fn verify(&self) -> Result<(), ProtocolError> {
        if self.version != 1
            || self.batch_id.is_nil()
            || self.tree_size == 0
            || self.tree_size as usize > MAX_BATCH_ITEMS
            || self.items.len() != self.tree_size as usize
        {
            return Err(invalid("invalid or incomplete batch"));
        }
        decode(&self.request_sha256)?;
        decode(&self.merkle_root)?;
        let unique: std::collections::BTreeSet<_> =
            self.items.iter().map(|item| item.leaf.job_id).collect();
        if unique.len() != self.items.len() || unique.contains(&Uuid::nil()) {
            return Err(invalid("duplicate or nil batch child id"));
        }
        for (i, item) in self.items.iter().enumerate() {
            if item.leaf.index != i as u32 {
                return Err(invalid("batch order changed"));
            }
            self.verify_item(item)?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proofs_bind_order_request_child_and_exact_results() {
        for n in 1..=MAX_BATCH_ITEMS {
            let mut result = InferenceBatchResult::build(
                Uuid::new_v4(),
                "aa".repeat(32),
                (0..n)
                    .map(|i| {
                        (
                            Uuid::new_v4(),
                            serde_json::json!({"text":format!("答案 {i}")}),
                        )
                    })
                    .collect(),
            )
            .unwrap();
            result.verify().unwrap();
            result.items[0].output = serde_json::json!({"text":"tampered"});
            assert!(result.verify().is_err());
        }
    }
    #[test]
    fn empty_nil_and_duplicate_results_are_not_proofs() {
        let mut result = InferenceBatchResult::build(
            Uuid::new_v4(),
            "aa".repeat(32),
            vec![(Uuid::new_v4(), serde_json::json!({"value":1}))],
        )
        .unwrap();
        result.tree_size = 0;
        result.items.clear();
        assert!(result.verify().is_err());
        let mut result = InferenceBatchResult::build(
            Uuid::new_v4(),
            "aa".repeat(32),
            vec![(Uuid::new_v4(), serde_json::json!({"value":1}))],
        )
        .unwrap();
        result.batch_id = Uuid::nil();
        assert!(result.verify().is_err());
        let mut result = InferenceBatchResult::build(
            Uuid::new_v4(),
            "aa".repeat(32),
            vec![
                (Uuid::new_v4(), serde_json::json!({"value":1})),
                (Uuid::new_v4(), serde_json::json!({"value":2})),
            ],
        )
        .unwrap();
        result.items[1].leaf.job_id = result.items[0].leaf.job_id;
        assert!(result.verify().is_err());
    }
    #[test]
    fn independent_merkle_vectors() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/compute/inference-batch-v1.json"
        ))
        .unwrap();
        let result: InferenceBatchResult =
            serde_json::from_value(fixture["result"].clone()).unwrap();
        result.verify().unwrap();
    }
}
