//! Per-call provenance records.
//!
//! Every AceData call an agent makes returns one [`Provenance`]: the
//! model used, a SHA-256 over the prompt, a SHA-256 over the canonical
//! JSON of what the API returned, the asset references, and AceData's
//! own task id. It travels in the tool result so the daemon's audit
//! trail captures it, and it is the record an on-chain provenance
//! certificate is later built from.
//!
//! The output hash is taken over the canonicalized response payload
//! (JCS), not the asset bytes — a stable, verifiable anchor without a
//! second network fetch. Hashing the asset bytes themselves is a later
//! refinement for image and audio.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Provider tag carried on every AceData provenance record.
pub const PROVIDER: &str = "acedata";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provenance {
    /// Always `"acedata"`.
    pub provider: String,
    /// The tool that produced this (`acedata.image.generate`).
    pub tool: String,
    /// Model invoked, when the call selects one (`flux-pro`). Empty for
    /// non-model tools like search.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// SHA-256 (hex) over the UTF-8 prompt / query.
    pub prompt_sha256: String,
    /// SHA-256 (hex) over the canonical JSON of the response payload.
    pub output_sha256: String,
    /// Asset URLs (images, audio, video) extracted from the response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assets: Vec<String>,
    /// AceData's own task id, for cross-referencing with their console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

/// Lowercase hex of a SHA-256 digest.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Canonical-JSON (JCS) SHA-256 (hex) of a value. Falls back to the
/// plain encoding if canonicalization ever fails, so a hash is always
/// produced.
pub fn canonical_sha256_hex(value: &Value) -> String {
    let bytes = serde_jcs::to_vec(value)
        .unwrap_or_else(|_| serde_json::to_vec(value).unwrap_or_default());
    sha256_hex(&bytes)
}

impl Provenance {
    /// Build a record from a prompt and the response payload, pulling any
    /// asset URLs and the task id out of the standard AceData generation
    /// envelope.
    pub fn from_response(tool: &str, model: &str, prompt: &str, response: &Value) -> Self {
        Self {
            provider: PROVIDER.to_string(),
            tool: tool.to_string(),
            model: model.to_string(),
            prompt_sha256: sha256_hex(prompt.as_bytes()),
            output_sha256: canonical_sha256_hex(response),
            assets: extract_assets(response),
            task_id: response
                .get("task_id")
                .and_then(Value::as_str)
                .map(String::from),
        }
    }

    /// Render as a JSON object for embedding in a tool result.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Pull asset URLs out of a `data: [ { image_url | audio_url | video_url
/// | url } ]` array.
fn extract_assets(response: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(items) = response.get("data").and_then(Value::as_array) {
        for item in items {
            for key in ["image_url", "audio_url", "video_url", "url"] {
                if let Some(u) = item.get(key).and_then(Value::as_str) {
                    out.push(u.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sha256_hex_is_64_lowercase_hex() {
        let h = sha256_hex(b"covenant");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn from_response_extracts_assets_task_id_and_prompt_hash() {
        let resp = json!({
            "success": true,
            "task_id": "abc",
            "data": [{ "image_url": "https://cdn/x.png" }]
        });
        let p = Provenance::from_response("acedata.image.generate", "flux-pro", "a leaf", &resp);
        assert_eq!(p.provider, "acedata");
        assert_eq!(p.tool, "acedata.image.generate");
        assert_eq!(p.model, "flux-pro");
        assert_eq!(p.assets, vec!["https://cdn/x.png".to_string()]);
        assert_eq!(p.task_id.as_deref(), Some("abc"));
        assert_eq!(p.prompt_sha256, sha256_hex(b"a leaf"));
        assert_eq!(p.output_sha256.len(), 64);
    }

    #[test]
    fn empty_model_is_omitted_from_json() {
        let resp = json!({ "organic": [] });
        let p = Provenance::from_response("acedata.search", "", "q", &resp);
        let v = p.to_json();
        assert!(v.get("model").is_none(), "empty model must be skipped");
        assert!(v.get("task_id").is_none(), "absent task id must be skipped");
        assert!(v.get("assets").is_none(), "empty assets must be skipped");
    }

    #[test]
    fn canonical_hash_is_stable_across_key_order() {
        let a = json!({ "a": 1, "b": 2, "c": { "x": 1, "y": 2 } });
        let b = json!({ "c": { "y": 2, "x": 1 }, "b": 2, "a": 1 });
        assert_eq!(canonical_sha256_hex(&a), canonical_sha256_hex(&b));
    }
}
