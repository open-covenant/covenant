//! Covenant stake keeper.
//!
//! Two independent loops:
//!
//! - **Sweep** (default 1h): read SOL balance of the creator wallet, compute
//!   a configurable split (default 25/25/30/20 stakers/buylock/treasury/subsidy),
//!   and route each leg. Stakers fold into the staking program via
//!   `deposit_sol_fees`. Buy-and-lock is a stub in v1 (logs and skips —
//!   Jupiter swap + `deposit_buylock_cvnt` lands in v1.1). Treasury and
//!   subsidy are plain SOL transfers.
//! - **Accrue** (default 6h): permissionlessly fold any orphaned
//!   `pending_sol_lamports` into the per-weight accumulator. Belt-and-suspenders
//!   because `deposit_sol_fees` already folds inline; this catches missed folds.

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use borsh::BorshSerialize;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{read_keypair_file, Keypair};
use solana_sdk::signer::Signer;
use solana_sdk::system_instruction;
use solana_sdk::transaction::Transaction;
use tracing::{info, warn};

pub const COVENANT_STAKE_PROGRAM_ID_STR: &str = "CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED";

pub const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 60 * 60;
pub const DEFAULT_ACCRUAL_INTERVAL_SECS: u64 = 6 * 60 * 60;
pub const DEFAULT_MIN_SWEEP_LAMPORTS: u64 = 10_000_000;
pub const DEFAULT_RESERVE_LAMPORTS: u64 = 50_000_000;
pub const DEFAULT_MIN_ACCRUE_LAMPORTS: u64 = 1_000_000;

#[derive(Clone, Debug, Deserialize)]
pub struct KeeperConfig {
    pub rpc_url: String,
    pub creator_keypair_path: String,
    pub treasury_recipient: String,
    pub subsidy_recipient: String,
    #[serde(default = "default_stakers_bps")]
    pub stakers_bps: u16,
    #[serde(default = "default_buylock_bps")]
    pub buylock_bps: u16,
    #[serde(default = "default_treasury_bps")]
    pub treasury_bps: u16,
    #[serde(default = "default_subsidy_bps")]
    pub subsidy_bps: u16,
    #[serde(default = "default_sweep_interval")]
    pub sweep_interval_secs: u64,
    #[serde(default = "default_accrual_interval")]
    pub accrual_interval_secs: u64,
    #[serde(default = "default_min_sweep")]
    pub min_sweep_lamports: u64,
    #[serde(default = "default_reserve")]
    pub reserve_lamports: u64,
    #[serde(default = "default_min_accrue")]
    pub min_accrue_lamports: u64,
    #[serde(default)]
    pub dry_run: bool,
}

fn default_stakers_bps() -> u16 {
    2500
}
fn default_buylock_bps() -> u16 {
    2500
}
fn default_treasury_bps() -> u16 {
    3000
}
fn default_subsidy_bps() -> u16 {
    2000
}
fn default_sweep_interval() -> u64 {
    DEFAULT_SWEEP_INTERVAL_SECS
}
fn default_accrual_interval() -> u64 {
    DEFAULT_ACCRUAL_INTERVAL_SECS
}
fn default_min_sweep() -> u64 {
    DEFAULT_MIN_SWEEP_LAMPORTS
}
fn default_reserve() -> u64 {
    DEFAULT_RESERVE_LAMPORTS
}
fn default_min_accrue() -> u64 {
    DEFAULT_MIN_ACCRUE_LAMPORTS
}

