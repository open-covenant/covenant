//! Stakes CVNT for a Covenant Compute node.
//!
//! ```text
//! covenant-compute-stake status  <node>
//! covenant-compute-stake stake   <node> <cvnt> --lock-days <days> --keypair <wallet>
//! covenant-compute-stake extend  <node> [--add <cvnt>] [--lock-days <days>] --keypair <wallet>
//! covenant-compute-stake unstake <node> --keypair <wallet>
//! covenant-compute-stake slash   <node> <base units> --reason <sha256 hex> --keypair <slash authority>
//! ```
//!
//! `<node>` is the operator identity the node registers with. The stake
//! belongs to the wallet that signs: only it can withdraw, and only once the
//! lock ends. Until then the protocol's slash authority can send it to the
//! treasury. A coordinator that requires stake counts a position until a day
//! plus its dispute window before the lock ends, so `extend` the lock before
//! then to stay matched.
//!
//! `slash` is the coordinator's: it prints one JSON line, `{"signature": ..}`
//! on success or `{"error": .., "stage": "not_submitted" | "maybe_submitted"}`
//! on failure, like the lease signer. Each slash carries a memo naming its
//! reason, and a slash whose reason already landed answers with that
//! transaction instead of taking the stake twice.
//!
//! `--rpc` (or `COVENANT_COMPUTE_STAKE_RPC_URL`) picks the cluster,
//! mainnet-beta by default; `--keypair` falls back to
//! `COVENANT_COMPUTE_STAKE_KEYPAIR`; `--program` names the settlement
//! program.

use std::process::ExitCode;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use covenant_compute_lease_signer::lease::{discriminator, memo};
use covenant_compute_lease_signer::rpc::{Rpc, SendError};
use covenant_compute_lease_signer::stake::{
    mint_decimals, parse_hash, slash_memo, Position, ProtocolConfig, StakeAccounts,
    DEFAULT_PROGRAM, POSITION_LEN,
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
  covenant-compute-stake extend  <node> [--add <cvnt>] [--lock-days <days>] --keypair <wallet>
  covenant-compute-stake unstake <node> --keypair <wallet>
  covenant-compute-stake slash   <node> <base units> --reason <sha256 hex> --keypair <slash authority>
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let answers_in_json = argv.first().map(String::as_str) == Some("slash");
    match run(argv).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            if answers_in_json {
                println!(
                    "{}",
                    json!({"error": failure.error, "stage": failure.stage})
                );
            }
            eprintln!("covenant-compute-stake: {}", failure.error);
            ExitCode::FAILURE
        }
    }
}

/// Why a command stopped. `stage` matters only to `slash`: whether a
/// transaction that moves stake may have landed.
struct Failure {
    error: String,
    stage: &'static str,
}

impl From<String> for Failure {
    fn from(error: String) -> Self {
        Self {
            error,
            stage: "not_submitted",
        }
    }
}

impl From<&str> for Failure {
    fn from(error: &str) -> Self {
        error.to_string().into()
    }
}

#[derive(Default)]
struct Args {
    positional: Vec<String>,
    keypair: Option<String>,
    rpc: Option<String>,
    program: Option<String>,
    lock_days: Option<String>,
    add: Option<String>,
    reason: Option<String>,
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
            "--add" => &mut args.add,
            "--reason" => &mut args.reason,
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
    Extend {
        node: Pubkey,
        add: Option<String>,
        lock_days: Option<u64>,
        owner: Keypair,
    },
    Unstake {
        node: Pubkey,
        owner: Keypair,
    },
    Slash {
        node: Pubkey,
        amount: u64,
        reason: [u8; 32],
        authority: Keypair,
    },
}

fn command(args: &Args) -> Result<Command, String> {
    let positional: Vec<&str> = args.positional.iter().map(String::as_str).collect();
    Ok(match positional.as_slice() {
        ["status", node] => Command::Status(parse_node(node)?),
        ["stake", node, amount] => Command::Stake {
            node: parse_node(node)?,
            amount: amount.to_string(),
            lock_days: lock_days(args)?.ok_or("stake needs --lock-days")?,
            owner: wallet(args)?,
        },
        ["extend", node] => {
            let lock_days = lock_days(args)?;
            if args.add.is_none() && lock_days.is_none() {
                return Err("extend needs --add, --lock-days or both".into());
            }
            Command::Extend {
                node: parse_node(node)?,
                add: args.add.clone(),
                lock_days,
                owner: wallet(args)?,
            }
        }
        ["unstake", node] => Command::Unstake {
            node: parse_node(node)?,
            owner: wallet(args)?,
        },
        ["slash", node, amount] => Command::Slash {
            node: parse_node(node)?,
            amount: amount
                .parse()
                .ok()
                .filter(|&units: &u64| units > 0)
                .ok_or_else(|| format!("{amount} is not a positive number of base units"))?,
            reason: args
                .reason
                .as_deref()
                .and_then(parse_hash)
                .ok_or("slash needs --reason, 64 hex characters")?,
            authority: wallet(args)?,
        },
        _ => return Err(USAGE.into()),
    })
}

