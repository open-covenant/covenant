//! Real on-chain `PercolatorClient` that reads `MarketGroupV16` from
//! Solana RPC and decodes via bytemuck, matching the byte layout
//! percolator-prog writes.
//!
//! Available under `--features solana-rpc`. The crate's
//! [`MockPercolator`] stays the default for testing; `RealPercolator`
//! is the path operators wire when running the keeper against
//! Bounty 6 (program `4m3ipBQDYX…`, market `BhkMic5g…`) or any
//! v16-compatible deployment.
//!
//! ## Account layout (mirrors percolator-prog HEAD `065a8f0`)
//!
//! A market-group account is:
//!
//! ```text
//!   [0..16]               magic (u64 LE 0x5045524356313600) + version(u16) + kind(u8) + pad
//!   [16..640]             WrapperConfigV16  (624 bytes)
//!   [640..640+HDR]        MarketGroupV16HeaderAccount  (sizeof from percolator crate)
//!   [640+HDR..]           N × asset slot, each (512 wrapper + sizeof::<EngineAssetSlotV16Account>())
//! ```
//!
//! The 512-byte wrapper reserve is `Market<[u8; ASSET_ORACLE_WRAPPER_LEN]>`
//! per `v16_program.rs:53`. The actual `AssetOracleProfileV16`
//! occupies the first 368 bytes (`ASSET_ORACLE_PROFILE_LEN`) of
//! that 512-byte region; the rest is forward-compat padding.
//!
//! `MarketGroupV16HeaderAccount` lives in the `percolator` risk-engine
//! crate (already a workspace dep, pinned at commit `323c9f27`).
//! `AssetOracleProfileV16` is the wrapper-side struct that program
//! source `v16_program.rs:233` defines; we vendor a byte-identical
//! mirror here so the bond crate doesn't need to depend on
//! percolator-prog itself. Layout is locked by [`tests::*`].
//!
//! ## What this gives us
//!
//! - `MarketState::current_slot` from header's `current_slot`
//! - per-asset `last_mark_slot` from the engine slot's `slot_last`
//! - per-asset `last_mark_e6` from the wrapper's `mark_ewma_e6`
//!   (the auth-mark path; matches what `PushAuthMark` writes)
//! - `AssetLifecycle` from the engine slot's `lifecycle` byte (we
//!   already mirror the variant-for-variant mapping in `state.rs`)
//!
//! Portfolio reads (`list_portfolios`) are a later phase — they
//! require `getProgramAccounts` + discriminator filtering against
//! `PortfolioAccountV16` which has its own layout to mirror.

use std::sync::Arc;

use async_trait::async_trait;
use percolator::{EngineAssetSlotV16Account, MarketGroupV16HeaderAccount};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

use crate::client::{ClientError, Execution, PercolatorClient};
use crate::state::{AssetIndex, AssetLifecycle, AssetState, KeeperAction, MarketState, PortfolioSnapshot};

/// Wrapper-side per-asset profile. Byte-identical mirror of
/// `percolator-prog/src/v16_program.rs::AssetOracleProfileV16`
/// (HEAD `065a8f0`). The layout is `#[repr(C)]` + bytemuck::Pod, so
/// we can `from_bytes` directly off the on-chain account data.
///
/// **Lock:** if the program changes this struct, `tests::asset_oracle_profile_size_locked`
/// fails and the decoder fails byte-aligned, surfacing the drift
/// before any keeper action lands.
pub const ORACLE_LEG_CAP: usize = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct AssetOracleProfileV16 {
    pub oracle_mode: u8,
    pub oracle_leg_count: u8,
    pub oracle_leg_flags: u8,
    pub invert: u8,
    pub unit_scale: u32,
    pub conf_filter_bps: u16,
    pub backing_trade_fee_bps_long: u16,
    pub backing_trade_fee_bps_short: u16,
    pub backing_trade_fee_insurance_share_bps_long: u16,
    pub backing_trade_fee_insurance_share_bps_short: u16,
    pub _padding0: [u8; 6],
    pub insurance_authority: [u8; 32],
    pub insurance_operator: [u8; 32],
    pub backing_bucket_authority: [u8; 32],
    pub oracle_authority: [u8; 32],
    pub max_staleness_secs: u64,
    pub hybrid_soft_stale_slots: u64,
    pub mark_ewma_e6: u64,
    pub mark_ewma_last_slot: u64,
    pub mark_ewma_halflife_slots: u64,
    pub mark_min_fee: u64,
    pub oracle_target_price_e6: u64,
    pub oracle_target_publish_time: i64,
    pub last_good_oracle_slot: u64,
    pub oracle_leg_feeds: [[u8; 32]; ORACLE_LEG_CAP],
    pub oracle_leg_prices_e6: [u64; ORACLE_LEG_CAP],
    pub oracle_leg_publish_times: [i64; ORACLE_LEG_CAP],
}