impl KeeperConfig {
    pub fn from_env() -> Result<Self> {
        let rpc_url = required_env("COVENANT_STAKE_KEEPER_RPC_URL")?;
        let creator_keypair_path = required_env("COVENANT_STAKE_KEEPER_CREATOR_KEYPAIR")?;
        let treasury_recipient = required_env("COVENANT_STAKE_KEEPER_TREASURY_RECIPIENT")?;
        let subsidy_recipient = required_env("COVENANT_STAKE_KEEPER_SUBSIDY_RECIPIENT")?;
        let stakers_bps = optional_env_u16("COVENANT_STAKE_KEEPER_STAKERS_BPS")?
            .unwrap_or_else(default_stakers_bps);
        let buylock_bps = optional_env_u16("COVENANT_STAKE_KEEPER_BUYLOCK_BPS")?
            .unwrap_or_else(default_buylock_bps);
        let treasury_bps = optional_env_u16("COVENANT_STAKE_KEEPER_TREASURY_BPS")?
            .unwrap_or_else(default_treasury_bps);
        let subsidy_bps = optional_env_u16("COVENANT_STAKE_KEEPER_SUBSIDY_BPS")?
            .unwrap_or_else(default_subsidy_bps);
        let sweep_interval_secs = optional_env_u64("COVENANT_STAKE_KEEPER_SWEEP_INTERVAL_SECS")?
            .unwrap_or(DEFAULT_SWEEP_INTERVAL_SECS);
        let accrual_interval_secs =
            optional_env_u64("COVENANT_STAKE_KEEPER_ACCRUAL_INTERVAL_SECS")?
                .unwrap_or(DEFAULT_ACCRUAL_INTERVAL_SECS);
        let min_sweep_lamports = optional_env_u64("COVENANT_STAKE_KEEPER_MIN_SWEEP_LAMPORTS")?
            .unwrap_or(DEFAULT_MIN_SWEEP_LAMPORTS);
        let reserve_lamports = optional_env_u64("COVENANT_STAKE_KEEPER_RESERVE_LAMPORTS")?
            .unwrap_or(DEFAULT_RESERVE_LAMPORTS);
        let min_accrue_lamports = optional_env_u64("COVENANT_STAKE_KEEPER_MIN_ACCRUE_LAMPORTS")?
            .unwrap_or(DEFAULT_MIN_ACCRUE_LAMPORTS);
        let dry_run = std::env::var("COVENANT_STAKE_KEEPER_DRY_RUN")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false);

        let cfg = Self {
            rpc_url,
            creator_keypair_path,
            treasury_recipient,
            subsidy_recipient,
            stakers_bps,
            buylock_bps,
            treasury_bps,
            subsidy_bps,
            sweep_interval_secs,
            accrual_interval_secs,
            min_sweep_lamports,
            reserve_lamports,
            min_accrue_lamports,
            dry_run,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        let total = self.stakers_bps as u32
            + self.buylock_bps as u32
            + self.treasury_bps as u32
            + self.subsidy_bps as u32;
        if total != 10_000 {
            bail!("split bps must sum to 10000, got {}", total);
        }
        if self.sweep_interval_secs == 0 || self.accrual_interval_secs == 0 {
            bail!("interval secs must be > 0");
        }
        Pubkey::from_str(&self.treasury_recipient)
            .with_context(|| format!("invalid treasury_recipient: {}", self.treasury_recipient))?;
        Pubkey::from_str(&self.subsidy_recipient)
            .with_context(|| format!("invalid subsidy_recipient: {}", self.subsidy_recipient))?;
        Ok(())
    }
}

pub struct Keeper {
    cfg: KeeperConfig,
    program_id: Pubkey,
    config_pda: Pubkey,
    fee_router_pda: Pubkey,
    reward_vault_pda: Pubkey,
    creator: Arc<Keypair>,
    rpc: Arc<RpcClient>,
    treasury: Pubkey,
    subsidy: Pubkey,
}

impl Keeper {
    pub fn from_config(cfg: KeeperConfig) -> Result<Self> {
        let program_id = Pubkey::from_str(COVENANT_STAKE_PROGRAM_ID_STR)?;
        let creator = Arc::new(load_keypair(&cfg.creator_keypair_path)?);
        let rpc = Arc::new(RpcClient::new_with_commitment(
            cfg.rpc_url.clone(),
            CommitmentConfig::confirmed(),
        ));
        let (config_pda, _) = Pubkey::find_program_address(&[b"stake_config"], &program_id);
        let (fee_router_pda, _) = Pubkey::find_program_address(&[b"fee_router"], &program_id);
        let (reward_vault_pda, _) = Pubkey::find_program_address(&[b"reward_vault"], &program_id);
        let treasury = Pubkey::from_str(&cfg.treasury_recipient)?;
        let subsidy = Pubkey::from_str(&cfg.subsidy_recipient)?;
        Ok(Self {
            cfg,
            program_id,
            config_pda,
            fee_router_pda,
            reward_vault_pda,
            creator,
            rpc,
            treasury,
            subsidy,
        })
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let sweep = tokio::spawn({
            let me = Arc::clone(&self);
            async move { me.sweep_loop().await }
        });
        let accrue = tokio::spawn({
            let me = Arc::clone(&self);
            async move { me.accrue_loop().await }
        });
        tokio::select! {
            r = sweep => r.context("sweep loop joined")?,
            r = accrue => r.context("accrue loop joined")?,
        }
    }

