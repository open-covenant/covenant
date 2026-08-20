//! The daemon-side attestor for a deployed `SpendGrantEscrow` — the spec-gated
//! release leg ("never pay for junk").
//!
//! An escrow call locks a provider's payment until its output clears a spec
//! bar. The daemon judges the run, then signs a secp256k1 EIP-712
//! [`QualityAttestation`]; the escrow releases the hold (minus fee) only when
//! `ecrecover` returns Covenant's attestor address. A `passed = false` verdict
//! rides the same signature to unwind the hold back to the grant immediately.
//! The verdict is the precondition the contract enforces before money moves,
//! not a receipt reconciled after.
//!
//! Two facts feed the verdict from two sources that cannot substitute for each
//! other. The *verdict* — did the output pass, over what bytes — comes from a
//! [`CompletionProof`] the daemon derives from its own audit chain, so the
//! caller cannot forge a passing result the run does not show. Everything that
//! binds the verdict to one deployment — chain, contract, call id, spec,
//! deadline — comes from the [`QualityBinding`] the daemon holds for the call,
//! never from the proof. The proof cannot forge the target; the binding cannot
//! forge the verdict.
//!
//! The escrow verifies on an EVM chain, so the verdict is signed over
//! secp256k1 (not ed25519), and the key's [`address`](SpendGrantAttestor::address)
//! is the `attestor` role set at deploy.

use std::path::Path;

use covenant_attestation::{QualityAttestation, QualityError, SignedQualityAttestation};
use covenant_identity::{IdentityError, Secp256k1IssuerKey};
use covenant_x402::{EvmRpc, EvmTxError, EvmTxSigner, TxReceipt, TxRequest};

use crate::escrow::CompletionProof;

