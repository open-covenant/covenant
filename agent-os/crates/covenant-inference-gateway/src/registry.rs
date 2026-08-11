use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use covenant_inference_protocol::{
    node_id, verifying_key, NodeEnrollment, NodeTelemetry, ProtocolError, SequenceGuard, TrustClass,
};
use ed25519_dalek::VerifyingKey;

/// A node is a routing candidate only while its last heartbeat is this fresh. It
/// mirrors the offer freshness the rest of the network uses, so a node that goes
/// quiet drops out of matching on its own, with no explicit deregistration.
pub const HEARTBEAT_MAX_AGE: Duration = Duration::from_secs(60);

/// A node's advertised model list is re-probed off its live surface once the cached
/// copy is older than this. It is the model *ids* that clients route by; the gateway
/// learns them from the node rather than trusting an enrolment field.
pub const CATALOG_TTL: Duration = Duration::from_secs(60);

/// Signed `issued_at` on an enrolment or registration is refused past this many
/// seconds in either direction, tolerating clock skew without accepting a replay of
/// a long-old message.
pub const ISSUED_AT_SKEW_SECONDS: i64 = 300;

/// Telemetry carries its own `observed_at`; the sequence guard stops replays, this
/// bounds a signed-but-stale beat.
const TELEMETRY_MAX_SKEW_SECONDS: i64 = 120;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("unknown node")]
    UnknownNode,
    #[error("node id does not match its device key")]
    IdentityMismatch,
    #[error("signed timestamp is stale")]
    Stale,
    #[error("telemetry sequence replayed or reordered")]
    Replay,
    #[error("device public key is invalid")]
    BadKey,
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

/// One `{id, digest}` the node actually serves, learned by probing its `/v1/models`.
#[derive(Clone)]
pub struct CatalogEntry {
    pub id: String,
    pub digest: String,
}

/// A node the router may hand a request to.
pub struct Candidate {
    pub node_id: String,
    pub rate_per_second: u64,
}

struct NodeRecord {
    key: VerifyingKey,
    /// Model digests from the signed enrolment. Lets a caller address a model by its
    /// digest before the friendly-id catalog has been probed.
    enrolled_digests: Vec<String>,
    rate_per_second: u64,
    guard: SequenceGuard,
    /// Derived from the last signed posture via `effective_class`, never asserted.
    trust_class: TrustClass,
    last_heartbeat: Option<Instant>,
    catalog: Option<Vec<CatalogEntry>>,
    catalog_at: Option<Instant>,
}

impl NodeRecord {
    fn is_online(&self, now: Instant) -> bool {
        self.last_heartbeat
            .is_some_and(|seen| now.duration_since(seen) <= HEARTBEAT_MAX_AGE)
    }

    fn catalog_stale(&self, now: Instant) -> bool {
        self.catalog_at
            .is_none_or(|at| now.duration_since(at) > CATALOG_TTL)
    }

    fn serves(&self, model: &str) -> bool {
        if self.enrolled_digests.iter().any(|digest| digest == model) {
            return true;
        }
        self.catalog.as_deref().is_some_and(|catalog| {
            catalog
                .iter()
                .any(|entry| entry.id == model || entry.digest == model)
        })
    }
}

#[derive(Default)]
pub struct NodeRegistry {
    nodes: Mutex<HashMap<String, NodeRecord>>,
}

impl NodeRegistry {
    /// Verifies an enrolment and records the node's identity, served models, rate and
    /// a starting trust class. A fresh enrolment replaces any prior record for the id.
    pub fn enroll(&self, enrollment: NodeEnrollment) -> Result<(), RegistryError> {
        let key =
            verifying_key(&enrollment.device_public_key).map_err(|_| RegistryError::BadKey)?;
        if node_id(&key) != enrollment.node_id {
            return Err(RegistryError::IdentityMismatch);
        }
        if skew_seconds(enrollment.issued_at) > ISSUED_AT_SKEW_SECONDS {
            return Err(RegistryError::Stale);
        }
        enrollment.verify(&key)?;

        let enrolled_digests = enrollment
            .served_models
            .iter()
            .map(|model| model.digest())
            .collect();
        self.lock().insert(
            enrollment.node_id,
            NodeRecord {
                key,
                enrolled_digests,
                rate_per_second: enrollment.rate_per_second,
                guard: SequenceGuard::default(),
                trust_class: TrustClass::Open,
                last_heartbeat: None,
                catalog: None,
                catalog_at: None,
            },
        );
        Ok(())
    }