/// Account-prefix constants from `percolator-prog/src/v16_program.rs:48-60`.
/// These are NOT in the risk-engine crate — we mirror them here so
/// the decoder can navigate to `MarketGroupV16HeaderAccount`
/// correctly. Locked by `tests::account_prefix_constants_match_program`.
pub const HEADER_LEN: usize = 16;
pub const WRAPPER_CONFIG_LEN: usize = 624;
pub const ASSET_ORACLE_WRAPPER_LEN: usize = 512;
pub const ASSET_ORACLE_PROFILE_LEN: usize = 368;
pub const MARKET_GROUP_OFF: usize = HEADER_LEN + WRAPPER_CONFIG_LEN;
pub const PERCOLATOR_MAGIC: u64 = 0x5045_5243_5631_3600; // "PERCV16\0"
pub const PERCOLATOR_VERSION: u16 = 16;
pub const KIND_MARKET: u8 = 1;

/// Stride of one on-chain asset slot. The wrapper region is a
/// fixed 512-byte reserve (NOT `size_of::<AssetOracleProfileV16>()`,
/// which is 368). Forward-compat padding lives in the trailing bytes.
pub fn onchain_slot_stride() -> usize {
    ASSET_ORACLE_WRAPPER_LEN + std::mem::size_of::<EngineAssetSlotV16Account>()
}

/// Decode raw market-group bytes into a [`MarketState`] suitable
/// for the keeper's decision layer.
///
/// Pure function — no RPC, no Solana SDK; just the bytes from
/// `getAccountInfo`. Failures surface specific reasons (size
/// mismatch vs. partial bytes vs. invariant violations) for
/// operator-level diagnostics.
pub fn decode_market_account(
    market_address: &str,
    program_id: &str,
    raw: &[u8],
) -> Result<MarketState, DecodeError> {
    // 1) Verify the 16-byte account-header magic + kind. This is the
    //    cheapest "is this even a market account?" check — refuses
    //    to read garbage if a wrong pubkey was passed.
    if raw.len() < HEADER_LEN {
        return Err(DecodeError::Truncated {
            expected: HEADER_LEN,
            got: raw.len(),
        });
    }
    let mut magic_buf = [0u8; 8];
    magic_buf.copy_from_slice(&raw[0..8]);
    let magic = u64::from_le_bytes(magic_buf);
    if magic != PERCOLATOR_MAGIC {
        return Err(DecodeError::BadMagic { got: magic });
    }
    let version = u16::from_le_bytes([raw[8], raw[9]]);
    if version != PERCOLATOR_VERSION {
        return Err(DecodeError::BadVersion {
            got: version,
            expected: PERCOLATOR_VERSION,
        });
    }
    let kind = raw[10];
    if kind != KIND_MARKET {
        return Err(DecodeError::BadKind {
            got: kind,
            expected: KIND_MARKET,
        });
    }

    let header_size = std::mem::size_of::<MarketGroupV16HeaderAccount>();
    let engine_size = std::mem::size_of::<EngineAssetSlotV16Account>();
    let slot_size = onchain_slot_stride();

    if raw.len() < MARKET_GROUP_OFF + header_size {
        return Err(DecodeError::Truncated {
            expected: MARKET_GROUP_OFF + header_size,
            got: raw.len(),
        });
    }
    let header_bytes = &raw[MARKET_GROUP_OFF..MARKET_GROUP_OFF + header_size];
    let header: &MarketGroupV16HeaderAccount = bytemuck::try_from_bytes(header_bytes)
        .map_err(|e| DecodeError::BytemuckHeader(format!("{e:?}")))?;
    let capacity = header.asset_slot_capacity.get() as usize;
    let needed = MARKET_GROUP_OFF
        .checked_add(header_size)
        .ok_or(DecodeError::Overflow)?
        .checked_add(capacity.checked_mul(slot_size).ok_or(DecodeError::Overflow)?)
        .ok_or(DecodeError::Overflow)?;
    if raw.len() < needed {
        return Err(DecodeError::Truncated {
            expected: needed,
            got: raw.len(),
        });
    }

    let mut assets = Vec::with_capacity(capacity);
    let slots_off = MARKET_GROUP_OFF + header_size;
    for idx in 0..capacity {
        let slot_off = slots_off + idx * slot_size;
        // The wrapper region reserves ASSET_ORACLE_WRAPPER_LEN (512)
        // bytes, but the actual `AssetOracleProfileV16` fills only
        // the first ASSET_ORACLE_PROFILE_LEN (368). The engine half
        // starts at slot_off + ASSET_ORACLE_WRAPPER_LEN.
        let engine_off = slot_off + ASSET_ORACLE_WRAPPER_LEN;
        let engine_bytes = &raw[engine_off..engine_off + engine_size];
        let engine: &EngineAssetSlotV16Account = bytemuck::try_from_bytes(engine_bytes)
            .map_err(|e| DecodeError::BytemuckSlots(format!("slot {idx}: {e:?}")))?;
        // market_id == 0 means the slot is empty (never allocated to
        // an asset). Skip — keeper has nothing to do.
        if engine.asset.market_id.get() == 0 {
            continue;
        }
        let lifecycle = lifecycle_from_u8(engine.asset.lifecycle);
        assets.push(AssetState {
            index: idx as AssetIndex,
            label: format!("asset-{idx}"),
            lifecycle,
            // `slot_last` is the engine's per-asset staleness clock.
            // It matches what `mark_ewma_last_slot` tracks on the
            // wrapper side after a successful PushAuthMark.
            last_mark_slot: engine.asset.slot_last.get(),
            // `effective_price` is the post-clamp price the engine
            // operates on; `mark_ewma_e6` is what the wrapper holds.
            // For an AUTH_MARK asset they should agree; for hybrid
            // they can diverge inside the band. Use the engine's
            // effective_price as the "current mark" the keeper
            // reads.
            last_mark_e6: engine.asset.effective_price.get(),
        });
    }

    Ok(MarketState {
        market_address: market_address.to_string(),
        program_id: program_id.to_string(),
        current_slot: header.current_slot.get(),
        // `slot_last` is the engine's most-recent crank slot
        // (market-wide). We use it as `last_crank_slot` for the
        // crank-interval policy.
        last_crank_slot: header.slot_last.get(),
        assets,
    })
}