/// The escrow-side facts a [`CompletionProof`] does not carry: which deployment
/// the verdict releases against, which call and spec it settles, and when it
/// expires. The daemon holds these per open call; they are never sourced from
/// the proof or the caller.
#[derive(Debug, Clone)]
pub struct QualityBinding {
    pub chain_id: u64,
    pub verifying_contract: [u8; 20],
    pub call_id: u128,
    pub spec_id: [u8; 32],
    pub deadline: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SpendGrantError {
    #[error("result hash must be 32 bytes (64 hex chars), got {0}")]
    ResultHashLen(usize),
    #[error("result hash contains a non-hex character")]
    ResultHashHex,
    #[error("escrow address must be 20 bytes (40 hex chars), got {0}")]
    AddressLen(usize),
    #[error("escrow address contains a non-hex character")]
    AddressHex,
    #[error("quality attestation: {0}")]
    Quality(#[from] QualityError),
    #[error("attestor key: {0}")]
    Key(#[from] IdentityError),
    #[error("submitter key: {0}")]
    SubmitterKey(String),
    #[error("submitter chain {submitter} does not match config chain {config}")]
    ChainMismatch { config: u64, submitter: u64 },
    #[error("no in-process submitter configured; set COVENANT_SPENDGRANT_RPC + COVENANT_SPENDGRANT_SUBMITTER_KEY")]
    NoSubmitter,
    #[error("broadcast: {0}")]
    Submit(#[from] EvmTxError),
}

/// Holds Covenant's secp256k1 attestor key and turns a daemon-derived
/// [`CompletionProof`] into the signed verdict a [`SpendGrantEscrow`]
/// authenticates with one `ecrecover` before it releases or junk-refunds a
/// call's hold.
pub struct SpendGrantAttestor {
    key: Secp256k1IssuerKey,
}

impl SpendGrantAttestor {
    /// Load the persisted attestor key, or create and persist one on first run,
    /// through covenant-identity's hardened store (`0o600` file, `0o700`
    /// parent, symlink rejection). On first run the operator reads
    /// [`address`](Self::address) to set as the escrow's `attestor` role.
    pub fn load_or_create(path: &Path) -> Result<Self, SpendGrantError> {
        Ok(Self {
            key: Secp256k1IssuerKey::load_or_create(path)?,
        })
    }

    /// Construct from a raw 32-byte secret scalar, for tests and explicit
    /// key provisioning.
    pub fn from_secret_bytes(bytes: &[u8; 32]) -> Result<Self, SpendGrantError> {
        Ok(Self {
            key: Secp256k1IssuerKey::from_secret_bytes(bytes)?,
        })
    }

    /// The 20-byte EVM address to set as the escrow's `attestor` role.
    pub fn address(&self) -> [u8; 20] {
        self.key.address()
    }

    /// Sign the release-or-refund verdict for one escrowed call. `passed` and
    /// `result_hash` are read from the proof (Covenant's own record of the
    /// run); the deployment binding supplies everything the digest ties to a
    /// specific escrow. A `passed = false` proof signs just the same — it is
    /// the junk-refund path.
    pub fn attest(
        &self,
        proof: &CompletionProof,
        binding: &QualityBinding,
    ) -> Result<SignedQualityAttestation, SpendGrantError> {
        let attestation = QualityAttestation {
            chain_id: binding.chain_id,
            verifying_contract: binding.verifying_contract,
            call_id: binding.call_id,
            result_hash: decode_result_hash(&proof.result_hash_hex)?,
            passed: proof.validation_passed,
            spec_id: binding.spec_id,
            deadline: binding.deadline,
        };
        Ok(attestation.sign(&self.key)?)
    }
}

/// The actionable settlement for one proven call: the escrow to call, the
/// calldata to broadcast, whether it releases (pass) or refunds (junk), and the
/// attestor the escrow will `ecrecover`. Mechanism-agnostic — the daemon's job
/// ends at producing this. A relay broadcasts it today (`cast send to
/// calldata`); an in-daemon RPC signer could later. Nothing here holds chain
/// gas: the daemon signs, the submitter pays.
#[derive(Debug)]
pub struct Submission {
    pub to: [u8; 20],
    pub calldata: Vec<u8>,
    pub release: bool,
    pub attestor: [u8; 20],
}

/// The in-process broadcaster: a funded facilitator [`EvmTxSigner`] plus the
/// RPC it submits to. This is the "an in-daemon RPC signer later" the
/// [`Submission`] docs anticipate — covenantd signs the verdict *and* pays the
/// gas to land it, so the charge → release/refund loop runs with no human at a
/// `cast` prompt. The key funds gas only; the escrow's guarantees ride the
/// attestor signature the calldata carries, not the identity of the submitter,
/// so a compromised submitter key can waste gas but cannot release a call the
/// attestor never passed.
pub struct SpendGrantSubmitter {
    rpc: EvmRpc,
    signer: EvmTxSigner,
}

impl SpendGrantSubmitter {
    pub fn new(
        rpc_url: impl Into<String>,
        chain_id: u64,
        secret: &[u8; 32],
    ) -> Result<Self, SpendGrantError> {
        let signer = EvmTxSigner::from_secret_bytes(secret)
            .map_err(|e| SpendGrantError::SubmitterKey(e.to_string()))?;
        Ok(Self {
            rpc: EvmRpc::new(rpc_url, chain_id),
            signer,
        })
    }

    /// The funded EVM address that pays gas for broadcasts.
    pub fn address(&self) -> [u8; 20] {
        self.signer.address()
    }

    /// Broadcast one call's calldata and confirm its receipt. A revert (an
    /// over-ceiling charge, a bad attestor signature) surfaces as
    /// [`EvmTxError::Rpc`] with the contract's custom-error selector in `data`
    /// — the same bound the contract enforces, observed at the submit boundary.
    pub async fn broadcast(
        &self,
        to: [u8; 20],
        calldata: Vec<u8>,
    ) -> Result<TxReceipt, EvmTxError> {
        self.rpc
            .submit(&self.signer, &TxRequest::call(to, calldata))
            .await
    }
}

/// Boot config for the daemon's spend-grant facilitator leg: Covenant's
/// secp256k1 attestor plus the one deployed [`SpendGrantEscrow`] it signs
/// release/refund verdicts for. `None` from [`from_env`](Self::from_env) is
/// the surface off (the default); an enabled-but-misconfigured surface also
/// resolves to `None`, fail-closed, rather than booting half-wired.
pub struct SpendGrantConfig {
    attestor: SpendGrantAttestor,
    chain_id: u64,
    verifying_contract: [u8; 20],
    submitter: Option<SpendGrantSubmitter>,
}

impl SpendGrantConfig {
    /// Assemble from explicit parts — the env-free path, for provisioning and
    /// tests. No submitter: the config signs verdicts but hands the
    /// [`Submission`] to an external broadcaster until
    /// [`with_submitter`](Self::with_submitter) attaches an in-process one.
    pub fn new(attestor: SpendGrantAttestor, chain_id: u64, verifying_contract: [u8; 20]) -> Self {
        Self {
            attestor,
            chain_id,
            verifying_contract,
            submitter: None,
        }
    }

    /// Attach the in-process broadcaster so `*_broadcast` methods sign *and*
    /// land their txs. The submitter's chain must match the config's — a
    /// mismatch is rejected here rather than surfacing later as a chain-id
    /// revert at submit time.
    pub fn with_submitter(
        mut self,
        submitter: SpendGrantSubmitter,
    ) -> Result<Self, SpendGrantError> {
        if submitter.rpc.chain_id() != self.chain_id {
            return Err(SpendGrantError::ChainMismatch {
                config: self.chain_id,
                submitter: submitter.rpc.chain_id(),
            });
        }
        self.submitter = Some(submitter);
        Ok(self)
    }

    /// Whether an in-process submitter is wired — `*_broadcast` methods return
    /// [`SpendGrantError::NoSubmitter`] when this is `false`.
    pub fn has_submitter(&self) -> bool {
        self.submitter.is_some()
    }

    /// The submitter's funded gas-payer address, when one is wired. The
    /// operator funds this address on the target chain.
    pub fn submitter_address(&self) -> Option<[u8; 20]> {
        self.submitter.as_ref().map(SpendGrantSubmitter::address)
    }

    /// Read the facilitator config from the environment. Off unless
    /// `COVENANT_SPENDGRANT_ENABLED` is truthy (`1`/`true`/`yes`), then all of:
    /// - `COVENANT_SPENDGRANT_ATTESTOR_KEY` — path to the secp256k1 attestor
    ///   key, loaded (or created on first run) through the hardened identity
    ///   store; its [`address`](SpendGrantAttestor::address) is the escrow's
    ///   `attestor` role.
    /// - `COVENANT_SPENDGRANT_ESCROW` — the escrow's 20-byte address (`0x…`).
    /// - `COVENANT_SPENDGRANT_CHAIN` — the EVM chain id it's deployed on.
    ///
    /// Optionally, to broadcast in-process instead of emitting a [`Submission`]
    /// for an external `cast`:
    /// - `COVENANT_SPENDGRANT_RPC` — the chain's JSON-RPC URL.
    /// - `COVENANT_SPENDGRANT_SUBMITTER_KEY` — the funded gas-payer secret, as
    ///   inline `0x…` hex or a path to a file holding it. The submitter pays gas
    ///   only; the escrow's guarantees ride the attestor signature, not the
    ///   submitter. With the RPC set but the key unset or malformed, the surface
    ///   boots emit-only (signs verdicts, broadcasts nothing) rather than
    ///   failing the whole facilitator.
    ///
    /// Any missing or malformed *required* value with the surface enabled logs
    /// and resolves to `None` (fail-closed): a half-configured facilitator never
    /// boots.
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("COVENANT_SPENDGRANT_ENABLED")
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        if !enabled {
            return None;
        }
        let key_path = require_env("COVENANT_SPENDGRANT_ATTESTOR_KEY")?;
        let verifying_contract = match decode_address(&require_env("COVENANT_SPENDGRANT_ESCROW")?) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "spend-grant escrow address invalid; surface off");
                return None;
            }
        };
        let chain_id = match require_env("COVENANT_SPENDGRANT_CHAIN")?
            .trim()
            .parse::<u64>()
        {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("spend-grant chain id is not a u64; surface off");
                return None;
            }
        };
        let attestor = match SpendGrantAttestor::load_or_create(Path::new(&key_path)) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "spend-grant attestor key load failed; surface off");
                return None;
            }
        };
        let config = Self::new(attestor, chain_id, verifying_contract);
        match load_submitter_from_env(chain_id) {
            Some(submitter) => match config.with_submitter(submitter) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(error = %e, "spend-grant submitter rejected; surface off");
                    None
                }
            },
            None => Some(config),
        }
    }

    /// The attestor's 20-byte EVM address — the escrow's `attestor` role.
    pub fn attestor_address(&self) -> [u8; 20] {
        self.attestor.address()
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn verifying_contract(&self) -> [u8; 20] {
        self.verifying_contract
    }

    /// Sign the release-or-refund verdict for one call of this escrow. The
    /// verdict — did it pass, over what bytes — rides the daemon-derived
    /// `proof`; the deployment it binds to (chain, contract) is this config's,
    /// so a caller supplies only the per-call `call_id`/`spec_id`/`deadline`,
    /// never the target.
    pub fn attest_call(
        &self,
        proof: &CompletionProof,
        call_id: u128,
        spec_id: [u8; 32],
        deadline: u64,
    ) -> Result<SignedQualityAttestation, SpendGrantError> {
        self.attestor.attest(
            proof,
            &QualityBinding {
                chain_id: self.chain_id,
                verifying_contract: self.verifying_contract,
                call_id,
                spec_id,
                deadline,
            },
        )
    }

    /// The `(to, calldata)` a submitter broadcasts to action `signed`'s verdict
    /// against this config's escrow: `releaseCallAttested` on pass,
    /// `refundCallAttested` on junk. The attested paths are permissionless, so
    /// any funded submitter — `cast` today, an in-daemon RPC signer later —
    /// broadcasts it; the daemon makes the verdict actionable without itself
    /// holding chain gas. `to` is the escrow address; the daemon signs, the
    /// submitter pays.
    pub fn submission(&self, signed: &SignedQualityAttestation) -> ([u8; 20], Vec<u8>) {
        let a = signed.attestation();
        let calldata = encode_attested_call(
            a.passed,
            a.call_id,
            &a.result_hash,
            &a.spec_id,
            a.deadline,
            signed.signature(),
        );
        (self.verifying_contract, calldata)
    }

    /// The intercept seam: sign the verdict for one proven call and package the
    /// [`Submission`] a broadcaster actions. This is what the payment path runs
    /// on a live [`CompletionProof`] — `prove_completion` → `settle` → hand the
    /// submission to a submitter — the automated twin of the demo's manual
    /// sign-and-`cast`. Fail-closed: a proof whose result hash won't decode
    /// yields no attestation and no submission, so the call refunds at its
    /// deadline rather than releasing on a signal the daemon can't stand behind.
    pub fn settle(
        &self,
        proof: &CompletionProof,
        call_id: u128,
        spec_id: [u8; 32],
        deadline: u64,
    ) -> Result<Submission, SpendGrantError> {
        let signed = self.attest_call(proof, call_id, spec_id, deadline)?;
        let (to, calldata) = self.submission(&signed);
        Ok(Submission {
            to,
            calldata,
            release: signed.passed(),
            attestor: self.attestor_address(),
        })
    }

    /// Lock a call's spend against the grant: broadcast `chargeCall` from the
    /// in-process submitter and confirm it. This is the *bounded-spend* leg —
    /// the contract reverts unless the provider is allowlisted and the amount
    /// is within the per-call ceiling and remaining budget, so a rogue agent's
    /// over-cap charge fails here on chain, not in an off-chain check it could
    /// route around. The revert's custom-error selector rides
    /// [`EvmTxError::Rpc`]'s `data`. Requires [`with_submitter`](Self::with_submitter).
    pub async fn broadcast_charge(
        &self,
        grant_id: u128,
        provider: [u8; 20],
        amount: u128,
        call_id: u128,
        deadline: u64,
    ) -> Result<TxReceipt, SpendGrantError> {
        let submitter = self
            .submitter
            .as_ref()
            .ok_or(SpendGrantError::NoSubmitter)?;
        let calldata = encode_charge_call(grant_id, &provider, amount, call_id, deadline);
        Ok(submitter
            .broadcast(self.verifying_contract, calldata)
            .await?)
    }

    /// Sign the verdict for one proven call and broadcast it in-process,
    /// releasing on a pass and junk-refunding otherwise. The whole spec-gate —
    /// derive the verdict, sign it, land the settlement tx — runs without a
    /// human at a `cast` prompt. Returns the [`Submission`] it signed alongside
    /// the confirmed receipt. Requires [`with_submitter`](Self::with_submitter);
    /// a config without one still [`settle`](Self::settle)s for an external
    /// broadcaster.
    pub async fn settle_and_broadcast(
        &self,
        proof: &CompletionProof,
        call_id: u128,
        spec_id: [u8; 32],
        deadline: u64,
    ) -> Result<(Submission, TxReceipt), SpendGrantError> {
        let submitter = self
            .submitter
            .as_ref()
            .ok_or(SpendGrantError::NoSubmitter)?;
        let submission = self.settle(proof, call_id, spec_id, deadline)?;
        let receipt = submitter
            .broadcast(submission.to, submission.calldata.clone())
            .await?;
        Ok((submission, receipt))
    }
}

