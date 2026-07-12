//! Map a Covenant spend-scoped capability onto a Coinbase Spend Permission.
//!
//! A Spend Permission is Base's onchain capability-plus-budget primitive: a
//! Base Account (the `account`) authorizes a `spender` to pull up to
//! `allowance` of one `token` per rolling `period`, inside a `[start, end)`
//! window, keyed by a `salt`. The account approves it once with a signature
//! the `SpendPermissionManager` verifies through ERC-6492 / ERC-1271;
//! afterwards the spender calls `spend` and the manager meters each pull
//! against the allowance left in the current period. Covenant already issues
//! capabilities with that exact shape (a subject, a budget, a validity
//! window), so [`SpendPermission::from_capability`] projects one onto the
//! other and the daemon never hand-assembles the struct at the call site.
//!
//! The crate is EIP-712 by hand, mirroring [`covenant_attestation`]'s bond
//! receipt: the typehash and the manager's domain are pinned by value, and
//! [`SpendPermission::digest`] reproduces the exact 32 bytes the deployed
//! `SpendPermissionManager.getHash` returns. The tests pin that against the
//! live contract on Base mainnet and Base Sepolia, and pin every calldata
//! builder against `cast calldata`. No abi or provider crate is pulled in;
//! the surface is keccak plus fixed-width ABI words, so the blast radius
//! stays as small as the bond receipt's.
//!
//! The manager is a CREATE2 deployment at one address on every supported
//! chain, so [`SPEND_PERMISSION_MANAGER`] is chain-independent: only the
//! domain's `chainId` separates a Base mainnet permission from a Sepolia
//! one, which is what keeps a wrong-chain digest from verifying.

use covenant_types::Capability;
use serde_json::{Map, Value};
use sha3::{Digest, Keccak256};

/// The `SpendPermissionManager` address, identical on every chain it is
/// deployed to (a CREATE2 deployment). Pinned to its hex form by
/// `manager_address_is_pinned`.
// 0xf85210B21cC50302F477BA56686d2019dC9b67Ad
pub const SPEND_PERMISSION_MANAGER: [u8; 20] = [
    0xf8, 0x52, 0x10, 0xb2, 0x1c, 0xc5, 0x03, 0x02, 0xf4, 0x77, 0xba, 0x56, 0x68, 0x6d, 0x20, 0x19,
    0xdc, 0x9b, 0x67, 0xad,
];

/// The action a spend-scoped [`Capability`] must carry. The mapping refuses
/// anything else so a memory or tool grant is never silently reshaped into
/// an onchain spending authority.
pub const SPEND_ACTION: &str = "chain.spend";

/// `type(uint48).max`. A capability with no expiry maps its `end` here, the
/// far-future sentinel the manager treats as an open-ended window, and every
/// `uint48` field is bounded by it before it can be encoded.
pub const UINT48_MAX: u64 = 0xFFFF_FFFF_FFFF;

const DOMAIN_NAME: &str = "Spend Permission Manager";
const DOMAIN_VERSION: &str = "1";
const EIP712_DOMAIN_TYPE: &[u8] =
    b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
const SPEND_PERMISSION_TYPE: &[u8] = b"SpendPermission(address account,address spender,address token,uint160 allowance,uint48 period,uint48 start,uint48 end,uint256 salt,bytes extraData)";

// The `extraData` tail sits after the nine head words of the struct's ABI
// encoding, so its offset inside the tuple is a constant 9 * 32.
const EXTRA_DATA_TUPLE_OFFSET: u128 = 9 * 32;

// Function selectors, verified against `cast sig` and re-derived from the
// canonical signature strings by `selectors_match_keccak_of_signature`.
const SELECTOR_APPROVE_WITH_SIGNATURE: [u8; 4] = [0xb9, 0xff, 0xc8, 0xe1];
const SELECTOR_SPEND: [u8; 4] = [0x41, 0x5a, 0x97, 0x35];
const SELECTOR_REVOKE: [u8; 4] = [0x78, 0x79, 0x2f, 0x76];
const SELECTOR_GET_CURRENT_PERIOD: [u8; 4] = [0x2c, 0x18, 0xd4, 0x2e];
const SELECTOR_GET_HASH: [u8; 4] = [0xe0, 0xa0, 0x0b, 0x79];

/// The Base chains a Spend Permission can live on, each pinned to its Circle
/// USDC contract. The manager address is shared; only `chain_id` moves the
/// domain, so a Sepolia permission cannot verify against a mainnet domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseNetwork {
    Mainnet,
    Sepolia,
}

impl BaseNetwork {
    pub fn chain_id(self) -> u64 {
        match self {
            BaseNetwork::Mainnet => 8453,
            BaseNetwork::Sepolia => 84_532,
        }
    }

    /// The canonical Circle USDC contract on this chain, the token a spend
    /// budget is denominated in. Pinned by `usdc_addresses_match_pinned_hex`
    /// and shared with [`covenant_x402`]'s EVM signer.
    pub fn usdc(self) -> [u8; 20] {
        match self {
            // 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
            BaseNetwork::Mainnet => [
                0x83, 0x35, 0x89, 0xfc, 0xd6, 0xed, 0xb6, 0xe0, 0x8f, 0x4c, 0x7c, 0x32, 0xd4, 0xf7,
                0x1b, 0x54, 0xbd, 0xa0, 0x29, 0x13,
            ],
            // 0x036CbD53842c5426634e7929541eC2318f3dCF7e
            BaseNetwork::Sepolia => [
                0x03, 0x6c, 0xbd, 0x53, 0x84, 0x2c, 0x54, 0x26, 0x63, 0x4e, 0x79, 0x29, 0x54, 0x1e,
                0xc2, 0x31, 0x8f, 0x3d, 0xcf, 0x7e,
            ],
        }
    }
}