async fn run(argv: Vec<String>) -> Result<(), Failure> {
    let args = parse(argv)?;
    let command = command(&args)?;
    let program = match &args.program {
        Some(id) => Pubkey::from_str(id).map_err(|e| format!("--program {id}: {e}"))?,
        None => DEFAULT_PROGRAM,
    };
    let rpc = args
        .rpc
        .clone()
        .or_else(|| std::env::var("COVENANT_COMPUTE_STAKE_RPC_URL").ok())
        .unwrap_or_else(|| DEFAULT_RPC.to_string());
    let chain = Chain::read(&rpc, program).await?;
    match command {
        Command::Status(node) => chain.status(node).await,
        Command::Stake {
            node,
            amount,
            lock_days,
            owner,
        } => chain.stake(node, &owner, &amount, lock_days).await,
        Command::Extend {
            node,
            add,
            lock_days,
            owner,
        } => chain.extend(node, &owner, add.as_deref(), lock_days).await,
        Command::Unstake { node, owner } => chain.unstake(node, &owner).await,
        Command::Slash {
            node,
            amount,
            reason,
            authority,
        } => chain.slash(node, &authority, amount, reason).await,
    }
}

fn parse_node(node: &str) -> Result<Pubkey, String> {
    Pubkey::from_str(node).map_err(|e| format!("node {node}: {e}"))
}

fn lock_days(args: &Args) -> Result<Option<u64>, String> {
    args.lock_days
        .as_deref()
        .map(|days| {
            days.parse()
                .map_err(|_| format!("--lock-days {days} is not a whole number of days"))
        })
        .transpose()
}

fn wallet(args: &Args) -> Result<Keypair, String> {
    let path = args
        .keypair
        .clone()
        .or_else(|| std::env::var("COVENANT_COMPUTE_STAKE_KEYPAIR").ok())
        .ok_or("this moves CVNT; name the wallet with --keypair")?;
    read_keypair_file(&path).map_err(|e| format!("read {path}: {e}"))
}

struct Chain {
    rpc: Rpc,
    program: Pubkey,
    config: ProtocolConfig,
    token_program: Pubkey,
    decimals: u8,
}

impl Chain {
    async fn read(url: &str, program: Pubkey) -> Result<Self, String> {
        let rpc = Rpc::new(url);
        let address = Pubkey::find_program_address(&[b"config"], &program).0;
        let account = rpc
            .account(&address)
            .await?
            .filter(|account| account.owner == program)
            .ok_or_else(|| format!("{program} has no settlement config on this cluster"))?;
        let config = ProtocolConfig::decode(&account.data)
            .ok_or_else(|| format!("config {address} is not a settlement config"))?;
        let mint_account = rpc
            .account(&config.mint)
            .await?
            .ok_or_else(|| format!("stake mint {} not found", config.mint))?;
        let decimals = mint_decimals(&mint_account.data)
            .ok_or_else(|| format!("stake mint {} is not a mint", config.mint))?;
        Ok(Self {
            rpc,
            program,
            config,
            token_program: mint_account.owner,
            decimals,
        })
    }

    fn accounts(&self, node: Pubkey, owner: Pubkey) -> StakeAccounts {
        StakeAccounts::derive(
            self.program,
            node.to_bytes(),
            owner,
            self.config.mint,
            self.token_program,
        )
    }

    /// Every stake position naming `node`, from any owner.
    async fn positions(&self, node: Pubkey) -> Result<Vec<(Pubkey, Position)>, String> {
        let accounts = self
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
        Ok(accounts
            .into_iter()
            .filter_map(|(address, data)| Some((address, Position::decode(&data)?)))
            .collect())
    }