    /// Verifies a telemetry beat against the enrolled key, enforces monotonic
    /// sequence and freshness, then refreshes liveness and the derived trust class.
    pub fn heartbeat(&self, id: &str, telemetry: NodeTelemetry) -> Result<(), RegistryError> {
        if telemetry.node_id != id {
            return Err(RegistryError::IdentityMismatch);
        }
        let mut nodes = self.lock();
        let record = nodes.get_mut(id).ok_or(RegistryError::UnknownNode)?;
        telemetry.verify(&record.key)?;
        if skew_seconds(telemetry.observed_at) > TELEMETRY_MAX_SKEW_SECONDS {
            return Err(RegistryError::Stale);
        }
        if !record.guard.admit(&telemetry) {
            return Err(RegistryError::Replay);
        }
        record.last_heartbeat = Some(Instant::now());
        if let Some(posture) = telemetry.posture.as_ref() {
            record.trust_class = posture.effective_class();
        }
        Ok(())
    }

    /// Online nodes that serve `model` at or above the trust floor. The caller
    /// intersects this with tunnel availability and picks one.
    pub fn candidates(&self, model: &str, min_trust: TrustClass, now: Instant) -> Vec<Candidate> {
        self.lock()
            .iter()
            .filter(|(_, record)| {
                record.is_online(now) && record.trust_class >= min_trust && record.serves(model)
            })
            .map(|(id, record)| Candidate {
                node_id: id.clone(),
                rate_per_second: record.rate_per_second,
            })
            .collect()
    }

