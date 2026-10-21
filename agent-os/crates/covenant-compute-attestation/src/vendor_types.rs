//! Compatibility types for the pinned upstream verifier and its vectors.
//! Covenant HTTP evidence uses binding.rs and full UUID session identities.
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use uuid::Uuid;
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("invalid channel key")]
    InvalidChannelKey,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrustClass {
    /// Bonded identity and metered billing. The operator can read anything the
    /// workload touches.
    #[default]
    Open,
    /// Kata VM with exclusive VFIO passthrough and a digest-pinned public
    /// image. Narrows the attack surface; the host is still privileged.
    Isolated,
    /// The launch measurement of the renter's own guest, reported by that guest
    /// rather than by the machine hosting it: SNP assumes a hostile host and
    /// gives it no way to attest, so this says what the VM booted and nothing
    /// about who ran it.
    Attested,
    /// Guest memory and VRAM are encrypted against the host.
    Confidential,
}

pub const MAX_VERIFIABLE_TRUST_CLASS: TrustClass = TrustClass::Confidential;
impl TrustClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Isolated => "isolated",
            Self::Attested => "attested",
            Self::Confidential => "confidential",
        }
    }
}
pub const MAX_CHANNEL_KEY_BYTES: usize = 2048;
pub const SNP_REPORT_DATA_DOMAIN: &[u8] = b"prism.snp.report-data.v1\0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Copy)]
pub enum AttestationKind {
    SevSnp,
    Tdx,
    NvidiaCc,
    /// An H100 device report: it proves which GPU signed and what firmware it
    /// runs, and says nothing about the host software that booted.
    NvidiaGpu,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Copy, Default)]
