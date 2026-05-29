//! Devnet smoke driver for covenant-stake.
//!
//! Steps when invoked with `pdas`:
//!   * Print all PDA addresses for the deployed program.
//!
//! Steps when invoked with `initialize <mint> <locked_vault> <buylock_vault>`:
//!   * Send `initialize` with the deployer keypair as authority and as
//!     pause_authority + fee_router_authority. Verify Config and FeeRouter.
//!
//! Steps when invoked with `smoke <mint> <locked_vault>`:
//!   * Build a user keypair, fund it, create ATA, mint 5_000 CVNT to it.
//!   * `create_position(nonce=1, amount=2_000_000_000, lock_tier_bps=10_000)`.
//!   * `deposit_sol_fees(1_000_000)` from the deployer (also fee_router auth).
//!   * `claim` and assert SOL delta on the user.

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
use solana_sdk::system_instruction;
use solana_sdk::transaction::Transaction;

const DEVNET_RPC: &str = "https://devnet.helius-rpc.com/?api-key=96047715-56a7-4ac4-aaa2-41fba9797a90";

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: devnet_smoke <pdas|initialize|smoke> [args...]");
        std::process::exit(2);
    }
    let program_id = Pubkey::from_str(COVENANT_STAKE_PROGRAM_ID_STR)?;
    let (config_pda, _) = Pubkey::find_program_address(&[b"stake_config"], &program_id);
    let (fee_router_pda, _) = Pubkey::find_program_address(&[b"fee_router"], &program_id);
    let (reward_vault_pda, _) = Pubkey::find_program_address(&[b"reward_vault"], &program_id);
    let (locked_auth_pda, _) = Pubkey::find_program_address(&[b"vault_auth"], &program_id);
    let (buylock_auth_pda, _) = Pubkey::find_program_address(&[b"buylock_auth"], &program_id);

    match args[0].as_str() {
        "pdas" => {
            println!("program_id            = {program_id}");
            println!("config                = {config_pda}");
            println!("fee_router            = {fee_router_pda}");
            println!("reward_vault          = {reward_vault_pda}");
            println!("locked_vault_authority= {locked_auth_pda}");
            println!("buylock_vault_authority={buylock_auth_pda}");
            Ok(())
        }
        "initialize" => {
            if args.len() < 4 {
                bail!("usage: devnet_smoke initialize <mint> <locked_vault> <buylock_vault>");
            }
            let mint = Pubkey::from_str(&args[1])?;
            let locked_vault = Pubkey::from_str(&args[2])?;
            let buylock_vault = Pubkey::from_str(&args[3])?;

            let rpc = RpcClient::new_with_commitment(DEVNET_RPC.to_string(), CommitmentConfig::confirmed());
            let deployer = load_default_keypair()?;
            println!("deployer = {}", deployer.pubkey());
            println!("mint     = {mint}");

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
                pause_authority: deployer.pubkey().to_bytes(),
                fee_router_authority: deployer.pubkey().to_bytes(),
                min_lock_amount: 1_000_000_000,
                fee_router_max_deposit_lamports: 500_000_000,
                fee_router_rate_limit_secs: 60,
            };
            init_args.serialize(&mut data)?;

            let (program_data, _) = Pubkey::find_program_address(
                &[program_id.as_ref()],
                &solana_sdk::bpf_loader_upgradeable::ID,
            );
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
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
            ];
            let ix = Instruction {
                program_id,
                accounts: metas,
                data,
            };
            let sig = send(&rpc, &deployer, &[ix], &[])?;
            println!("initialize tx = {sig}");

            // Verify Config exists by reading it.
            let cfg = rpc
                .get_account(&config_pda)
                .context("fetch config")?;
            println!("config owner   = {}", cfg.owner);
            println!("config len     = {}", cfg.data.len());
            let fr = rpc.get_account(&fee_router_pda).context("fetch fee_router")?;
            println!("fee_router owner = {}", fr.owner);
            let rv = rpc.get_account(&reward_vault_pda).context("fetch reward_vault")?;
            println!("reward_vault owner = {}, lamports = {}", rv.owner, rv.lamports);
            Ok(())
        }
        "smoke" => {
            if args.len() < 3 {
                bail!("usage: devnet_smoke smoke <mint> <locked_vault>");
            }
            let mint = Pubkey::from_str(&args[1])?;
            let locked_vault = Pubkey::from_str(&args[2])?;

            let rpc = RpcClient::new_with_commitment(DEVNET_RPC.to_string(), CommitmentConfig::confirmed());
            let deployer = load_default_keypair()?;

            // Create a fresh user, fund them, give them an ATA.
            let user = Keypair::new();
            println!("user = {}", user.pubkey());
            let fund_ix =
                system_instruction::transfer(&deployer.pubkey(), &user.pubkey(), 200_000_000);
            send(&rpc, &deployer, &[fund_ix], &[])?;

            // Create user's $CVNT ATA. Use legacy SPL associated_token_account.
            let ata = spl_associated_token_account::get_associated_token_address(
                &user.pubkey(),
                &mint,
            );
            let create_ata = spl_associated_token_account::instruction::create_associated_token_account(
                &deployer.pubkey(),
                &user.pubkey(),
                &mint,
                &spl_token::ID,
            );
            send(&rpc, &deployer, &[create_ata], &[])?;
            println!("user ata = {ata}");

            // Mint 5_000 CVNT to user (deployer is mint authority).
            let mint_to_ix = spl_token::instruction::mint_to(
                &spl_token::ID,
                &mint,
                &ata,
                &deployer.pubkey(),
                &[],
                5_000_000_000,
            )?;
            send(&rpc, &deployer, &[mint_to_ix], &[])?;

            // Use a unique nonce per smoke run so subsequent invocations don't
            // collide on the [b"stake_v2", user, nonce] PDA. We generate a
            // random user above so the seed combination stays unique either
            // way, but explicit args.get(3) lets the operator override.
            let nonce: u64 = args
                .get(3)
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            let amount: u64 = 2_000_000_000;
            let tier_bps: u16 = 10_000;
            let (position_pda, _) = Pubkey::find_program_address(
                &[b"stake_v2", user.pubkey().as_ref(), &nonce.to_le_bytes()],
                &program_id,
            );

            let mut data = anchor_discriminator("create_position").to_vec();
            nonce.serialize(&mut data)?;
            amount.serialize(&mut data)?;
            tier_bps.serialize(&mut data)?;

            let metas = vec![
                AccountMeta::new(config_pda, false),
                AccountMeta::new(position_pda, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(locked_auth_pda, false),
                AccountMeta::new(locked_vault, false),
                AccountMeta::new(ata, false),
                AccountMeta::new(user.pubkey(), true),
                AccountMeta::new_readonly(spl_token::ID, false),
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
                &[&user],
            )?;
            println!("create_position tx = {sig}");
            println!("position pda = {position_pda}");

            // deposit_sol_fees(1_000_000) from deployer (fee_router auth)
            let mut data = anchor_discriminator("deposit_sol_fees").to_vec();
            1_000_000u64.serialize(&mut data)?;
            let metas = vec![
                AccountMeta::new(config_pda, false),
                AccountMeta::new(fee_router_pda, false),
                AccountMeta::new(reward_vault_pda, false),
                AccountMeta::new(deployer.pubkey(), true),
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
            println!("deposit_sol_fees tx = {sig}");

            // claim
            let data = anchor_discriminator("claim").to_vec();
            let metas = vec![
                AccountMeta::new(config_pda, false),
                AccountMeta::new(position_pda, false),
                AccountMeta::new(reward_vault_pda, false),
                AccountMeta::new(user.pubkey(), true),
            ];
            let user_before = rpc.get_balance(&user.pubkey()).unwrap();
            let sig = send(
                &rpc,
                &deployer,
                &[Instruction {
                    program_id,
                    accounts: metas,
                    data,
                }],
                &[&user],
            )?;
            let user_after = rpc.get_balance(&user.pubkey()).unwrap();
            println!("claim tx = {sig}");
            println!("user SOL delta = {} lamports (expected ~1_000_000 less tx fee)", user_after as i64 - user_before as i64);
            Ok(())
        }
        other => bail!("unknown subcommand: {other}"),
    }
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