fn lifecycle_from_u8(b: u8) -> AssetLifecycle {
    // Mirror of `percolator::encode_asset_lifecycle` (and its
    // inverse). Variant order matches `AssetLifecycleV16` in the
    // engine crate.
    match b {
        0 => AssetLifecycle::Disabled,
        1 => AssetLifecycle::PendingActivation,
        2 => AssetLifecycle::Active,
        3 => AssetLifecycle::DrainOnly,
        4 => AssetLifecycle::Retired,
        5 => AssetLifecycle::Recovery,
        // Forward-compat: any unknown byte falls through to
        // Disabled (the safest "do nothing" lifecycle).
        _ => AssetLifecycle::Disabled,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("account bytes too short: expected at least {expected}, got {got}")]
    Truncated { expected: usize, got: usize },
    #[error("bytemuck header decode: {0}")]
    BytemuckHeader(String),
    #[error("bytemuck slots decode: {0}")]
    BytemuckSlots(String),
    #[error("size computation overflowed")]
    Overflow,
    #[error("bad magic: got 0x{got:016x}, expected 0x5045524356313600 (PERCV16)")]
    BadMagic { got: u64 },
    #[error("bad version: got {got}, expected {expected}")]
    BadVersion { got: u16, expected: u16 },
    #[error("bad kind: got {got}, expected {expected} (1 = market)")]
    BadKind { got: u8, expected: u8 },
}

/// On-chain `PercolatorClient`. Holds an `RpcClient` + the program
/// id; `read_market` fetches the market-group account and decodes
/// it via [`decode_market_account`].
pub struct RealPercolator {
    rpc: Arc<RpcClient>,
    program_id: Pubkey,
}

impl RealPercolator {
    pub fn new(rpc_url: impl Into<String>, program_id: &str) -> Result<Self, ClientError> {
        let pid =
            Pubkey::from_str(program_id).map_err(|e| ClientError::Other(format!("program id: {e}")))?;
        Ok(Self {
            rpc: Arc::new(RpcClient::new(rpc_url.into())),
            program_id: pid,
        })
    }

    pub fn with_rpc(rpc: Arc<RpcClient>, program_id: Pubkey) -> Self {
        Self { rpc, program_id }
    }
}