    async fn sweep_loop(self: Arc<Self>) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(self.cfg.sweep_interval_secs));
        loop {
            tick.tick().await;
            if let Err(e) = self.sweep_once().await {
                warn!(error = %e, "sweep failed");
            }
        }
    }

    async fn accrue_loop(self: Arc<Self>) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(self.cfg.accrual_interval_secs));
        loop {
            tick.tick().await;
            if let Err(e) = self.accrue_once().await {
                warn!(error = %e, "accrue failed");
            }
        }
    }

    pub async fn sweep_once(&self) -> Result<()> {
        let balance = self
            .rpc
            .get_balance(&self.creator.pubkey())
            .await
            .context("get creator wallet balance")?;
        let reserve = self.cfg.reserve_lamports;
        let surplus = balance.saturating_sub(reserve);
        if surplus < self.cfg.min_sweep_lamports {
            info!(
                balance,
                reserve, surplus, "sweep skipped — surplus below min_sweep_lamports"
            );
            return Ok(());
        }

        let split = SweepSplit::compute(surplus, &self.cfg);
        info!(
            balance,
            reserve,
            surplus,
            stakers = split.stakers,
            buylock = split.buylock,
            treasury = split.treasury,
            subsidy = split.subsidy,
            "sweep split"
        );

        if self.cfg.dry_run {
            info!("dry_run: skipping actual sends");
            return Ok(());
        }

        if split.stakers > 0 {
            let sig = self.send_deposit_sol_fees(split.stakers).await?;
            info!(sig = %sig, lamports = split.stakers, "deposited stakers SOL");
        }
        if split.treasury > 0 {
            let sig = self
                .send_system_transfer(&self.treasury, split.treasury)
                .await?;
            info!(sig = %sig, lamports = split.treasury, "sent treasury cut");
        }
        if split.subsidy > 0 {
            let sig = self
                .send_system_transfer(&self.subsidy, split.subsidy)
                .await?;
            info!(sig = %sig, lamports = split.subsidy, "sent subsidy cut");
        }
        if split.buylock > 0 {
            warn!(
                lamports = split.buylock,
                "buylock leg requires Jupiter SOL→CVNT swap + deposit_buylock_cvnt — not implemented in v1, lamports left in creator wallet for the next sweep cycle"
            );
        }
        Ok(())
    }

    pub async fn accrue_once(&self) -> Result<()> {
        if self.cfg.dry_run {
            info!("dry_run: skipping accrue");
            return Ok(());
        }
        let sig = self.send_accrue().await?;
        info!(sig = %sig, "accrue tick");
        Ok(())
    }

    async fn send_deposit_sol_fees(&self, amount: u64) -> Result<String> {
        let mut data = anchor_discriminator("deposit_sol_fees").to_vec();
        amount.serialize(&mut data)?;
        let ix = Instruction {
            program_id: self.program_id,
            accounts: vec![
                AccountMeta::new(self.config_pda, false),
                AccountMeta::new(self.fee_router_pda, false),
                AccountMeta::new(self.reward_vault_pda, false),
                AccountMeta::new(self.creator.pubkey(), true),
                AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
            ],
            data,
        };
        self.send_ix(ix).await
    }

    async fn send_system_transfer(&self, to: &Pubkey, amount: u64) -> Result<String> {
        let ix = system_instruction::transfer(&self.creator.pubkey(), to, amount);
        self.send_ix(ix).await
    }

    async fn send_accrue(&self) -> Result<String> {
        let data = anchor_discriminator("accrue").to_vec();
        let ix = Instruction {
            program_id: self.program_id,
            accounts: vec![AccountMeta::new(self.config_pda, false)],
            data,
        };
        self.send_ix(ix).await
    }

    async fn send_ix(&self, ix: Instruction) -> Result<String> {
        let blockhash = self
            .rpc
            .get_latest_blockhash()
            .await
            .context("get latest blockhash")?;
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.creator.pubkey()),
            &[self.creator.as_ref()],
            blockhash,
        );
        let sig = self
            .rpc
            .send_transaction_with_config(
                &tx,
                RpcSendTransactionConfig {
                    skip_preflight: false,
                    preflight_commitment: Some(CommitmentConfig::confirmed().commitment),
                    ..Default::default()
                },
            )
            .await
            .context("send transaction")?;
        Ok(sig.to_string())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SweepSplit {
    pub stakers: u64,
    pub buylock: u64,
    pub treasury: u64,
    pub subsidy: u64,
}

