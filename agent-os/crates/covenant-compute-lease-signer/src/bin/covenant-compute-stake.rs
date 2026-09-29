//! Stakes CVNT for a Covenant Compute node.
//!
//! ```text
//! covenant-compute-stake status  <node>
//! covenant-compute-stake stake   <node> <cvnt> --lock-days <days> --keypair <wallet>
//! covenant-compute-stake unstake <node> --keypair <wallet>
//! ```
//!
//! `<node>` is the operator identity the node registers with. The stake
//! belongs to the wallet that signs: only it can withdraw, and only once the
//! lock ends. Until then the protocol's slash authority can send it to the
//! treasury. A coordinator that requires stake counts a position until a day
//! plus its dispute window before the lock ends, so stake again from a second
//! wallet before then to stay matched.
//!
//! `--rpc` picks the cluster (mainnet-beta by default), `--program` the
//! settlement program.

use std::process::ExitCode;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use covenant_compute_lease_signer::lease::discriminator;
use covenant_compute_lease_signer::rpc::{Rpc, SendError};
use covenant_compute_lease_signer::stake::{
    config_mint_and_min_lock, mint_decimals, Position, StakeAccounts, DEFAULT_PROGRAM, POSITION_LEN,
};
use serde_json::json;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signer::keypair::{read_keypair_file, Keypair};
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;

const USAGE: &str = "usage:
  covenant-compute-stake status  <node>
  covenant-compute-stake stake   <node> <cvnt> --lock-days <days> --keypair <wallet>
  covenant-compute-stake unstake <node> --keypair <wallet>
options: --rpc <url>  --program <settlement program id>";
const DEFAULT_RPC: &str = "https://api.mainnet-beta.solana.com";
const PRIORITY_MICRO_LAMPORTS: u64 = 100_000;
const COMPUTE_UNITS: u32 = 200_000;
const CONFIRM: Duration = Duration::from_secs(90);
/// Added to every lock so a lock of exactly the program's minimum survives a
/// cluster clock running ahead of ours.
const LOCK_SLACK_SECS: u64 = 600;
/// `Agent.active`: discriminator, agent_key, operator, metadata_hash,
/// capability_hash, stake, reputation.
const AGENT_ACTIVE_OFFSET: usize = 8 + 32 * 4 + 8 + 8;

#[tokio::main]
async fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("covenant-compute-stake: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Default)]
struct Args {
    positional: Vec<String>,
    keypair: Option<String>,
    rpc: Option<String>,
    program: Option<String>,
    lock_days: Option<String>,
}

fn parse(argv: Vec<String>) -> Result<Args, String> {
    let mut args = Args::default();
    let mut argv = argv.into_iter();
    while let Some(arg) = argv.next() {
        let slot = match arg.as_str() {
            "--keypair" => &mut args.keypair,
            "--rpc" => &mut args.rpc,
            "--program" => &mut args.program,
            "--lock-days" => &mut args.lock_days,
            "-h" | "--help" => return Err(USAGE.into()),
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{USAGE}"))
            }
            _ => {
                args.positional.push(arg);
                continue;
            }
        };
        *slot = Some(argv.next().ok_or_else(|| format!("{arg} needs a value"))?);
    }
    Ok(args)
}

enum Command {
    Status(Pubkey),
    Stake {
        node: Pubkey,
        amount: String,
        lock_days: u64,
        owner: Keypair,
    },
    Unstake {
        node: Pubkey,
        owner: Keypair,
    },
}

fn command(args: &Args) -> Result<Command, String> {
    let positional: Vec<&str> = args.positional.iter().map(String::as_str).collect();
    Ok(match positional.as_slice() {
        ["status", node] => Command::Status(parse_node(node)?),
        ["stake", node, amount] => {
            let days = args.lock_days.as_deref().ok_or("stake needs --lock-days")?;
            Command::Stake {
                node: parse_node(node)?,
                amount: amount.to_string(),
                lock_days: days
                    .parse()
                    .map_err(|_| format!("--lock-days {days} is not a whole number of days"))?,
                owner: wallet(args)?,
            }
        }
        ["unstake", node] => Command::Unstake {
            node: parse_node(node)?,
            owner: wallet(args)?,
        },
        _ => return Err(USAGE.into()),
    })
}

async fn run(argv: Vec<String>) -> Result<(), String> {
    let args = parse(argv)?;
    let command = command(&args)?;
    let program = match &args.program {
        Some(id) => Pubkey::from_str(id).map_err(|e| format!("--program {id}: {e}"))?,
        None => DEFAULT_PROGRAM,
    };
    let chain = Chain::read(args.rpc.as_deref().unwrap_or(DEFAULT_RPC), program).await?;
    match command {
        Command::Status(node) => chain.status(node).await,
        Command::Stake {
            node,
            amount,
            lock_days,
            owner,
        } => chain.stake(node, &owner, &amount, lock_days).await,
        Command::Unstake { node, owner } => chain.unstake(node, &owner).await,
    }
}