/// Every way the mapping or a decode can fail. Each variant names the exact
/// check so a reviewer can tell a malformed capability from an unsupported
/// one, and so a decode error is never confused with a policy error.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SpendPermissionError {
    #[error("capability action {0:?} is not a spend action (must start with {SPEND_ACTION:?})")]
    WrongAction(String),
    #[error("capability scope must be a JSON object")]
    ScopeNotObject,
    #[error("capability scope is missing required field {0:?}")]
    MissingScopeField(&'static str),
    #[error("capability scope field {0:?} is malformed for its type")]
    MalformedScopeField(&'static str),
    #[error("allowance must be greater than zero")]
    ZeroAllowance,
    #[error("period must be greater than zero seconds")]
    ZeroPeriod,
    #[error("{0} address is all-zero")]
    ZeroAddress(&'static str),
    #[error("{field} does not fit uint48: {value}")]
    Uint48Overflow { field: &'static str, value: u64 },
    #[error("validity window is inverted: start {start} >= end {end}")]
    WindowInverted { start: u64, end: u64 },
    #[error("malformed contract return: {0}")]
    MalformedReturn(String),
}

/// The EVM coordinates a Solana-native [`Capability`] cannot itself carry:
/// which Base Account holds the funds, which secp256k1 address may spend
/// them, which token, and the replay salt. The capability supplies the
/// policy (allowance, period, window); the binding supplies the chain
/// identities. `spender` is the agent's own secp256k1 address, the one it
/// signs `spend` calls from; `account` is the granter's Base Account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpendBinding {
    pub network: BaseNetwork,
    pub account: [u8; 20],
    pub spender: [u8; 20],
    pub token: [u8; 20],
    pub salt: [u8; 32],
}

/// The `SpendPermissionManager.SpendPermission` struct, field-for-field.
///
/// `network` is not part of the signed struct; it selects the domain the
/// digest is bound to (the struct hash is chain-independent). Construct it
/// through [`SpendPermission::from_capability`], which validates, or as a
/// literal followed by [`SpendPermission::validate`] before it is signed or
/// submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpendPermission {
    /// Selects the domain `chainId`; the manager address is the same on all.
    pub network: BaseNetwork,
    /// The Base Account (smart wallet) whose balance the spender draws from.
    pub account: [u8; 20],
    /// The address authorized to call `spend`; the agent's secp256k1 address.
    pub spender: [u8; 20],
    /// The ERC-20 the budget is denominated in (USDC in the Covenant path).
    pub token: [u8; 20],
    /// The per-period ceiling, in the token's smallest unit. Encoded as
    /// `uint160`; a `u128` covers every realistic USDC budget and matches
    /// how the bond receipt carries a USDC amount.
    pub allowance: u128,
    /// Rolling period length in seconds (`uint48`); the allowance refreshes
    /// each period.
    pub period: u64,
    /// Window start, unix seconds (`uint48`); the permission is inactive
    /// before it.
    pub start: u64,
    /// Window end, unix seconds (`uint48`), exclusive; the manager rejects a
    /// spend at or after it.
    pub end: u64,
    /// Uniqueness salt (`uint256`) that distinguishes two otherwise identical
    /// permissions.
    pub salt: [u8; 32],
    /// Manager extension data. Empty for a plain token budget; the
    /// permissioned / MagicSpend path is out of scope for this mapping.
    pub extra_data: Vec<u8>,
}

impl SpendPermission {
    /// Project a spend-scoped [`Capability`] onto a Spend Permission.
    ///
    /// The capability provides the policy: `scope.allowance` (a JSON number
    /// or decimal string, token smallest units), `scope.period_secs`, and an
    /// optional `scope.start_unix` (default 0, active from genesis). The
    /// window `end` comes from `capability.expires_at` (epoch milliseconds,
    /// floored to seconds); a perpetual capability maps to [`UINT48_MAX`].
    /// The [`SpendBinding`] provides the EVM identities and salt. The result
    /// is validated before it is returned, so a caller never receives a
    /// permission the manager would reject at `approve`.
    pub fn from_capability(
        capability: &Capability,
        binding: SpendBinding,
    ) -> Result<Self, SpendPermissionError> {
        if !capability.action.starts_with(SPEND_ACTION) {
            return Err(SpendPermissionError::WrongAction(capability.action.clone()));
        }
        let scope = capability
            .scope
            .as_object()
            .ok_or(SpendPermissionError::ScopeNotObject)?;

        let allowance = scope_u128(scope, "allowance")?;
        let period = scope_u64(scope, "period_secs")?;
        let start = scope_opt_u64(scope, "start_unix")?.unwrap_or(0);
        let end = match capability.expires_at {
            Some(ms) => ms / 1000,
            None => UINT48_MAX,
        };

        let permission = Self {
            network: binding.network,
            account: binding.account,
            spender: binding.spender,
            token: binding.token,
            allowance,
            period,
            start,
            end,
            salt: binding.salt,
            extra_data: Vec::new(),
        };
        permission.validate()?;
        Ok(permission)
    }

    /// Fail-closed field checks, mirroring the manager's `_approve` reverts
    /// (`ZeroToken`, `ZeroSpender`, `ZeroPeriod`, `ZeroAllowance`,
    /// `InvalidStartEnd`) plus a zero-account guard the contract leaves to
    /// signature recovery, and the `uint48` bounds the ABI encoding assumes.
    pub fn validate(&self) -> Result<(), SpendPermissionError> {
        if self.account == [0u8; 20] {
            return Err(SpendPermissionError::ZeroAddress("account"));
        }
        if self.spender == [0u8; 20] {
            return Err(SpendPermissionError::ZeroAddress("spender"));
        }
        if self.token == [0u8; 20] {
            return Err(SpendPermissionError::ZeroAddress("token"));
        }
        if self.allowance == 0 {
            return Err(SpendPermissionError::ZeroAllowance);
        }
        if self.period == 0 {
            return Err(SpendPermissionError::ZeroPeriod);
        }
        for (field, value) in [
            ("period", self.period),
            ("start", self.start),
            ("end", self.end),
        ] {
            if value > UINT48_MAX {
                return Err(SpendPermissionError::Uint48Overflow { field, value });
            }
        }
        if self.start >= self.end {
            return Err(SpendPermissionError::WindowInverted {
                start: self.start,
                end: self.end,
            });
        }
        Ok(())
    }

    /// `keccak256(abi.encode(domainTypeHash, keccak(name), keccak(version),
    /// chainId, verifyingContract))` for the manager's domain on this chain.
    pub fn domain_separator(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 5);
        buf.extend_from_slice(&keccak256(EIP712_DOMAIN_TYPE));
        buf.extend_from_slice(&keccak256(DOMAIN_NAME.as_bytes()));
        buf.extend_from_slice(&keccak256(DOMAIN_VERSION.as_bytes()));
        buf.extend_from_slice(&word_u256(u128::from(self.network.chain_id())));
        buf.extend_from_slice(&word_address(&SPEND_PERMISSION_MANAGER));
        keccak256(&buf)
    }

    /// The EIP-712 struct hash (`hashStruct`): `keccak256(abi.encode(
    /// typeHash, fields..., keccak(extraData)))`. `extraData` is a dynamic
    /// `bytes`, so it enters the hash as `keccak256` of its contents, not
    /// inline.
    pub fn hash(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 10);
        buf.extend_from_slice(&keccak256(SPEND_PERMISSION_TYPE));
        buf.extend_from_slice(&word_address(&self.account));
        buf.extend_from_slice(&word_address(&self.spender));
        buf.extend_from_slice(&word_address(&self.token));
        buf.extend_from_slice(&word_u256(self.allowance));
        buf.extend_from_slice(&word_u256(u128::from(self.period)));
        buf.extend_from_slice(&word_u256(u128::from(self.start)));
        buf.extend_from_slice(&word_u256(u128::from(self.end)));
        buf.extend_from_slice(&self.salt);
        buf.extend_from_slice(&keccak256(&self.extra_data));
        keccak256(&buf)
    }

    /// The EIP-712 signing digest: `keccak256(0x1901 ‖ domainSeparator ‖
    /// hashStruct)`. This is byte-for-byte what `SpendPermissionManager
    /// .getHash(spendPermission)` returns on the matching chain, so an
    /// approval signed over it verifies onchain, and a caller can confirm it
    /// against [`Self::get_hash_calldata`] before spending.
    pub fn digest(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(2 + 32 + 32);
        buf.push(0x19);
        buf.push(0x01);
        buf.extend_from_slice(&self.domain_separator());
        buf.extend_from_slice(&self.hash());
        keccak256(&buf)
    }

    /// Calldata for `approveWithSignature(SpendPermission, bytes)`: the
    /// account's approval, submittable by anyone. `signature` is the account
    /// wallet's ERC-6492 / ERC-1271 signature over [`Self::digest`].
    pub fn approve_with_signature_calldata(&self, signature: &[u8]) -> Vec<u8> {
        let tuple = self.encode_tuple();
        // Two dynamic args: the permission tuple then the signature bytes.
        // The tuple starts right after the two head words; the signature
        // starts right after the tuple.
        let signature_offset = 64 + tuple.len() as u128;
        let signature_word = encode_bytes(signature);
        let mut out = Vec::with_capacity(4 + 64 + tuple.len() + signature_word.len());
        out.extend_from_slice(&SELECTOR_APPROVE_WITH_SIGNATURE);
        out.extend_from_slice(&word_u256(64));
        out.extend_from_slice(&word_u256(signature_offset));
        out.extend_from_slice(&tuple);
        out.extend_from_slice(&signature_word);
        out
    }

    /// Calldata for `spend(SpendPermission, uint160 value)`, the call the
    /// spender makes to pull `value` of the token against the allowance.
    pub fn spend_calldata(&self, value: u128) -> Vec<u8> {
        let tuple = self.encode_tuple();
        let mut out = Vec::with_capacity(4 + 64 + tuple.len());
        out.extend_from_slice(&SELECTOR_SPEND);
        // arg0 (tuple) is dynamic so its head word is an offset; arg1
        // (uint160 value) is static so its value sits inline in the head.
        out.extend_from_slice(&word_u256(64));
        out.extend_from_slice(&word_u256(value));
        out.extend_from_slice(&tuple);
        out
    }

    /// Calldata for `revoke(SpendPermission)`. The manager restricts this to
    /// the permission's `account`, so it is the account's own kill switch.
    pub fn revoke_calldata(&self) -> Vec<u8> {
        self.encode_single_arg_call(SELECTOR_REVOKE)
    }

    /// Calldata for `getCurrentPeriod(SpendPermission) -> PeriodSpend`, the
    /// status accessor: the active period's window and cumulative spend.
    /// (The contract exposes no `getPermissionStatus`; this is the primitive
    /// it offers, and `allowance - spend` is the remaining budget.) Decode
    /// the return with [`decode_current_period`].
    pub fn get_current_period_calldata(&self) -> Vec<u8> {
        self.encode_single_arg_call(SELECTOR_GET_CURRENT_PERIOD)
    }

    /// Calldata for `getHash(SpendPermission) -> bytes32`, so a caller can
    /// cross-check [`Self::digest`] against the deployed contract. Decode the
    /// return with [`decode_hash`].
    pub fn get_hash_calldata(&self) -> Vec<u8> {
        self.encode_single_arg_call(SELECTOR_GET_HASH)
    }

    /// The ABI encoding of the `SpendPermission` tuple: nine head words then
    /// the `extraData` tail. Because the struct holds a dynamic member, its
    /// ninth head word is the offset to that tail, a constant here since the
    /// head is always nine words.
    fn encode_tuple(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 * 10 + self.extra_data.len());
        out.extend_from_slice(&word_address(&self.account));
        out.extend_from_slice(&word_address(&self.spender));
        out.extend_from_slice(&word_address(&self.token));
        out.extend_from_slice(&word_u256(self.allowance));
        out.extend_from_slice(&word_u256(u128::from(self.period)));
        out.extend_from_slice(&word_u256(u128::from(self.start)));
        out.extend_from_slice(&word_u256(u128::from(self.end)));
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&word_u256(EXTRA_DATA_TUPLE_OFFSET));
        out.extend_from_slice(&encode_bytes(&self.extra_data));
        out
    }

    /// A call whose only argument is the (dynamic) permission tuple: selector
    /// then the offset word (a single arg starts one word in) then the tuple.
    fn encode_single_arg_call(&self, selector: [u8; 4]) -> Vec<u8> {
        let tuple = self.encode_tuple();
        let mut out = Vec::with_capacity(4 + 32 + tuple.len());
        out.extend_from_slice(&selector);
        out.extend_from_slice(&word_u256(32));
        out.extend_from_slice(&tuple);
        out
    }
}

