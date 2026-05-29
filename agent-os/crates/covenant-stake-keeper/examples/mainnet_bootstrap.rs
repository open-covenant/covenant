//! Mainnet bootstrap for covenant-stake.
//!
//! Subcommands:
//!   * `pdas` — print all PDA addresses (handy for the spl-token vault setup).
//!   * `vault-setup` — create the locked-CVNT and buylock-CVNT ATAs against
//!     the production Token-2022 mint, owned by the respective PDA authorities.
//!     Uses the `spl-token` CLI under the hood is recommended; this command
//!     just prints the exact commands to run if you want a script.
//!   * `initialize <locked_vault> <buylock_vault> <pause_authority>` — sends
//!     the on-chain `initialize` with conservative production parameters
//!     (10k CVNT min lock, 0.5 SOL max deposit, 60s rate limit). The fee
//!     router authority is set to the creator wallet pubkey.
//!   * `genesis-position` — opens a small founder position (1000 CVNT at the
//!     30d tier) from the deployer wallet so `total_weight > 0` at t=0 and
//!     the keeper's first deposit doesn't trip the B2 guard.

use std::env;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use borsh::BorshSerialize;
use covenant_stake_keeper::{anchor_discriminator, COVENANT_STAKE_PROGRAM_ID_STR};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{read_keypair_file, Keypair};
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;

const MAINNET_RPC: &str = "https://api.mainnet-beta.solana.com";
const CVNT_MAINNET_MINT: &str = "2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump";
const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
const CREATOR_WALLET: &str = "2JXuvXb6Q5YREk9KmhtgNmseq2aKtYnu5zLRi2i5Vaeb";

const MIN_LOCK_AMOUNT: u64 = 10_000_000_000;
const MAX_DEPOSIT_LAMPORTS: u64 = 500_000_000;
const RATE_LIMIT_SECS: i64 = 60;