impl SweepSplit {
    pub fn compute(surplus: u64, cfg: &KeeperConfig) -> Self {
        let pct = |bps: u16| -> u64 {
            ((surplus as u128) * (bps as u128) / 10_000)
                .try_into()
                .unwrap_or(u64::MAX)
        };
        let stakers = pct(cfg.stakers_bps);
        let buylock = pct(cfg.buylock_bps);
        let treasury = pct(cfg.treasury_bps);
        // Subsidy absorbs rounding residue so the four legs sum to exactly `surplus`.
        let subsidy = surplus - stakers - buylock - treasury;
        Self {
            stakers,
            buylock,
            treasury,
            subsidy,
        }
    }
}

pub fn anchor_discriminator(method: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(b"global:");
    hasher.update(method.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

fn required_env(key: &str) -> Result<String> {
    std::env::var(key).map_err(|_| anyhow!("missing required env: {}", key))
}

fn optional_env_u64(key: &str) -> Result<Option<u64>> {
    match std::env::var(key) {
        Ok(v) => Ok(Some(
            v.parse()
                .with_context(|| format!("env {} not a u64: {}", key, v))?,
        )),
        Err(_) => Ok(None),
    }
}

fn optional_env_u16(key: &str) -> Result<Option<u16>> {
    match std::env::var(key) {
        Ok(v) => Ok(Some(
            v.parse()
                .with_context(|| format!("env {} not a u16: {}", key, v))?,
        )),
        Err(_) => Ok(None),
    }
}

fn load_keypair(path: &str) -> Result<Keypair> {
    read_keypair_file(Path::new(path))
        .map_err(|e| anyhow!("read keypair {}: {}", path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cfg() -> KeeperConfig {
        KeeperConfig {
            rpc_url: "http://localhost:8899".into(),
            creator_keypair_path: "/tmp/none.json".into(),
            treasury_recipient: "11111111111111111111111111111111".into(),
            subsidy_recipient: "11111111111111111111111111111111".into(),
            stakers_bps: 2500,
            buylock_bps: 2500,
            treasury_bps: 3000,
            subsidy_bps: 2000,
            sweep_interval_secs: 3600,
            accrual_interval_secs: 21_600,
            min_sweep_lamports: 10_000_000,
            reserve_lamports: 50_000_000,
            min_accrue_lamports: 1_000_000,
            dry_run: false,
        }
    }

    #[test]
    fn split_sums_to_surplus() {
        let cfg = base_cfg();
        let s = SweepSplit::compute(1_000_000_000, &cfg);
        assert_eq!(s.stakers + s.buylock + s.treasury + s.subsidy, 1_000_000_000);
        assert_eq!(s.stakers, 250_000_000);
        assert_eq!(s.buylock, 250_000_000);
        assert_eq!(s.treasury, 300_000_000);
        assert_eq!(s.subsidy, 200_000_000);
    }

    #[test]
    fn split_absorbs_rounding_residue_into_subsidy() {
        let cfg = base_cfg();
        // 7 lamports: 25/25/30/20 doesn't round cleanly. Subsidy takes residue.
        let s = SweepSplit::compute(7, &cfg);
        assert_eq!(s.stakers + s.buylock + s.treasury + s.subsidy, 7);
    }

    #[test]
    fn config_validates_split_must_sum_to_10000() {
        let mut cfg = base_cfg();
        cfg.stakers_bps = 9999;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn config_validates_recipient_pubkeys() {
        let mut cfg = base_cfg();
        cfg.treasury_recipient = "not-a-pubkey".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn anchor_discriminator_is_deterministic_8_bytes() {
        let a = anchor_discriminator("deposit_sol_fees");
        let b = anchor_discriminator("deposit_sol_fees");
        assert_eq!(a, b);
        let c = anchor_discriminator("accrue");
        assert_ne!(a, c);
    }
}
