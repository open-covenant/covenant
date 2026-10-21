//! GPU/compute capability-profile schema.
//!
//! Does not exist anywhere else in the codebase. `covenant-manifest`'s
//! `Capabilities` (`covenant-manifest/src/lib.rs:90`) is a permission-scope
//! grant for locally-run agent packages — an unrelated concept despite
//! the name overlap (design-02 §1.1). This is the network's own
//! vocabulary: what hardware an operator offers, and what a buyer's job
//! requires.

use covenant_types::AgentId;
use serde::{Deserialize, Serialize};

use crate::sign::ProtocolError;

/// The longest a declared label (a served model name or the hardware
/// model string) may be. Real model ids and GPU names sit well under
/// this; the bound only stops a hostile registration from stuffing the
/// public capacity directory with a multi-kilobyte string.
const MAX_LABEL_LEN: usize = 128;

/// Upper bound on how many models one profile may declare. A generic
/// exec node advertises `["any"]`; a real model server lists the handful
/// it has pulled. The bound is far above any honest count and only stops
/// a hostile registration from stuffing the vec — the whole profile is
/// deep-cloned on every match and served in the public capacity view, so
/// its size is an amplification vector, not just a directory-hygiene one.
const MAX_MODELS_SERVED: usize = 256;

/// Upper bound on job-kind entries. There are only a handful of distinct
/// kinds, so an honest profile never approaches this; it exists solely to
/// bound a padded vec the same way [`MAX_MODELS_SERVED`] does.
const MAX_JOB_KINDS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// One prompt/request in, one completion out; billed per token.
    InferenceCall,
    /// Fixed input set, run to completion; billed wall-time x GPU-class.
    BatchJob,
    /// Time-boxed exclusive capacity; billed per GPU-hour. Mirrors
    /// `services/compute-broker`'s `LeaseRequest{gpuHours, durationSecs}`
    /// (`services/compute-broker/src/providers.ts:1-5`).
    LeaseSession,
    /// Text in, an embedding vector out; billed per input token. The
    /// workhorse of retrieval and semantic memory, and a distinct
    /// operation from [`JobKind::InferenceCall`]: an embedding model
    /// produces no completion, so it fails a generation probe and needs
    /// its own serving path (`/api/embed`, `/v1/embeddings`).
    Embedding,
    /// Audio in, a text transcript out; billed per envelope like the rest.
    /// A distinct operation again: a speech model takes no prompt and
    /// produces no completion, so it fails both a generation and an
    /// embedding probe and needs its own serving path (a whisper backend's
    /// `/inference`, OpenAI's `/v1/audio/transcriptions`).
    Transcription,
    /// Text in, a synthesized audio clip out; billed per envelope like the
    /// rest. The mirror of [`JobKind::Transcription`]: a text-to-speech
    /// backend reads words and returns audio, producing no completion and
    /// no vector, so it fails a generation, an embedding, and a
    /// transcription probe alike and needs its own serving path (a local
    /// TTS engine, OpenAI's `/v1/audio/speech`).
    SpeechSynthesis,
    /// A coding agent works a task against a repository and answers with
    /// a patch ([`crate::agent`]); billed per job. Settlement waits on an
    /// [`JobKind::AgentCheck`] the coordinator orders from another
    /// operator, so a signed receipt alone never pays for it.
    AgentTask,
    /// Apply one builder's patch to a fresh copy of the repository, run the
    /// task's acceptance commands with no network, and answer with a
    /// verdict. Ordered by the coordinator; billed per job.
    AgentCheck,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum HardwareClass {
    ConsumerGpu { model: String },
    DatacenterGpu { model: String },
    CpuOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceUnit {
    PerMillionTokens,
    PerGpuSecond,
    PerLeaseHour,
    PerJob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceAsk {
    pub unit: PriceUnit,
    pub micro_usdc: u64,
}

impl PriceAsk {
    /// The floor this ask puts under a single job's offer, in micro-USDC:
    /// what a matched offer must cover for the operator to serve the job
    /// without a pay cut. Settlement pays the whole job at the envelope's
    /// offer whatever unit the ask declares, so the ask floors the whole
    /// job — except a lease priced by the GPU-hour, which settles by meter
    /// over its window and so floors on the per-hour rate scaled to that
    /// window, rounded up so the floor never understates the ask.
    ///
    /// The coordinator's matcher and every operator node's admission both
    /// price the offer through this one function, so the gate a buyer must
    /// clear to match is the same gate the node applies before it serves —
    /// they cannot drift into a lease the coordinator routes and the node
    /// then rejects. `window_secs` is the job's metered window; it bears on
    /// the floor only for a per-hour lease.
    pub fn job_floor_micro_usdc(&self, kind: JobKind, window_secs: u32) -> u64 {
        if kind == JobKind::LeaseSession && self.unit == PriceUnit::PerLeaseHour {
            let per_hour = u128::from(self.micro_usdc);
            let secs = u128::from(window_secs);
            return u64::try_from((per_hour * secs).div_ceil(3_600)).unwrap_or(u64::MAX);
        }
        self.micro_usdc
    }
}

/// What an operator node declares at registration. The functional
/// claims — which models it serves and which job kinds it runs — are
/// demonstrated before registration for a self-testable backend:
/// benchmark-on-register (B5, in `covenant-compute-node::benchmark`)
/// drives each such claim through the node's real executor and refuses
/// to register any it cannot serve. Two cases have no boot-time
/// self-test: a broker's `LeaseSession` capacity, proven per rental when
/// a buyer pays rather than by renting a machine at boot; and a node
/// started with the explicit skip flag, which registers its claims
/// undemonstrated and logs that it did. The hardware claims are
/// self-declared and not attested: `vram_gb` and the `HardwareClass` model string are
/// matched against a buyer's requirement by
/// [`CapabilityProfile::satisfies`] but never verified, and
/// `tee_capable` is recorded without bearing on routing or pricing in
/// v1. The separate hardware-evidence verifier binds diagnostics to a live
/// lease; it does not promote registration claims or change matching. Its
/// released trust ceiling remains Open, so `tee_capable` remains self-declared
/// and every operator remains in the untrusted-hardware tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityProfile {
    /// Explicit, versioned fixed-price output-cap tariffs. Empty preserves legacy pricing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inference_tariffs: Vec<crate::InferenceTariff>,
    pub operator: AgentId,
    pub hardware: HardwareClass,
    pub vram_gb: u32,
    /// Model ids served, or `["any"]` for a generic exec/command node.
    pub models_served: Vec<String>,
    pub job_kinds: Vec<JobKind>,
    pub price: PriceAsk,
    pub tee_capable: bool,
    /// Asks that differ by kind. A node that builds agent work and checks
    /// other operators' work prices the two differently; a kind absent here
    /// uses `price`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kind_prices: Vec<KindAsk>,
    /// Models that differ by kind: an agent node builds with a harness and
    /// checks in container images, and a build must never route by an image
    /// name. A kind absent here serves `models_served`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kind_models: Vec<KindModels>,
}

/// One kind's ask, where it differs from the profile's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindAsk {
    pub kind: JobKind,
    pub price: PriceAsk,
}

