//! Covenant stake keeper.
//!
//! Two independent loops:
//!
//! - **Harvest** (start of each sweep): collect accrued PumpSwap coin-creator
//!   fees from the on-chain fee vault into the creator wallet and unwrap the
//!   wSOL to native SOL, so the split below covers 100% of fees automatically
//!   instead of only whatever has been manually claimed.
//! - **Sweep** (default 1h): read SOL balance of the creator wallet, compute
//!   a configurable split (default 25/25/30/20 stakers/buylock/treasury/subsidy),
//!   and route each leg. Stakers fold into the staking program via
//!   `deposit_sol_fees`. Buy-and-lock is a stub in v1 (logs and skips —
//!   Jupiter swap + `deposit_buylock_cvnt` lands in v1.1). Treasury and
//!   subsidy are plain SOL transfers.
//! - **Accrue** (default 6h): permissionlessly fold any orphaned
//!   `pending_sol_lamports` into the per-weight accumulator. Belt-and-suspenders
//!   because `deposit_sol_fees` already folds inline; this catches missed folds.

pub mod jupiter;

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

use crate::jupiter::{sign_and_send_jupiter_tx, JupiterClient};

pub const COVENANT_STAKE_PROGRAM_ID_STR: &str = "CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED";

pub const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 60 * 60;
pub const DEFAULT_ACCRUAL_INTERVAL_SECS: u64 = 6 * 60 * 60;
pub const DEFAULT_MIN_SWEEP_LAMPORTS: u64 = 10_000_000;
pub const DEFAULT_RESERVE_LAMPORTS: u64 = 50_000_000;
pub const DEFAULT_MIN_ACCRUE_LAMPORTS: u64 = 1_000_000;

/// Hardcoded treasury recipient. Compile-time const so an attacker with
/// write access to the keeper's env can't redirect the 30% sweep cut.
/// Rotation requires a keeper redeploy.
pub const TREASURY_RECIPIENT: &str = "8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM";

/// Hardcoded subsidy recipient. Same rationale as TREASURY_RECIPIENT.
pub const SUBSIDY_RECIPIENT: &str = "8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM";

/// Mainnet $CVNT mint (Token-2022). Hardcoded so the keeper cannot be
/// pointed at a wrong-mint via env tampering.
pub const CVNT_MINT: &str = "2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump";

/// Token-2022 program — required for the $CVNT mint.
pub const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// Default Jupiter slippage tolerance (200 bps = 2%).
pub const DEFAULT_JUPITER_SLIPPAGE_BPS: u16 = 200;

/// Below this lamport amount, defer the buylock leg to the next sweep —
/// dust swaps lose more to fees + slippage than they buy back.
pub const DEFAULT_MIN_BUYLOCK_LAMPORTS: u64 = 50_000_000;

/// PumpSwap (pump AMM) program — graduated $CVNT trades pay the coin-creator
/// fee into a per-creator vault owned by this program.
pub const PUMPSWAP_PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

/// Wrapped-SOL mint. The $CVNT pool's quote mint, so creator fees accrue as
/// wSOL in the vault and must be unwrapped to native SOL after collecting.
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Legacy SPL Token program — wSOL is a legacy-SPL mint (NOT Token-2022).
pub const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Skip the harvest when the creator-fee vault holds less than this, so we
/// don't burn a tx fee collecting dust.
pub const HARVEST_MIN_LAMPORTS: u64 = 1_000_000;

#[derive(Clone, Debug, Deserialize)]
pub struct KeeperConfig {
    pub rpc_url: String,
    pub creator_keypair_path: String,
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
    #[serde(default = "default_jupiter_slippage")]
    pub jupiter_slippage_bps: u16,
    #[serde(default = "default_min_buylock")]
    pub min_buylock_lamports: u64,
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
fn default_jupiter_slippage() -> u16 {
    DEFAULT_JUPITER_SLIPPAGE_BPS
}
fn default_min_buylock() -> u64 {
    DEFAULT_MIN_BUYLOCK_LAMPORTS
}

impl KeeperConfig {
    pub fn from_env() -> Result<Self> {
        let rpc_url = required_env("COVENANT_STAKE_KEEPER_RPC_URL")?;
        let creator_keypair_path = required_env("COVENANT_STAKE_KEEPER_CREATOR_KEYPAIR")?;
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
        let jupiter_slippage_bps = optional_env_u16("COVENANT_STAKE_KEEPER_JUPITER_SLIPPAGE_BPS")?
            .unwrap_or(DEFAULT_JUPITER_SLIPPAGE_BPS);
        let min_buylock_lamports = optional_env_u64("COVENANT_STAKE_KEEPER_MIN_BUYLOCK_LAMPORTS")?
            .unwrap_or(DEFAULT_MIN_BUYLOCK_LAMPORTS);
        let dry_run = std::env::var("COVENANT_STAKE_KEEPER_DRY_RUN")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false);