fn parse_node(node: &str) -> Result<Pubkey, String> {
    Pubkey::from_str(node).map_err(|e| format!("node {node}: {e}"))
}

fn wallet(args: &Args) -> Result<Keypair, String> {
    let path = args
        .keypair
        .as_deref()
        .ok_or("this moves CVNT; name the wallet with --keypair")?;
    read_keypair_file(path).map_err(|e| format!("read {path}: {e}"))
}

struct Chain {
    rpc: Rpc,
    program: Pubkey,
    mint: Pubkey,
    token_program: Pubkey,
    decimals: u8,
    min_lock_secs: u64,
}

impl Chain {
    async fn read(url: &str, program: Pubkey) -> Result<Self, String> {
        let rpc = Rpc::new(url);
        let config = Pubkey::find_program_address(&[b"config"], &program).0;
        let account = rpc
            .account(&config)
            .await?
            .filter(|account| account.owner == program)
            .ok_or_else(|| format!("{program} has no settlement config on this cluster"))?;
        let (mint, min_lock_secs) = config_mint_and_min_lock(&account.data)
            .ok_or_else(|| format!("config {config} is not a settlement config"))?;
        let mint_account = rpc
            .account(&mint)
            .await?
            .ok_or_else(|| format!("stake mint {mint} not found"))?;
        let decimals = mint_decimals(&mint_account.data)
            .ok_or_else(|| format!("stake mint {mint} is not a mint"))?;
        Ok(Self {
            rpc,
            program,
            mint,
            token_program: mint_account.owner,
            decimals,
            min_lock_secs,
        })
    }

    fn accounts(&self, node: Pubkey, owner: Pubkey) -> StakeAccounts {
        StakeAccounts::derive(
            self.program,
            node.to_bytes(),
            owner,
            self.mint,
            self.token_program,
        )
    }

    async fn status(&self, node: Pubkey) -> Result<(), String> {
        let positions = self
            .rpc
            .program_accounts(
                &self.program,
                json!([
                    {"dataSize": POSITION_LEN},
                    {"memcmp": {
                        "offset": 0,
                        "bytes": BASE64.encode(discriminator("account", "StakePosition")),
                        "encoding": "base64",
                    }},
                    {"memcmp": {"offset": 8, "bytes": node.to_string()}},
                ]),
            )
            .await?;
        let now = unix_now();
        let mut locked = 0u64;
        println!("node {node}");
        for (address, data) in &positions {
            let Some(position) = Position::decode(data) else {
                continue;
            };
            let state = if !position.active {
                "inactive"
            } else if position.lock_until > now {
                locked = locked.saturating_add(position.amount);
                "locked"
            } else {
                "unlocked"
            };
            println!(
                "  {address}: {} CVNT from {}, {state} until {}",
                cvnt(position.amount, self.decimals),
                position.owner,
                utc(position.lock_until),
            );
        }
        if positions.is_empty() {
            println!("  no stake");
        }
        println!("locked: {} CVNT", cvnt(locked, self.decimals));
        Ok(())
    }

    async fn stake(
        &self,
        node: Pubkey,
        owner: &Keypair,
        amount: &str,
        lock_days: u64,
    ) -> Result<(), String> {
        let amount = base_units(amount, self.decimals)?;
        let lock_secs = lock_days.saturating_mul(86_400);
        if lock_secs < self.min_lock_secs {
            return Err(format!(
                "the program locks a stake for at least {} days",
                self.min_lock_secs.div_ceil(86_400)
            ));
        }
        let lock_until = unix_now() + lock_secs + LOCK_SLACK_SECS;
        let accounts = self.accounts(node, owner.pubkey());

        if let Some(existing) = self.position(&accounts).await? {
            return Err(format!(
                "{} already stakes {} CVNT for this node until {}; stake more from another wallet",
                owner.pubkey(),
                cvnt(existing.amount, self.decimals),
                utc(existing.lock_until),
            ));
        }
        let balance = self.token_balance(&accounts.owner_tokens).await?;
        if balance < amount {
            return Err(format!(
                "{} holds {} CVNT, short of {}",
                owner.pubkey(),
                cvnt(balance, self.decimals),
                cvnt(amount, self.decimals),
            ));
        }

        let mut instructions = Vec::new();
        match self.rpc.account(&accounts.agent).await? {
            None => instructions.push(accounts.register_agent()),
            Some(agent) if agent.data.get(AGENT_ACTIVE_OFFSET) != Some(&1) => {
                return Err(format!("node {node} is deactivated in the protocol"));
            }
            Some(_) => {}
        }
        instructions.push(create_associated_token_account_idempotent(
            &owner.pubkey(),
            &accounts.position,
            &self.mint,
            &self.token_program,
        ));
        instructions.push(accounts.stake(amount, lock_until));

        let signature = self.send(owner, instructions).await?;
        println!(
            "staked {} CVNT for {node} until {}: {signature}",
            cvnt(amount, self.decimals),
            utc(lock_until),
        );
        Ok(())
    }

