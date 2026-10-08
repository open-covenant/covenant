//! End-to-end mainnet stake test from a user keypair.
//!
//! Mimics exactly what the landing/stake page produces:
//!   1. Discover the user's CVNT accounts on Token-2022 (the frontend uses
//!      `getParsedTokenAccountsByOwner({programId: Token-2022})` — we do
//!      the same via `get_token_accounts_by_owner`).
//!   2. Build a `create_position` instruction with `owner_cvnt_ata` = the
//!      largest CVNT account the user owns (matches the frontend's
//!      `pickStakeSource`).
//!   3. Sign with the user's keypair, send + confirm on mainnet, fetch the
//!      resulting StakePosition PDA to verify state.
//!
//! Usage: `cargo run --example mainnet_stake_e2e <keypair_path> <amount_cvnt> <tier_days>`
//!        e.g. `cargo run --example mainnet_stake_e2e ~/.config/solana/covenant-test-wallet.json 10000 30`

use std::env;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use borsh::BorshSerialize;
use covenant_stake_keeper::{anchor_discriminator, COVENANT_STAKE_PROGRAM_ID_STR};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_client::rpc_request::TokenAccountsFilter;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::read_keypair_file;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;

const MAINNET_RPC: &str = "https://api.mainnet-beta.solana.com";
const CVNT_MINT: &str = "2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump";
const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        bail!(
            "usage: mainnet_stake_e2e <keypair> <amount> <days>  |  claim <keypair> <position_pda>"
        );
    }
    if args[0] == "claim" {
        return run_claim(&args[1..]);
    }
    if args.len() < 3 {
        bail!("usage: mainnet_stake_e2e <keypair_path> <amount_cvnt> <tier_days>");
    }
    let keypair_path = &args[0];
    let amount_cvnt: u64 = args[1].parse().context("amount_cvnt")?;
    let tier_days: u32 = args[2].parse().context("tier_days")?;
    let tier_bps: u16 = match tier_days {
        7 => 5_000,
        30 => 10_000,
        90 => 15_000,
        180 => 20_000,
        _ => bail!("tier_days must be 7, 30, 90, or 180"),
    };
    let amount_raw = amount_cvnt
        .checked_mul(1_000_000)
        .ok_or_else(|| anyhow!("amount overflow"))?;

    let user =
        read_keypair_file(keypair_path).map_err(|e| anyhow!("read {}: {}", keypair_path, e))?;
    let user_key = user.pubkey();
    let program_id = Pubkey::from_str(COVENANT_STAKE_PROGRAM_ID_STR)?;
    let mint = Pubkey::from_str(CVNT_MINT)?;
    let token_program = Pubkey::from_str(TOKEN_2022_PROGRAM_ID)?;

    let (config_pda, _) = Pubkey::find_program_address(&[b"stake_config"], &program_id);
    let (locked_auth_pda, _) = Pubkey::find_program_address(&[b"vault_auth"], &program_id);

    let rpc =
        RpcClient::new_with_commitment(MAINNET_RPC.to_string(), CommitmentConfig::confirmed());

    println!("user            = {user_key}");
    println!("program         = {program_id}");
    println!("config_pda      = {config_pda}");
    println!("amount (raw)    = {amount_raw}");
    println!("tier_bps        = {tier_bps}");
    println!();

    // Step 1: discover user's largest CVNT account (mimics frontend logic).
    let token_accounts = rpc.get_token_accounts_by_owner_with_commitment(
        &user_key,
        TokenAccountsFilter::ProgramId(token_program),
        CommitmentConfig::confirmed(),
    )?;
    let mut cvnt_accounts: Vec<(Pubkey, u64)> = Vec::new();
    for acc in token_accounts.value {
        let pubkey = Pubkey::from_str(&acc.pubkey)?;
        if let solana_account_decoder::UiAccountData::Json(parsed) = acc.account.data {
            let info = parsed.parsed.get("info").cloned().unwrap_or_default();
            let acc_mint = info
                .get("mint")
                .and_then(|m| m.as_str())
                .unwrap_or_default();
            if acc_mint != CVNT_MINT {
                continue;
            }
            let amount_str = info
                .get("tokenAmount")
                .and_then(|t| t.get("amount"))
                .and_then(|a| a.as_str())
                .unwrap_or("0");
            let amount: u64 = amount_str.parse().unwrap_or(0);
            cvnt_accounts.push((pubkey, amount));
        }
    }
    println!(
        "discovered {} CVNT account(s) on Token-2022:",
        cvnt_accounts.len()
    );
    for (pk, amt) in &cvnt_accounts {
        println!("  {pk}  amount={} ({} CVNT)", amt, amt / 1_000_000);
    }
    let (source_ata, source_amt) = cvnt_accounts
        .into_iter()
        .max_by_key(|(_, a)| *a)
        .ok_or_else(|| anyhow!("no CVNT accounts owned by user"))?;
    if source_amt < amount_raw {
        bail!(
            "largest source has {source_amt} raw ({}), need {amount_raw} ({} CVNT)",
            source_amt / 1_000_000,
            amount_cvnt
        );
    }
    println!(
        "\nstaking source: {source_ata} ({} CVNT)",
        source_amt / 1_000_000
    );

    // Step 2: derive locked vault ATA.
    let locked_vault = spl_associated_token_account::get_associated_token_address_with_program_id(
        &locked_auth_pda,
        &mint,
        &token_program,
    );

    // Step 3: choose nonce + derive position PDA.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nonce: u64 = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
    let (position_pda, _) = Pubkey::find_program_address(
        &[b"stake_v2", user_key.as_ref(), &nonce.to_le_bytes()],
        &program_id,
    );
    println!("position pda    = {position_pda}");
    println!("nonce           = {nonce}");

    // Step 4: build create_position ix matching the frontend's account list.
    let mut data = anchor_discriminator("create_position").to_vec();
    nonce.serialize(&mut data)?;
    amount_raw.serialize(&mut data)?;
    tier_bps.serialize(&mut data)?;

    let metas = vec![
        AccountMeta::new(config_pda, false),
        AccountMeta::new(position_pda, false),
        AccountMeta::new_readonly(mint, false),
        AccountMeta::new_readonly(locked_auth_pda, false),
        AccountMeta::new(locked_vault, false),
        AccountMeta::new(source_ata, false),
        AccountMeta::new(user_key, true),
        AccountMeta::new_readonly(token_program, false),
        AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
    ];
    let ix = Instruction {
        program_id,
        accounts: metas,
        data,
    };

    // Step 5: sign + send + confirm.
    let blockhash = rpc.get_latest_blockhash().context("blockhash")?;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&user_key), &[&user], blockhash);
    println!("\nsubmitting create_position...");
    let sig = rpc
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            CommitmentConfig::confirmed(),
            RpcSendTransactionConfig {
                skip_preflight: false,
                preflight_commitment: Some(CommitmentConfig::confirmed().commitment),
                ..Default::default()
            },
        )
        .context("send+confirm")?;
    println!("create_position tx = {sig}");

    // Step 6: verify position exists.
    let pos_account = rpc.get_account(&position_pda)?;
    println!(
        "\nposition account owner = {}, lamports = {}, data len = {}",
        pos_account.owner,
        pos_account.lamports,
        pos_account.data.len()
    );
    if pos_account.owner != program_id {
        bail!("position not owned by stake program");
    }
    println!("\nEND-TO-END SUCCESS");
    println!("explorer: https://explorer.solana.com/tx/{sig}");
    println!("position: https://explorer.solana.com/address/{position_pda}");
    Ok(())
}