        let cfg = Self {
            rpc_url,
            creator_keypair_path,
            stakers_bps,
            buylock_bps,
            treasury_bps,
            subsidy_bps,
            sweep_interval_secs,
            accrual_interval_secs,
            min_sweep_lamports,
            reserve_lamports,
            min_accrue_lamports,
            jupiter_slippage_bps,
            min_buylock_lamports,
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
        Pubkey::from_str(TREASURY_RECIPIENT)
            .with_context(|| format!("invalid hardcoded TREASURY_RECIPIENT: {TREASURY_RECIPIENT}"))?;
        Pubkey::from_str(SUBSIDY_RECIPIENT)
            .with_context(|| format!("invalid hardcoded SUBSIDY_RECIPIENT: {SUBSIDY_RECIPIENT}"))?;
        Ok(())
    }
}

pub struct Keeper {
    cfg: KeeperConfig,
    program_id: Pubkey,
    config_pda: Pubkey,
    fee_router_pda: Pubkey,
    reward_vault_pda: Pubkey,
    buylock_vault_authority_pda: Pubkey,
    creator: Arc<Keypair>,
    rpc: Arc<RpcClient>,
    treasury: Pubkey,
    subsidy: Pubkey,
    cvnt_mint: Pubkey,
    token_program: Pubkey,
    jupiter: JupiterClient,
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
        let (buylock_vault_authority_pda, _) =
            Pubkey::find_program_address(&[b"buylock_auth"], &program_id);
        let treasury = Pubkey::from_str(TREASURY_RECIPIENT)?;
        let subsidy = Pubkey::from_str(SUBSIDY_RECIPIENT)?;
        let cvnt_mint = Pubkey::from_str(CVNT_MINT)?;
        let token_program = Pubkey::from_str(TOKEN_2022_PROGRAM_ID)?;
        Ok(Self {
            cfg,
            program_id,
            config_pda,
            fee_router_pda,
            reward_vault_pda,
            buylock_vault_authority_pda,
            creator,
            rpc,
            treasury,
            subsidy,
            cvnt_mint,
            token_program,
            jupiter: JupiterClient::new(),
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
                warn!(error = ?e, "sweep failed");
            }
        }
    }