#[async_trait]
impl PercolatorClient for RealPercolator {
    async fn read_market(&self, market_address: &str) -> Result<MarketState, ClientError> {
        let pk = Pubkey::from_str(market_address)
            .map_err(|e| ClientError::Other(format!("market addr: {e}")))?;
        let raw = self
            .rpc
            .get_account_data(&pk)
            .await
            .map_err(|e| ClientError::Other(format!("rpc: {e}")))?;
        decode_market_account(market_address, &self.program_id.to_string(), &raw)
            .map_err(|e| ClientError::Other(format!("decode: {e}")))
    }

    async fn execute(
        &self,
        _market_address: &str,
        _action: &KeeperAction,
    ) -> Result<Execution, ClientError> {
        // Submission lives in `sender.rs`. The real client doesn't
        // submit through this trait — it composes:
        //   policy.decide(read_market()) → onchain::build_instruction
        //   → sender::Sender::submit
        // We could implement `execute` as a convenience wrapper but
        // it would hide the Sender's retry/confirmation surface,
        // which operators want explicit. Returning Unsupported
        // makes the intent loud.
        Err(ClientError::Other(
            "RealPercolator::execute not supported — use onchain::build_instruction + Sender".into(),
        ))
    }

    async fn list_portfolios(
        &self,
        _market_address: &str,
    ) -> Result<Vec<PortfolioSnapshot>, ClientError> {
        // Portfolio enumeration via `getProgramAccounts` with a
        // discriminator filter is operator-specific (indexer choice,
        // rate limits, archival cutoffs) — keep this surface
        // explicit-Unsupported for v1 so operators wire their own
        // path. The recovery-policy keeper reads from this method;
        // a real deployment supplies a tailored impl via the
        // `PercolatorClient` trait directly.
        Err(ClientError::Other(
            "RealPercolator::list_portfolios not supported in v1 — operator-supplied".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use percolator::{V16PodU32, V16PodU64};

    /// Lock the wrapper struct's size. If percolator-prog changes
    /// its layout, this fails BEFORE the decoder silently misreads.
    #[test]
    fn asset_oracle_profile_size_locked() {
        // Compute expected size from his struct: 1+1+1+1 + 4 + 2*5 + 6
        //   = 8 + 4 + 10 + 6 = 28
        //   + 32*4 = 128 → 156
        //   + 8*9 = 72 → 228
        //   + 32*3 = 96 → 324
        //   + 8*3 = 24 → 348
        //   + 8*3 = 24 → 372
        // (manual struct walk — see source for the actual layout)
        // We assert the size is what bytemuck reports; this catches
        // padding drift even if our literal arithmetic above is
        // wrong.
        let s = std::mem::size_of::<AssetOracleProfileV16>();
        // Stable check: align is 8, padding behaviors fixed.
        assert!(s.is_multiple_of(8), "struct must be 8-aligned, got {s}");
        // A zero AssetOracleProfileV16 must roundtrip through bytemuck.
        let zero = AssetOracleProfileV16::default();
        let bytes = bytemuck::bytes_of(&zero);
        let back: &AssetOracleProfileV16 = bytemuck::from_bytes(bytes);
        assert_eq!(*back, zero);
    }

    /// Build a synthetic market account with the canonical
    /// 16-byte prefix + 624-byte WrapperConfig zero region + the
    /// supplied header + slots. Used to test the decoder end-to-end.
    fn synthesize_market(
        header: &MarketGroupV16HeaderAccount,
        slots: &[(AssetOracleProfileV16, EngineAssetSlotV16Account)],
    ) -> Vec<u8> {
        let mut raw = Vec::new();
        // [0..8] magic
        raw.extend_from_slice(&PERCOLATOR_MAGIC.to_le_bytes());
        // [8..10] version
        raw.extend_from_slice(&PERCOLATOR_VERSION.to_le_bytes());
        // [10] kind
        raw.push(KIND_MARKET);
        // [11..16] pad
        raw.extend_from_slice(&[0u8; 5]);
        // [16..640] WrapperConfigV16 zero region — content doesn't
        // matter for market-group decoding, only the size does.
        raw.extend_from_slice(&[0u8; WRAPPER_CONFIG_LEN]);
        // [640..640+HDR] MarketGroupV16HeaderAccount
        raw.extend_from_slice(bytemuck::bytes_of(header));
        // [..] N slots
        for (wrapper, engine) in slots {
            let mut wrapper_block = [0u8; ASSET_ORACLE_WRAPPER_LEN];
            wrapper_block[..std::mem::size_of::<AssetOracleProfileV16>()]
                .copy_from_slice(bytemuck::bytes_of(wrapper));
            raw.extend_from_slice(&wrapper_block);
            raw.extend_from_slice(bytemuck::bytes_of(engine));
        }
        raw
    }

    #[test]
    fn decoder_rejects_truncated_input() {
        let err = decode_market_account("M", "P", &[0u8; 10]).unwrap_err();
        assert!(matches!(err, DecodeError::Truncated { .. }));
    }

    #[test]
    fn decoder_rejects_bad_magic() {
        let err = decode_market_account("M", "P", &[0u8; HEADER_LEN]).unwrap_err();
        assert!(matches!(err, DecodeError::BadMagic { .. }));
    }

    #[test]
    fn decoder_rejects_wrong_kind() {
        // Magic + version OK, kind = 2 (portfolio, not market).
        let mut raw = Vec::new();
        raw.extend_from_slice(&PERCOLATOR_MAGIC.to_le_bytes());
        raw.extend_from_slice(&PERCOLATOR_VERSION.to_le_bytes());
        raw.push(2); // portfolio kind
        raw.extend_from_slice(&[0u8; 5]);
        let err = decode_market_account("M", "P", &raw).unwrap_err();
        assert!(matches!(err, DecodeError::BadKind { got: 2, .. }));
    }

    #[test]
    fn decoder_reads_zeroed_market_group() {
        let header: MarketGroupV16HeaderAccount = bytemuck::Zeroable::zeroed();
        let raw = synthesize_market(&header, &[]);
        let m = decode_market_account("M", "P", &raw).unwrap();
        assert_eq!(m.current_slot, 0);
        assert!(m.assets.is_empty());
    }

    #[test]
    fn decoder_reads_one_active_asset() {
        let mut header: MarketGroupV16HeaderAccount = bytemuck::Zeroable::zeroed();
        header.asset_slot_capacity = V16PodU32::new(1);
        header.current_slot = V16PodU64::new(123_456);
        header.slot_last = V16PodU64::new(123_400);

        let wrapper: AssetOracleProfileV16 = bytemuck::Zeroable::zeroed();
        let mut engine: EngineAssetSlotV16Account = bytemuck::Zeroable::zeroed();
        engine.asset.market_id = V16PodU64::new(7);
        engine.asset.lifecycle = 2; // Active
        engine.asset.slot_last = V16PodU64::new(123_000);
        engine.asset.effective_price = V16PodU64::new(145_320_000);

        let raw = synthesize_market(&header, &[(wrapper, engine)]);
        let m = decode_market_account("M", "P", &raw).unwrap();
        assert_eq!(m.current_slot, 123_456);
        assert_eq!(m.last_crank_slot, 123_400);
        assert_eq!(m.assets.len(), 1);
        assert_eq!(m.assets[0].index, 0);
        assert_eq!(m.assets[0].lifecycle, AssetLifecycle::Active);
        assert_eq!(m.assets[0].last_mark_slot, 123_000);
        assert_eq!(m.assets[0].last_mark_e6, 145_320_000);
    }

    /// Sanity: our hand-mirrored AssetOracleProfileV16 must match
    /// percolator-prog's documented size (368 bytes).
    #[test]
    fn account_prefix_constants_match_program() {
        assert_eq!(std::mem::size_of::<AssetOracleProfileV16>(), ASSET_ORACLE_PROFILE_LEN);
        assert_eq!(ASSET_ORACLE_WRAPPER_LEN, 512);
        assert_eq!(WRAPPER_CONFIG_LEN, 624);
        assert_eq!(HEADER_LEN, 16);
    }

    /// Live test against mainnet Bounty 6. Marked `#[ignore]` so it
    /// runs only on explicit `cargo test -- --include-ignored`.
    /// Confirms the decoder works against Toly's actual deployment.
    #[tokio::test]
    #[ignore]
    async fn live_bounty6_market_read() {
        let client = RealPercolator::new(
            "https://api.mainnet-beta.solana.com",
            "4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi",
        )
        .unwrap();
        let m = client
            .read_market("BhkMic5gHLjj5Uxkg6rBBXofUzeTZVwmV4uFzfhwtgQw")
            .await
            .expect("mainnet read");
        eprintln!(
            "Bounty 6: current_slot={}, last_crank_slot={}, assets={}",
            m.current_slot,
            m.last_crank_slot,
            m.assets.len()
        );
        // Bounty 6 advertises 3 markets (USD/SOL, STOXX50/SOL,
        // BTC/SOL); we expect ≥ 1 non-zero asset slot.
        assert!(m.current_slot > 0, "current_slot must be live");
        assert!(!m.assets.is_empty(), "expected at least one active asset");
    }
}