// SpendGrantEscrow selectors for the permissionless attested paths, both
// `f(uint256 callId, bytes32 resultHash, bytes32 specId, uint256 deadline, bytes signature)`.
// Pinned by the golden calldata test against `cast calldata`.
const RELEASE_SELECTOR: [u8; 4] = [0x27, 0xdc, 0xd9, 0x74];
const REFUND_SELECTOR: [u8; 4] = [0xf9, 0x20, 0x37, 0x6f];

// `chargeCall(uint256 grantId, address provider, uint256 amount, uint256 callId,
// uint256 deadline)` — the bounded-spend leg. Five static words, no dynamic
// tail. Pinned by the golden calldata test against `cast calldata`.
const CHARGE_SELECTOR: [u8; 4] = [0x86, 0x10, 0xd0, 0x14];

/// ABI-encode `chargeCall` to lock one call's spend against the grant. All five
/// arguments are static 32-byte words, so the encoding is just the selector
/// then each argument left-padded — no head/tail. The contract enforces the
/// provider allowlist, per-call ceiling, remaining budget, and expiry; this
/// only marshals the request it reverts or accepts.
fn encode_charge_call(
    grant_id: u128,
    provider: &[u8; 20],
    amount: u128,
    call_id: u128,
    deadline: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 32 * 5);
    out.extend_from_slice(&CHARGE_SELECTOR);
    out.extend_from_slice(&u128_word(grant_id));
    out.extend_from_slice(&address_word(provider));
    out.extend_from_slice(&u128_word(amount));
    out.extend_from_slice(&u128_word(call_id));
    out.extend_from_slice(&u64_word(deadline));
    out
}