    async fn accrue_loop(self: Arc<Self>) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(self.cfg.accrual_interval_secs));
        loop {
            tick.tick().await;
            if let Err(e) = self.accrue_once().await {
                warn!(error = ?e, "accrue failed");
            }
        }
    }

    /// Collect accrued PumpSwap coin-creator fees into the creator wallet.
    ///
    /// Graduated $CVNT trades pay the coin-creator fee (as wSOL) into a vault
    /// ATA owned by a PumpSwap PDA. `collect_coin_creator_fee` is permissionless
    /// and moves that wSOL into the creator's own wSOL ATA; we create that ATA
    /// idempotently and close it in the same tx to unwrap to native SOL. The
    /// rent for the temp ATA round-trips, so the creator wallet nets exactly the
    /// harvested fees. Runs before the split so 100% of fees are distributed.
    async fn harvest_creator_fees(&self) -> Result<()> {
        let pumpswap = Pubkey::from_str(PUMPSWAP_PROGRAM_ID)?;
        let wsol = Pubkey::from_str(WSOL_MINT)?;
        let spl_token = Pubkey::from_str(SPL_TOKEN_PROGRAM_ID)?;
        let ata_program = Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM_ID_STR)?;
        let coin_creator = self.creator.pubkey();

        let (vault_authority, _) =
            Pubkey::find_program_address(&[b"creator_vault".as_ref(), coin_creator.as_ref()], &pumpswap);
        let vault_ata = derive_ata(&vault_authority, &wsol, &spl_token);
        let (event_authority, _) =
            Pubkey::find_program_address(&[b"__event_authority"], &pumpswap);
        let creator_wsol_ata = derive_ata(&coin_creator, &wsol, &spl_token);

        // Skip dust so we don't pay a tx fee to collect ~nothing. A missing
        // vault ATA (no fees ever) reads as zero.
        let pending = match self.rpc.get_token_account_balance(&vault_ata).await {
            Ok(bal) => bal.amount.parse::<u64>().unwrap_or(0),
            Err(_) => 0,
        };
        if pending < HARVEST_MIN_LAMPORTS {
            info!(pending, "harvest skipped — creator-fee vault below min");
            return Ok(());
        }

        // 1) idempotently create the creator's wSOL ATA (collect destination).
        let create_ata_ix = Instruction {
            program_id: ata_program,
            accounts: vec![
                AccountMeta::new(coin_creator, true),
                AccountMeta::new(creator_wsol_ata, false),
                AccountMeta::new_readonly(coin_creator, false),
                AccountMeta::new_readonly(wsol, false),
                AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
                AccountMeta::new_readonly(spl_token, false),
            ],
            data: vec![1u8], // CreateIdempotent
        };

        // 2) collect_coin_creator_fee → wSOL into the creator's wSOL ATA.
        let collect_ix = Instruction {
            program_id: pumpswap,
            accounts: vec![
                AccountMeta::new_readonly(wsol, false),
                AccountMeta::new_readonly(spl_token, false),
                AccountMeta::new_readonly(coin_creator, false),
                AccountMeta::new_readonly(vault_authority, false),
                AccountMeta::new(vault_ata, false),
                AccountMeta::new(creator_wsol_ata, false),
                AccountMeta::new_readonly(event_authority, false),
                AccountMeta::new_readonly(pumpswap, false),
            ],
            data: anchor_discriminator("collect_coin_creator_fee").to_vec(),
        };

        // 3) close the wSOL ATA → unwrap (rent + harvested wSOL) to native SOL.
        let close_ix = Instruction {
            program_id: spl_token,
            accounts: vec![
                AccountMeta::new(creator_wsol_ata, false),
                AccountMeta::new(coin_creator, false),
                AccountMeta::new_readonly(coin_creator, true),
            ],
            data: vec![9u8], // SPL Token CloseAccount
        };

        let sig = self
            .send_ixs(&[create_ata_ix, collect_ix, close_ix])
            .await
            .context("harvest creator fees")?;
        info!(sig = %sig, pending, "harvested PumpSwap creator fees → creator wallet");
        Ok(())
    }

    pub async fn sweep_once(&self) -> Result<()> {
        // Pull accrued PumpSwap creator fees into the creator wallet first so
        // the split below covers 100% of fees. A failure here must not abort the
        // cycle — the sweep should still run on whatever is already on hand.
        if let Err(e) = self.harvest_creator_fees().await {
            warn!(error = ?e, "creator-fee harvest failed; sweeping on-hand balance");
        }

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

        // Each leg runs and logs independently. A failure on one leg does
        // NOT abort the cycle — drift across cycles is preferable to losing
        // a whole sweep because of a single transient RPC error.
        if split.stakers > 0 {
            match self.send_deposit_sol_fees(split.stakers).await {
                Ok(sig) => info!(sig = %sig, lamports = split.stakers, "deposited stakers SOL"),
                Err(e) => warn!(error = ?e, lamports = split.stakers, "stakers leg failed; continuing"),
            }
        }
        if split.treasury > 0 || split.subsidy > 0 {
            match self
                .send_treasury_and_subsidy(split.treasury, split.subsidy)
                .await
            {
                Ok(sig) => info!(
                    sig = %sig,
                    treasury = split.treasury,
                    subsidy = split.subsidy,
                    "sent treasury+subsidy cuts"
                ),
                Err(e) => warn!(error = ?e, "treasury+subsidy leg failed; continuing"),
            }
        }
        if split.buylock > 0 {
            if split.buylock < self.cfg.min_buylock_lamports {
                info!(
                    lamports = split.buylock,
                    min = self.cfg.min_buylock_lamports,
                    "buylock leg below min — deferred to next sweep"
                );
            } else {
                match self.run_buylock_leg(split.buylock).await {
                    Ok(BuylockResult { swap_sig, deposit_sig, cvnt_received }) => info!(
                        swap_sig = %swap_sig,
                        deposit_sig = %deposit_sig,
                        sol_in = split.buylock,
                        cvnt_out = cvnt_received,
                        "buylock leg complete"
                    ),
                    Err(e) => warn!(
                        error = ?e,
                        lamports = split.buylock,
                        "buylock leg failed; deferring to next sweep"
                    ),
                }
            }
        }
        Ok(())
    }

    async fn run_buylock_leg(&self, lamports: u64) -> Result<BuylockResult> {
        let sol_mint = Pubkey::from_str(crate::jupiter::SOL_MINT)?;
        let pre_swap_cvnt = self.read_creator_cvnt_balance().await?;

        let (quote, raw) = self
            .jupiter
            .quote(
                &sol_mint,
                &self.cvnt_mint,
                lamports,
                self.cfg.jupiter_slippage_bps,
            )
            .await
            .context("jupiter quote")?;
        let estimated_out = quote.out_amount_u64().unwrap_or(0);
        if estimated_out == 0 {
            return Err(anyhow!("jupiter returned zero out_amount"));
        }
        info!(
            sol_in = lamports,
            cvnt_out_estimate = estimated_out,
            slippage_bps = self.cfg.jupiter_slippage_bps,
            "jupiter quote received"
        );

        let tx = self
            .jupiter
            .swap(&raw, &self.creator.pubkey())
            .await
            .context("jupiter swap build")?;
        let swap_sig = sign_and_send_jupiter_tx(&self.rpc, tx, self.creator.as_ref())
            .await
            .context("sign+send jupiter swap")?;

        let post_swap_cvnt = self.read_creator_cvnt_balance().await?;
        let cvnt_received = post_swap_cvnt.saturating_sub(pre_swap_cvnt);
        if cvnt_received == 0 {
            return Err(anyhow!(
                "post-swap CVNT delta is 0; swap may not have settled"
            ));
        }

        let deposit_sig = self
            .send_deposit_buylock_cvnt(cvnt_received)
            .await
            .context("deposit_buylock_cvnt")?;
        Ok(BuylockResult {
            swap_sig,
            deposit_sig,
            cvnt_received,
        })
    }

    async fn read_creator_cvnt_balance(&self) -> Result<u64> {
        let ata = derive_ata(&self.creator.pubkey(), &self.cvnt_mint, &self.token_program);
        let acc = match self.rpc.get_account(&ata).await {
            Ok(a) => a,
            Err(_) => return Ok(0),
        };
        if acc.data.len() < 72 {
            return Ok(0);
        }
        let mut amount = [0u8; 8];
        amount.copy_from_slice(&acc.data[64..72]);
        Ok(u64::from_le_bytes(amount))
    }

    async fn send_deposit_buylock_cvnt(&self, amount: u64) -> Result<String> {
        let depositor_ata = derive_ata(&self.creator.pubkey(), &self.cvnt_mint, &self.token_program);
        let buylock_vault =
            derive_ata(&self.buylock_vault_authority_pda, &self.cvnt_mint, &self.token_program);
        let mut data = anchor_discriminator("deposit_buylock_cvnt").to_vec();
        amount.serialize(&mut data)?;
        let ix = Instruction {
            program_id: self.program_id,
            accounts: vec![
                AccountMeta::new(self.config_pda, false),
                AccountMeta::new_readonly(self.fee_router_pda, false),
                AccountMeta::new_readonly(self.cvnt_mint, false),
                AccountMeta::new_readonly(self.buylock_vault_authority_pda, false),
                AccountMeta::new(buylock_vault, false),
                AccountMeta::new(depositor_ata, false),
                AccountMeta::new(self.creator.pubkey(), true),
                AccountMeta::new_readonly(self.token_program, false),
            ],
            data,
        };
        self.send_ix(ix).await
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

    async fn send_treasury_and_subsidy(&self, treasury: u64, subsidy: u64) -> Result<String> {
        let mut ixs = Vec::with_capacity(2);
        if treasury > 0 {
            ixs.push(system_instruction::transfer(
                &self.creator.pubkey(),
                &self.treasury,
                treasury,
            ));
        }
        if subsidy > 0 {
            ixs.push(system_instruction::transfer(
                &self.creator.pubkey(),
                &self.subsidy,
                subsidy,
            ));
        }
        self.send_ixs(&ixs).await
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
        self.send_ixs(&[ix]).await
    }

    async fn send_ixs(&self, ixs: &[Instruction]) -> Result<String> {
        let blockhash = self
            .rpc
            .get_latest_blockhash()
            .await
            .context("get latest blockhash")?;
        let tx = Transaction::new_signed_with_payer(
            ixs,
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
            .map_err(|e| anyhow::anyhow!("send transaction: {e:?}"))?;
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

#[derive(Debug, Clone)]
struct BuylockResult {
    swap_sig: String,
    deposit_sig: String,
    cvnt_received: u64,
}

const ASSOCIATED_TOKEN_PROGRAM_ID_STR: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

fn derive_ata(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    let ata_program = Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM_ID_STR).expect("valid pubkey");
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    )
    .0
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
            stakers_bps: 2500,
            buylock_bps: 2500,
            treasury_bps: 3000,
            subsidy_bps: 2000,
            sweep_interval_secs: 3600,
            accrual_interval_secs: 21_600,
            min_sweep_lamports: 10_000_000,
            reserve_lamports: 50_000_000,
            min_accrue_lamports: 1_000_000,
            jupiter_slippage_bps: 200,
            min_buylock_lamports: 50_000_000,
            dry_run: false,
        }
    }

    #[test]
    fn collect_discriminator_matches_idl() {
        // PumpSwap's on-chain IDL lists this exact discriminator for
        // collect_coin_creator_fee. Guard against an accidental rename.
        assert_eq!(
            anchor_discriminator("collect_coin_creator_fee"),
            [160, 57, 89, 42, 181, 139, 43, 66]
        );
    }

    #[test]
    fn harvest_derives_the_live_creator_fee_vault() {
        // Regression guard: the vault-ATA derivation must reproduce the live
        // $CVNT coin-creator fee vault (5i3V4w…) for creator 2JXuvX… on
        // PumpSwap, or the harvest would collect from the wrong account.
        let pumpswap = Pubkey::from_str(PUMPSWAP_PROGRAM_ID).unwrap();
        let wsol = Pubkey::from_str(WSOL_MINT).unwrap();
        let spl_token = Pubkey::from_str(SPL_TOKEN_PROGRAM_ID).unwrap();
        let coin_creator =
            Pubkey::from_str("2JXuvXb6Q5YREk9KmhtgNmseq2aKtYnu5zLRi2i5Vaeb").unwrap();
        let (vault_authority, _) =
            Pubkey::find_program_address(&[b"creator_vault".as_ref(), coin_creator.as_ref()], &pumpswap);
        let vault_ata = derive_ata(&vault_authority, &wsol, &spl_token);
        assert_eq!(
            vault_ata.to_string(),
            "5i3V4w2Xwzr2spoCCwPECn8pGVB1NLLadKu6gV4jpoXD"
        );
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
    fn config_rejects_zero_sweep_or_accrual_interval() {
        // The keeper builds its sweep and accrual tickers straight from
        // these fields via tokio::time::interval (lib.rs:269/279), which
        // panics on a zero period. validate() must reject 0 up front,
        // and per-field: the guard ORs the two operands, so a refactor
        // that dropped either side would let a zero through and crash the
        // keeper loop on its first tick. base_cfg is otherwise valid, so
        // each case isolates the field it zeroes.
        let mut zero_sweep = base_cfg();
        zero_sweep.sweep_interval_secs = 0;
        assert!(
            zero_sweep
                .validate()
                .unwrap_err()
                .to_string()
                .contains("interval secs must be > 0"),
            "zero sweep_interval_secs must be rejected by validate()"
        );

        let mut zero_accrual = base_cfg();
        zero_accrual.accrual_interval_secs = 0;
        assert!(
            zero_accrual
                .validate()
                .unwrap_err()
                .to_string()
                .contains("interval secs must be > 0"),
            "zero accrual_interval_secs must be rejected by validate()"
        );
    }

    #[test]
    fn hardcoded_recipients_parse_to_valid_pubkeys() {
        let cfg = base_cfg();
        cfg.validate().expect("hardcoded recipient consts must parse");
        Pubkey::from_str(TREASURY_RECIPIENT).expect("TREASURY_RECIPIENT valid");
        Pubkey::from_str(SUBSIDY_RECIPIENT).expect("SUBSIDY_RECIPIENT valid");
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