/// One kind's models, where they differ from the profile's own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindModels {
    pub kind: JobKind,
    pub models: Vec<String>,
}

/// What a buyer's job envelope asks for. The hardware/model fields are
/// matched against a [`CapabilityProfile`] by
/// [`CapabilityProfile::satisfies`]; `min_reputation_bps` is deliberately
/// not, because reputation is a dynamic signal the coordinator's
/// `ReputationSource` derives at match time, not a static property a
/// profile declares — the matcher enforces it separately (see
/// `select_operator`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRequirement {
    /// Buyer-signed tariff snapshot; absent preserves the legacy envelope bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_tariff: Option<crate::InferenceTariffQuote>,
    pub gpu_class: Option<String>,
    pub min_vram_gb: Option<u32>,
    pub model_id: Option<String>,
    pub kind: JobKind,
    pub max_duration_secs: u32,
    /// The lowest operator reputation, in basis points (`8_000` = 80%),
    /// that may serve this job. The coordinator excludes any operator
    /// scoring below it, so a buyer confines their work to a proven pool
    /// even when a cheaper, worse-rated operator would otherwise win on
    /// price. Absent places no floor; the coordinator's own standing
    /// floor still applies, and the effective floor is the higher of the
    /// two. Buyer-signed, so the coordinator cannot quietly lower it.
    #[serde(default)]
    pub min_reputation_bps: Option<u32>,
}

impl CapabilityProfile {
    /// The ask this profile prices `kind` at.
    pub fn ask_for(&self, kind: JobKind) -> PriceAsk {
        self.kind_prices
            .iter()
            .find(|k| k.kind == kind)
            .map(|k| k.price)
            .unwrap_or(self.price)
    }

    /// Whole-job floor shared by routing and admission. Tariff quotes are exact offers;
    /// legacy asks retain their existing unit semantics.
    pub fn job_floor_micro_usdc(&self, req: &CapabilityRequirement) -> u64 {
        req.inference_tariff
            .as_ref()
            .map(|q| q.price_micro_usdc)
            .unwrap_or_else(|| {
                self.ask_for(req.kind)
                    .job_floor_micro_usdc(req.kind, req.max_duration_secs)
            })
    }

    pub fn satisfies_at(&self, req: &CapabilityRequirement, now_ms: u64) -> bool {
        self.satisfies(req)
            && req
                .inference_tariff
                .as_ref()
                .is_none_or(|quote| quote.tariff.valid_at(now_ms))
    }

    /// The models this profile serves `kind` with.
    pub fn models_for(&self, kind: JobKind) -> &[String] {
        self.kind_models
            .iter()
            .find(|k| k.kind == kind)
            .map(|k| k.models.as_slice())
            .unwrap_or(&self.models_served)
    }