/// The manager's `PeriodSpend`: the active period's window and the token
/// amount already spent in it. `getCurrentPeriod` returns this static
/// three-word struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodSpend {
    /// Period start, unix seconds (`uint48`).
    pub start: u64,
    /// Period end, unix seconds (`uint48`).
    pub end: u64,
    /// Token already spent this period (`uint160`).
    pub spend: u128,
}

impl PeriodSpend {
    /// Budget left in the current period against a permission's allowance.
    /// Saturates at zero so an over-count never underflows.
    pub fn remaining(&self, allowance: u128) -> u128 {
        allowance.saturating_sub(self.spend)
    }
}

/// Decode a `getCurrentPeriod` return (three 32-byte words: `uint48 start`,
/// `uint48 end`, `uint160 spend`). Errors rather than truncates if a word
/// carries more than its Rust type holds.
pub fn decode_current_period(ret: &[u8]) -> Result<PeriodSpend, SpendPermissionError> {
    if ret.len() != 96 {
        return Err(SpendPermissionError::MalformedReturn(format!(
            "getCurrentPeriod returns three 32-byte words (96 bytes), got {}",
            ret.len()
        )));
    }
    Ok(PeriodSpend {
        start: word_to_u64(&ret[0..32], "start")?,
        end: word_to_u64(&ret[32..64], "end")?,
        spend: word_to_u128(&ret[64..96], "spend")?,
    })
}