fn address_word(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// ABI-encode the release-or-refund call for one signed verdict. Four static
/// words, then the dynamic `signature` as an offset (`0xa0`) into a tail of
/// `[len, sig padded to a 32-byte boundary]` — the standard head/tail layout.
fn encode_attested_call(
    passed: bool,
    call_id: u128,
    result_hash: &[u8; 32],
    spec_id: &[u8; 32],
    deadline: u64,
    signature: &[u8; 65],
) -> Vec<u8> {
    let selector = if passed {
        RELEASE_SELECTOR
    } else {
        REFUND_SELECTOR
    };
    let mut out = Vec::with_capacity(4 + 32 * 9);
    out.extend_from_slice(&selector);
    out.extend_from_slice(&u128_word(call_id));
    out.extend_from_slice(result_hash);
    out.extend_from_slice(spec_id);
    out.extend_from_slice(&u64_word(deadline));
    out.extend_from_slice(&u64_word(0xa0)); // offset to the bytes tail = 5 * 32
    out.extend_from_slice(&u64_word(signature.len() as u64));
    out.extend_from_slice(signature);
    out.extend_from_slice(&[0u8; 32 - (65 % 32)]); // right-pad sig to a word boundary
    out
}

fn u128_word(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn u64_word(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// Read a required env var, or `None` (with a warning) when it's absent or
/// blank — the fail-closed leg of [`SpendGrantConfig::from_env`].
fn require_env(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => {
            tracing::warn!(
                key,
                "spend-grant surface enabled but {key} is unset; surface off"
            );
            None
        }
    }
}

/// Build the optional in-process submitter from `COVENANT_SPENDGRANT_RPC` +
/// `COVENANT_SPENDGRANT_SUBMITTER_KEY`. No RPC → no submitter (emit-only, the
/// default). RPC set but the key unset or unusable → `None` with a warning:
/// the facilitator still boots and signs verdicts, it just can't broadcast
/// them. The submitter's chain is the config's, so it can never mismatch here.
fn load_submitter_from_env(chain_id: u64) -> Option<SpendGrantSubmitter> {
    let rpc = std::env::var("COVENANT_SPENDGRANT_RPC")
        .ok()
        .filter(|v| !v.trim().is_empty())?;
    let Some(key_src) = std::env::var("COVENANT_SPENDGRANT_SUBMITTER_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
    else {
        tracing::warn!(
            "COVENANT_SPENDGRANT_RPC set but COVENANT_SPENDGRANT_SUBMITTER_KEY unset; \
             in-process broadcast off (emit-only)"
        );
        return None;
    };
    let secret = match load_secret(&key_src) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "spend-grant submitter key unusable; broadcast off");
            return None;
        }
    };
    match SpendGrantSubmitter::new(rpc, chain_id, &secret) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!(error = %e, "spend-grant submitter init failed; broadcast off");
            None
        }
    }
}