    async fn unstake(&self, node: Pubkey, owner: &Keypair) -> Result<(), String> {
        let accounts = self.accounts(node, owner.pubkey());
        let position = self
            .position(&accounts)
            .await?
            .ok_or_else(|| format!("{} has no stake for {node}", owner.pubkey()))?;
        if position.lock_until > unix_now() {
            return Err(format!("locked until {}", utc(position.lock_until)));
        }
        let instructions = vec![
            create_associated_token_account_idempotent(
                &owner.pubkey(),
                &owner.pubkey(),
                &self.mint,
                &self.token_program,
            ),
            accounts.unstake(),
        ];
        let signature = self.send(owner, instructions).await?;
        println!(
            "withdrew {} CVNT: {signature}",
            cvnt(position.amount, self.decimals)
        );
        Ok(())
    }

    async fn position(&self, accounts: &StakeAccounts) -> Result<Option<Position>, String> {
        let Some(account) = self.rpc.account(&accounts.position).await? else {
            return Ok(None);
        };
        Position::decode(&account.data)
            .map(Some)
            .ok_or_else(|| format!("{} is not a stake position", accounts.position))
    }

    async fn token_balance(&self, address: &Pubkey) -> Result<u64, String> {
        let Some(account) = self.rpc.account(address).await? else {
            return Ok(0);
        };
        let bytes = account
            .data
            .get(64..72)
            .ok_or_else(|| format!("{address} is not a token account"))?;
        Ok(u64::from_le_bytes(bytes.try_into().expect("8 bytes")))
    }

    async fn send(
        &self,
        payer: &Keypair,
        instructions: Vec<Instruction>,
    ) -> Result<String, String> {
        let mut all = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_UNITS),
            ComputeBudgetInstruction::set_compute_unit_price(PRIORITY_MICRO_LAMPORTS),
        ];
        all.extend(instructions);
        let blockhash = self.rpc.latest_blockhash().await?;
        let mut tx = Transaction::new_with_payer(&all, Some(&payer.pubkey()));
        tx.try_sign(&[payer], blockhash)
            .map_err(|e| format!("sign: {e}"))?;
        self.rpc
            .send_and_confirm(&tx, CONFIRM)
            .await
            .map_err(|error| match error {
                SendError::Refused(message) => message,
                SendError::Unknown { signature, message } => format!(
                    "{message}; transaction {signature} may still land, run status before trying again"
                ),
            })
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `text` CVNT in the mint's base units.
fn base_units(text: &str, decimals: u8) -> Result<u64, String> {
    let invalid = || format!("{text:?} is not an amount of CVNT with at most {decimals} decimals");
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    if (whole.is_empty() && fraction.is_empty())
        || fraction.len() > usize::from(decimals)
        || !digits(whole)
        || !digits(fraction)
    {
        return Err(invalid());
    }
    let padded = format!("{whole}{fraction:0<width$}", width = usize::from(decimals));
    padded
        .parse::<u64>()
        .ok()
        .filter(|&units| units > 0)
        .ok_or_else(invalid)
}

fn cvnt(units: u64, decimals: u8) -> String {
    let scale = 10u64.pow(u32::from(decimals));
    let fraction = units % scale;
    if fraction == 0 {
        return (units / scale).to_string();
    }
    let fraction = format!("{fraction:0width$}", width = usize::from(decimals));
    format!("{}.{}", units / scale, fraction.trim_end_matches('0'))
}

/// `secs` since the epoch as a UTC date and time, by Howard Hinnant's
/// days-to-civil algorithm.
fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let clock = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        clock / 3_600,
        clock % 3_600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_convert_exactly() {
        assert_eq!(base_units("1000000", 6), Ok(1_000_000_000_000));
        assert_eq!(base_units("0.5", 6), Ok(500_000));
        assert_eq!(base_units("2.000001", 6), Ok(2_000_001));
        assert!(base_units("0", 6).is_err());
        assert!(base_units("1.0000001", 6).is_err());
        assert!(base_units("1e6", 6).is_err());
        assert!(base_units("-1", 6).is_err());
        assert!(base_units("99999999999999999999", 6).is_err());
        assert_eq!(cvnt(1_000_000_000_000, 6), "1000000");
        assert_eq!(cvnt(2_000_001, 6), "2.000001");
        assert_eq!(cvnt(500_000, 6), "0.5");
    }

    #[test]
    fn dates_print_in_utc() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(utc(1_900_000_000), "2030-03-17 17:46 UTC");
    }

    #[test]
    fn options_take_values_and_the_rest_is_positional() {
        let args = parse(
            [
                "stake",
                "N",
                "5",
                "--lock-days",
                "30",
                "--keypair",
                "w.json",
            ]
            .map(String::from)
            .to_vec(),
        )
        .unwrap();
        assert_eq!(args.positional, ["stake", "N", "5"]);
        assert_eq!(args.lock_days.as_deref(), Some("30"));
        assert_eq!(args.keypair.as_deref(), Some("w.json"));
        assert!(parse(vec!["--rpc".into()]).is_err());
        assert!(parse(vec!["--lock".into(), "3".into()]).is_err());
    }
}