/// Decode a `getHash` return, the 32-byte digest. Equals [`SpendPermission
/// ::digest`] for the same permission on the same chain.
pub fn decode_hash(ret: &[u8]) -> Result<[u8; 32], SpendPermissionError> {
    ret.try_into().map_err(|_| {
        SpendPermissionError::MalformedReturn(format!(
            "getHash returns one 32-byte word, got {} bytes",
            ret.len()
        ))
    })
}

fn scope_u128(
    scope: &Map<String, Value>,
    field: &'static str,
) -> Result<u128, SpendPermissionError> {
    match scope.get(field) {
        None => Err(SpendPermissionError::MissingScopeField(field)),
        // A number covers budgets up to u64; a decimal string carries larger
        // allowances without losing precision to a JSON float.
        Some(Value::Number(n)) => n
            .as_u64()
            .map(u128::from)
            .ok_or(SpendPermissionError::MalformedScopeField(field)),
        Some(Value::String(s)) => s
            .parse::<u128>()
            .map_err(|_| SpendPermissionError::MalformedScopeField(field)),
        Some(_) => Err(SpendPermissionError::MalformedScopeField(field)),
    }
}

fn scope_u64(scope: &Map<String, Value>, field: &'static str) -> Result<u64, SpendPermissionError> {
    scope_opt_u64(scope, field)?.ok_or(SpendPermissionError::MissingScopeField(field))
}

fn scope_opt_u64(
    scope: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<u64>, SpendPermissionError> {
    match scope.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .map(Some)
            .ok_or(SpendPermissionError::MalformedScopeField(field)),
        Some(_) => Err(SpendPermissionError::MalformedScopeField(field)),
    }
}

fn keccak256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(bytes));
    out
}

/// A `uint256` ABI word from a `u128`: big-endian, right-aligned in 32
/// bytes. A `uint160` or narrower value is the same encoding with more
/// leading zeros, so this serves every fixed-width numeric field here.
fn word_u256(value: u128) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[16..].copy_from_slice(&value.to_be_bytes());
    word
}