    pub fn online_nodes(&self, now: Instant) -> Vec<String> {
        self.lock()
            .iter()
            .filter(|(_, record)| record.is_online(now))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Online nodes whose catalog is missing or past its TTL and so worth re-probing.
    pub fn needs_catalog(&self, now: Instant) -> Vec<String> {
        self.lock()
            .iter()
            .filter(|(_, record)| record.is_online(now) && record.catalog_stale(now))
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn set_catalog(&self, id: &str, entries: Vec<CatalogEntry>) {
        if let Some(record) = self.lock().get_mut(id) {
            record.catalog = Some(entries);
            record.catalog_at = Some(Instant::now());
        }
    }

    /// The models served across all online nodes, for the client-facing `/v1/models`.
    pub fn catalog_union(&self, now: Instant) -> Vec<CatalogEntry> {
        self.lock()
            .values()
            .filter(|record| record.is_online(now))
            .filter_map(|record| record.catalog.clone())
            .flatten()
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, NodeRecord>> {
        self.nodes.lock().expect("node registry mutex poisoned")
    }
}

fn skew_seconds(timestamp: DateTime<Utc>) -> i64 {
    timestamp
        .signed_duration_since(Utc::now())
        .num_seconds()
        .abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use covenant_inference_protocol::{
        AttestationKind, AttestationRef, IsolationMode, ModelIdentity, NodePosture, SamplingParams,
        UnsignedNodeEnrollment, UnsignedTelemetry,
    };
    use ed25519_dalek::SigningKey;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[9_u8; 32])
    }

    fn model() -> ModelIdentity {
        ModelIdentity {
            weights_hash: "sha256:abcd".into(),
            quantization: "q4_k_m".into(),
            runtime: "llama.cpp".into(),
            runtime_version: "b1".into(),
            sampling_params: SamplingParams {
                temperature: 0.0,
                top_p: 1.0,
                seed: 0,
                max_tokens: 512,
            },
        }
    }

    fn enroll(registry: &NodeRegistry, key: &SigningKey) -> String {
        let id = node_id(&key.verifying_key());
        let enrollment = NodeEnrollment::sign(
            UnsignedNodeEnrollment {
                node_id: id.clone(),
                device_public_key: URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
                operator_wallet: "operator".into(),
                payout_wallet: "payout".into(),
                served_models: vec![model()],
                max_tokens_per_second: 200,
                rate_per_second: 1_000,
                issued_at: Utc::now(),
            },
            key,
        )
        .unwrap();
        registry.enroll(enrollment).unwrap();
        id
    }

    fn beat(key: &SigningKey, sequence: u64, posture: Option<NodePosture>) -> NodeTelemetry {
        NodeTelemetry::sign(
            UnsignedTelemetry {
                node_id: node_id(&key.verifying_key()),
                sequence,
                observed_at: Utc::now(),
                active_sessions: 0,
                tokens_per_second: 0,
                tunnel_connected: true,
                served_model_digest: Some(model().digest()),
                posture,
            },
            key,
        )
        .unwrap()
    }

    #[test]
    fn enrolled_node_is_offline_until_it_beats() {
        let registry = NodeRegistry::default();
        let key = key();
        let id = enroll(&registry, &key);
        assert!(registry.online_nodes(Instant::now()).is_empty());
        registry.heartbeat(&id, beat(&key, 1, None)).unwrap();
        assert_eq!(registry.online_nodes(Instant::now()), vec![id]);
    }

    #[test]
    fn heartbeat_requires_prior_enrolment_and_rejects_replays() {
        let registry = NodeRegistry::default();
        let key = key();
        let id = node_id(&key.verifying_key());
        assert!(matches!(
            registry.heartbeat(&id, beat(&key, 1, None)),
            Err(RegistryError::UnknownNode)
        ));
        enroll(&registry, &key);
        registry.heartbeat(&id, beat(&key, 1, None)).unwrap();
        assert!(matches!(
            registry.heartbeat(&id, beat(&key, 1, None)),
            Err(RegistryError::Replay)
        ));
    }

    #[test]
    fn candidates_apply_the_trust_floor_from_derived_class() {
        let registry = NodeRegistry::default();
        let key = key();
        let id = enroll(&registry, &key);
        // A confidential *claim* is clamped to what the network can verify (isolated),
        // so it clears an isolated floor but never an attested one.
        let posture = NodePosture {
            isolation: IsolationMode::Isolated,
            attestation: Some(AttestationRef {
                kind: AttestationKind::NvidiaCc,
                quote_sha256: "0".repeat(64),
            }),
        };
        registry
            .heartbeat(&id, beat(&key, 1, Some(posture)))
            .unwrap();
        let now = Instant::now();
        assert_eq!(
            registry
                .candidates(&model().digest(), TrustClass::Open, now)
                .len(),
            1
        );
        assert_eq!(
            registry
                .candidates(&model().digest(), TrustClass::Isolated, now)
                .len(),
            1
        );
        assert!(registry
            .candidates(&model().digest(), TrustClass::Attested, now)
            .is_empty());
    }

    #[test]
    fn routing_matches_enrolled_digest_and_probed_id() {
        let registry = NodeRegistry::default();
        let key = key();
        let id = enroll(&registry, &key);
        registry.heartbeat(&id, beat(&key, 1, None)).unwrap();
        let now = Instant::now();
        assert!(registry
            .candidates("qwen3:8b", TrustClass::Open, now)
            .is_empty());
        assert_eq!(
            registry
                .candidates(&model().digest(), TrustClass::Open, now)
                .len(),
            1
        );

        registry.set_catalog(
            &id,
            vec![CatalogEntry {
                id: "qwen3:8b".into(),
                digest: model().digest(),
            }],
        );
        assert_eq!(
            registry.candidates("qwen3:8b", TrustClass::Open, now).len(),
            1
        );
    }
}