pub struct HostTeeCapability {
    pub sev: bool,
    pub sev_es: bool,
    pub sev_snp: bool,
    pub sev_guest_device: bool,
    pub kata_runtime: bool,
    pub kata_confidential_runtime: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeAttestation {
    pub node_id: String,
    pub challenge_id: Uuid,
    pub kind: AttestationKind,
    pub evidence_base64: String,
    pub certificate_chain_base64: Vec<String>,
    /// TDX evidence only: the runtime event log the verifier replays against
    /// the quoted registers. Empty for every other kind, and absent from the
    /// canonical payload when empty, so attestations signed before the field
    /// existed still verify.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tdx_event_log: Vec<TdxEventEntry>,
    /// TDX evidence only: the Intel collateral (TCB info, QE identity, CRLs)
    /// the quote verifies against, fetched by the node from a PCCS. Nothing
    /// in it is taken on the node's word: every piece is Intel-signed and the
    /// verifier enforces its validity windows, so the most a node choosing
    /// its own collateral can do is present the newest material Intel had
    /// already published, which is the same exposure a caching fetcher has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tdx_collateral_json: Option<String>,
    pub capability: HostTeeCapability,
    pub pci_address: String,
    pub collected_at: DateTime<Utc>,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttestationVerdict {
    pub node_id: String,
    pub kind: AttestationKind,
    /// The attested device as the verifier read it off the leaf certificate,
    /// subject plus board serial. Indexed uniquely, so one physical GPU cannot
    /// back two node ids.
    pub device_identity: String,
    pub measurement_digest: String,
    /// What the node said about its own TEE support, carried for diagnostics
    /// and named for what it is. Nothing verifies it and nothing may grant a
    /// class from it: a host that lies here is caught by no check in this
    /// crate.
    pub claimed_capability: HostTeeCapability,
    pub granted_class: TrustClass,
    pub verifier_version: String,
    pub verified_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuestAttestation {
    pub node_id: String,
    pub lease_id: u64,
    pub challenge_id: Uuid,
    pub kind: AttestationKind,
    pub report_base64: String,
    pub certificate_chain_base64: Vec<String>,
    /// The OpenSSH public key line the guest generated for this lease.
    pub guest_channel_key: String,
    pub collected_at: DateTime<Utc>,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Copy)]
pub struct SnpTcb {
    pub bootloader: u8,
    pub tee: u8,
    pub snp: u8,
    pub microcode: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttestedGuest {
    /// The launch digest over the initial image, its guest physical layout,
    /// page types and per-vCPU VMSA. It is only meaningful against a reference
    /// value computed from published inputs, never against one read off the
    /// machine being attested.
    pub measurement: String,
    pub host_data: String,
    /// sha256 of CHIP_ID. The raw value names a physical machine and receipts
    /// are pseudonymous, so it stays in the verifier.
    pub chip_id_digest: String,
    pub reported_tcb: SnpTcb,
    pub policy_debug: bool,
    pub vmpl: u32,
    pub channel_key_fingerprint: String,
    /// The container digest the measured guest agent will run, taken from
    /// HOST_DATA. The host fixes this at launch and cannot change it after, but
    /// it is still a host input: it means something only because the agent
    /// inside the measured image refuses to run anything else.
    pub image_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseAttestationVerdict {
    pub lease_id: u64,
    pub node_id: String,
    pub kind: AttestationKind,
    pub guest: AttestedGuest,
    pub granted_class: TrustClass,
    pub verifier_version: String,
    pub verified_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseGpuCcVerdict {
    pub lease_id: u64,
    pub node_id: String,
    pub kind: AttestationKind,
    /// The device as the verifier read it off the leaf certificate, its common
    /// name and the firmware identity the report is bound to.
    pub device_identity: String,
    pub measurement_digest: String,
    pub granted_class: TrustClass,
    pub verifier_version: String,
    pub verified_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseTdxGuestVerdict {
    pub lease_id: u64,
    pub node_id: String,
    pub kind: AttestationKind,
    /// The instance identity the TD extended into RTMR3, unique per deployment.
    pub device_identity: String,
    /// The compose file the event log bound the TD to.
    pub compose_hash: String,
    /// The fingerprint of the guest channel key bound into the quote, so the
    /// renter can pin the endpoint their session terminates on.
    pub channel_key_fingerprint: String,
    pub measurement_digest: String,
    pub granted_class: TrustClass,
    pub verifier_version: String,
    pub verified_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TdxEventEntry {
    pub imr: u32,
    pub event_type: u32,
    pub event: String,
    /// 48 bytes, hex.
    pub digest: String,
    /// Hex; empty for events that carry no payload.
    pub event_payload: String,
    /// dstack omits this field for V1. V2 uses a canonical JSON digest preimage.
    #[serde(default = "tdx_event_version", skip_serializing_if = "is_tdx_event_v1")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preimage: Option<String>,
}

fn tdx_event_version() -> u32 {
    1
}

fn is_tdx_event_v1(version: &u32) -> bool {
    *version == 1
}

pub fn attestation_report_nonce(
    challenge_nonce: &[u8],
    node_id: &str,
    device_public_key: &str,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(challenge_nonce);
    hasher.update(node_id.as_bytes());
    hasher.update(device_public_key.as_bytes());
    hasher.finalize().into()
}

pub fn snp_report_data(challenge_nonce: &[u8], lease_id: u64, guest_channel_key: &str) -> [u8; 64] {
    let mut hasher = Sha512::new();
    hasher.update(SNP_REPORT_DATA_DOMAIN);
    hasher.update(challenge_nonce);
    hasher.update(lease_id.to_be_bytes());
    hasher.update(guest_channel_key.as_bytes());
    let mut report_data = [0_u8; 64];
    report_data.copy_from_slice(&hasher.finalize());
    report_data
}

pub fn channel_key_fingerprint(key_line: &str) -> Result<String, ProtocolError> {
    if key_line.len() > MAX_CHANNEL_KEY_BYTES {
        return Err(ProtocolError::InvalidChannelKey);
    }
    let blob = key_line
        .split_whitespace()
        .nth(1)
        .and_then(|encoded| STANDARD.decode(encoded).ok())
        .ok_or(ProtocolError::InvalidChannelKey)?;
    Ok(format!(
        "SHA256:{}",
        STANDARD_NO_PAD.encode(Sha256::digest(&blob))
    ))
}

pub fn class_for_lease(
    lease_id: u64,
    node_id: &str,
    node_class: TrustClass,
    guest_verdict: Option<&LeaseAttestationVerdict>,
    tdx_guest_verdict: Option<&LeaseTdxGuestVerdict>,
    gpu_cc_verdict: Option<&LeaseGpuCcVerdict>,
    now: DateTime<Utc>,
) -> TrustClass {
    let guest_bound = |verdict: &&LeaseAttestationVerdict| {
        verdict.lease_id == lease_id && verdict.node_id == node_id && verdict.expires_at > now
    };
    let tdx_bound = |verdict: &&LeaseTdxGuestVerdict| {
        verdict.lease_id == lease_id && verdict.node_id == node_id && verdict.expires_at > now
    };
    let gpu_bound = |verdict: &&LeaseGpuCcVerdict| {
        verdict.lease_id == lease_id && verdict.node_id == node_id && verdict.expires_at > now
    };

    // The node must already stand at `Isolated`, because a guest report says
    // nothing about the bonded identity, the live tunnel or the GPU
    // underneath it and cannot stand in for them. Either guest kind proves the
    // same thing on its own silicon: a SEV-SNP guest report or a TDX quote,
    // each bound to this lease and each earning Attested.
    let snp_attested = guest_verdict.filter(guest_bound).is_some_and(|verdict| {
        verdict.kind == AttestationKind::SevSnp && verdict.granted_class >= TrustClass::Attested
    });
    let tdx_attested = tdx_guest_verdict.filter(tdx_bound).is_some_and(|verdict| {
        verdict.kind == AttestationKind::Tdx && verdict.granted_class >= TrustClass::Attested
    });
    let guest_attested = (snp_attested || tdx_attested) && node_class >= TrustClass::Isolated;
    let vram_confidential = gpu_cc_verdict.filter(gpu_bound).is_some_and(|verdict| {
        verdict.kind == AttestationKind::NvidiaCc
            && verdict.granted_class >= TrustClass::Confidential
    });

    let class = if guest_attested && vram_confidential {
        TrustClass::Confidential
    } else if guest_attested {
        TrustClass::Attested
    } else {
        node_class
    };
    class.min(MAX_VERIFIABLE_TRUST_CLASS)
}