/// Resolve a 32-byte secret from either an inline `0x…`/bare hex string or a
/// path to a file holding one. A 64-hex-char value is read inline; anything
/// else is treated as a file path and its trimmed contents parsed. Fail-closed
/// on any length but 32 bytes or a non-hex character.
fn load_secret(src: &str) -> Result<[u8; 32], String> {
    let trimmed = src.trim();
    let looks_inline = {
        let body = trimmed.strip_prefix("0x").unwrap_or(trimmed);
        body.len() == 64 && body.bytes().all(|c| c.is_ascii_hexdigit())
    };
    let hex = if looks_inline {
        trimmed.to_string()
    } else {
        std::fs::read_to_string(trimmed)
            .map_err(|e| format!("read key file {trimmed}: {e}"))?
            .trim()
            .to_string()
    };
    let body = hex.strip_prefix("0x").unwrap_or(&hex);
    if body.len() != 64 {
        return Err(format!(
            "secret must be 32 bytes, got {} hex chars",
            body.len()
        ));
    }
    let bytes = body.as_bytes();
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = nibble(bytes[2 * i]).ok_or("secret contains a non-hex character")?;
        let lo = nibble(bytes[2 * i + 1]).ok_or("secret contains a non-hex character")?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}

/// Decode a 20-byte EVM address from `0x…`/bare hex, fail-closed on any other
/// length or a non-hex character.
fn decode_address(hex: &str) -> Result<[u8; 20], SpendGrantError> {
    let s = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex);
    if s.len() != 40 {
        return Err(SpendGrantError::AddressLen(s.len()));
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; 20];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = nibble(bytes[2 * i]).ok_or(SpendGrantError::AddressHex)?;
        let lo = nibble(bytes[2 * i + 1]).ok_or(SpendGrantError::AddressHex)?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}