fn run_claim(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("usage: mainnet_stake_e2e claim <keypair_path> <position_pda>");
    }
    let keypair_path = &args[0];
    let position_pda = Pubkey::from_str(&args[1]).context("position pda")?;

    let user =
        read_keypair_file(keypair_path).map_err(|e| anyhow!("read {}: {}", keypair_path, e))?;
    let user_key = user.pubkey();
    let program_id = Pubkey::from_str(COVENANT_STAKE_PROGRAM_ID_STR)?;
    let (config_pda, _) = Pubkey::find_program_address(&[b"stake_config"], &program_id);
    let (reward_vault_pda, _) = Pubkey::find_program_address(&[b"reward_vault"], &program_id);

    let rpc =
        RpcClient::new_with_commitment(MAINNET_RPC.to_string(), CommitmentConfig::confirmed());

    println!("user         = {user_key}");
    println!("position     = {position_pda}");
    println!("config       = {config_pda}");
    println!("reward_vault = {reward_vault_pda}");
    println!();

    let data = anchor_discriminator("claim").to_vec();
    let metas = vec![
        AccountMeta::new(config_pda, false),
        AccountMeta::new(position_pda, false),
        AccountMeta::new(reward_vault_pda, false),
        AccountMeta::new(user_key, true),
    ];
    let ix = Instruction {
        program_id,
        accounts: metas,
        data,
    };

    let lamports_before = rpc.get_balance(&user_key)?;
    let blockhash = rpc.get_latest_blockhash().context("blockhash")?;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&user_key), &[&user], blockhash);
    println!("submitting claim...");
    let result = rpc.send_and_confirm_transaction_with_spinner_and_config(
        &tx,
        CommitmentConfig::confirmed(),
        RpcSendTransactionConfig {
            skip_preflight: false,
            preflight_commitment: Some(CommitmentConfig::confirmed().commitment),
            ..Default::default()
        },
    );

    match result {
        Ok(sig) => {
            let lamports_after = rpc.get_balance(&user_key)?;
            let delta = lamports_after as i128 - lamports_before as i128;
            println!("claim tx = {sig}");
            println!("user balance delta = {delta} lamports");
            println!("(positive delta minus tx fee = claimed amount; negative or zero delta = nothing claimed)");
            println!("CLAIM PATH: SUCCESS");
        }
        Err(e) => {
            let msg = format!("{e}");
            println!("claim returned error: {msg}");
            if msg.contains("NothingToClaim") || msg.contains("0x17d5") {
                println!("CLAIM PATH: NothingToClaim (expected — keeper is in DRY_RUN, no SOL distributed yet)");
            } else {
                println!("CLAIM PATH: unexpected error");
                return Err(e.into());
            }
        }
    }
    Ok(())
}