/// An `address` ABI word: the 20 address bytes right-aligned in 32.
fn word_address(address: &[u8; 20]) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(address);
    word
}

/// ABI `bytes`: a length word then the contents right-padded to a 32-byte
/// boundary. Empty bytes encode to the single zero length word.
fn encode_bytes(data: &[u8]) -> Vec<u8> {
    let padded = data.len().div_ceil(32) * 32;
    let mut out = Vec::with_capacity(32 + padded);
    out.extend_from_slice(&word_u256(data.len() as u128));
    out.extend_from_slice(data);
    out.resize(32 + padded, 0);
    out
}

/// Read a `uint48`-or-narrower ABI word as `u64`, erroring if the high 24
/// bytes carry anything (i.e. the value does not fit `u64`).
fn word_to_u64(word: &[u8], field: &'static str) -> Result<u64, SpendPermissionError> {
    if word[..24].iter().any(|&b| b != 0) {
        return Err(SpendPermissionError::MalformedReturn(format!(
            "{field} exceeds u64"
        )));
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&word[24..32]);
    Ok(u64::from_be_bytes(buf))
}

/// Read a `uint160`-or-narrower ABI word as `u128`, erroring if the high 16
/// bytes carry anything (i.e. the value does not fit `u128`).
fn word_to_u128(word: &[u8], field: &'static str) -> Result<u128, SpendPermissionError> {
    if word[..16].iter().any(|&b| b != 0) {
        return Err(SpendPermissionError::MalformedReturn(format!(
            "{field} exceeds u128"
        )));
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&word[16..32]);
    Ok(u128::from_be_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_types::AgentId;

    // Live anchors: `SpendPermissionManager.getHash` on the deployed contract
    // (0xf852...67ad) for the SAMPLE permission below, read over RPC. If the
    // typehash, field order, word packing, domain, or 0x1901 assembly drift,
    // digest() stops equalling the chain.
    const ONCHAIN_GETHASH_MAINNET: &str =
        "593e024ebff7c7f80ddd362871a24272ceb28c8d754af58ab79f276dc62cb564";
    const ONCHAIN_GETHASH_SEPOLIA: &str =
        "10a540108d419bd010995ae245fe5e4ab18b6f349c7febcdaaf7b3103333c08b";

    // The reference calldata `cast calldata` emits for the SAMPLE permission,
    // pinning each builder to the canonical ABI encoding.
    const CALLDATA_APPROVE_WITH_SIG: &str = "b9ffc8e10000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000018000000000000000000000000011111111111111111111111111111111111111110000000000000000000000002222222222222222222222222222222222222222000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda0291300000000000000000000000000000000000000000000000000000000000f42400000000000000000000000000000000000000000000000000000000000015180000000000000000000000000000000000000000000000000000000006553f100000000000000000000000000000000000000000000000000000000006b49d2000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000012000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000041aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa00000000000000000000000000000000000000000000000000000000000000";
    const CALLDATA_SPEND: &str = "415a97350000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000007a12000000000000000000000000011111111111111111111111111111111111111110000000000000000000000002222222222222222222222222222222222222222000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda0291300000000000000000000000000000000000000000000000000000000000f42400000000000000000000000000000000000000000000000000000000000015180000000000000000000000000000000000000000000000000000000006553f100000000000000000000000000000000000000000000000000000000006b49d200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001200000000000000000000000000000000000000000000000000000000000000000";
    const CALLDATA_GET_CURRENT_PERIOD: &str = "2c18d42e000000000000000000000000000000000000000000000000000000000000002000000000000000000000000011111111111111111111111111111111111111110000000000000000000000002222222222222222222222222222222222222222000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda0291300000000000000000000000000000000000000000000000000000000000f42400000000000000000000000000000000000000000000000000000000000015180000000000000000000000000000000000000000000000000000000006553f100000000000000000000000000000000000000000000000000000000006b49d200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001200000000000000000000000000000000000000000000000000000000000000000";
    const CALLDATA_GET_HASH: &str = "e0a00b79000000000000000000000000000000000000000000000000000000000000002000000000000000000000000011111111111111111111111111111111111111110000000000000000000000002222222222222222222222222222222222222222000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda0291300000000000000000000000000000000000000000000000000000000000f42400000000000000000000000000000000000000000000000000000000000015180000000000000000000000000000000000000000000000000000000006553f100000000000000000000000000000000000000000000000000000000006b49d200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001200000000000000000000000000000000000000000000000000000000000000000";
    const CALLDATA_REVOKE: &str = "78792f76000000000000000000000000000000000000000000000000000000000000002000000000000000000000000011111111111111111111111111111111111111110000000000000000000000002222222222222222222222222222222222222222000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda0291300000000000000000000000000000000000000000000000000000000000f42400000000000000000000000000000000000000000000000000000000000015180000000000000000000000000000000000000000000000000000000006553f100000000000000000000000000000000000000000000000000000000006b49d200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001200000000000000000000000000000000000000000000000000000000000000000";

    fn addr(byte: u8) -> [u8; 20] {
        [byte; 20]
    }

    // The vector the live getHash and cast calldata above were taken from:
    // account 0x11..11, spender 0x22..22, token USDC-per-network, allowance
    // 1_000_000, period 86_400, start 1_700_000_000, end 1_800_000_000,
    // salt 0, empty extraData.
    fn sample(network: BaseNetwork) -> SpendPermission {
        SpendPermission {
            network,
            account: addr(0x11),
            spender: addr(0x22),
            token: network.usdc(),
            allowance: 1_000_000,
            period: 86_400,
            start: 1_700_000_000,
            end: 1_800_000_000,
            salt: [0u8; 32],
            extra_data: Vec::new(),
        }
    }

    #[test]
    fn manager_address_is_pinned() {
        assert_eq!(
            hex_encode(&SPEND_PERMISSION_MANAGER),
            "f85210b21cc50302f477ba56686d2019dc9b67ad"
        );
    }

    #[test]
    fn usdc_addresses_match_pinned_hex() {
        assert_eq!(
            hex_encode(&BaseNetwork::Mainnet.usdc()),
            "833589fcd6edb6e08f4c7c32d4f71b54bda02913"
        );
        assert_eq!(
            hex_encode(&BaseNetwork::Sepolia.usdc()),
            "036cbd53842c5426634e7929541ec2318f3dcf7e"
        );
        assert_eq!(BaseNetwork::Mainnet.chain_id(), 8453);
        assert_eq!(BaseNetwork::Sepolia.chain_id(), 84_532);
    }

    #[test]
    fn typehash_matches_keccak_of_type_string() {
        // The SPEND_PERMISSION_TYPEHASH the contract computes, from `cast
        // keccak` over the canonical type string.
        assert_eq!(
            hex_encode(&keccak256(SPEND_PERMISSION_TYPE)),
            "c9fa0f0252014cf89ab0539e3bb3adcb76f93e6bb6494e8cc61c14e2761ee2e4"
        );
        // The standard EIP-712 domain typehash, shared with every EIP-712
        // signer in the workspace.
        assert_eq!(
            hex_encode(&keccak256(EIP712_DOMAIN_TYPE)),
            "8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f"
        );
    }

    #[test]
    fn domain_separator_is_pinned_per_chain() {
        // Independently computed as keccak(abi.encode(domainTypeHash,
        // keccak(name), keccak(version), chainId, manager)) via cast.
        assert_eq!(
            hex_encode(&sample(BaseNetwork::Mainnet).domain_separator()),
            "3187a50f7c68cc46a370ce9830bcd5da649e5f41d60dd75ba256ced435601516"
        );
        assert_eq!(
            hex_encode(&sample(BaseNetwork::Sepolia).domain_separator()),
            "883f123e973ff58f2ae32b4abb6c220919b25b4147bb3c39287c6323518fd11b"
        );
        // The domain is the only thing that moves between chains.
        assert_ne!(
            sample(BaseNetwork::Mainnet).domain_separator(),
            sample(BaseNetwork::Sepolia).domain_separator()
        );
    }

    #[test]
    fn digest_reproduces_onchain_gethash() {
        // The strongest anchor: our locally computed EIP-712 digest equals
        // the 32 bytes the deployed SpendPermissionManager.getHash returns
        // for the same permission, on each chain.
        assert_eq!(
            hex_encode(&sample(BaseNetwork::Mainnet).digest()),
            ONCHAIN_GETHASH_MAINNET
        );
        assert_eq!(
            hex_encode(&sample(BaseNetwork::Sepolia).digest()),
            ONCHAIN_GETHASH_SEPOLIA
        );
    }

    #[test]
    fn digest_is_the_1901_frame_over_hash() {
        // digest == keccak(0x1901 ‖ domainSeparator ‖ hashStruct), and the
        // struct hash is not itself the digest.
        let p = sample(BaseNetwork::Mainnet);
        let mut framed = vec![0x19, 0x01];
        framed.extend_from_slice(&p.domain_separator());
        framed.extend_from_slice(&p.hash());
        assert_eq!(keccak256(&framed), p.digest());
        assert_ne!(p.hash(), p.digest());
    }

    #[test]
    fn struct_hash_is_chain_independent() {
        // hashStruct carries no chainId; only the domain (hence the digest)
        // is chain-bound. Same fields, same token -> same struct hash.
        let mut mainnet = sample(BaseNetwork::Mainnet);
        let mut sepolia = sample(BaseNetwork::Sepolia);
        mainnet.token = addr(0x33);
        sepolia.token = addr(0x33);
        sepolia.network = BaseNetwork::Sepolia;
        assert_eq!(mainnet.hash(), sepolia.hash());
        assert_ne!(mainnet.digest(), sepolia.digest());
    }

    #[test]
    fn selectors_match_keccak_of_signature() {
        let cases: [(&[u8], [u8; 4]); 5] = [
            (b"approveWithSignature((address,address,address,uint160,uint48,uint48,uint48,uint256,bytes),bytes)", SELECTOR_APPROVE_WITH_SIGNATURE),
            (b"spend((address,address,address,uint160,uint48,uint48,uint48,uint256,bytes),uint160)", SELECTOR_SPEND),
            (b"revoke((address,address,address,uint160,uint48,uint48,uint48,uint256,bytes))", SELECTOR_REVOKE),
            (b"getCurrentPeriod((address,address,address,uint160,uint48,uint48,uint48,uint256,bytes))", SELECTOR_GET_CURRENT_PERIOD),
            (b"getHash((address,address,address,uint160,uint48,uint48,uint48,uint256,bytes))", SELECTOR_GET_HASH),
        ];
        for (signature, selector) in cases {
            assert_eq!(&keccak256(signature)[..4], &selector);
        }
    }

    #[test]
    fn approve_with_signature_calldata_matches_cast() {
        let signature = [0xaau8; 65];
        assert_eq!(
            hex_encode(&sample(BaseNetwork::Mainnet).approve_with_signature_calldata(&signature)),
            CALLDATA_APPROVE_WITH_SIG
        );
    }

    #[test]
    fn spend_calldata_matches_cast() {
        assert_eq!(
            hex_encode(&sample(BaseNetwork::Mainnet).spend_calldata(500_000)),
            CALLDATA_SPEND
        );
    }

    #[test]
    fn single_arg_calldata_matches_cast() {
        let p = sample(BaseNetwork::Mainnet);
        assert_eq!(
            hex_encode(&p.get_current_period_calldata()),
            CALLDATA_GET_CURRENT_PERIOD
        );
        assert_eq!(hex_encode(&p.get_hash_calldata()), CALLDATA_GET_HASH);
        assert_eq!(hex_encode(&p.revoke_calldata()), CALLDATA_REVOKE);
    }

    #[test]
    fn from_capability_maps_the_sample_vector() {
        // A spend-scoped capability plus a binding reproduces the sample
        // permission exactly, and therefore its onchain digest.
        let capability = Capability {
            subject: AgentId::new("agent@local", [7u8; 32]),
            action: SPEND_ACTION.to_string(),
            scope: serde_json::json!({
                "version": 1,
                "allowance": "1000000",
                "period_secs": 86_400,
                "start_unix": 1_700_000_000u64,
            }),
            granted_by: AgentId::new("granter@local", [9u8; 32]),
            // Epoch milliseconds: 1_800_000_000 seconds.
            expires_at: Some(1_800_000_000_000),
        };
        let binding = SpendBinding {
            network: BaseNetwork::Mainnet,
            account: addr(0x11),
            spender: addr(0x22),
            token: BaseNetwork::Mainnet.usdc(),
            salt: [0u8; 32],
        };
        let mapped = SpendPermission::from_capability(&capability, binding).unwrap();
        assert_eq!(mapped, sample(BaseNetwork::Mainnet));
        assert_eq!(hex_encode(&mapped.digest()), ONCHAIN_GETHASH_MAINNET);
    }

    #[test]
    fn from_capability_accepts_numeric_allowance_and_defaults_start() {
        let capability = Capability {
            subject: AgentId::new("agent@local", [7u8; 32]),
            action: "chain.spend.usdc".to_string(),
            scope: serde_json::json!({ "allowance": 5_000u64, "period_secs": 3_600 }),
            granted_by: AgentId::new("granter@local", [9u8; 32]),
            expires_at: Some(2_000_000_000_000),
        };
        let binding = SpendBinding {
            network: BaseNetwork::Sepolia,
            account: addr(0x11),
            spender: addr(0x22),
            token: BaseNetwork::Sepolia.usdc(),
            salt: [0xEE; 32],
        };
        let mapped = SpendPermission::from_capability(&capability, binding).unwrap();
        assert_eq!(mapped.allowance, 5_000);
        assert_eq!(mapped.start, 0);
        assert_eq!(mapped.end, 2_000_000_000);
        assert_eq!(mapped.salt, [0xEE; 32]);
    }

    #[test]
    fn perpetual_capability_maps_end_to_uint48_max() {
        let capability = Capability {
            subject: AgentId::new("agent@local", [7u8; 32]),
            action: SPEND_ACTION.to_string(),
            scope: serde_json::json!({ "allowance": 1u64, "period_secs": 1 }),
            granted_by: AgentId::new("granter@local", [9u8; 32]),
            expires_at: None,
        };
        let binding = SpendBinding {
            network: BaseNetwork::Mainnet,
            account: addr(0x11),
            spender: addr(0x22),
            token: BaseNetwork::Mainnet.usdc(),
            salt: [0u8; 32],
        };
        let mapped = SpendPermission::from_capability(&capability, binding).unwrap();
        assert_eq!(mapped.end, UINT48_MAX);
        assert!(mapped.validate().is_ok());
    }

    #[test]
    fn from_capability_rejects_non_spend_action() {
        let capability = Capability {
            subject: AgentId::new("agent@local", [7u8; 32]),
            action: "memory.write".to_string(),
            scope: serde_json::json!({ "allowance": 1u64, "period_secs": 1 }),
            granted_by: AgentId::new("granter@local", [9u8; 32]),
            expires_at: Some(1_800_000_000_000),
        };
        let binding = SpendBinding {
            network: BaseNetwork::Mainnet,
            account: addr(0x11),
            spender: addr(0x22),
            token: BaseNetwork::Mainnet.usdc(),
            salt: [0u8; 32],
        };
        assert_eq!(
            SpendPermission::from_capability(&capability, binding),
            Err(SpendPermissionError::WrongAction("memory.write".into()))
        );
    }

    #[test]
    fn from_capability_rejects_bad_scopes() {
        let binding = SpendBinding {
            network: BaseNetwork::Mainnet,
            account: addr(0x11),
            spender: addr(0x22),
            token: BaseNetwork::Mainnet.usdc(),
            salt: [0u8; 32],
        };
        let cap = |scope: Value, expires_at: Option<u64>| Capability {
            subject: AgentId::new("agent@local", [7u8; 32]),
            action: SPEND_ACTION.to_string(),
            scope,
            granted_by: AgentId::new("granter@local", [9u8; 32]),
            expires_at,
        };

        // Missing allowance / period.
        assert_eq!(
            SpendPermission::from_capability(
                &cap(
                    serde_json::json!({ "period_secs": 1 }),
                    Some(1_800_000_000_000)
                ),
                binding
            ),
            Err(SpendPermissionError::MissingScopeField("allowance"))
        );
        assert_eq!(
            SpendPermission::from_capability(
                &cap(
                    serde_json::json!({ "allowance": 1u64 }),
                    Some(1_800_000_000_000)
                ),
                binding
            ),
            Err(SpendPermissionError::MissingScopeField("period_secs"))
        );
        // Zero allowance / zero period reach validate().
        assert_eq!(
            SpendPermission::from_capability(
                &cap(
                    serde_json::json!({ "allowance": 0u64, "period_secs": 1 }),
                    Some(1_800_000_000_000)
                ),
                binding
            ),
            Err(SpendPermissionError::ZeroAllowance)
        );
        assert_eq!(
            SpendPermission::from_capability(
                &cap(
                    serde_json::json!({ "allowance": 1u64, "period_secs": 0 }),
                    Some(1_800_000_000_000)
                ),
                binding
            ),
            Err(SpendPermissionError::ZeroPeriod)
        );
        // Non-object scope, and a malformed allowance string.
        assert_eq!(
            SpendPermission::from_capability(
                &cap(serde_json::json!("nope"), Some(1_800_000_000_000)),
                binding
            ),
            Err(SpendPermissionError::ScopeNotObject)
        );
        assert_eq!(
            SpendPermission::from_capability(
                &cap(
                    serde_json::json!({ "allowance": "0x10", "period_secs": 1 }),
                    Some(1_800_000_000_000)
                ),
                binding
            ),
            Err(SpendPermissionError::MalformedScopeField("allowance"))
        );
    }

    #[test]
    fn expiry_beyond_uint48_is_refused() {
        // An expiry in milliseconds whose second value overflows uint48 must
        // not be silently truncated into the encoding.
        let capability = Capability {
            subject: AgentId::new("agent@local", [7u8; 32]),
            action: SPEND_ACTION.to_string(),
            scope: serde_json::json!({ "allowance": 1u64, "period_secs": 1 }),
            granted_by: AgentId::new("granter@local", [9u8; 32]),
            expires_at: Some((UINT48_MAX + 1) * 1000),
        };
        let binding = SpendBinding {
            network: BaseNetwork::Mainnet,
            account: addr(0x11),
            spender: addr(0x22),
            token: BaseNetwork::Mainnet.usdc(),
            salt: [0u8; 32],
        };
        assert_eq!(
            SpendPermission::from_capability(&capability, binding),
            Err(SpendPermissionError::Uint48Overflow {
                field: "end",
                value: UINT48_MAX + 1
            })
        );
    }

    #[test]
    fn validate_rejects_zero_addresses_and_inverted_window() {
        let mut p = sample(BaseNetwork::Mainnet);
        p.account = [0u8; 20];
        assert_eq!(
            p.validate(),
            Err(SpendPermissionError::ZeroAddress("account"))
        );

        let mut p = sample(BaseNetwork::Mainnet);
        p.spender = [0u8; 20];
        assert_eq!(
            p.validate(),
            Err(SpendPermissionError::ZeroAddress("spender"))
        );

        let mut p = sample(BaseNetwork::Mainnet);
        p.token = [0u8; 20];
        assert_eq!(
            p.validate(),
            Err(SpendPermissionError::ZeroAddress("token"))
        );

        let mut p = sample(BaseNetwork::Mainnet);
        p.end = p.start;
        assert_eq!(
            p.validate(),
            Err(SpendPermissionError::WindowInverted {
                start: p.start,
                end: p.start
            })
        );
    }

    #[test]
    fn decode_current_period_round_trips() {
        // cast abi-encode "f(uint48,uint48,uint160)" 1700000000 1700086400 250000
        let ret = hex_decode("000000000000000000000000000000000000000000000000000000006553f1000000000000000000000000000000000000000000000000000000000065554280000000000000000000000000000000000000000000000000000000000003d090");
        let period = decode_current_period(&ret).unwrap();
        assert_eq!(period.start, 1_700_000_000);
        assert_eq!(period.end, 1_700_086_400);
        assert_eq!(period.spend, 250_000);
        assert_eq!(period.remaining(1_000_000), 750_000);
        // Saturates rather than underflowing on an over-count.
        assert_eq!(period.remaining(100_000), 0);
    }

    #[test]
    fn decode_current_period_rejects_bad_length_and_overflow() {
        assert!(matches!(
            decode_current_period(&[0u8; 64]),
            Err(SpendPermissionError::MalformedReturn(_))
        ));
        // A `spend` word whose high 16 bytes are set does not fit u128.
        let mut ret = [0u8; 96];
        ret[64] = 0x01;
        assert!(matches!(
            decode_current_period(&ret),
            Err(SpendPermissionError::MalformedReturn(_))
        ));
    }

    #[test]
    fn decode_hash_reads_one_word() {
        let ret = hex_decode(ONCHAIN_GETHASH_MAINNET);
        assert_eq!(
            decode_hash(&ret).unwrap(),
            sample(BaseNetwork::Mainnet).digest()
        );
        assert!(matches!(
            decode_hash(&[0u8; 31]),
            Err(SpendPermissionError::MalformedReturn(_))
        ));
    }

    #[test]
    fn empty_extra_data_encodes_to_one_zero_word() {
        assert_eq!(encode_bytes(&[]), vec![0u8; 32]);
        // 65 bytes -> length word plus three padded words.
        assert_eq!(encode_bytes(&[0xaa; 65]).len(), 32 + 96);
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";

    fn hex_encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }

    fn hex_decode(value: &str) -> Vec<u8> {
        let body = value.strip_prefix("0x").unwrap_or(value);
        body.as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let hi = (pair[0] as char).to_digit(16).unwrap() as u8;
                let lo = (pair[1] as char).to_digit(16).unwrap() as u8;
                (hi << 4) | lo
            })
            .collect()
    }
}