    async fn status(&self, node: Pubkey) -> Result<(), Failure> {
        let positions = self.positions(node).await?;
        let now = unix_now();
        let mut locked = 0u64;
        println!("node {node}");
        for (address, position) in &positions {
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
    ) -> Result<(), Failure> {
        let amount = base_units(amount, self.decimals)?;
        let lock_until = self.lock_until(lock_days)?;
        let accounts = self.accounts(node, owner.pubkey());

        if let Some(existing) = self.position(&accounts).await? {
            return Err(format!(
                "{} already stakes {} CVNT for this node until {}; use extend to add to it",
                owner.pubkey(),
                cvnt(existing.amount, self.decimals),
                utc(existing.lock_until),
            )
            .into());
        }
        self.require_balance(owner, &accounts, amount).await?;

        let mut instructions = Vec::new();
        match self.rpc.account(&accounts.agent).await? {
            None => instructions.push(accounts.register_agent()),
            Some(agent) if agent.data.get(AGENT_ACTIVE_OFFSET) != Some(&1) => {
                return Err(format!("node {node} is deactivated in the protocol").into());
            }
            Some(_) => {}
        }
        instructions.push(create_associated_token_account_idempotent(
            &owner.pubkey(),
            &accounts.position,
            &self.config.mint,
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

    async fn extend(
        &self,
        node: Pubkey,
        owner: &Keypair,
        add: Option<&str>,
        lock_days: Option<u64>,
    ) -> Result<(), Failure> {
        let accounts = self.accounts(node, owner.pubkey());
        let position = self
            .position(&accounts)
            .await?
            .ok_or_else(|| format!("{} has no stake for {node}", owner.pubkey()))?;
        if !position.active {
            return Err("this position was slashed to zero; unstake it and stake again".into());
        }
        let add = add
            .map(|text| base_units(text, self.decimals))
            .transpose()?
            .unwrap_or(0);
        let lock_until = match lock_days {
            Some(days) => self.lock_until(days)?,
            None => position.lock_until,
        };
        if lock_until < position.lock_until {
            return Err(format!(
                "already locked until {}; a lock only moves forward",
                utc(position.lock_until)
            )
            .into());
        }
        if add > 0 {
            self.require_balance(owner, &accounts, add).await?;
        }

        let signature = self
            .send(owner, vec![accounts.extend(add, lock_until)])
            .await?;
        println!(
            "{} CVNT staked for {node} until {}: {signature}",
            cvnt(position.amount.saturating_add(add), self.decimals),
            utc(lock_until),
        );
        Ok(())
    }

    async fn unstake(&self, node: Pubkey, owner: &Keypair) -> Result<(), Failure> {
        let accounts = self.accounts(node, owner.pubkey());
        let position = self
            .position(&accounts)
            .await?
            .ok_or_else(|| format!("{} has no stake for {node}", owner.pubkey()))?;
        if position.lock_until > unix_now() {
            return Err(format!("locked until {}", utc(position.lock_until)).into());
        }
        let instructions = vec![
            create_associated_token_account_idempotent(
                &owner.pubkey(),
                &owner.pubkey(),
                &self.config.mint,
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

    /// Takes up to `amount` from the node's largest live position. A slash
    /// with this `reason` already on chain is answered, not repeated.
    async fn slash(
        &self,
        node: Pubkey,
        authority: &Keypair,
        amount: u64,
        reason: [u8; 32],
    ) -> Result<(), Failure> {
        if authority.pubkey() != self.config.slash_authority {
            return Err(format!(
                "{} is not the protocol's slash authority",
                authority.pubkey()
            )
            .into());
        }
        let positions = self.positions(node).await?;
        let memo_text = slash_memo(&reason);
        for (address, _) in &positions {
            if let Some(signature) = self.rpc.signature_with_memo(address, &memo_text).await? {
                println!(
                    "{}",
                    json!({"signature": signature, "position": address.to_string(), "already": true})
                );
                return Ok(());
            }
        }
        let (address, position) = positions
            .into_iter()
            .filter(|(_, position)| position.active && position.amount > 0)
            .max_by_key(|(_, position)| position.amount)
            .ok_or_else(|| format!("node {node} has no live stake to slash"))?;
        let take = amount.min(position.amount);
        let accounts = self.accounts(node, position.owner);
        let instructions = vec![
            memo(&memo_text),
            accounts.slash(authority.pubkey(), self.config.treasury, take, reason),
        ];
        let signature = self.send(authority, instructions).await?;
        println!(
            "{}",
            json!({"signature": signature, "position": address.to_string(), "amount": take})
        );
        Ok(())
    }

    fn lock_until(&self, lock_days: u64) -> Result<u64, String> {
        let lock_secs = lock_days.saturating_mul(86_400);
        if lock_secs < self.config.min_stake_lock {
            return Err(format!(
                "the program locks a stake for at least {} days",
                self.config.min_stake_lock.div_ceil(86_400)
            ));
        }
        Ok(unix_now() + lock_secs + LOCK_SLACK_SECS)
    }

    async fn require_balance(
        &self,
        owner: &Keypair,
        accounts: &StakeAccounts,
        amount: u64,
    ) -> Result<(), String> {
        let balance = self.token_balance(&accounts.owner_tokens).await?;
        if balance < amount {
            return Err(format!(
                "{} holds {} CVNT, short of {}",
                owner.pubkey(),
                cvnt(balance, self.decimals),
                cvnt(amount, self.decimals),
            ));
        }
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
    ) -> Result<String, Failure> {
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
                SendError::Refused(message) => message.into(),
                SendError::Unknown { signature, message } => Failure {
                    error: format!(
                        "{message}; transaction {signature} may still land, run status before trying again"
                    ),
                    stage: "maybe_submitted",
                },
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

    #[test]
    fn a_slash_names_its_reason_and_an_extend_names_a_change() {
        let node = Pubkey::new_unique().to_string();
        let slash = |extra: &[&str]| {
            let mut argv = vec!["slash".to_string(), node.clone(), "50".into()];
            argv.extend(extra.iter().map(|s| s.to_string()));
            command(&parse(argv).unwrap()).err()
        };
        assert!(slash(&[]).unwrap().contains("--reason"));
        assert!(slash(&["--reason", "abc"]).unwrap().contains("--reason"));

        let extend = parse(vec!["extend".into(), node]).unwrap();
        assert!(command(&extend)
            .err()
            .unwrap()
            .contains("--add, --lock-days"));
    }
}