    /// Fail-closed capability match: every constraint the requirement
    /// states must hold against this profile. An absent constraint
    /// (`None`) is satisfied by any profile.
    pub fn satisfies(&self, req: &CapabilityRequirement) -> bool {
        if let Some(quote) = &req.inference_tariff {
            if req.kind != JobKind::InferenceCall
                || quote.validate().is_err()
                || req.model_id.as_ref().is_none_or(|model| {
                    canonical_model(model) != canonical_model(&quote.tariff.model)
                })
                || !self.inference_tariffs.contains(&quote.tariff)
            {
                return false;
            }
        }
        if !self.job_kinds.contains(&req.kind) {
            return false;
        }
        if let Some(min_vram) = req.min_vram_gb {
            if self.vram_gb < min_vram {
                return false;
            }
        }
        if let Some(model_id) = &req.model_id {
            if !self
                .models_for(req.kind)
                .iter()
                .any(|m| model_matches(m, model_id))
            {
                return false;
            }
        }
        if let Some(gpu_class) = &req.gpu_class {
            let matches_class = match &self.hardware {
                HardwareClass::ConsumerGpu { model } | HardwareClass::DatacenterGpu { model } => {
                    // A GPU class is a case-insensitive identifier: a buyer
                    // asking for "H100" must reach an operator that declared
                    // "h100", not fall to a pricier match or refund. Mirrors
                    // the CpuOnly arm, which already folds case.
                    model.eq_ignore_ascii_case(gpu_class)
                }
                HardwareClass::CpuOnly => gpu_class.eq_ignore_ascii_case("cpu"),
            };
            if !matches_class {
                return false;
            }
        }
        true
    }

    /// Validates the declared capability vectors before the coordinator
    /// trusts a registration. Every profile must name at least one job
    /// kind and at least one served model (`"any"` for a generic exec
    /// node): an empty vector serves nothing a buyer can pin yet still
    /// counts toward the matchable-operator tally, so it would only ever
    /// misrepresent the capacity directory. Neither vector may exceed its
    /// bound.
    ///
    /// Also rejects declared labels — served model names and the hardware
    /// model string — that carry a control character or run past
    /// [`MAX_LABEL_LEN`]. These strings ride an operator's registration
    /// into every buyer's capacity view, and a registration is open to
    /// anyone with a keypair; nothing legitimate needs a control byte,
    /// so refusing them at the wire keeps a smuggled ANSI escape (or a
    /// directory-stuffing blob) out of the network's public directory
    /// before any client renders it. The coordinator calls this at
    /// registration, next to the payout-address check.
    pub fn validate_labels(&self) -> Result<(), ProtocolError> {
        if self.job_kinds.is_empty() {
            return Err(ProtocolError::Invalid(
                "a capability profile must declare at least one job kind".into(),
            ));
        }
        if self.models_served.is_empty() {
            return Err(ProtocolError::Invalid(
                "a capability profile must declare at least one served model \
                 (\"any\" for a generic exec node)"
                    .into(),
            ));
        }
        if self.models_served.len() > MAX_MODELS_SERVED {
            return Err(ProtocolError::Invalid(format!(
                "a profile declares {} served models, past the {MAX_MODELS_SERVED} limit",
                self.models_served.len()
            )));
        }
        if self.job_kinds.len() > MAX_JOB_KINDS {
            return Err(ProtocolError::Invalid(format!(
                "a profile declares {} job kinds, past the {MAX_JOB_KINDS} limit",
                self.job_kinds.len()
            )));
        }
        for model in &self.models_served {
            validate_label("a served model name", model)?;
        }
        if self.kind_prices.len() > MAX_JOB_KINDS || self.kind_models.len() > MAX_JOB_KINDS {
            return Err(ProtocolError::Invalid(format!(
                "a profile declares more than {MAX_JOB_KINDS} per-kind asks or model sets"
            )));
        }
        for kind in self
            .kind_prices
            .iter()
            .map(|k| k.kind)
            .chain(self.kind_models.iter().map(|k| k.kind))
        {
            if !self.job_kinds.contains(&kind) {
                return Err(ProtocolError::Invalid(format!(
                    "a per-kind ask or model set names {kind:?}, a kind the profile does not serve"
                )));
            }
        }
        for set in &self.kind_models {
            if set.models.is_empty() || set.models.len() > MAX_MODELS_SERVED {
                return Err(ProtocolError::Invalid(format!(
                    "the model set for {:?} must name 1..={MAX_MODELS_SERVED} models",
                    set.kind
                )));
            }
            for model in &set.models {
                validate_label("a served model name", model)?;
            }
        }
        if self.inference_tariffs.len() > MAX_MODELS_SERVED {
            return Err(ProtocolError::Invalid("too many inference tariffs".into()));
        }
        let mut tariff_ids = std::collections::HashSet::new();
        for tariff in &self.inference_tariffs {
            tariff.validate()?;
            if !tariff_ids.insert((&tariff.id, tariff.revision)) {
                return Err(ProtocolError::Invalid(
                    "duplicate tariff id/revision".into(),
                ));
            }
            if !self.job_kinds.contains(&JobKind::InferenceCall)
                || !self
                    .models_for(JobKind::InferenceCall)
                    .iter()
                    .any(|m| m == "any" || canonical_model(m) == canonical_model(&tariff.model))
            {
                return Err(ProtocolError::Invalid(
                    "tariff model is not served by this profile".into(),
                ));
            }
        }
        match &self.hardware {
            HardwareClass::ConsumerGpu { model } | HardwareClass::DatacenterGpu { model } => {
                validate_label("the hardware model", model)?;
            }
            HardwareClass::CpuOnly => {}
        }
        Ok(())
    }