/// Decode a 32-byte result hash from its hex form. Production proofs carry the
/// 64-char lowercase SHA-256 hex `covenant_audit::hash_hex` emits (no `0x`);
/// an optional prefix is tolerated. Fail-closed: any length but 32 bytes, or a
/// non-hex character, is an error rather than a truncated or padded hash.
fn decode_result_hash(hex: &str) -> Result<[u8; 32], SpendGrantError> {
    let s = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex);
    if s.len() != 64 {
        return Err(SpendGrantError::ResultHashLen(s.len()));
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = nibble(bytes[2 * i]).ok_or(SpendGrantError::ResultHashHex)?;
        let lo = nibble(bytes[2 * i + 1]).ok_or(SpendGrantError::ResultHashHex)?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn proof(result_hash_hex: &str, passed: bool) -> CompletionProof {
        CompletionProof {
            proof_id: Uuid::nil(),
            escrow_id: "esc-1".into(),
            job_id: Uuid::nil(),
            hirer_address: "0xhirer".into(),
            worker_address: "0xworker".into(),
            amount: "1000000".into(),
            asset: "USDG".into(),
            network: "robinhood".into(),
            provider: "0xprovider".into(),
            result_hash_hex: result_hash_hex.into(),
            validation_passed: passed,
            audit_root_hex: "00".repeat(32),
            proven_at: 1_700_000_000,
        }
    }

    fn binding() -> QualityBinding {
        QualityBinding {
            chain_id: 4663,
            verifying_contract: [0xAB; 20],
            call_id: 7,
            spec_id: [0x22; 32],
            deadline: 1_800_000_000,
        }
    }

    #[test]
    fn attest_round_trips_and_binds_to_the_attestor() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[9u8; 32]).unwrap();
        let signed = attestor
            .attest(&proof(&"11".repeat(32), true), &binding())
            .unwrap();
        assert!(signed.passed());
        assert!(signed.verify(&attestor.address(), 1_750_000_000).is_ok());
        assert_eq!(signed.recover_signer().unwrap(), attestor.address());
    }

    #[test]
    fn junk_verdict_signs_and_recovers() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[9u8; 32]).unwrap();
        let signed = attestor
            .attest(&proof(&"11".repeat(32), false), &binding())
            .unwrap();
        assert!(!signed.passed());
        assert!(signed.verify(&attestor.address(), 1_750_000_000).is_ok());
    }

    // The transform must feed the primitive the exact bytes the cross-language
    // golden vector pins: `covenant-attestation::quality::eip712_encoding_is_pinned`
    // and `SpendGrantEscrow.t.sol::test_Golden_QualityDigest` assert the same
    // digest. If the hex decode or field mapping drifts, this literal breaks.
    #[test]
    fn attest_reproduces_the_golden_digest() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[9u8; 32]).unwrap();
        let mut c0de = [0u8; 20];
        c0de[18] = 0xc0;
        c0de[19] = 0xde;
        let golden = QualityBinding {
            chain_id: 42_161,
            verifying_contract: c0de,
            call_id: 42,
            spec_id: [0x22; 32],
            deadline: 1_700_003_600,
        };
        let signed = attestor
            .attest(&proof(&"11".repeat(32), true), &golden)
            .unwrap();
        let expected =
            decode_result_hash("8dcd9e33febda1edb51bfa3a8aa86fcc7a50d330f1cd9fd50cace7ceb37ece45")
                .unwrap();
        assert_eq!(signed.digest(), expected);
    }

    #[test]
    fn attest_is_faithful_to_a_direct_construction() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[3u8; 32]).unwrap();
        let b = binding();
        let signed = attestor.attest(&proof(&"ab".repeat(32), true), &b).unwrap();
        let direct = QualityAttestation {
            chain_id: b.chain_id,
            verifying_contract: b.verifying_contract,
            call_id: b.call_id,
            result_hash: [0xAB; 32],
            passed: true,
            spec_id: b.spec_id,
            deadline: b.deadline,
        };
        assert_eq!(signed.digest(), direct.digest());
    }

    #[test]
    fn from_secret_bytes_is_deterministic() {
        let a = SpendGrantAttestor::from_secret_bytes(&[7u8; 32]).unwrap();
        let b = SpendGrantAttestor::from_secret_bytes(&[7u8; 32]).unwrap();
        assert_eq!(a.address(), b.address());
    }

    #[test]
    fn short_result_hash_is_refused() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[9u8; 32]).unwrap();
        let err = attestor
            .attest(&proof("deadbeef", true), &binding())
            .unwrap_err();
        assert!(matches!(err, SpendGrantError::ResultHashLen(8)));
    }

    #[test]
    fn non_hex_result_hash_is_refused() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[9u8; 32]).unwrap();
        let err = attestor
            .attest(&proof(&"zz".repeat(32), true), &binding())
            .unwrap_err();
        assert!(matches!(err, SpendGrantError::ResultHashHex));
    }

    #[test]
    fn zero_prefixed_hash_decodes() {
        assert_eq!(
            decode_result_hash(&format!("0x{}", "11".repeat(32))).unwrap(),
            [0x11; 32]
        );
    }

    #[test]
    fn address_decodes_with_and_without_prefix() {
        assert_eq!(
            decode_address(&format!("0x{}", "ee".repeat(20))).unwrap(),
            [0xEE; 20]
        );
        assert_eq!(decode_address(&"ab".repeat(20)).unwrap(), [0xAB; 20]);
        assert!(matches!(
            decode_address("0x1234").unwrap_err(),
            SpendGrantError::AddressLen(4)
        ));
        assert!(matches!(
            decode_address(&format!("0x{}", "zz".repeat(20))).unwrap_err(),
            SpendGrantError::AddressHex
        ));
    }

    // attest_call must bind the verdict to the *config's* deployment, taking
    // only the per-call fields from the caller. Same digest as a direct
    // QualityAttestation over chain 46630 + this contract proves chain_id and
    // verifying_contract came from the config, not the proof or the caller.
    #[test]
    fn config_attest_call_binds_its_deployment() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[5u8; 32]).unwrap();
        let addr = attestor.address();
        let cfg = SpendGrantConfig::new(attestor, 46630, [0xEE; 20]);
        let signed = cfg
            .attest_call(&proof(&"11".repeat(32), true), 9, [0x22; 32], 1_800_000_000)
            .unwrap();
        assert!(signed.passed());
        assert_eq!(signed.recover_signer().unwrap(), addr);
        assert_eq!(cfg.attestor_address(), addr);
        let direct = QualityAttestation {
            chain_id: 46630,
            verifying_contract: [0xEE; 20],
            call_id: 9,
            result_hash: [0x11; 32],
            passed: true,
            spec_id: [0x22; 32],
            deadline: 1_800_000_000,
        };
        assert_eq!(signed.digest(), direct.digest());
    }

    fn hexs(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn golden_sig() -> [u8; 65] {
        let mut s = [0xABu8; 65];
        s[64] = 0x1c;
        s
    }

    // Golden calldata from `cast calldata "releaseCallAttested(uint256,bytes32,
    // bytes32,uint256,bytes)" 7 0x11..11 0x22..22 1800000000 0xab..ab1c`. Pins
    // the selector and the head/tail ABI layout against foundry cross-tool.
    #[test]
    fn release_calldata_matches_cast_golden() {
        let cd = encode_attested_call(
            true,
            7,
            &[0x11; 32],
            &[0x22; 32],
            1_800_000_000,
            &golden_sig(),
        );
        assert_eq!(
            hexs(&cd),
            "27dcd974\
             0000000000000000000000000000000000000000000000000000000000000007\
             1111111111111111111111111111111111111111111111111111111111111111\
             2222222222222222222222222222222222222222222222222222222222222222\
             000000000000000000000000000000000000000000000000000000006b49d200\
             00000000000000000000000000000000000000000000000000000000000000a0\
             0000000000000000000000000000000000000000000000000000000000000041\
             abababababababababababababababababababababababababababababababab\
             abababababababababababababababababababababababababababababababab\
             1c00000000000000000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn refund_calldata_matches_cast_golden() {
        let cd = encode_attested_call(
            false,
            7,
            &[0x11; 32],
            &[0x22; 32],
            1_800_000_000,
            &golden_sig(),
        );
        assert_eq!(&hexs(&cd)[..8], "f920376f");
        // identical body after the selector — only the verdict-selected function differs
        assert_eq!(
            cd[4..],
            encode_attested_call(
                true,
                7,
                &[0x11; 32],
                &[0x22; 32],
                1_800_000_000,
                &golden_sig()
            )[4..]
        );
    }

    #[test]
    fn submission_targets_escrow_and_routes_by_verdict() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[5u8; 32]).unwrap();
        let cfg = SpendGrantConfig::new(attestor, 46630, [0xEE; 20]);
        let pass = cfg
            .attest_call(&proof(&"11".repeat(32), true), 7, [0x22; 32], 1_800_000_000)
            .unwrap();
        let (to, cd) = cfg.submission(&pass);
        assert_eq!(to, [0xEE; 20]);
        assert_eq!(cd[..4], RELEASE_SELECTOR);
        assert_eq!(cd.len(), 4 + 32 * 9);
        let junk = cfg
            .attest_call(
                &proof(&"11".repeat(32), false),
                7,
                [0x22; 32],
                1_800_000_000,
            )
            .unwrap();
        let (_, cd2) = cfg.submission(&junk);
        assert_eq!(cd2[..4], REFUND_SELECTOR);
    }

    #[test]
    fn settle_packages_release_for_a_pass() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[5u8; 32]).unwrap();
        let addr = attestor.address();
        let cfg = SpendGrantConfig::new(attestor, 46630, [0xEE; 20]);
        let s = cfg
            .settle(&proof(&"11".repeat(32), true), 7, [0x22; 32], 1_800_000_000)
            .unwrap();
        assert!(s.release);
        assert_eq!(s.to, [0xEE; 20]);
        assert_eq!(s.attestor, addr);
        assert_eq!(s.calldata[..4], RELEASE_SELECTOR);
        // exactly the attest_call -> submission it composes
        let signed = cfg
            .attest_call(&proof(&"11".repeat(32), true), 7, [0x22; 32], 1_800_000_000)
            .unwrap();
        assert_eq!((s.to, s.calldata), cfg.submission(&signed));
    }

    #[test]
    fn settle_packages_refund_for_junk() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[5u8; 32]).unwrap();
        let cfg = SpendGrantConfig::new(attestor, 46630, [0xEE; 20]);
        let s = cfg
            .settle(
                &proof(&"11".repeat(32), false),
                7,
                [0x22; 32],
                1_800_000_000,
            )
            .unwrap();
        assert!(!s.release);
        assert_eq!(s.calldata[..4], REFUND_SELECTOR);
    }

    #[test]
    fn settle_is_fail_closed_on_a_malformed_proof() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[5u8; 32]).unwrap();
        let cfg = SpendGrantConfig::new(attestor, 46630, [0xEE; 20]);
        let err = cfg
            .settle(&proof("deadbeef", true), 7, [0x22; 32], 1_800_000_000)
            .unwrap_err();
        assert!(matches!(err, SpendGrantError::ResultHashLen(8)));
    }

    // Golden calldata from `cast calldata "chargeCall(uint256,address,uint256,
    // uint256,uint256)" 1 0x8387c23137fc2FF5F392D2A106F5506E1dB2F085 5000000 7
    // 1800000000`. Pins the selector and the five-static-word layout cross-tool.
    #[test]
    fn charge_calldata_matches_cast_golden() {
        let provider = [
            0x83, 0x87, 0xc2, 0x31, 0x37, 0xfc, 0x2f, 0xf5, 0xf3, 0x92, 0xd2, 0xa1, 0x06, 0xf5,
            0x50, 0x6e, 0x1d, 0xb2, 0xf0, 0x85,
        ];
        let cd = encode_charge_call(1, &provider, 5_000_000, 7, 1_800_000_000);
        assert_eq!(
            hexs(&cd),
            "8610d014\
             0000000000000000000000000000000000000000000000000000000000000001\
             0000000000000000000000008387c23137fc2ff5f392d2a106f5506e1db2f085\
             00000000000000000000000000000000000000000000000000000000004c4b40\
             0000000000000000000000000000000000000000000000000000000000000007\
             000000000000000000000000000000000000000000000000000000006b49d200"
        );
    }

    #[test]
    fn config_has_no_submitter_by_default() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[5u8; 32]).unwrap();
        let cfg = SpendGrantConfig::new(attestor, 46630, [0xEE; 20]);
        assert!(!cfg.has_submitter());
        assert_eq!(cfg.submitter_address(), None);
    }

    #[test]
    fn with_submitter_binds_a_matching_chain() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[5u8; 32]).unwrap();
        let submitter = SpendGrantSubmitter::new("http://127.0.0.1:0", 46630, &[7u8; 32]).unwrap();
        let want = submitter.address();
        let cfg = SpendGrantConfig::new(attestor, 46630, [0xEE; 20])
            .with_submitter(submitter)
            .unwrap();
        assert!(cfg.has_submitter());
        assert_eq!(cfg.submitter_address(), Some(want));
        assert_eq!(
            want,
            EvmTxSigner::from_secret_bytes(&[7u8; 32])
                .unwrap()
                .address()
        );
    }

    #[test]
    fn with_submitter_rejects_a_chain_mismatch() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[5u8; 32]).unwrap();
        let submitter = SpendGrantSubmitter::new("http://127.0.0.1:0", 8453, &[7u8; 32]).unwrap();
        let result = SpendGrantConfig::new(attestor, 46630, [0xEE; 20]).with_submitter(submitter);
        assert!(matches!(
            result,
            Err(SpendGrantError::ChainMismatch {
                config: 46630,
                submitter: 8453
            })
        ));
    }

    #[tokio::test]
    async fn broadcast_without_a_submitter_is_refused() {
        let attestor = SpendGrantAttestor::from_secret_bytes(&[5u8; 32]).unwrap();
        let cfg = SpendGrantConfig::new(attestor, 46630, [0xEE; 20]);
        let charge = cfg
            .broadcast_charge(1, [0x83; 20], 5_000_000, 7, 1_800_000_000)
            .await
            .unwrap_err();
        assert!(matches!(charge, SpendGrantError::NoSubmitter));
        let settle = cfg
            .settle_and_broadcast(&proof(&"11".repeat(32), true), 7, [0x22; 32], 1_800_000_000)
            .await
            .unwrap_err();
        assert!(matches!(settle, SpendGrantError::NoSubmitter));
    }

    #[test]
    fn load_secret_reads_inline_and_file() {
        assert_eq!(
            load_secret(&format!("0x{}", "07".repeat(32))).unwrap(),
            [7u8; 32]
        );
        assert_eq!(load_secret(&"07".repeat(32)).unwrap(), [7u8; 32]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.hex");
        std::fs::write(&path, format!("0x{}\n", "07".repeat(32))).unwrap();
        assert_eq!(load_secret(path.to_str().unwrap()).unwrap(), [7u8; 32]);

        assert!(load_secret("0xdead").is_err());
        assert!(load_secret("/no/such/spend-grant/key").is_err());
    }
}