const GENESIS_POSITION_AMOUNT: u64 = 10_000_000_000;
const GENESIS_POSITION_TIER_BPS: u16 = 10_000;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        std::process::exit(2);
    }
    let program_id = Pubkey::from_str(COVENANT_STAKE_PROGRAM_ID_STR)?;
    let mint = Pubkey::from_str(CVNT_MAINNET_MINT)?;
    let token_program = Pubkey::from_str(TOKEN_2022_PROGRAM_ID)?;
    let creator_wallet = Pubkey::from_str(CREATOR_WALLET)?;

    let (config_pda, _) = Pubkey::find_program_address(&[b"stake_config"], &program_id);
    let (fee_router_pda, _) = Pubkey::find_program_address(&[b"fee_router"], &program_id);
    let (reward_vault_pda, _) = Pubkey::find_program_address(&[b"reward_vault"], &program_id);
    let (locked_auth_pda, _) = Pubkey::find_program_address(&[b"vault_auth"], &program_id);
    let (buylock_auth_pda, _) = Pubkey::find_program_address(&[b"buylock_auth"], &program_id);
    let (program_data, _) = Pubkey::find_program_address(
        &[program_id.as_ref()],
        &solana_sdk::bpf_loader_upgradeable::ID,
    );

    match args[0].as_str() {
        "pdas" => {
            println!("program_id              = {program_id}");
            println!("mint                    = {mint}");
            println!("token_program           = {token_program}");
            println!("creator_wallet          = {creator_wallet}");
            println!("config                  = {config_pda}");
            println!("fee_router              = {fee_router_pda}");
            println!("reward_vault            = {reward_vault_pda}");
            println!("locked_vault_authority  = {locked_auth_pda}");
            println!("buylock_vault_authority = {buylock_auth_pda}");
            println!("program_data            = {program_data}");
            Ok(())
        }
        "vault-setup" => {
            println!(
                "# Run these in a separate terminal. Confirm fee-payer balance first.\n"
            );
            println!("spl-token --program-id {token_program} \\");
            println!("    --url {MAINNET_RPC} \\");
            println!("    create-account {mint} \\");
            println!("    --owner {locked_auth_pda} \\");
            println!("    --fee-payer ~/.config/solana/id.json\n");
            println!("spl-token --program-id {token_program} \\");
            println!("    --url {MAINNET_RPC} \\");
            println!("    create-account {mint} \\");
            println!("    --owner {buylock_auth_pda} \\");
            println!("    --fee-payer ~/.config/solana/id.json\n");
            println!(
                "# Record the two ATA addresses printed by spl-token — pass them to `initialize`."
            );
            Ok(())
        }
        "initialize" => {
            if args.len() < 4 {
                bail!(
                    "usage: mainnet_bootstrap initialize <locked_vault> <buylock_vault> <pause_authority>"
                );
            }
            let locked_vault = Pubkey::from_str(&args[1])?;
            let buylock_vault = Pubkey::from_str(&args[2])?;
            let pause_authority = Pubkey::from_str(&args[3])?;

            let rpc = RpcClient::new_with_commitment(
                MAINNET_RPC.to_string(),
                CommitmentConfig::confirmed(),
            );
            let deployer = load_default_keypair()?;
            println!("deployer          = {}", deployer.pubkey());
            println!("pause_authority   = {pause_authority}");
            println!("fee_router_auth   = {creator_wallet}");
            println!("min_lock_amount   = {MIN_LOCK_AMOUNT} ({} CVNT)", MIN_LOCK_AMOUNT / 1_000_000);
            println!("max_deposit       = {MAX_DEPOSIT_LAMPORTS} lamports ({} SOL)", MAX_DEPOSIT_LAMPORTS as f64 / 1e9);
            println!("rate_limit        = {RATE_LIMIT_SECS}s");
            println!();
            println!("press Ctrl-C now if any of the above looks wrong; waiting 5s...");
            std::thread::sleep(std::time::Duration::from_secs(5));

            let mut data = anchor_discriminator("initialize").to_vec();
            #[derive(BorshSerialize)]
            struct InitializeArgs {
                pause_authority: [u8; 32],
                fee_router_authority: [u8; 32],
                min_lock_amount: u64,
                fee_router_max_deposit_lamports: u64,
                fee_router_rate_limit_secs: i64,
            }
            let init_args = InitializeArgs {
                pause_authority: pause_authority.to_bytes(),
                fee_router_authority: creator_wallet.to_bytes(),
                min_lock_amount: MIN_LOCK_AMOUNT,
                fee_router_max_deposit_lamports: MAX_DEPOSIT_LAMPORTS,
                fee_router_rate_limit_secs: RATE_LIMIT_SECS,
            };
            init_args.serialize(&mut data)?;

            let metas = vec![
                AccountMeta::new(config_pda, false),
                AccountMeta::new(fee_router_pda, false),
                AccountMeta::new_readonly(locked_auth_pda, false),
                AccountMeta::new(reward_vault_pda, false),
                AccountMeta::new_readonly(buylock_auth_pda, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(locked_vault, false),
                AccountMeta::new_readonly(buylock_vault, false),
                AccountMeta::new(deployer.pubkey(), true),
                AccountMeta::new_readonly(program_data, false),
                AccountMeta::new_readonly(token_program, false),
                AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
            ];

            let sig = send(
                &rpc,
                &deployer,
                &[Instruction {
                    program_id,
                    accounts: metas,
                    data,
                }],
                &[],
            )?;
            println!("initialize tx = {sig}");
            Ok(())
        }
        "genesis-position" => {
            let rpc = RpcClient::new_with_commitment(
                MAINNET_RPC.to_string(),
                CommitmentConfig::confirmed(),
            );
            let deployer = load_default_keypair()?;
            println!(
                "Opening genesis position: {} CVNT at 30d tier, from {}",
                GENESIS_POSITION_AMOUNT / 1_000_000,
                deployer.pubkey()
            );
            println!("press Ctrl-C now if wrong wallet; waiting 5s...");
            std::thread::sleep(std::time::Duration::from_secs(5));

            let deployer_ata = spl_associated_token_account::get_associated_token_address_with_program_id(
                &deployer.pubkey(),
                &mint,
                &token_program,
            );
            let locked_vault = spl_associated_token_account::get_associated_token_address_with_program_id(
                &locked_auth_pda,
                &mint,
                &token_program,
            );

            let nonce: u64 = 1;
            let position = Pubkey::find_program_address(
                &[b"stake_v2", deployer.pubkey().as_ref(), &nonce.to_le_bytes()],
                &program_id,
            )
            .0;

            let mut data = anchor_discriminator("create_position").to_vec();
            nonce.serialize(&mut data)?;
            GENESIS_POSITION_AMOUNT.serialize(&mut data)?;
            GENESIS_POSITION_TIER_BPS.serialize(&mut data)?;

            let metas = vec![
                AccountMeta::new(config_pda, false),
                AccountMeta::new(position, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(locked_auth_pda, false),
                AccountMeta::new(locked_vault, false),
                AccountMeta::new(deployer_ata, false),
                AccountMeta::new(deployer.pubkey(), true),
                AccountMeta::new_readonly(token_program, false),
                AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
            ];
            let sig = send(
                &rpc,
                &deployer,
                &[Instruction {
                    program_id,
                    accounts: metas,
                    data,
                }],
                &[],
            )?;
            println!("genesis create_position tx = {sig}");
            println!("position pda = {position}");
            println!("Keeper can now run without tripping B2 (NoActiveStakers).");
            Ok(())
        }
        _ => {
            usage();
            std::process::exit(2);
        }
    }
}

fn usage() {
    eprintln!(
        "usage: mainnet_bootstrap <pdas|vault-setup|initialize|genesis-position> [args...]"
    );
}

fn load_default_keypair() -> Result<Keypair> {
    let home = env::var("HOME").context("HOME unset")?;
    let path = format!("{home}/.config/solana/id.json");
    read_keypair_file(&path).map_err(|e| anyhow!("read keypair {}: {}", path, e))
}

fn send(
    rpc: &RpcClient,
    payer: &Keypair,
    ixs: &[Instruction],
    extra: &[&Keypair],
) -> Result<String> {
    let blockhash = rpc.get_latest_blockhash().context("blockhash")?;
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra);
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &signers, blockhash);
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
    Ok(sig.to_string())
}