    /// Refuses a lease-serving profile priced by any unit but the
    /// GPU-hour. A [`JobKind::LeaseSession`] settles by meter — the
    /// buyer-signed per-second rate times the seconds actually served — and
    /// the buyer may close early, so the operator is only made whole when
    /// its ask floors that per-second *rate*. [`PriceAsk::job_floor_micro_usdc`]
    /// scales the ask to the window (flooring the rate at `per_hour / 3600`)
    /// for exactly one unit, [`PriceUnit::PerLeaseHour`]. Every other unit
    /// floors the flat figure against the whole-window *offer*, which says
    /// nothing about the rate: a [`PriceUnit::PerJob`] ask of `3_600_000`
    /// clears any offer that reaches it (a rate of 42 µUSDC/s over an
    /// 86_400s window prices at `3_628_800 ≥ 3_600_000`), then a close one
    /// second in meters `42` and the operator that priced the job at
    /// `3_600_000` is paid `42`. `PerGpuSecond`/`PerMillionTokens` fail the
    /// same way from the other side — a per-unit rate read as a flat figure.
    /// Only the per-hour ask survives an early close intact, so it is the
    /// only lease unit permitted. No downstream gate catches the others: the
    /// matcher, the node's admission and the bond floor all read that same
    /// flat figure. Refused at the coordinator's registration and at the
    /// node's boot, the way the payout-address invariant is gated on both
    /// sides.
    pub fn validate_lease_pricing(&self) -> Result<(), ProtocolError> {
        let unit = self.ask_for(JobKind::LeaseSession).unit;
        if self.job_kinds.contains(&JobKind::LeaseSession) && unit != PriceUnit::PerLeaseHour {
            return Err(ProtocolError::Invalid(format!(
                "a lease-serving operator must price by {:?}, not {:?}: a lease settles by meter \
                 over the seconds it serves and only a per-hour ask floors that per-second rate, \
                 so a {:?} floor would clear a buyer's offer above the advertised rate yet meter \
                 a fraction of the window on an early close, underpaying the session",
                PriceUnit::PerLeaseHour,
                unit,
                unit,
            )));
        }
        Ok(())
    }
}

fn validate_label(what: &str, value: &str) -> Result<(), ProtocolError> {
    if value.chars().count() > MAX_LABEL_LEN {
        return Err(ProtocolError::Invalid(format!(
            "{what} is longer than {MAX_LABEL_LEN} characters"
        )));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(ProtocolError::Invalid(format!(
            "{what} contains a control character"
        )));
    }
    Ok(())
}

/// A model reference with no tag names the `:latest` tag — the
/// Docker-style rule Ollama resolves by, and Ollama's `/api/tags` (the
/// source of an auto-advertised `models_served`) always reports the
/// tagged form. Without this, a buyer asking for `qwen2.5-coder` would
/// never route to a node advertising `qwen2.5-coder:latest` even though
/// that node serves exactly that request. Every explicit tag other than
/// `:latest` (`qwen2.5-coder:7b`) stays distinct: the node may not hold
/// whatever `:latest` resolves to, so matching those would route jobs a
/// node must refuse.
pub fn canonical_model(reference: &str) -> &str {
    // Compiler contracts contain immutable image identity. Capacity rows must
    // retain the same exact name used by admission and matching.
    if reference.starts_with(crate::knowledge::KNOWLEDGE_MODEL_PREFIX) {
        return reference;
    }
    reference.strip_suffix(":latest").unwrap_or(reference)
}

/// Match an advertised model without widening versioned execution contracts to
/// a legacy wildcard. Native empty checks also require an exact image label.
pub fn model_matches(served: &str, required: &str) -> bool {
    if required.starts_with(crate::NATIVE_EMPTY_CHECK_MODEL_PREFIX)
        || required.starts_with(crate::knowledge::KNOWLEDGE_MODEL_PREFIX)
    {
        return served == required;
    }
    (served == "any" && !required.starts_with("kani-v1:") && required != "claude-code:kani-v1")
        || canonical_model(served) == canonical_model(required)
}

/// One (kind, model) row of [`CapacityView`]: how much matchable supply
/// serves the pairing right now, and what it asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityEntry {
    /// Live tariff supply with hardware kept together to avoid aggregate price/hardware mismatches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inference_tariffs: Vec<crate::InferenceTariffOffer>,
    pub kind: JobKind,
    /// Canonical model reference ([`canonical_model`]), or `"any"` for
    /// generic exec nodes that serve whatever a job names.
    pub model: String,
    pub operators: usize,
    /// The cheapest ask in the row — the envelope price at which the
    /// matcher starts finding this row non-empty. Asks compare on
    /// `micro_usdc` whatever their declared unit, exactly as the
    /// matcher reads them (a metered ask still floors the whole job).
    pub min_ask_micro_usdc: u64,
    pub min_ask_unit: PriceUnit,
    /// The dearest ask — the price at which every operator in the row
    /// is within reach.
    pub max_ask_micro_usdc: u64,
    pub max_vram_gb: u32,
    /// Distinct requestable `gpu_class` constraint values present in
    /// the row (`"cpu"` for CPU-only operators), sorted.
    pub gpu_classes: Vec<String>,
    /// Whether any operator in the row declares TEE capability
    /// (self-declared, not attested — see [`CapabilityProfile`]).
    pub tee_capable: bool,
}

/// The coordinator's live-capacity directory (`GET
/// /federation/capacity`): what a buyer can purchase right now,
/// aggregated from the declared profiles of operators the matcher would
/// actually consider. Counts, asks, and the floors in force — never an
/// operator identity, the same anonymous-aggregate posture as
/// `/metrics`. Rows are sorted by kind then model, so the view is
/// byte-stable for a given pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityView {
    pub registered_operators: usize,
    /// Operators passing the matcher's standing gates — Online, seen
    /// within the liveness window, above both floors. The pool the
    /// entries aggregate; zero with `registered_operators` non-zero
    /// means supply exists but none of it is currently matchable.
    pub matchable_operators: usize,
    pub liveness_window_ms: u64,
    pub min_score_bps: u32,
    pub min_bond_micro_usdc: u64,
    pub entries: Vec<CapacityEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> CapabilityProfile {
        CapabilityProfile {
            inference_tariffs: Vec::new(),
            operator: AgentId::new("operator@local", [3u8; 32]),
            hardware: HardwareClass::ConsumerGpu {
                model: "rtx-4090".into(),
            },
            vram_gb: 24,
            models_served: vec!["llama-3-8b".into()],
            job_kinds: vec![JobKind::InferenceCall],
            price: PriceAsk {
                unit: PriceUnit::PerMillionTokens,
                micro_usdc: 500_000,
            },
            tee_capable: false,
            kind_prices: Vec::new(),
            kind_models: Vec::new(),
        }
    }

    fn requirement() -> CapabilityRequirement {
        CapabilityRequirement {
            inference_tariff: None,
            gpu_class: Some("rtx-4090".into()),
            min_vram_gb: Some(16),
            model_id: Some("llama-3-8b".into()),
            kind: JobKind::InferenceCall,
            max_duration_secs: 60,
            min_reputation_bps: None,
        }
    }

    #[test]
    fn capability_profile_serde_round_trips() {
        let p = profile();
        let json = serde_json::to_string(&p).unwrap();
        let back: CapabilityProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn capability_requirement_serde_round_trips() {
        let mut r = requirement();
        r.min_reputation_bps = Some(8_000);
        let json = serde_json::to_string(&r).unwrap();
        let back: CapabilityRequirement = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    /// A requirement serialized before the reputation floor existed
    /// carries no `min_reputation_bps` key; it must decode as no floor,
    /// not fail, so an envelope signed against the older shape still
    /// verifies against this one.
    #[test]
    fn capability_requirement_without_a_reputation_floor_decodes_as_none() {
        let json = r#"{
            "gpu_class": null,
            "min_vram_gb": null,
            "model_id": null,
            "kind": "inference_call",
            "max_duration_secs": 30
        }"#;
        let req: CapabilityRequirement = serde_json::from_str(json).unwrap();
        assert_eq!(req.min_reputation_bps, None);
    }

    #[test]
    fn capacity_view_serde_round_trips_and_keeps_its_wire_names() {
        let view = CapacityView {
            registered_operators: 3,
            matchable_operators: 1,
            liveness_window_ms: 45_000,
            min_score_bps: 0,
            min_bond_micro_usdc: 0,
            entries: vec![CapacityEntry {
                inference_tariffs: Vec::new(),
                kind: JobKind::InferenceCall,
                model: "llama-3-8b".into(),
                operators: 1,
                min_ask_micro_usdc: 400,
                min_ask_unit: PriceUnit::PerMillionTokens,
                max_ask_micro_usdc: 400,
                max_vram_gb: 24,
                gpu_classes: vec!["rtx-4090".into()],
                tee_capable: false,
            }],
        };
        let json = serde_json::to_string(&view).unwrap();
        let back: CapacityView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, back);
        // The names three consumers parse (buyer crate -> MCP tool ->
        // covenantd capability); renaming any of them is a wire break.
        for field in [
            "\"matchable_operators\"",
            "\"min_ask_micro_usdc\"",
            "\"inference_call\"",
            "\"per_million_tokens\"",
        ] {
            assert!(json.contains(field), "missing {field} in {json}");
        }
    }

    #[test]
    fn per_kind_asks_and_models_override_the_profile() {
        let mut p = profile();
        p.job_kinds = vec![JobKind::AgentTask, JobKind::AgentCheck];
        p.models_served = vec!["claude-code".into(), "python:3.12-slim".into()];
        p.kind_prices = vec![KindAsk {
            kind: JobKind::AgentCheck,
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 5_000,
            },
        }];
        p.kind_models = vec![
            KindModels {
                kind: JobKind::AgentTask,
                models: vec!["claude-code".into()],
            },
            KindModels {
                kind: JobKind::AgentCheck,
                models: vec!["python:3.12-slim".into()],
            },
        ];
        assert_eq!(p.ask_for(JobKind::AgentCheck).micro_usdc, 5_000);
        assert_eq!(p.ask_for(JobKind::AgentTask), p.price);
        let ask = |kind, model: &str| CapabilityRequirement {
            inference_tariff: None,
            gpu_class: None,
            min_vram_gb: None,
            model_id: Some(model.into()),
            kind,
            max_duration_secs: 60,
            min_reputation_bps: None,
        };
        assert!(p.satisfies(&ask(JobKind::AgentTask, "claude-code")));
        assert!(p.satisfies(&ask(JobKind::AgentCheck, "python:3.12-slim")));
        assert!(
            !p.satisfies(&ask(JobKind::AgentTask, "python:3.12-slim")),
            "a build never routes by a check image"
        );
        assert!(p.validate_labels().is_ok());

        let mut stray = p.clone();
        stray.kind_prices[0].kind = JobKind::Embedding;
        assert!(
            stray.validate_labels().is_err(),
            "an ask for a kind the node does not serve"
        );
        let mut empty = p.clone();
        empty.kind_models[0].models.clear();
        assert!(empty.validate_labels().is_err());
    }

    #[test]
    fn a_profile_without_per_kind_fields_keeps_its_wire_bytes() {
        let json = serde_json::to_string(&profile()).unwrap();
        assert!(!json.contains("kind_prices") && !json.contains("kind_models"));
    }

    #[test]
    fn job_kind_embedding_keeps_its_snake_case_wire_name() {
        // Three surfaces read this over the wire; the name is the
        // contract.
        assert_eq!(
            serde_json::to_string(&JobKind::Embedding).unwrap(),
            "\"embedding\""
        );
        let back: JobKind = serde_json::from_str("\"embedding\"").unwrap();
        assert_eq!(back, JobKind::Embedding);
    }

    #[test]
    fn job_kind_speech_synthesis_keeps_its_snake_case_wire_name() {
        // The same wire contract the node's kind maps, the coordinator's
        // capacity view, and the buyer's front door all read.
        assert_eq!(
            serde_json::to_string(&JobKind::SpeechSynthesis).unwrap(),
            "\"speech_synthesis\""
        );
        let back: JobKind = serde_json::from_str("\"speech_synthesis\"").unwrap();
        assert_eq!(back, JobKind::SpeechSynthesis);
    }

    #[test]
    fn an_embedding_node_satisfies_an_embedding_ask() {
        let mut p = profile();
        p.job_kinds = vec![JobKind::Embedding];
        p.models_served = vec!["nomic-embed-text".into()];
        let req = CapabilityRequirement {
            inference_tariff: None,
            gpu_class: None,
            min_vram_gb: None,
            model_id: Some("nomic-embed-text".into()),
            kind: JobKind::Embedding,
            max_duration_secs: 30,
            min_reputation_bps: None,
        };
        assert!(p.satisfies(&req));
        // An inference ask never lands on an embedding-only node.
        assert!(!p.satisfies(&requirement()));
    }

    #[test]
    fn satisfies_matches_a_compatible_requirement() {
        assert!(profile().satisfies(&requirement()));
    }

    #[test]
    fn satisfies_rejects_missing_job_kind() {
        let mut req = requirement();
        req.kind = JobKind::BatchJob;
        assert!(!profile().satisfies(&req));
    }

    #[test]
    fn satisfies_rejects_insufficient_vram() {
        let mut req = requirement();
        req.min_vram_gb = Some(80);
        assert!(!profile().satisfies(&req));
    }

    #[test]
    fn satisfies_rejects_unserved_model() {
        let mut req = requirement();
        req.model_id = Some("gpt-oss-120b".into());
        assert!(!profile().satisfies(&req));
    }

    #[test]
    fn satisfies_accepts_any_model_wildcard() {
        let mut p = profile();
        p.models_served = vec!["any".into()];
        let mut req = requirement();
        req.model_id = Some("whatever".into());
        assert!(p.satisfies(&req));
    }

    #[test]
    fn kani_builder_and_checker_require_explicit_capabilities() {
        let checker =
            crate::kani_checker_model(&format!("sha256:{}", "a".repeat(64)), 28672, 6, 8192);
        for model in ["claude-code:kani-v1".to_string(), checker] {
            let mut p = profile();
            p.models_served = vec!["any".into(), "claude-code".into()];
            let mut req = requirement();
            req.model_id = Some(model.clone());
            assert!(!p.satisfies(&req), "generic capabilities matched {model}");
            p.models_served.push(model);
            assert!(p.satisfies(&req));
        }
    }

    #[test]
    fn native_empty_checks_require_the_exact_upgraded_image_capability() {
        let image = "covenant-compute-check:rust";
        let model = crate::native_empty_check_model(image);
        let mut p = profile();
        p.job_kinds = vec![JobKind::AgentCheck];
        let mut req = requirement();
        req.kind = JobKind::AgentCheck;
        req.model_id = Some(model.clone());
        for legacy in [
            "any".to_string(),
            image.to_string(),
            format!("{model}:latest"),
            crate::native_empty_check_model("another:check"),
        ] {
            p.models_served = vec![legacy];
            assert!(!p.satisfies(&req));
        }
        p.models_served.push(model.clone());
        assert!(p.satisfies(&req));
        p.kind_models = vec![KindModels {
            kind: JobKind::AgentCheck,
            models: vec!["any".into()],
        }];
        assert!(
            !p.satisfies(&req),
            "the per-kind declaration overrides the shared models"
        );
        p.kind_models[0].models.push(model);
        assert!(p.satisfies(&req));
    }

    /// The `:latest` rule both ways: Ollama advertises `name:latest`
    /// while buyers naturally ask for `name` (and vice versa when an
    /// operator hand-writes `models_served`). Both name the same model,
    /// so both route.
    #[test]
    fn satisfies_treats_a_bare_model_and_its_latest_tag_as_one() {
        let mut p = profile();
        p.models_served = vec!["qwen2.5-coder:latest".into()];
        let mut req = requirement();
        req.model_id = Some("qwen2.5-coder".into());
        assert!(p.satisfies(&req));

        p.models_served = vec!["qwen2.5-coder".into()];
        req.model_id = Some("qwen2.5-coder:latest".into());
        assert!(p.satisfies(&req));
    }

    /// Any tag other than `:latest` stays distinct — a node holding only
    /// the 7b build can't serve whatever `:latest` resolves to, and a
    /// buyer pinning 7b must not land on some other build.
    #[test]
    fn satisfies_keeps_explicit_tags_distinct_from_bare_names() {
        let mut p = profile();
        p.models_served = vec!["qwen2.5-coder:7b".into()];
        let mut req = requirement();
        req.model_id = Some("qwen2.5-coder".into());
        assert!(!p.satisfies(&req));

        req.model_id = Some("qwen2.5-coder:7b".into());
        assert!(p.satisfies(&req));

        p.models_served = vec!["qwen2.5-coder:latest".into()];
        assert!(!p.satisfies(&req));
    }

    #[test]
    fn satisfies_rejects_mismatched_gpu_class() {
        let mut req = requirement();
        req.gpu_class = Some("h100".into());
        assert!(!profile().satisfies(&req));
    }

    #[test]
    fn satisfies_matches_a_gpu_class_case_insensitively() {
        // profile() declares a ConsumerGpu "rtx-4090"; a buyer asking for
        // "RTX-4090" means the same card and must match, not refund.
        let mut req = requirement();
        req.gpu_class = Some("RTX-4090".into());
        assert!(profile().satisfies(&req));
    }

    #[test]
    fn satisfies_ignores_absent_constraints() {
        let req = CapabilityRequirement {
            inference_tariff: None,
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::InferenceCall,
            max_duration_secs: 30,
            min_reputation_bps: None,
        };
        assert!(profile().satisfies(&req));
    }

    #[test]
    fn satisfies_matches_a_cpu_only_node_by_the_cpu_class_case_insensitively() {
        let cpu_node = CapabilityProfile {
            inference_tariffs: Vec::new(),
            operator: AgentId::new("operator@local", [3u8; 32]),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec!["any".into()],
            job_kinds: vec![JobKind::BatchJob],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 1_000,
            },
            tee_capable: false,
            kind_prices: Vec::new(),
            kind_models: Vec::new(),
        };
        let demand = |class: &str, kind: JobKind| CapabilityRequirement {
            inference_tariff: None,
            gpu_class: Some(class.into()),
            min_vram_gb: None,
            model_id: None,
            kind,
            max_duration_secs: 30,
            min_reputation_bps: None,
        };

        // `--gpu-class cpu` (documented for forcing a CPU node) is met
        // whatever the buyer's casing.
        assert!(cpu_node.satisfies(&demand("cpu", JobKind::BatchJob)));
        assert!(cpu_node.satisfies(&demand("CPU", JobKind::BatchJob)));
        // A GPU class never resolves to the CPU node, and "cpu" never
        // resolves to a GPU node: the class stays exclusive both ways.
        assert!(!cpu_node.satisfies(&demand("rtx-4090", JobKind::BatchJob)));
        assert!(!profile().satisfies(&demand("cpu", JobKind::InferenceCall)));
    }

    #[test]
    fn validate_labels_passes_a_clean_profile() {
        profile().validate_labels().expect("real labels are clean");
    }

    #[test]
    fn validate_labels_rejects_a_control_char_in_a_model_name() {
        let mut p = profile();
        p.models_served = vec!["llama-3-8b".into(), "gpt-4\x1b[2Kfake".into()];
        let err = p.validate_labels().unwrap_err().to_string();
        assert!(err.contains("served model name"), "{err}");
        assert!(err.contains("control character"), "{err}");
    }

    #[test]
    fn validate_labels_rejects_a_control_char_in_the_hardware_model() {
        let mut p = profile();
        p.hardware = HardwareClass::DatacenterGpu {
            model: "h100\rspoof".into(),
        };
        let err = p.validate_labels().unwrap_err().to_string();
        assert!(err.contains("hardware model"), "{err}");
        assert!(err.contains("control character"), "{err}");
    }

    #[test]
    fn validate_labels_rejects_a_directory_stuffing_model_name() {
        let mut p = profile();
        p.models_served = vec!["a".repeat(MAX_LABEL_LEN + 1)];
        let err = p.validate_labels().unwrap_err().to_string();
        assert!(err.contains("longer than"), "{err}");
    }

    #[test]
    fn validate_labels_caps_the_number_of_declared_models() {
        let mut p = profile();
        p.models_served = vec!["m".to_string(); MAX_MODELS_SERVED + 1];
        let err = p.validate_labels().unwrap_err().to_string();
        assert!(err.contains("served models"), "{err}");
        // The bound is inclusive: a profile right at the cap still passes.
        p.models_served = vec!["m".to_string(); MAX_MODELS_SERVED];
        assert!(p.validate_labels().is_ok());
    }

    #[test]
    fn validate_labels_caps_the_number_of_declared_job_kinds() {
        let mut p = profile();
        p.job_kinds = vec![JobKind::InferenceCall; MAX_JOB_KINDS + 1];
        let err = p.validate_labels().unwrap_err().to_string();
        assert!(err.contains("job kinds"), "{err}");
    }

    #[test]
    fn validate_lease_pricing_ignores_a_non_lease_profile() {
        // A per-token inference node prices however it likes; the lease
        // floor rule bears only on a lease-serving profile.
        assert_eq!(profile().price.unit, PriceUnit::PerMillionTokens);
        profile()
            .validate_lease_pricing()
            .expect("a non-lease profile is unconstrained by the lease floor rule");
    }

    #[test]
    fn validate_lease_pricing_accepts_only_the_per_hour_unit() {
        // The per-hour ask is the one lease unit whose floor scales to the
        // metered window, flooring the per-second rate so an early close
        // still pays the operator its advertised rate for every second.
        let mut p = profile();
        p.job_kinds = vec![JobKind::LeaseSession];
        p.price = PriceAsk {
            unit: PriceUnit::PerLeaseHour,
            micro_usdc: 3_600_000,
        };
        p.validate_lease_pricing()
            .expect("a per-hour ask is the valid lease unit");
    }

    #[test]
    fn validate_lease_pricing_refuses_every_unit_the_meter_underpays() {
        // A lease settles on the seconds it serves, not the whole window.
        // A per-second/per-token ask floors flat (a rate read as a total);
        // a per-job ask floors the whole-window offer but not the rate, so
        // a one-second close meters a fraction of what the operator priced.
        // All three underpay the metered session and are refused before a
        // match, unlike the per-hour ask whose floor scales to the window.
        for unit in [
            PriceUnit::PerGpuSecond,
            PriceUnit::PerMillionTokens,
            PriceUnit::PerJob,
        ] {
            let mut p = profile();
            p.job_kinds = vec![JobKind::LeaseSession];
            p.price = PriceAsk {
                unit,
                micro_usdc: 3_600_000,
            };
            let err = p
                .validate_lease_pricing()
                .expect_err("a lease priced by a unit the meter underpays is refused")
                .to_string();
            assert!(err.contains("lease-serving operator must price"), "{err}");
        }
    }

    #[test]
    fn validate_labels_rejects_an_empty_job_kind_list() {
        // An operator serving no job kind matches no work, yet still
        // counts toward the capacity directory's matchable tally.
        let mut p = profile();
        p.job_kinds = vec![];
        let err = p.validate_labels().unwrap_err().to_string();
        assert!(err.contains("job kind"), "{err}");
    }

    #[test]
    fn validate_labels_rejects_an_empty_model_list() {
        // Empty is not the generic-node case — that is `["any"]`. An
        // empty list benchmarks nothing at the node and shows no row in
        // the capacity directory while still being matchable for a
        // model-less request.
        let mut p = profile();
        p.models_served = vec![];
        let err = p.validate_labels().unwrap_err().to_string();
        assert!(err.contains("served model"), "{err}");
        // The generic exec node still validates.
        p.models_served = vec!["any".into()];
        assert!(p.validate_labels().is_ok());
    }

    #[test]
    fn a_per_hour_lease_ask_floors_on_its_metered_window() {
        // The one shape the floor converts: a lease priced by the GPU-hour
        // scales to the window the escrow covers, rounded up. 3_600_000/hr
        // is 1000 micro/s, so a 300s lease floors at exactly 300_000 — the
        // figure the escrow holds and settlement caps at, so a buyer quoted
        // it clears the floor precisely.
        let per_hour = PriceAsk {
            unit: PriceUnit::PerLeaseHour,
            micro_usdc: 3_600_000,
        };
        assert_eq!(
            per_hour.job_floor_micro_usdc(JobKind::LeaseSession, 300),
            300_000
        );
        assert_eq!(
            per_hour.job_floor_micro_usdc(JobKind::LeaseSession, 3_600),
            3_600_000
        );
        // A sub-hour window rounds the fractional micro up, never down into
        // a pay cut the operator never agreed to.
        assert_eq!(
            PriceAsk {
                unit: PriceUnit::PerLeaseHour,
                micro_usdc: 1_000,
            }
            .job_floor_micro_usdc(JobKind::LeaseSession, 1),
            1
        );

        // Every other shape floors on the whole ask, unscaled: a flat
        // per-job lease, and a per-hour figure on any non-lease kind, which
        // is not metered by window and so takes the ask as written.
        assert_eq!(
            PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 12_000,
            }
            .job_floor_micro_usdc(JobKind::LeaseSession, 300),
            12_000
        );
        assert_eq!(
            per_hour.job_floor_micro_usdc(JobKind::InferenceCall, 300),
            3_600_000
        );
    }
}
