//! `covenant-compute` — the person's buyer for the Covenant compute
//! network. `covenant-compute-mcp` is the agent's demand path (tools an
//! MCP client calls); this is the human one: fund a wallet, read the
//! live market, buy an inference or batch job, check the receipt, and
//! move unspent balance back out. Every operator commitment is
//! re-verified locally — the coordinator is never trusted with the
//! verdict.
//!
//! It is the same buyer principal as the MCP server: both load the
//! identity under `$COVENANT_COMPUTE_MCP_HOME` (default
//! `$HOME/.covenant-compute-mcp`), so a wallet funded here buys jobs
//! there and the other way round. `COVENANT_COMPUTE_COORDINATOR_URL`
//! is required for everything but `whoami`. `COVENANT_COMPUTE_RPC_URL`
//! (this buyer's own Solana RPC, never the coordinator's suggestion)
//! backs `verify`'s on-chain read-back.
//!
//! Output is human-readable; `--json` prints the underlying record for
//! scripting. Diagnostics go to stderr; stdout carries the answer.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use base64::Engine as _;
use covenant_compute_buyer::{
    apply_patch, cancel_job, capacity, cheapest_matching_ask, claim_deposit, close_lease,
    describe_reworks, describe_round, describe_verdict, dispatch_and_verify, dispatch_signed,
    dispatch_streaming, dispute_job, fetch_job_output, funds_with_deposit_info, hire_agent,
    http_client, lease_view, list_verified_jobs, list_withdrawals, prepare_agent_task,
    preview_value, sign_envelope, submit_streaming, vault_delete, vault_fetch, vault_list,
    vault_store, verify_payout, withdraw, AgentArgs, AgentOutcome, BuyerConfig, DispatchOutcome,
    JobRequest, PriceQuote, PurchaseBook, PurchaseEntry, VaultKeyring, DEFAULT_AGENT_DEADLINE_MS,
    DEFAULT_AGENT_OFFER_MICRO_USDC,
};
use covenant_compute_protocol::{
    chat_input, generation_input, lease_input, parse_assistant_output, parse_embedding_output,
    parse_speech_output, parse_transcription_output, speech_input, tools_input,
    transcription_input, AgentRuntime, AssistantReply, ChatMessage, FinishReason, GenerationParams,
    JobKind, LeaseTerms, LeaseView, NamedFunction, NamedToolChoice, PriceUnit, ResponseFormat,
    SpeechInput, SpeechResult, ToolChoice, ToolChoiceMode, ToolDefinition, ToolKind,
    TranscriptionInput, VaultKey, LEASE_DEADLINE_SLACK_MS,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use serde_json::Value;
use uuid::Uuid;

const USAGE: &str = "\
covenant-compute — buy compute from the Covenant network, and manage the
wallet that pays for it

Usage:
  covenant-compute whoami                       this buyer's pubkey and home
  covenant-compute balance                      funds and how to top up
  covenant-compute capacity                     what is purchasable right now
  covenant-compute deposit <tx-signature>       claim an on-chain top-up (the transfer's signature)
  covenant-compute infer <prompt>               buy one inference call ('-' reads stdin)
  covenant-compute embed <text>                 buy one embedding vector ('-' reads stdin)
  covenant-compute transcribe --audio <path>    buy one speech-to-text transcription
  covenant-compute speak <text>                 buy one text-to-speech clip ('-' reads stdin)
  covenant-compute run <command>                buy one batch command ('-' reads stdin)
  covenant-compute receipts                     your verified job history
  covenant-compute output <job-id>              re-read a past job's verified output
  covenant-compute verify <job-id>              hold the chain to a job's payout
  covenant-compute withdraw <micro-usdc> <to>   move unspent balance to a wallet
  covenant-compute withdrawals                  your withdrawal history
  covenant-compute dispute <job-id> <reason>    file a signed dispute of a job
  covenant-compute cancel <job-id>              refund a job no operator took yet
  covenant-compute agent --repo <path|url> --accept <cmd> <task>   hire a coding agent; pay only if its work passes
  covenant-compute lease open --minutes N --rate R    rent a whole GPU for a window
  covenant-compute lease view <job-id>          a live lease's endpoint, meter and cost
  covenant-compute lease close <job-id>         end a running lease and settle the meter
  covenant-compute vault put <label> [value]    store a secret, sealed under a key only you hold
  covenant-compute vault get <label>            fetch and open a secret (raw bytes to stdout)
  covenant-compute vault ls                     your stored secrets, and which you can open here
  covenant-compute vault rm <label>             delete a secret and drop its local key
  covenant-compute vault key export <label>     print a secret's key, to back it up or move it
  covenant-compute vault key import <label> <key>   save a key exported from another machine
  covenant-compute --version                    print the version

Buy flags (infer, embed, run):
  --model <id>          require an operator serving this model (infer, embed)
  --gpu-class <class>   require this GPU class (e.g. rtx-4090, h100) or cpu; see `capacity`
  --min-vram-gb <gb>    require at least this much VRAM, in whole GB
  --min-reputation-bps <bps>  require operators rated >= this (8000 = 80%); refunds if none clear
  --price <micro-usdc>  offered price; defaults to the cheapest matching ask
  --deadline-ms <ms>    job deadline; defaults to COVENANT_COMPUTE_DEADLINE_MS
  --idempotency-key <k> make the buy exactly-once: a retry replays, never re-buys
  --dry-run             show the price, match, and deadline a buy would use; spend nothing
  --system <text>       infer: a system prompt sent ahead of your prompt
  --image <path>        infer: attach an image to your prompt for a vision model; repeatable
  --audio <path>        transcribe: the audio file to transcribe (16 kHz mono WAV reads everywhere)
  --language <code>     transcribe: the spoken language (ISO code, or 'auto'); default lets the model detect
  --translate           transcribe: translate the speech into English instead of its own language
  --timestamps          transcribe: also print per-segment start/end times (for captions)
  --voice <name>        speak: a voice the operator's backend serves; default is its own
  --format <fmt>        speak: the audio container, wav (default) or aiff
  --speed <n>           speak: playback rate, 0.25 to 4.0 (1.0 = the natural pace)
  --messages-file <p>   infer: a full conversation as JSON [{role,content},…] ('-' reads stdin)
  --stream              infer: print tokens as they generate (not with --json)
  --tools <path>        infer: a JSON file of tool definitions the model may call
  --tool-choice <v>     infer: auto (default), none, required, or a function name to force one
Sampling flags (infer only; unset means the operator's backend default):
  --temperature <n>     sampling temperature, 0..=2
  --top-p <n>           nucleus sampling cutoff, in (0, 1]
  --max-tokens <n>      cap the generated output length in tokens
  --seed <n>            fix the seed; with --temperature 0 makes a run repeatable
  --presence-penalty <n>   penalize reused tokens, in [-2, 2] (+ favors new topics)
  --frequency-penalty <n>  penalize frequent tokens, in [-2, 2] (+ damps repetition)
  --stop <seq>          stop sequence; repeatable, up to 4
  --response-format <v> constrain the reply: 'json' for any valid JSON, or a
                        path to a JSON schema file for structured output
  --logprobs <n>        return per-token log probabilities with n alternatives
                        each (0..=20; 0 = the chosen token only)
Lease flags (lease open):
  --minutes <n>         the window to rent, in minutes (or --duration-secs <n>)
  --rate <micro-usdc>   price per second; the window's ceiling (rate x duration) is escrowed
                        up front and billed by the second, the rest refunded on close
  --ssh-key <path>      your OpenSSH public key (e.g. ~/.ssh/id_ed25519.pub) — how you reach the box
  --wait-secs <n>       how long to wait for the machine to come up (default 240)
  --no-wait             return as soon as the lease is placed, before the machine answers
  --gpu-class, --min-vram-gb, --min-reputation-bps   constrain supply as for a buy
Agent flags (agent):
  --repo <path|url>     a local git repository (sent as a bundle) or a public https URL
  --commit <sha>        the commit to work from; defaults to the local repository's HEAD
  --accept <cmd>        a command the work must pass, run from the repository root with no
                        network; repeatable, in order
  --check-image <ref>   the container image the commands run in (default python:3.12-slim)
  --check-timeout <s>   how long the commands may take, all together (default 300)
  --protect <path>      a path the work may not change (tests/, Cargo.lock); repeatable
  --apply               apply an accepted patch to the local repository's working tree
  --hidden <path>       a test file the builder never sees, read from the repository's working
                        tree (it must not be committed) and added only for the check; repeatable
  --hidden-accept <cmd> a command run after the visible ones, also kept from the builder
  --skill <name>        code.change (default) or code.tests: write tests that catch a bug
  --fix <path>          code.tests: a file whose working-tree version fixes the bug at the
                        commit; only checkers see it; repeatable
  --model <id>          the model the agent should drive; the operator's default otherwise
  --out <path>          where to write the accepted patch (default agent-<job-id>.patch)
  --price, --deadline-ms   as for a buy; the deadline defaults to 30 minutes
Vault flags:
  --file <path>         put: seal the bytes of this file instead of an inline value ('-' reads stdin)
  --force               key import: replace a different key already held for the label
Other flags:
  --limit <n>           receipts: how many rows to show (default 20)
  --rpc-url <url>       verify: this buyer's own Solana RPC for the read-back
  --withdrawal-id <id>  withdraw: reuse an id to retry the same withdrawal safely
  --json                print the underlying JSON record instead of a summary

COVENANT_COMPUTE_COORDINATOR_URL is required for every command but
whoami. The buyer identity lives under COVENANT_COMPUTE_MCP_HOME
(default ~/.covenant-compute-mcp), shared with covenant-compute-mcp.
Vault keys live there too, in vault-keys.json — the coordinator stores
only ciphertext, so back that file up: lose it and its secrets cannot be
recovered.
Spend is capped per call by COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC
(default $1). The full knob table is in the covenant-compute-buyer
README.
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // A warning-level stderr subscriber: a human running a command wants
    // its answer on stdout, not an info-log narration, but a genuine
    // warning from the buyer path should still surface.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "covenant_compute=warn".into()),
        )
        .init();

    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Help and version answer before any filesystem or config touch — a
    // question must get an answer, never mint an identity as a side
    // effect.
    match args.first().map(String::as_str) {
        None | Some("--help" | "-h" | "help") => {
            print!("{USAGE}");
            return Ok(());
        }
        Some("--version" | "-V" | "version") => {
            println!("{} {}", env!("CARGO_BIN_NAME"), env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }

    let json_out = take_flag(&mut args, "--json");
    // `--json` with no command left is the same "asked nothing" case as a
    // bare invocation — answer with usage, never panic popping a command
    // that isn't there.
    if args.is_empty() {
        print!("{USAGE}");
        return Ok(());
    }
    // `<command> --help`/`-h` answers with usage and exits clean, before
    // any home, identity, or coordinator touch — the same contract the
    // top-level help keeps. Without it `withdraw --help` reads the flag as
    // the amount and `infer --help` runs the buy, the exact surprises a
    // first-time buyer hits reaching for a subcommand's syntax.
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return Ok(());
    }
    let command = args.remove(0);

    // Home (and the identity it holds) is resolved only for a command
    // that needs it, so an unknown command refuses cleanly instead of
    // failing on a missing HOME first.
    match command.as_str() {
        "whoami" => cmd_whoami(json_out).await,
        "balance" => cmd_balance(&load_ctx()?, json_out).await,
        "capacity" => cmd_capacity(&load_ctx()?, json_out).await,
        "deposit" => {
            let deposit_id = positional(&args, 0, "tx-signature")?;
            cmd_deposit(&load_ctx()?, &deposit_id, json_out).await
        }
        "infer" => {
            let stream = take_flag(&mut args, "--stream");
            anyhow::ensure!(
                !(stream && json_out),
                "--stream prints tokens as they generate and can't combine with --json"
            );
            let model = take_flag_value(&mut args, "--model");
            let gpu_class = take_gpu_class(&mut args)?;
            let min_vram_gb: Option<u32> =
                take_number(&mut args, "--min-vram-gb", "a whole number of GB")?;
            let min_reputation_bps: Option<u32> = take_number(
                &mut args,
                "--min-reputation-bps",
                "a whole number of basis points (8000 = 80%)",
            )?;
            let price = take_price(&mut args)?;
            let deadline_ms = take_flag_value(&mut args, "--deadline-ms")
                .map(|v| v.parse())
                .transpose()
                .context("--deadline-ms must be a whole number of milliseconds")?;
            let generation = infer_generation(&mut args)?;
            let tools = infer_tools(&mut args)?;
            let system = take_flag_value(&mut args, "--system");
            let messages_file = take_flag_value(&mut args, "--messages-file");
            let image_paths = take_flag_values(&mut args, "--image");
            let idempotency_key = take_flag_value(&mut args, "--idempotency-key");
            let dry_run = take_flag(&mut args, "--dry-run");
            reject_stray_flags(&mut args)?;
            let prompt = {
                let joined = args.join(" ").trim().to_string();
                (!joined.is_empty()).then_some(joined)
            };
            let images = image_paths
                .iter()
                .map(|p| read_image(p))
                .collect::<anyhow::Result<Vec<_>>>()?;
            let mut input = infer_input(system, messages_file, prompt, images, generation)?;
            input.extend(tools);
            cmd_buy(
                &load_ctx()?,
                JobKind::InferenceCall,
                input,
                model,
                gpu_class,
                min_vram_gb,
                min_reputation_bps,
                price,
                deadline_ms,
                json_out,
                stream,
                idempotency_key,
                dry_run,
            )
            .await
        }
        "run" => {
            let gpu_class = take_gpu_class(&mut args)?;
            let min_vram_gb: Option<u32> =
                take_number(&mut args, "--min-vram-gb", "a whole number of GB")?;
            let min_reputation_bps: Option<u32> = take_number(
                &mut args,
                "--min-reputation-bps",
                "a whole number of basis points (8000 = 80%)",
            )?;
            let price = take_price(&mut args)?;
            let deadline_ms = take_flag_value(&mut args, "--deadline-ms")
                .map(|v| v.parse())
                .transpose()
                .context("--deadline-ms must be a whole number of milliseconds")?;
            let idempotency_key = take_flag_value(&mut args, "--idempotency-key");
            let dry_run = take_flag(&mut args, "--dry-run");
            let command = resolve_text(rest_joined(&args, "command")?)?;
            cmd_buy(
                &load_ctx()?,
                JobKind::BatchJob,
                vec![Content::Text { text: command }],
                None,
                gpu_class,
                min_vram_gb,
                min_reputation_bps,
                price,
                deadline_ms,
                json_out,
                false,
                idempotency_key,
                dry_run,
            )
            .await
        }
        "embed" => {
            let model = take_flag_value(&mut args, "--model");
            let gpu_class = take_gpu_class(&mut args)?;
            let min_vram_gb: Option<u32> =
                take_number(&mut args, "--min-vram-gb", "a whole number of GB")?;
            let min_reputation_bps: Option<u32> = take_number(
                &mut args,
                "--min-reputation-bps",
                "a whole number of basis points (8000 = 80%)",
            )?;
            let price = take_price(&mut args)?;
            let deadline_ms = take_flag_value(&mut args, "--deadline-ms")
                .map(|v| v.parse())
                .transpose()
                .context("--deadline-ms must be a whole number of milliseconds")?;
            let idempotency_key = take_flag_value(&mut args, "--idempotency-key");
            let dry_run = take_flag(&mut args, "--dry-run");
            reject_stray_flags(&mut args)?;
            let text = resolve_text(rest_joined(&args, "text")?)?;
            cmd_buy(
                &load_ctx()?,
                JobKind::Embedding,
                vec![Content::Text { text }],
                model,
                gpu_class,
                min_vram_gb,
                min_reputation_bps,
                price,
                deadline_ms,
                json_out,
                false,
                idempotency_key,
                dry_run,
            )
            .await
        }
        "transcribe" => {
            let audio_path = take_flag_value(&mut args, "--audio")
                .context("transcribe needs --audio <path> naming the audio file to transcribe")?;
            let language = take_flag_value(&mut args, "--language");
            let translate = take_flag(&mut args, "--translate");
            let timestamps = take_flag(&mut args, "--timestamps");
            let model = take_flag_value(&mut args, "--model");
            let gpu_class = take_gpu_class(&mut args)?;
            let min_vram_gb: Option<u32> =
                take_number(&mut args, "--min-vram-gb", "a whole number of GB")?;
            let min_reputation_bps: Option<u32> = take_number(
                &mut args,
                "--min-reputation-bps",
                "a whole number of basis points (8000 = 80%)",
            )?;
            let price = take_price(&mut args)?;
            let deadline_ms = take_flag_value(&mut args, "--deadline-ms")
                .map(|v| v.parse())
                .transpose()
                .context("--deadline-ms must be a whole number of milliseconds")?;
            let idempotency_key = take_flag_value(&mut args, "--idempotency-key");
            let dry_run = take_flag(&mut args, "--dry-run");
            reject_stray_flags(&mut args)?;
            let format = std::path::Path::new(&audio_path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            let request = TranscriptionInput {
                audio_base64: read_audio(&audio_path)?,
                format,
                language,
                translate,
                timestamps,
            };
            cmd_buy(
                &load_ctx()?,
                JobKind::Transcription,
                transcription_input(request),
                model,
                gpu_class,
                min_vram_gb,
                min_reputation_bps,
                price,
                deadline_ms,
                json_out,
                false,
                idempotency_key,
                dry_run,
            )
            .await
        }
        "speak" => {
            let voice = take_flag_value(&mut args, "--voice");
            let format = take_flag_value(&mut args, "--format");
            let speed: Option<f32> = take_flag_value(&mut args, "--speed")
                .map(|v| v.parse())
                .transpose()
                .context("--speed must be a decimal rate between 0.25 and 4.0")?;
            let model = take_flag_value(&mut args, "--model");
            let gpu_class = take_gpu_class(&mut args)?;
            let min_vram_gb: Option<u32> =
                take_number(&mut args, "--min-vram-gb", "a whole number of GB")?;
            let min_reputation_bps: Option<u32> = take_number(
                &mut args,
                "--min-reputation-bps",
                "a whole number of basis points (8000 = 80%)",
            )?;
            let price = take_price(&mut args)?;
            let deadline_ms = take_flag_value(&mut args, "--deadline-ms")
                .map(|v| v.parse())
                .transpose()
                .context("--deadline-ms must be a whole number of milliseconds")?;
            let idempotency_key = take_flag_value(&mut args, "--idempotency-key");
            let dry_run = take_flag(&mut args, "--dry-run");
            reject_stray_flags(&mut args)?;
            let text = resolve_text(rest_joined(&args, "text")?)?;
            let request = SpeechInput {
                text,
                voice,
                format,
                speed,
            };
            cmd_buy(
                &load_ctx()?,
                JobKind::SpeechSynthesis,
                speech_input(request),
                model,
                gpu_class,
                min_vram_gb,
                min_reputation_bps,
                price,
                deadline_ms,
                json_out,
                false,
                idempotency_key,
                dry_run,
            )
            .await
        }
        "receipts" => {
            let limit = take_flag_value(&mut args, "--limit")
                .map(|v| v.parse())
                .transpose()
                .context("--limit must be a whole number")?
                .unwrap_or(20);
            cmd_receipts(&load_ctx()?, limit, json_out).await
        }
        "output" => {
            let job_id = parse_job_id(&args, 0)?;
            cmd_output(&load_ctx()?, job_id, json_out).await
        }
        "verify" => {
            let mut ctx = load_ctx()?;
            if let Some(url) = take_flag_value(&mut args, "--rpc-url") {
                ctx.config.rpc_url = Some(url);
            }
            let job_id = parse_job_id(&args, 0)?;
            cmd_verify(&ctx, job_id, json_out).await
        }
        "withdraw" => {
            let withdrawal_id = take_flag_value(&mut args, "--withdrawal-id")
                .map(|v| v.parse())
                .transpose()
                .context("--withdrawal-id must be a UUID")?;
            let amount: u64 = positional(&args, 0, "micro-usdc")?
                .parse()
                .context("<micro-usdc> must be a whole number of micro-USDC")?;
            let recipient = positional(&args, 1, "to")?;
            cmd_withdraw(&load_ctx()?, amount, &recipient, withdrawal_id, json_out).await
        }
        "withdrawals" => cmd_withdrawals(&load_ctx()?, json_out).await,
        "dispute" => {
            let job_id = parse_job_id(&args, 0)?;
            let reason = rest_joined(args.get(1..).unwrap_or_default(), "reason")?;
            cmd_dispute(&load_ctx()?, job_id, reason, json_out).await
        }
        "cancel" => {
            let job_id = parse_job_id(&args, 0)?;
            cmd_cancel(&load_ctx()?, job_id, json_out).await
        }
        "agent" => cmd_agent(&load_ctx()?, &mut args, json_out).await,
        "lease" => {
            // A lease needs an explicit action: `open` spends money, so a
            // bare `lease` with a stray flag must never fall through into
            // opening a paid session by accident.
            let action = positional(&args, 0, "action (open|view|close)")?;
            let mut rest = args.split_off(1);
            match action.as_str() {
                "open" => {
                    let minutes: Option<u64> =
                        take_number(&mut rest, "--minutes", "a whole number of minutes")?;
                    let duration_secs: Option<u64> =
                        take_number(&mut rest, "--duration-secs", "a whole number of seconds")?;
                    let rate: Option<u64> = take_number(
                        &mut rest,
                        "--rate",
                        "a whole number of micro-USDC per second",
                    )?;
                    let ssh_key = take_flag_value(&mut rest, "--ssh-key");
                    let gpu_class = take_gpu_class(&mut rest)?;
                    let min_vram_gb: Option<u32> =
                        take_number(&mut rest, "--min-vram-gb", "a whole number of GB")?;
                    let min_reputation_bps: Option<u32> = take_number(
                        &mut rest,
                        "--min-reputation-bps",
                        "a whole number of basis points (8000 = 80%)",
                    )?;
                    let wait_secs: Option<u64> =
                        take_number(&mut rest, "--wait-secs", "a whole number of seconds")?;
                    let no_wait = take_flag(&mut rest, "--no-wait");
                    reject_stray_flags(&mut rest)?;
                    cmd_lease_open(
                        &load_ctx()?,
                        LeaseOpenArgs {
                            minutes,
                            duration_secs,
                            rate,
                            ssh_key,
                            gpu_class,
                            min_vram_gb,
                            min_reputation_bps,
                            wait_secs,
                            no_wait,
                        },
                        json_out,
                    )
                    .await
                }
                "view" => cmd_lease_view(&load_ctx()?, parse_job_id(&rest, 0)?, json_out).await,
                "close" => cmd_lease_close(&load_ctx()?, parse_job_id(&rest, 0)?, json_out).await,
                other => {
                    anyhow::bail!("unknown lease action {other:?} — use open, view or close")
                }
            }
        }
        "vault" => {
            let action = positional(&args, 0, "action (put|get|ls|rm|key)")?;
            let mut rest = args.split_off(1);
            match action.as_str() {
                "put" => {
                    let file = take_flag_value(&mut rest, "--file");
                    reject_stray_flags(&mut rest)?;
                    let label = positional(&rest, 0, "label")?;
                    let value = rest.get(1).cloned();
                    cmd_vault_put(&load_ctx()?, &label, value, file, json_out).await
                }
                "get" => {
                    reject_stray_flags(&mut rest)?;
                    let label = positional(&rest, 0, "label")?;
                    cmd_vault_get(&load_ctx()?, &label, json_out).await
                }
                "ls" => {
                    reject_stray_flags(&mut rest)?;
                    cmd_vault_ls(&load_ctx()?, json_out).await
                }
                "rm" => {
                    reject_stray_flags(&mut rest)?;
                    let label = positional(&rest, 0, "label")?;
                    cmd_vault_rm(&load_ctx()?, &label, json_out).await
                }
                "key" => {
                    let sub = positional(&rest, 0, "key action (export|import)")?;
                    let mut krest = rest.split_off(1);
                    match sub.as_str() {
                        "export" => {
                            cmd_vault_key_export(&positional(&krest, 0, "label")?, json_out)
                        }
                        "import" => {
                            let force = take_flag(&mut krest, "--force");
                            let label = positional(&krest, 0, "label")?;
                            let key_b58 = positional(&krest, 1, "key")?;
                            cmd_vault_key_import(&label, &key_b58, force, json_out)
                        }
                        other => {
                            anyhow::bail!(
                                "unknown vault key action {other:?} — use export or import"
                            )
                        }
                    }
                }
                other => {
                    anyhow::bail!("unknown vault action {other:?} — use put, get, ls, rm or key")
                }
            }
        }
        other => anyhow::bail!("unknown command {other:?} — run `--help` for usage"),
    }
}

/// Everything the coordinator-facing commands share: the version-stamped
/// client, the endpoint config, this buyer's key, and the per-call spend
/// ceiling.
struct Ctx {
    http: reqwest::Client,
    config: BuyerConfig,
    identity: LocalIdentity,
    max_price_micro_usdc: u64,
}

fn load_ctx() -> anyhow::Result<Ctx> {
    let coordinator_url = std::env::var("COVENANT_COMPUTE_COORDINATOR_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .context("COVENANT_COMPUTE_COORDINATOR_URL must be set and non-empty")?;
    anyhow::ensure!(
        coordinator_url.starts_with("http://") || coordinator_url.starts_with("https://"),
        "COVENANT_COMPUTE_COORDINATOR_URL must start with http:// or https:// (got \
         {coordinator_url:?})"
    );
    Ok(Ctx {
        http: http_client(),
        config: BuyerConfig {
            coordinator_url,
            poll_interval: Duration::from_millis(500),
            referral_code: env_opt("COVENANT_COMPUTE_REFERRAL_CODE"),
            rpc_url: env_opt("COVENANT_COMPUTE_RPC_URL"),
        },
        identity: load_identity(&prepared_home()?)?,
        max_price_micro_usdc: env_or(
            "COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC",
            1_000_000,
            "must be a whole number of micro-USDC",
        )?,
    })
}

async fn cmd_whoami(json_out: bool) -> anyhow::Result<()> {
    let home = prepared_home()?;
    let identity = load_identity(&home)?;
    let coordinator = env_opt("COVENANT_COMPUTE_COORDINATOR_URL");
    if json_out {
        let doc = serde_json::json!({
            "pubkey": identity.agent_id().pubkey_base58(),
            "home": home.display().to_string(),
            "coordinator": coordinator,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    println!("pubkey:      {}", identity.agent_id().pubkey_base58());
    println!("home:        {}", home.display());
    match coordinator {
        Some(url) => println!("coordinator: {url}"),
        None => println!("coordinator: (unset — set COVENANT_COMPUTE_COORDINATOR_URL to buy)"),
    }
    Ok(())
}

async fn cmd_balance(ctx: &Ctx, json_out: bool) -> anyhow::Result<()> {
    let view = funds_with_deposit_info(&ctx.http, &ctx.config, &ctx.identity).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    let funds = &view["balance"];
    println!("balance for {}", ctx.identity.agent_id().pubkey_base58());
    println!(
        "  deposited:  {}",
        usdc(field_u64(funds, "deposited_micro_usdc"))
    );
    println!(
        "  charged:    {}",
        usdc(field_u64(funds, "charged_micro_usdc"))
    );
    println!(
        "  withdrawn:  {}",
        usdc(field_u64(funds, "withdrawn_micro_usdc"))
    );
    println!(
        "  available:  {}",
        usdc(field_u64(funds, "available_micro_usdc"))
    );

    let info = &view["deposit_info"];
    println!();
    if info["configured"].as_bool() == Some(true) {
        println!("to top up, send USDC per these instructions, then claim it with `deposit`:");
        println!(
            "{}",
            indent(&serde_json::to_string_pretty(&info["instructions"])?)
        );
    } else {
        println!("this coordinator has no deposit rail configured — nothing to top up against");
    }
    Ok(())
}

async fn cmd_capacity(ctx: &Ctx, json_out: bool) -> anyhow::Result<()> {
    let view = capacity(&ctx.http, &ctx.config).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    println!(
        "{} operator(s) registered, {} matchable now",
        view.registered_operators, view.matchable_operators
    );
    if view.entries.is_empty() {
        println!("nothing is purchasable right now");
        return Ok(());
    }
    println!();
    println!(
        "{:<16} {:<24} {:>4}  {:>22}  {:>6}  gpu",
        "kind", "model", "ops", "ask", "vram"
    );
    for e in &view.entries {
        let ask = if e.min_ask_micro_usdc == e.max_ask_micro_usdc {
            format!("{} {}", e.min_ask_micro_usdc, unit_label(e.min_ask_unit))
        } else {
            format!(
                "{}–{} {}",
                e.min_ask_micro_usdc,
                e.max_ask_micro_usdc,
                unit_label(e.min_ask_unit)
            )
        };
        println!(
            "{:<16} {:<24} {:>4}  {:>22}  {:>6}  {}",
            kind_label(e.kind),
            flatten_controls(&e.model),
            e.operators,
            ask,
            format!("{}GB", e.max_vram_gb),
            flatten_controls(&e.gpu_classes.join(","))
        );
    }
    Ok(())
}

async fn cmd_deposit(ctx: &Ctx, deposit_id: &str, json_out: bool) -> anyhow::Result<()> {
    let outcome = claim_deposit(&ctx.http, &ctx.config, &ctx.identity, deposit_id).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
        return Ok(());
    }
    if outcome.credited {
        println!(
            "credited {} from deposit {}",
            usdc(outcome.amount_micro_usdc),
            outcome.deposit_id
        );
    } else {
        println!(
            "deposit {} was already claimed — balance unchanged",
            outcome.deposit_id
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cmd_buy(
    ctx: &Ctx,
    kind: JobKind,
    input: Vec<Content>,
    model: Option<String>,
    gpu_class: Option<String>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    price: Option<u64>,
    deadline_ms: Option<u64>,
    json_out: bool,
    stream: bool,
    idempotency_key: Option<String>,
    dry_run: bool,
) -> anyhow::Result<()> {
    if dry_run {
        anyhow::ensure!(
            !stream,
            "--dry-run only previews a buy, so there is nothing to stream — drop --stream"
        );
        anyhow::ensure!(
            idempotency_key.is_none(),
            "--dry-run buys nothing, so there is no idempotency key to reserve — drop \
             --idempotency-key"
        );
        return preview_buy(
            ctx,
            kind,
            input,
            model,
            gpu_class,
            min_vram_gb,
            min_reputation_bps,
            price,
            deadline_ms,
            json_out,
        )
        .await;
    }
    if let Some(key) = idempotency_key {
        anyhow::ensure!(
            !stream,
            "--idempotency-key can't combine with --stream: a stream hands back a job id before \
             the answer is verified, so a retry has nothing to replay — drop --stream to make the \
             buy exactly-once"
        );
        return cmd_buy_keyed(
            ctx,
            &key,
            kind,
            input,
            model,
            gpu_class,
            min_vram_gb,
            min_reputation_bps,
            price,
            deadline_ms,
            json_out,
        )
        .await;
    }
    let price = resolve_price(
        ctx,
        kind,
        model.as_deref(),
        gpu_class.as_deref(),
        min_vram_gb,
        min_reputation_bps,
        price,
    )
    .await?;
    let deadline_ms = resolve_deadline(deadline_ms)?;
    let request = JobRequest {
        kind,
        input,
        model,
        gpu_class,
        min_vram_gb,
        min_reputation_bps,
        price_micro_usdc: price,
        deadline_ms,
    };

    if stream {
        return cmd_buy_streaming(ctx, request).await;
    }
    let outcome = dispatch_and_verify(&ctx.http, &ctx.config, &ctx.identity, request)
        .await
        .map_err(with_funding_hint)?;
    render_outcome(&outcome, json_out)
}

/// The idempotent twin of the buy path. A purchase made under a key
/// journals its signed envelope to a CLI-local book *before* the first
/// submission, so a retry — this run or the next — re-drives the exact
/// same bytes into the coordinator's duplicate detection instead of
/// minting a second job and paying twice. A settled key replays its
/// recorded answer without spending again; an in-flight one re-drives; a
/// reused key whose explicit arguments disagree refuses rather than
/// answer a new question with the old job.
///
/// The book is guarded by an advisory file lock so two concurrent keyed
/// buys serialize onto one purchase. It is CLI-local — a separate file
/// from the `covenant-compute-mcp` server's own book under the shared
/// home — so the two demand paths never race each other's writer; a key
/// is idempotent within the CLI, not across both clients.
#[allow(clippy::too_many_arguments)]
async fn cmd_buy_keyed(
    ctx: &Ctx,
    key: &str,
    kind: JobKind,
    input: Vec<Content>,
    model: Option<String>,
    gpu_class: Option<String>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    price: Option<u64>,
    deadline_ms: Option<u64>,
    json_out: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        (1..=128).contains(&key.len()),
        "--idempotency-key must be 1..=128 bytes"
    );
    let home = prepared_home()?;
    // Serialize concurrent keyed buys onto one book, held across the whole
    // purchase and released when this process exits — a crash mid-buy
    // never wedges the next run.
    let _lock = CliLock::acquire(&home.join("purchases-cli.lock"))?;
    let book = PurchaseBook::open(&home.join("purchases-cli.jsonl"))
        .map_err(|e| anyhow::anyhow!("open purchase book: {e}"))?;
    let scoped = format!("{}:{key}", ctx.identity.agent_id().pubkey_base58());

    // Resolve the key against the book first: only a fresh key signs
    // anything new (and only then is the market queried for a price).
    let envelope = match book.lookup(&scoped) {
        Some(entry) => {
            if let Some(argument) = entry.conflicting_argument(
                kind,
                &input,
                model.as_deref(),
                gpu_class.as_deref(),
                min_vram_gb,
                min_reputation_bps,
                price,
                deadline_ms,
            ) {
                anyhow::bail!(
                    "idempotency key {key:?} was already used with a different {argument}: a key \
                     names one purchase, so repeat its original arguments to retrieve it, or use a \
                     fresh key for new work"
                );
            }
            entry.envelope
        }
        None => {
            let price = resolve_price(
                ctx,
                kind,
                model.as_deref(),
                gpu_class.as_deref(),
                min_vram_gb,
                min_reputation_bps,
                price,
            )
            .await?;
            let deadline_ms = resolve_deadline(deadline_ms)?;
            let envelope = sign_envelope(
                &ctx.config,
                &ctx.identity,
                JobRequest {
                    kind,
                    input,
                    model,
                    gpu_class,
                    min_vram_gb,
                    min_reputation_bps,
                    price_micro_usdc: price,
                    deadline_ms,
                },
            )?;
            book.record(PurchaseEntry {
                key: scoped.clone(),
                envelope: envelope.clone(),
                opened_at_ms: epoch_ms(),
                receipt_id: None,
                voided: false,
            })
            .map_err(|e| anyhow::anyhow!("record purchase: {e}"))?;
            envelope
        }
    };

    match dispatch_signed(&ctx.http, &ctx.config, &ctx.identity, envelope).await {
        Ok(outcome) => {
            // The money moved (or already had): pin the key to its receipt
            // so from here it only ever replays the recorded answer.
            if let Err(e) = book.settle(&scoped, outcome.receipt.receipt.job_id) {
                tracing::warn!(key = %scoped, error = %e, "purchase book settle failed");
            }
            render_outcome(&outcome, json_out)
        }
        // Refunded, rejected, failed, or refused at submission with a
        // verdict on the envelope itself: the money never moved, so free
        // the key for an honest re-buy.
        Err(e) if e.concludes_purchase_unpaid() => {
            if let Err(ve) = book.void(&scoped) {
                tracing::warn!(key = %scoped, error = %ve, "purchase book void failed");
            }
            Err(with_funding_hint(e))
        }
        // Ambiguous — a timeout or transport failure. The entry stays in
        // flight; the next run under this key re-drives the same bytes.
        Err(e) => Err(with_funding_hint(e)),
    }
}

/// Frees the CLI idempotency key that bought `job_id`, if any — under
/// the same lock a keyed buy takes. A cancelled job's money never moved,
/// so its key may buy again. Best-effort: absent a CLI book (no keyed
/// buy ever ran) there is nothing to free, and any book error only leaves
/// the key to self-free on its next re-drive, never failing the cancel.
fn free_keyed_purchase(job_id: Uuid) {
    let Ok(home) = prepared_home() else {
        return;
    };
    let path = home.join("purchases-cli.jsonl");
    if !path.exists() {
        return;
    }
    let Ok(_lock) = CliLock::acquire(&home.join("purchases-cli.lock")) else {
        return;
    };
    match PurchaseBook::open(&path).and_then(|book| book.void_by_job(job_id)) {
        Ok(_) => {}
        Err(e) => tracing::warn!(%job_id, error = %e, "freeing the cancelled job's key failed"),
    }
}

/// Milliseconds since the Unix epoch — the book's own opened-at stamp,
/// bookkeeping only (dedup keys off the key and the signed envelope).
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// An advisory exclusive lock on a file, held for the guard's lifetime
/// and released when the fd closes — process exit included, so a crash
/// leaves no stale lock to reap. Serializes concurrent keyed purchases
/// onto one book.
#[cfg(unix)]
struct CliLock {
    _file: std::fs::File,
}

#[cfg(unix)]
impl CliLock {
    fn acquire(path: &Path) -> anyhow::Result<Self> {
        use std::os::unix::io::AsRawFd;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("open lock {}", path.display()))?;
        // Blocks until the lock is ours.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(anyhow::Error::from(std::io::Error::last_os_error())
                .context("acquire idempotency lock"));
        }
        Ok(Self { _file: file })
    }
}

#[cfg(not(unix))]
struct CliLock;

#[cfg(not(unix))]
impl CliLock {
    fn acquire(_path: &Path) -> anyhow::Result<Self> {
        // No advisory lock off unix (the supported targets are macOS and
        // Linux). Sequential retries stay safe — the book is re-read on
        // open — only concurrent same-key buys would race.
        Ok(Self)
    }
}

/// The price actually offered: an explicit `--price` held under the
/// per-call ceiling, or — with none named — the cheapest matching ask,
/// also ceiling-checked and echoed to stderr so a defaulted buy is never
/// silent. The same guardrail the MCP server enforces, refused here
/// before anything is signed or dispatched.
///
/// Settlement charges the envelope's price, so defaulting to the ceiling
/// would pay an operator that asked far less its full cap; the cheapest
/// matching ask is the honest default, the ceiling only the bound above it.
async fn resolve_price(
    ctx: &Ctx,
    kind: JobKind,
    model: Option<&str>,
    gpu_class: Option<&str>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    price: Option<u64>,
) -> anyhow::Result<u64> {
    let quote = resolve_price_source(
        ctx,
        kind,
        model,
        gpu_class,
        min_vram_gb,
        min_reputation_bps,
        price,
    )
    .await?;
    if let PriceQuote::CheapestAsk(offer) = quote {
        eprintln!(
            "offering {offer} micro-USDC, the cheapest matching ask (pass --price to override)"
        );
    }
    Ok(quote.micro_usdc())
}

/// The pure resolution behind [`resolve_price`]: the price a buy would
/// offer and where it came from, with no side effect. `resolve_price`
/// echoes a defaulted offer to stderr; `--dry-run` renders its own
/// preview instead. Keeping the resolution in one place is what lets a
/// dry run promise the exact figure a real buy would pay.
///
/// This is the terminal twin of the buyer lib's [`quote_price`], which
/// serves the agent surfaces; the two carry the same decision logic and
/// must move together. The CLI keeps its own wording so a refusal names
/// the flag or environment knob a human would reach for.
async fn resolve_price_source(
    ctx: &Ctx,
    kind: JobKind,
    model: Option<&str>,
    gpu_class: Option<&str>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    price: Option<u64>,
) -> anyhow::Result<PriceQuote> {
    let cap = ctx.max_price_micro_usdc;
    match price {
        Some(p) => {
            anyhow::ensure!(
                p <= cap,
                "offered price {p} micro-USDC exceeds the per-call ceiling {cap} — lower --price \
                 or raise COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC"
            );
            Ok(PriceQuote::Explicit(p))
        }
        None => match cheapest_matching_ask(
            &ctx.http,
            &ctx.config,
            kind,
            model,
            gpu_class,
            min_vram_gb,
            min_reputation_bps,
        )
        .await?
        {
            None => anyhow::bail!(
                "no operator is serving {} right now — see `capacity`; nothing was dispatched",
                describe_request(kind, model, gpu_class, min_vram_gb)
            ),
            Some(floor) if floor > cap => anyhow::bail!(
                "the cheapest matching ask is {floor} micro-USDC, above your per-call ceiling \
                 {cap} — raise COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC or pass --price to authorize it"
            ),
            Some(floor) => Ok(PriceQuote::CheapestAsk(floor)),
        },
    }
}

/// The deadline in force: an explicit `--deadline-ms`, or the
/// `COVENANT_COMPUTE_DEADLINE_MS` default.
fn resolve_deadline(deadline_ms: Option<u64>) -> anyhow::Result<u64> {
    match deadline_ms {
        Some(ms) => Ok(ms),
        None => env_or(
            "COVENANT_COMPUTE_DEADLINE_MS",
            60_000,
            "must be a whole number of milliseconds",
        ),
    }
}

/// `--dry-run`: resolve exactly what a real buy would — the routing
/// constraints, the price offered (an explicit `--price` or the cheapest
/// matching ask), the deadline — and print it without signing,
/// dispatching, or moving any funds. It shares `resolve_price_source`
/// and `resolve_deadline` with the buy path, so the figures it shows are
/// the figures a buy would use. `capacity` is the market; this is one
/// job held against it, priced. A market with no capable operator (or an
/// ask above the per-call ceiling) refuses here with the same message
/// the buy would, having dispatched nothing.
#[allow(clippy::too_many_arguments)]
async fn preview_buy(
    ctx: &Ctx,
    kind: JobKind,
    input: Vec<Content>,
    model: Option<String>,
    gpu_class: Option<String>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    price: Option<u64>,
    deadline_ms: Option<u64>,
    json_out: bool,
) -> anyhow::Result<()> {
    let quote = resolve_price_source(
        ctx,
        kind,
        model.as_deref(),
        gpu_class.as_deref(),
        min_vram_gb,
        min_reputation_bps,
        price,
    )
    .await?;
    let deadline_ms = resolve_deadline(deadline_ms)?;
    let cap = ctx.max_price_micro_usdc;
    let offer = quote.micro_usdc();
    let source_note = match quote {
        PriceQuote::Explicit(_) => "your --price",
        PriceQuote::CheapestAsk(_) => "cheapest matching ask",
    };

    if json_out {
        let doc = preview_value(
            kind,
            model.as_deref(),
            gpu_class.as_deref(),
            min_vram_gb,
            min_reputation_bps,
            quote,
            deadline_ms,
            cap,
            input.len(),
        );
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

    let any = || "any".to_string();
    println!("dry run: nothing dispatched, no funds moved");
    println!();
    println!("  job        {}", kind_label(kind));
    println!(
        "  model      {}",
        model.as_deref().map(flatten_controls).unwrap_or_else(any)
    );
    println!(
        "  gpu class  {}",
        gpu_class
            .as_deref()
            .map(flatten_controls)
            .unwrap_or_else(any)
    );
    println!(
        "  min vram   {}",
        min_vram_gb.map(|v| format!("{v} GB")).unwrap_or_else(any)
    );
    println!(
        "  min rep    {}",
        min_reputation_bps
            .map(|b| format!("{b} bps"))
            .unwrap_or_else(any)
    );
    println!("  price      {} ({source_note})", usdc(offer));
    println!("  deadline   {deadline_ms} ms");
    println!("  spend cap  {} per call", usdc(cap));
    println!();
    println!("run the same command without --dry-run to buy.");
    Ok(())
}

/// Hires a coding agent for a task against a repository. The task pays
/// only once a second operator applies the agent's patch to a fresh copy
/// of the commit and every acceptance command passes, so a failed run
/// costs nothing. On success the patch is written to a file; either way
/// the check's verdict is shown.
async fn cmd_agent(ctx: &Ctx, args: &mut Vec<String>, json_out: bool) -> anyhow::Result<()> {
    let agent_args = AgentArgs {
        repo: take_flag_value(args, "--repo").context("--repo is required")?,
        commit: take_flag_value(args, "--commit"),
        accept: take_flag_values(args, "--accept"),
        check_image: take_flag_value(args, "--check-image"),
        check_timeout_secs: take_number(args, "--check-timeout", "a whole number of seconds")?,
        protect: take_flag_values(args, "--protect"),
        hidden: take_flag_values(args, "--hidden"),
        hidden_accept: take_flag_values(args, "--hidden-accept"),
        skill: take_flag_value(args, "--skill"),
        fix: take_flag_values(args, "--fix"),
        model: take_flag_value(args, "--model"),
        price_micro_usdc: take_price(args)?,
        deadline_ms: take_number(args, "--deadline-ms", "a whole number of milliseconds")?,
        apply: take_flag(args, "--apply"),
        task: String::new(),
    };
    let out = take_flag_value(args, "--out");
    let agent_args = AgentArgs {
        task: resolve_text(rest_joined(args, "task")?)?,
        ..agent_args
    };
    let task = prepare_agent_task(&agent_args)?;
    let runtime = AgentRuntime::ClaudeCode.label().to_string();
    let price = resolve_price(
        ctx,
        JobKind::AgentTask,
        Some(&runtime),
        None,
        None,
        None,
        Some(
            agent_args
                .price_micro_usdc
                .unwrap_or(DEFAULT_AGENT_OFFER_MICRO_USDC),
        ),
    )
    .await?;
    let deadline_ms = agent_args.deadline_ms.unwrap_or(DEFAULT_AGENT_DEADLINE_MS);
    if !json_out {
        eprintln!(
            "offering up to {}: if the work passes you pay what the build spent plus its \
             checks, and nothing if it fails. An agent builds it and another operator checks \
             the result (up to {} min)",
            usdc(price),
            deadline_ms / 60_000
        );
    }
    let outcome = hire_agent(
        &ctx.http,
        &ctx.config,
        &ctx.identity,
        task,
        price,
        deadline_ms,
    )
    .await
    .map_err(with_funding_hint)?;
    match outcome {
        AgentOutcome::NotPaid {
            job_id,
            status,
            reason,
            verdict,
            round,
            reworks,
        } => {
            if json_out {
                let doc = serde_json::json!({
                    "job_id": job_id,
                    "status": status,
                    "refund_reason": reason,
                    "check": verdict,
                    "round": round,
                    "reworks": reworks,
                });
                println!("{}", serde_json::to_string_pretty(&doc)?);
            } else {
                println!(
                    "not paid: {status} ({}), your balance is untouched",
                    reason.as_deref().unwrap_or("no reason given")
                );
                if let Some(verdict) = &verdict {
                    println!("{}", describe_verdict(verdict));
                }
                if let Some(line) = describe_reworks(reworks, false) {
                    println!("{line}");
                }
                if let Some(round) = &round {
                    println!("{}", describe_round(round));
                }
            }
        }
        AgentOutcome::Accepted {
            outcome,
            built,
            patch,
            verdict,
            round,
            charged_micro_usdc,
            reworks,
        } => {
            let job_id = outcome.receipt.receipt.job_id;
            let path = PathBuf::from(out.unwrap_or_else(|| format!("agent-{job_id}.patch")));
            std::fs::write(&path, &patch).with_context(|| format!("write {}", path.display()))?;
            let applied = agent_args.apply && !agent_args.repo.starts_with("https://");
            if applied {
                apply_patch(Path::new(&agent_args.repo), &patch)?;
            }
            if json_out {
                let doc = serde_json::json!({
                    "job_id": job_id,
                    "patch_path": path,
                    "applied": applied,
                    "patch_sha256": built.patch_sha256,
                    "files_changed": built.files_changed,
                    "summary": built.summary,
                    "model": built.model,
                    "price_micro_usdc": outcome.envelope.payload.price_micro_usdc,
                    "check": verdict,
                    "round": round,
                    "charged_micro_usdc": charged_micro_usdc,
                    "reworks": reworks,
                    "payout": outcome.payout,
                });
                println!("{}", serde_json::to_string_pretty(&doc)?);
                return Ok(());
            }
            println!(
                "accepted: {} file(s) changed, patch written to {}{}",
                built.files_changed,
                path.display(),
                if applied {
                    " and applied to the working tree".to_string()
                } else {
                    format!(" (apply with `git apply {}`)", path.display())
                }
            );
            if !built.summary.is_empty() {
                println!();
                println!("{}", built.summary);
            }
            if let Some(verdict) = &verdict {
                println!();
                println!("{}", describe_verdict(verdict));
            }
            if let Some(line) = describe_reworks(reworks, true) {
                println!("{line}");
            }
            if let Some(round) = &round {
                println!("{}", describe_round(round));
            }
            if let Some(charged) = charged_micro_usdc {
                println!(
                    "charged: {} of your {} ceiling",
                    usdc(charged),
                    usdc(outcome.envelope.payload.price_micro_usdc)
                );
            }
            println!();
            print_receipt_block(&outcome);
        }
    }
    Ok(())
}

/// A dispatch refused for want of funds is the most common first
/// purchase on a prefunded coordinator, before the buyer has deposited.
/// Name the top-up path the rest of the CLI already documents, so this
/// refusal ends with its fix the way every other one does. Any other
/// failure passes through unchanged.
fn with_funding_hint(e: covenant_compute_buyer::BuyerError) -> anyhow::Error {
    if e.is_underfunded() {
        anyhow::anyhow!(
            "{e}\ntop up first: run `covenant-compute balance` for this deployment's deposit \
             instructions, then `covenant-compute deposit <tx-signature>`"
        )
    } else {
        e.into()
    }
}

/// The streaming twin of the buy path: tokens print as the operator
/// generates them, then the same verified receipt block underneath. The
/// live feed is a preview — the receipt is still the authority, so if a
/// node couldn't stream (no chunks arrived) the verified output prints
/// once at the end instead.
async fn cmd_buy_streaming(ctx: &Ctx, request: JobRequest) -> anyhow::Result<()> {
    use std::io::Write;
    let mut streamed_any = false;
    let outcome = dispatch_streaming(
        &ctx.http,
        &ctx.config,
        &ctx.identity,
        request,
        |chunk: &str| {
            print!("{chunk}");
            let _ = std::io::stdout().flush();
            streamed_any = true;
        },
    )
    .await
    .map_err(with_funding_hint)?;

    if streamed_any {
        println!();
    } else {
        // The node served the job one-shot; show the verified output the
        // live feed never carried. A tools or logprobs turn always lands
        // here — the backend runs non-streaming for both — so its calls or
        // per-token probabilities print whole, the same rendering the
        // non-streaming buy gives.
        let reply = parse_assistant_output(&outcome.outcome.output);
        if !reply.tool_calls.is_empty() {
            print!("{}", render_tool_calls(&reply));
        } else if let Some(tokens) = &reply.logprobs {
            if !reply.text.is_empty() {
                println!("{}", reply.text);
            }
            print!("{}", render_logprobs(tokens));
        } else {
            println!("{}", output_to_string(&outcome.outcome.output));
        }
    }
    println!();
    print_receipt_block(&outcome.outcome);
    if streamed_any && !outcome.stream_matched_output {
        eprintln!(
            "note: the live preview didn't match the verified output exactly — the receipt above \
             is authoritative"
        );
    }
    Ok(())
}

/// Writes a synthesized clip into the working directory, named for its
/// job, returning the path and byte count. Thin wrapper over the shared
/// [`covenant_compute_buyer::save_speech_clip`] so the CLI and the MCP
/// `compute.speak` tool write a clip the same way.
fn save_speech_clip(speech: &SpeechResult, job_id: Uuid) -> anyhow::Result<(String, usize)> {
    let (path, bytes) = covenant_compute_buyer::save_speech_clip(Path::new("."), speech, job_id)
        .map_err(anyhow::Error::msg)?;
    Ok((path.display().to_string(), bytes))
}

fn render_outcome(outcome: &DispatchOutcome, json_out: bool) -> anyhow::Result<()> {
    let receipt = &outcome.receipt.receipt;
    // Report the charged offer, not the receipt's claim: the coordinator
    // releases the escrowed offer even when a node signs a receipt priced
    // below it (verify_receipt allows that), so the receipt figure would
    // under-report what the buyer paid.
    let charged = outcome.envelope.payload.price_micro_usdc;
    // A synthesized clip is audio, not text: write the bytes to a file and
    // say where, rather than print a wall of base64. The file is named for
    // the job, so `output <job-id>` later lands the same clip.
    if let Ok(speech) = parse_speech_output(&outcome.output) {
        let (path, bytes) = save_speech_clip(&speech, receipt.job_id)?;
        if json_out {
            let doc = serde_json::json!({
                "path": path,
                "bytes": bytes,
                "format": speech.format,
                "model": speech.model,
                "sample_rate_hz": speech.sample_rate_hz,
                "job_id": receipt.job_id,
                "operator_pubkey_b58": receipt.operator.pubkey_base58(),
                "price_micro_usdc": charged,
                "result_hash_hex": receipt.result_hash_hex,
                "wall_ms": receipt.meter.wall_ms,
                "receipt_verified": true,
                "payout": outcome.payout,
            });
            println!("{}", serde_json::to_string_pretty(&doc)?);
            return Ok(());
        }
        println!("wrote {bytes} bytes of {} audio to {path}", speech.format);
        println!();
        print_receipt_block(outcome);
        return Ok(());
    }
    // A tool-calling turn carries the calls in a JSON block; render the
    // calls the model asked for, not the raw block. Only when calls are
    // present, so plain completions, embeddings and transcripts fall
    // through to their own rendering below unchanged.
    let reply = parse_assistant_output(&outcome.output);
    if !reply.tool_calls.is_empty() {
        if json_out {
            let doc = serde_json::json!({
                "output": reply.text,
                "tool_calls": reply.tool_calls,
                "job_id": receipt.job_id,
                "operator_pubkey_b58": receipt.operator.pubkey_base58(),
                "price_micro_usdc": charged,
                "result_hash_hex": receipt.result_hash_hex,
                "wall_ms": receipt.meter.wall_ms,
                "tokens_in": receipt.meter.tokens_in,
                "tokens_out": receipt.meter.tokens_out,
                "finish_reason": receipt.meter.finish_reason,
                "receipt_verified": true,
                "payout": outcome.payout,
            });
            println!("{}", serde_json::to_string_pretty(&doc)?);
            return Ok(());
        }
        print!("{}", render_tool_calls(&reply));
        println!();
        print_receipt_block(outcome);
        return Ok(());
    }
    // A completion the buyer asked logprobs for carries them in their own
    // JSON block; render the per-token probabilities, not the raw block,
    // and keep them out of the plain `output` string on the `--json` path.
    if let Some(tokens) = &reply.logprobs {
        if json_out {
            let doc = serde_json::json!({
                "output": reply.text,
                "logprobs": tokens,
                "job_id": receipt.job_id,
                "operator_pubkey_b58": receipt.operator.pubkey_base58(),
                "price_micro_usdc": charged,
                "result_hash_hex": receipt.result_hash_hex,
                "wall_ms": receipt.meter.wall_ms,
                "tokens_in": receipt.meter.tokens_in,
                "tokens_out": receipt.meter.tokens_out,
                "finish_reason": receipt.meter.finish_reason,
                "receipt_verified": true,
                "payout": outcome.payout,
            });
            println!("{}", serde_json::to_string_pretty(&doc)?);
            return Ok(());
        }
        if !reply.text.is_empty() {
            println!("{}", reply.text);
        }
        print!("{}", render_logprobs(tokens));
        println!();
        print_receipt_block(outcome);
        return Ok(());
    }
    if json_out {
        let doc = serde_json::json!({
            "output": output_to_string(&outcome.output),
            "job_id": receipt.job_id,
            "operator_pubkey_b58": receipt.operator.pubkey_base58(),
            "price_micro_usdc": charged,
            "result_hash_hex": receipt.result_hash_hex,
            "wall_ms": receipt.meter.wall_ms,
            "tokens_in": receipt.meter.tokens_in,
            "tokens_out": receipt.meter.tokens_out,
            "finish_reason": receipt.meter.finish_reason,
            "receipt_verified": true,
            "payout": outcome.payout,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    // An embedding's product is a wide float vector — a summary reads on
    // a terminal where the raw numbers do not; the full vector is one
    // `--json` (or `output <job-id>`) away.
    match parse_embedding_output(&outcome.output) {
        Ok(emb) => println!(
            "embedding: {} vector(s) x {} dimensions ({})",
            emb.embeddings.len(),
            emb.dimensions,
            emb.model
        ),
        // A transcription's product is its text: print the transcript
        // itself, not the JSON block that carries it. With timestamps, each
        // segment prints under its own timing, the form a caption reads in.
        Err(_) => match parse_transcription_output(&outcome.output) {
            Ok(t) => match &t.segments {
                Some(segments) if !segments.is_empty() => {
                    for s in segments {
                        println!("[{} -> {}] {}", clock(s.start_ms), clock(s.end_ms), s.text);
                    }
                }
                _ => println!("{}", t.transcript),
            },
            Err(_) => println!("{}", output_to_string(&outcome.output)),
        },
    }
    println!();
    print_receipt_block(outcome);
    Ok(())
}

/// A segment offset as a caption-style clock: `M:SS.mmm`, growing an hours
/// field only when the clip runs past an hour.
fn clock(ms: u64) -> String {
    let (h, m, s, milli) = (
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000,
    );
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{milli:03}")
    } else {
        format!("{m:02}:{s:02}.{milli:03}")
    }
}

/// The verified-receipt lines under a bought job's output: job id,
/// operator, price, metering, the verification verdict and payout state.
/// Shared by the synchronous and streaming buy paths.
fn print_receipt_block(outcome: &DispatchOutcome) {
    let _ = write_receipt_block(&mut std::io::stdout().lock(), outcome);
}

fn write_receipt_block(
    w: &mut impl std::io::Write,
    outcome: &DispatchOutcome,
) -> std::io::Result<()> {
    let receipt = &outcome.receipt.receipt;
    writeln!(w, "receipt:")?;
    writeln!(w, "  job:       {}", receipt.job_id)?;
    writeln!(w, "  operator:  {}", receipt.operator.pubkey_base58())?;
    // The charge is the escrowed offer, not the receipt's possibly-lower
    // claim (see render_outcome / verify_receipt).
    writeln!(
        w,
        "  price:     {}",
        usdc(outcome.envelope.payload.price_micro_usdc)
    )?;
    if let (Some(tin), Some(tout)) = (receipt.meter.tokens_in, receipt.meter.tokens_out) {
        writeln!(w, "  tokens:    {tin} in, {tout} out")?;
    }
    writeln!(w, "  wall:      {} ms", receipt.meter.wall_ms)?;
    // Name why generation stopped when it was not a clean end: `content_filter`
    // or `length` means the output above was cut short, not finished. A clean
    // stop and a non-generation job (no reason) stay quiet.
    if let Some(reason) = receipt.meter.finish_reason {
        if reason != FinishReason::Stop {
            writeln!(w, "  finish:    {}", reason.as_openai())?;
        }
    }
    writeln!(w, "  verified:  yes (signature, operator key, output hash)")?;
    match &outcome.payout {
        Some(p) => writeln!(
            w,
            "  payout:    {} {}",
            usdc(p.amount_micro_usdc),
            p.tx_signature.as_deref().unwrap_or("(off-chain record)")
        ),
        None => writeln!(
            w,
            "  payout:    pending (the coordinator's retry sweep is pushing it)"
        ),
    }
}

async fn cmd_receipts(ctx: &Ctx, limit: usize, json_out: bool) -> anyhow::Result<()> {
    let rows = list_verified_jobs(&ctx.http, &ctx.config, &ctx.identity, limit).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no jobs yet — buy one with `infer`, `embed`, or `run`");
        return Ok(());
    }
    let failing = rows
        .iter()
        .filter(|r| r.receipt_verified == Some(false))
        .count();
    println!(
        "{} job(s), {failing} failing local verification",
        rows.len()
    );
    for r in &rows {
        println!();
        // Show what settled, not the ceiling: a lease escrows its whole window
        // but is charged only for the seconds it ran, so the committed price
        // over-reports the charge. A row still in flight has not settled, so
        // the reserved ceiling is all there is to show.
        match r.charged_micro_usdc {
            Some(charged) if charged != r.price_micro_usdc => println!(
                "{}  {}  {} (of {} reserved)",
                r.job_id,
                r.status,
                usdc(charged),
                usdc(r.price_micro_usdc)
            ),
            Some(charged) => println!("{}  {}  {}", r.job_id, r.status, usdc(charged)),
            None => println!("{}  {}  {}", r.job_id, r.status, usdc(r.price_micro_usdc)),
        }
        if let Some(op) = &r.operator_pubkey_b58 {
            println!("  operator:  {op}");
        }
        let verdict = match r.receipt_verified {
            Some(true) => "verified",
            Some(false) => "FAILED VERIFICATION",
            None => "no receipt yet",
        };
        println!("  receipt:   {verdict}");
        if let Some(err) = &r.verification_error {
            println!("  error:     {err}");
        }
        if let Some(reason) = &r.refund_reason {
            println!("  refund:    {reason}");
        }
        if let Some(p) = &r.payout {
            println!(
                "  payout:    {} {}",
                usdc(p.amount_micro_usdc),
                p.tx_signature.as_deref().unwrap_or("(off-chain record)")
            );
        }
    }
    Ok(())
}

/// Re-read a past job's output and its locally re-verified receipt — the
/// answer the buyer paid for, recoverable after the terminal that showed
/// it at buy time is gone.
async fn cmd_output(ctx: &Ctx, job_id: Uuid, json_out: bool) -> anyhow::Result<()> {
    let view = fetch_job_output(&ctx.http, &ctx.config, &ctx.identity, job_id).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    let Some(receipt) = view.receipt.as_ref() else {
        // No receipt: still in flight, or ended without producing output.
        println!("job {}: {}", view.job_id, view.status);
        if let Some(reason) = &view.refund_reason {
            println!("  refund: {reason}");
        }
        println!(
            "{}",
            match view.status.as_str() {
                "offered" | "accepted" =>
                    "no output yet — the job is still in flight; try again once it completes",
                _ => "no output — this job was not served",
            }
        );
        return Ok(());
    };
    // A receipt the output does not hash to means the coordinator handed
    // back bytes the operator never signed: say so before anything on
    // stdout, so a piped reader can't mistake unverified bytes for the
    // answer.
    if view.receipt_verified == Some(false) {
        eprintln!(
            "WARNING: this output failed local verification ({}) — do not trust it",
            view.verification_error
                .as_deref()
                .unwrap_or("hash mismatch")
        );
    }
    // A refunded job's output is the operator's signed failure cause, not an
    // answer, and its hold came back. A verified failure receipt hashes fine,
    // so the check above stays silent; warn on stderr the same way, so a piped
    // reader can't mistake the cause for the work it paid for.
    if let Some(reason) = &view.refund_reason {
        eprintln!(
            "WARNING: this job was not served ({reason}). The text below is the operator's \
             signed failure cause, not an answer; your hold was refunded and nothing was charged."
        );
    } else if view.receipt_reports_failure() {
        // The operator signed a non-Ok receipt but the coordinator served the
        // job as completed with no refund. Trust the operator's signed verdict:
        // the text below is the failure cause, and any charge should be
        // disputed rather than accepted as a served answer.
        eprintln!(
            "WARNING: the operator's signed receipt reports this job as {:?}, not a served \
             answer, yet the coordinator did not refund it. The text below is the failure cause; \
             dispute any charge with `dispute`.",
            receipt.receipt.status
        );
    }
    // Re-render the answer the way the buy first showed it: a clip back to
    // a file, tool calls and logprobs whole rather than as their raw JSON
    // block.
    if let Ok(speech) = parse_speech_output(&view.output) {
        let (path, bytes) = save_speech_clip(&speech, view.job_id)?;
        println!("wrote {bytes} bytes of {} audio to {path}", speech.format);
    } else {
        let reply = parse_assistant_output(&view.output);
        if !reply.tool_calls.is_empty() {
            print!("{}", render_tool_calls(&reply));
        } else if let Some(tokens) = &reply.logprobs {
            if !reply.text.is_empty() {
                println!("{}", reply.text);
            }
            print!("{}", render_logprobs(tokens));
        } else {
            println!("{}", output_to_string(&view.output));
        }
    }
    println!();
    let r = &receipt.receipt;
    println!("receipt:");
    println!("  job:       {}", r.job_id);
    println!("  operator:  {}", r.operator.pubkey_base58());
    // Show what settled, not the ceiling: a lease escrows its whole window but
    // is charged only for the seconds it ran, so the receipt's committed price
    // over-reports an early-closed lease. `None` (still in flight, or an older
    // coordinator) leaves the reserved ceiling as all there is to show.
    match view.charged_micro_usdc {
        Some(charged) if charged != r.price_micro_usdc => println!(
            "  charged:   {} (of {} reserved)",
            usdc(charged),
            usdc(r.price_micro_usdc)
        ),
        Some(charged) => println!("  charged:   {}", usdc(charged)),
        None => println!("  price:     {}", usdc(r.price_micro_usdc)),
    }
    if let (Some(tin), Some(tout)) = (r.meter.tokens_in, r.meter.tokens_out) {
        println!("  tokens:    {tin} in, {tout} out");
    }
    println!("  wall:      {} ms", r.meter.wall_ms);
    // Name a non-clean stop so a re-read of a content-filtered or truncated
    // answer shows its cause, not just its text (see write_receipt_block).
    if let Some(reason) = r.meter.finish_reason {
        if reason != FinishReason::Stop {
            println!("  finish:    {}", reason.as_openai());
        }
    }
    match view.receipt_verified {
        Some(true) => println!("  verified:  yes (signature, operator key, output hash)"),
        Some(false) => println!(
            "  verified:  NO — {}",
            view.verification_error
                .as_deref()
                .unwrap_or("failed local verification")
        ),
        None => {}
    }
    println!(
        "{}",
        outcome_line(
            view.refund_reason.as_deref(),
            view.payout
                .as_ref()
                .map(|p| (p.amount_micro_usdc, p.tx_signature.as_deref())),
        )
    );
    Ok(())
}

/// The closing status line of a re-read job. A receipt carrying a refund
/// reason is a failed job — its output is the operator's signed cause, the
/// hold was refunded, and no payout is or ever will be pending; saying
/// "payout pending" there (the old behaviour) reads as money still owed.
/// A completed job shows its payout, or that the retry sweep still owes it.
fn outcome_line(refund_reason: Option<&str>, payout: Option<(u64, Option<&str>)>) -> String {
    if let Some(reason) = refund_reason {
        format!("  outcome:   not served ({reason}) — your hold was refunded, nothing charged")
    } else if let Some((amount, tx)) = payout {
        format!(
            "  payout:    {} {}",
            usdc(amount),
            tx.unwrap_or("(off-chain record)")
        )
    } else {
        "  payout:    pending (the coordinator's retry sweep is pushing it)".to_string()
    }
}

async fn cmd_verify(ctx: &Ctx, job_id: Uuid, json_out: bool) -> anyhow::Result<()> {
    let v = verify_payout(&ctx.http, &ctx.config, &ctx.identity, job_id).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("job:      {}", v.job_id);
    println!("verdict:  {}", v.verdict);
    println!("detail:   {}", v.detail);
    if let Some(memo) = &v.memo {
        println!("memo:     {memo}");
    }
    if let Some(sig) = &v.tx_signature {
        println!("tx:       {sig}");
    }
    if let Some(url) = &v.explorer_url {
        println!("explorer: {url}");
    }
    Ok(())
}

async fn cmd_withdraw(
    ctx: &Ctx,
    amount_micro_usdc: u64,
    recipient: &str,
    withdrawal_id: Option<Uuid>,
    json_out: bool,
) -> anyhow::Result<()> {
    let id = withdrawal_id.unwrap_or_else(Uuid::new_v4);
    let outcome = withdraw(
        &ctx.http,
        &ctx.config,
        &ctx.identity,
        id,
        amount_micro_usdc,
        recipient,
    )
    .await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
        return Ok(());
    }
    println!(
        "withdrawal {} of {}",
        outcome.withdrawal_id,
        usdc(outcome.amount_micro_usdc)
    );
    println!("  to:     {}", outcome.recipient_address_b58);
    if outcome.pushed {
        println!(
            "  status: pushed {}",
            outcome
                .tx_signature
                .as_deref()
                .unwrap_or("(off-chain record)")
        );
    } else {
        println!("  status: debited, transfer pending (the coordinator's sweep re-pushes it)");
    }
    println!("  memo:   {}", outcome.memo);
    Ok(())
}

async fn cmd_withdrawals(ctx: &Ctx, json_out: bool) -> anyhow::Result<()> {
    let rows = list_withdrawals(&ctx.http, &ctx.config, &ctx.identity).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no withdrawals yet");
        return Ok(());
    }
    for w in &rows {
        println!();
        println!(
            "{}  {}  {}",
            w.withdrawal_id,
            if w.pushed { "pushed" } else { "pending" },
            usdc(w.amount_micro_usdc)
        );
        println!("  to:  {}", w.recipient_address_b58);
        if let Some(sig) = &w.tx_signature {
            println!("  tx:  {sig}");
        }
    }
    Ok(())
}

async fn cmd_dispute(
    ctx: &Ctx,
    job_id: Uuid,
    reason: String,
    json_out: bool,
) -> anyhow::Result<()> {
    let outcome = dispute_job(&ctx.http, &ctx.config, &ctx.identity, job_id, reason).await?;
    if json_out {
        let doc = serde_json::json!({
            "job_id": outcome.job_id,
            "operator_pubkey_b58": outcome.operator_pubkey_b58,
            "disputed": outcome.disputed,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    println!(
        "dispute recorded for job {} against operator {}",
        outcome.job_id, outcome.operator_pubkey_b58
    );
    println!("no refund — the outcome is the reputation fault on the coordinator's books");
    Ok(())
}

async fn cmd_cancel(ctx: &Ctx, job_id: Uuid, json_out: bool) -> anyhow::Result<()> {
    let view = cancel_job(&ctx.http, &ctx.config, &ctx.identity, job_id).await?;
    // A cancelled job concluded unpaid, so free any idempotency key that
    // bought it — a later keyed buy may then honestly re-buy without a
    // manual key change. Best-effort: the key also self-frees on its next
    // re-drive, so a book hiccup here never blocks the completed refund.
    free_keyed_purchase(job_id);
    if json_out {
        let doc = serde_json::json!({
            "job_id": view.job_id,
            "status": view.status,
            "refunded_micro_usdc": view.refunded_micro_usdc,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    println!(
        "job {} {} — refunded {}",
        view.job_id,
        view.status,
        usdc(view.refunded_micro_usdc)
    );
    Ok(())
}

/// The flags of `lease open`, gathered so the command's own signature
/// stays readable.
struct LeaseOpenArgs {
    minutes: Option<u64>,
    duration_secs: Option<u64>,
    rate: Option<u64>,
    ssh_key: Option<String>,
    gpu_class: Option<String>,
    min_vram_gb: Option<u32>,
    min_reputation_bps: Option<u32>,
    wait_secs: Option<u64>,
    no_wait: bool,
}

/// `lease open` — rent a whole machine for a bounded window. The buyer
/// escrows the window's ceiling (rate x duration) up front and is billed
/// only for the seconds the session runs; the unused remainder is
/// refunded on close. The operator holds the session open, not this
/// command, so the machine keeps running after `open` returns until
/// `lease close` ends it or the window expires.
async fn cmd_lease_open(ctx: &Ctx, args: LeaseOpenArgs, json_out: bool) -> anyhow::Result<()> {
    let max_duration_secs = match (args.minutes, args.duration_secs) {
        (Some(_), Some(_)) => anyhow::bail!("give --minutes or --duration-secs, not both"),
        (Some(m), None) => m
            .checked_mul(60)
            .context("--minutes is too large to express as seconds")?,
        (None, Some(s)) => s,
        (None, None) => {
            anyhow::bail!("lease open needs a window — pass --minutes N (or --duration-secs N)")
        }
    };
    let rate_micro_usdc_per_sec = args.rate.context(
        "lease open needs --rate <micro-USDC per second>: the price for each second the session \
         runs. The whole window (rate x duration) is escrowed, and the unused part is refunded \
         when you close.",
    )?;
    let client_public_key = args.ssh_key.as_deref().map(read_ssh_key).transpose()?;
    if client_public_key.is_none() {
        eprintln!(
            "note: no --ssh-key given. A GPU broker cannot hand you a machine without your public \
             key, so this lease may find no supply — pass --ssh-key ~/.ssh/id_ed25519.pub."
        );
    }
    let terms = LeaseTerms {
        max_duration_secs,
        rate_micro_usdc_per_sec,
        client_public_key,
    };
    terms.validate().map_err(|e| anyhow::anyhow!("{e}"))?;
    let price = terms
        .max_price_micro_usdc()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let cap = ctx.max_price_micro_usdc;
    anyhow::ensure!(
        price <= cap,
        "this lease escrows {price} micro-USDC (rate x window), above the per-call ceiling {cap} \
         — shorten the window, lower --rate, or raise COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC"
    );
    let deadline_ms = max_duration_secs
        .saturating_mul(1_000)
        .saturating_add(LEASE_DEADLINE_SLACK_MS);
    let request = JobRequest {
        kind: JobKind::LeaseSession,
        input: vec![lease_input(terms).map_err(|e| anyhow::anyhow!("{e}"))?],
        model: None,
        gpu_class: args.gpu_class,
        min_vram_gb: args.min_vram_gb,
        min_reputation_bps: args.min_reputation_bps,
        price_micro_usdc: price,
        deadline_ms,
    };
    let envelope = submit_streaming(
        &ctx.http,
        &ctx.config,
        &ctx.identity,
        Uuid::new_v4(),
        request,
    )
    .await
    .map_err(with_funding_hint)?;
    let job_id = envelope.payload.job_id;
    eprintln!(
        "lease {job_id} opened — {} escrowed for up to {max_duration_secs}s at \
         {rate_micro_usdc_per_sec} micro-USDC/s. Billed by the second; close early to stop the \
         meter.",
        usdc(price)
    );

    // The machine comes up out of band: the operator publishes its
    // address to the coordinator, and the buyer reads it from the lease
    // view (it never rides the token stream). Poll until it lands, unless
    // the buyer asked not to wait.
    if args.no_wait {
        eprintln!("watch it come up with:  covenant-compute lease view {job_id}");
        let view = lease_view(&ctx.http, &ctx.config, &ctx.identity, job_id).await?;
        return render_lease_view(&view, json_out);
    }
    let wait = Duration::from_secs(args.wait_secs.unwrap_or(240));
    let stop_at = std::time::Instant::now() + wait;
    loop {
        let view = lease_view(&ctx.http, &ctx.config, &ctx.identity, job_id).await?;
        let terminal = matches!(
            view.status.as_str(),
            "completed" | "failed" | "refunded" | "rejected"
        );
        if view.access.is_some() || terminal {
            return render_lease_view(&view, json_out);
        }
        if std::time::Instant::now() >= stop_at {
            eprintln!(
                "the machine is not reachable yet after {}s; it may still be coming up. \
                 Check again with:  covenant-compute lease view {job_id}",
                wait.as_secs()
            );
            return render_lease_view(&view, json_out);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// `lease view <job-id>` — the buyer's live meter for one lease: where
/// the machine is, how long it has run, and what it has cost so far.
async fn cmd_lease_view(ctx: &Ctx, job_id: Uuid, json_out: bool) -> anyhow::Result<()> {
    let view = lease_view(&ctx.http, &ctx.config, &ctx.identity, job_id).await?;
    render_lease_view(&view, json_out)
}

/// `lease close <job-id>` — end a running session now. Records the
/// buyer's signed close; the operator sees it, releases the machine and
/// submits its receipt, which settles the meter. Safe to retry.
async fn cmd_lease_close(ctx: &Ctx, job_id: Uuid, json_out: bool) -> anyhow::Result<()> {
    let view = close_lease(&ctx.http, &ctx.config, &ctx.identity, job_id).await?;
    render_lease_view(&view, json_out)
}

/// One rendering of a lease view, shared by open, view and close so the
/// three never drift. `--json` prints the raw record for scripting.
fn render_lease_view(view: &LeaseView, json_out: bool) -> anyhow::Result<()> {
    if json_out {
        println!("{}", serde_json::to_string_pretty(view)?);
        return Ok(());
    }
    println!("lease {} — {}", view.job_id, view.status);
    let terminal = matches!(
        view.status.as_str(),
        "completed" | "failed" | "refunded" | "rejected"
    );
    if terminal {
        println!("  the session has ended");
    } else {
        match &view.access {
            Some(access) => {
                println!("  reach it:  {}", access.endpoint);
                if let Some(note) = &access.note {
                    println!("  {note}");
                }
            }
            None => println!("  the machine is not reachable yet"),
        }
    }
    let ceiling = view
        .rate_micro_usdc_per_sec
        .saturating_mul(view.max_duration_secs);
    println!(
        "  {}s of {}s elapsed, billed {} of a {} ceiling ({} micro-USDC/s)",
        view.elapsed_ms / 1_000,
        view.max_duration_secs,
        usdc(view.charged_micro_usdc),
        usdc(ceiling),
        view.rate_micro_usdc_per_sec
    );
    if view.close_requested && !terminal {
        println!("  close requested — the session is ending");
    }
    Ok(())
}

/// This buyer's vault keyring, under the same home the identity lives in.
/// The keys it holds open the ciphertext the coordinator stores; they
/// never leave this machine.
fn vault_keyring() -> anyhow::Result<VaultKeyring> {
    let home = prepared_home()?;
    VaultKeyring::open(home.join("vault-keys.json")).map_err(|e| anyhow::anyhow!("{e}"))
}

/// The bytes to seal for `vault put`: an inline argument, a file, or
/// stdin (`-`). A secret may be binary, so this reads raw bytes rather
/// than a string, and refuses more than one source so there is no
/// ambiguity about what was stored.
fn read_secret_value(inline: Option<String>, file: Option<String>) -> anyhow::Result<Vec<u8>> {
    match (inline, file) {
        (Some(v), Some(_)) if v != "-" => {
            anyhow::bail!("pass the secret inline or with --file, not both")
        }
        (_, Some(path)) => {
            std::fs::read(&path).with_context(|| format!("read the secret from {path}"))
        }
        (Some(v), None) if v == "-" => {
            use std::io::Read;
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("read the secret from stdin")?;
            anyhow::ensure!(!buf.is_empty(), "stdin was empty — nothing to store");
            Ok(buf)
        }
        (Some(v), None) => Ok(v.into_bytes()),
        (None, None) => anyhow::bail!(
            "vault put needs a value — pass it inline, read a file with --file <path>, or pipe it \
             and use `-`"
        ),
    }
}

/// `vault put <label> [value]` — seal a secret under this buyer's key for
/// `label` and store the ciphertext. A fresh label mints and saves a key;
/// re-storing a label reuses the key already held, so the value can still
/// be opened. The key is persisted before the store, so a store that
/// fails leaves a reusable key, never an unopenable secret.
async fn cmd_vault_put(
    ctx: &Ctx,
    label: &str,
    value: Option<String>,
    file: Option<String>,
    json_out: bool,
) -> anyhow::Result<()> {
    let plaintext = read_secret_value(value, file)?;
    let mut ring = vault_keyring()?;
    let (key, minted) = ring.ensure(label).map_err(|e| anyhow::anyhow!("{e}"))?;
    vault_store(
        &ctx.http,
        &ctx.config,
        &ctx.identity,
        &key,
        label,
        &plaintext,
    )
    .await?;
    if json_out {
        let doc = serde_json::json!({
            "label": label,
            "sealed_bytes": plaintext.len(),
            "key_minted": minted,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    println!("stored '{label}' — {} bytes sealed", plaintext.len());
    if minted {
        println!(
            "a new key for '{label}' was saved to your keyring on this machine only. Back it up \
             with:  covenant-compute vault key export {label}"
        );
    }
    Ok(())
}

/// `vault get <label>` — fetch and open a secret, writing the raw
/// plaintext to stdout (no trailing newline) so it round-trips a binary
/// value through a pipe. Refuses clearly when this machine holds no key
/// for the label, which is distinct from the coordinator having no such
/// secret.
async fn cmd_vault_get(ctx: &Ctx, label: &str, json_out: bool) -> anyhow::Result<()> {
    let ring = vault_keyring()?;
    let key = ring.get(label).ok_or_else(|| {
        anyhow::anyhow!(
            "no local key for '{label}' — this machine cannot open that secret. If you stored it \
             from another machine, import its key with:  covenant-compute vault key import {label} \
             <key>"
        )
    })?;
    let plaintext = vault_fetch(&ctx.http, &ctx.config, &ctx.identity, &key, label).await?;
    if json_out {
        let doc = serde_json::json!({
            "label": label,
            "bytes": plaintext.len(),
            "value": std::str::from_utf8(&plaintext).ok(),
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    use std::io::Write;
    std::io::stdout().write_all(&plaintext)?;
    Ok(())
}

/// `vault ls` — the buyer's stored secrets, each marked with whether this
/// machine holds the key to open it, plus any keys held locally with
/// nothing stored under them.
async fn cmd_vault_ls(ctx: &Ctx, json_out: bool) -> anyhow::Result<()> {
    let stored = vault_list(&ctx.http, &ctx.config, &ctx.identity).await?;
    let ring = vault_keyring()?;
    let local: std::collections::BTreeSet<String> = ring.labels().map(str::to_string).collect();
    let stored_labels: std::collections::BTreeSet<String> =
        stored.iter().map(|m| m.label.clone()).collect();
    let local_only: Vec<&String> = local.difference(&stored_labels).collect();

    if json_out {
        let secrets: Vec<Value> = stored
            .iter()
            .map(|m| {
                serde_json::json!({
                    "label": m.label,
                    "ciphertext_len": m.ciphertext_len,
                    "created_at_ms": m.created_at_ms,
                    "updated_at_ms": m.updated_at_ms,
                    "openable": local.contains(&m.label),
                })
            })
            .collect();
        let doc = serde_json::json!({ "secrets": secrets, "local_only_keys": local_only });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

    if stored.is_empty() && local.is_empty() {
        println!("no secrets stored, and no keys held on this machine");
        return Ok(());
    }
    if !stored.is_empty() {
        println!("stored with the coordinator:");
        for m in &stored {
            let openable = if local.contains(&m.label) {
                "openable"
            } else {
                "NO LOCAL KEY"
            };
            println!(
                "  {}  ({} bytes sealed)  [{openable}]",
                m.label, m.ciphertext_len
            );
        }
    }
    if !local_only.is_empty() {
        println!("keys held on this machine with nothing stored:");
        for label in local_only {
            println!("  {label}");
        }
    }
    Ok(())
}

/// `vault rm <label>` — delete the secret with the coordinator, then drop
/// the local key. Deleting first means a failure never leaves a stored
/// secret this machine can no longer open. Idempotent on both sides.
async fn cmd_vault_rm(ctx: &Ctx, label: &str, json_out: bool) -> anyhow::Result<()> {
    vault_delete(&ctx.http, &ctx.config, &ctx.identity, label).await?;
    let mut ring = vault_keyring()?;
    let key_removed = ring.remove(label).map_err(|e| anyhow::anyhow!("{e}"))?;
    if json_out {
        let doc = serde_json::json!({
            "label": label,
            "deleted": true,
            "local_key_removed": key_removed,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    let dropped = if key_removed {
        " and dropped its local key"
    } else {
        ""
    };
    println!("deleted '{label}'{dropped}");
    Ok(())
}

/// `vault key export <label>` — print the base58 key that opens `label`,
/// for backup or to move it to another machine. Prints the key alone so
/// it pipes cleanly; keep the output as private as the secret itself.
fn cmd_vault_key_export(label: &str, json_out: bool) -> anyhow::Result<()> {
    let ring = vault_keyring()?;
    let key = ring
        .get(label)
        .ok_or_else(|| anyhow::anyhow!("no local key for '{label}'"))?;
    if json_out {
        let doc = serde_json::json!({ "label": label, "key": key.to_b58() });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    println!("{}", key.to_b58());
    Ok(())
}

/// `vault key import <label> <key>` — save a key exported from another
/// machine so this one can open the label's secret. Replacing a differing
/// key already held needs `--force`, since the old key (and anything only
/// it opens) becomes unrecoverable from here.
fn cmd_vault_key_import(
    label: &str,
    key_b58: &str,
    force: bool,
    json_out: bool,
) -> anyhow::Result<()> {
    let key = VaultKey::from_b58(key_b58.trim())
        .map_err(|e| anyhow::anyhow!("not a valid vault key: {e}"))?;
    let mut ring = vault_keyring()?;
    let mut replaced = false;
    if let Some(existing) = ring.get(label) {
        if existing.as_bytes() == key.as_bytes() {
            if json_out {
                let doc =
                    serde_json::json!({ "label": label, "imported": false, "reason": "unchanged" });
                println!("{}", serde_json::to_string_pretty(&doc)?);
            } else {
                println!("key for '{label}' is already held — nothing to do");
            }
            return Ok(());
        }
        anyhow::ensure!(
            force,
            "a different key for '{label}' is already held — pass --force to replace it (the old \
             key, and any secret only it opens, becomes unrecoverable from here)"
        );
        replaced = true;
    }
    ring.insert(label, key)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if json_out {
        let doc = serde_json::json!({ "label": label, "imported": true, "replaced": replaced });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    let verb = if replaced { "replaced" } else { "imported" };
    println!("{verb} the key for '{label}'");
    Ok(())
}

/// Reads an OpenSSH public-key line for a lease's `client_public_key`,
/// mirroring how `--messages-file` reads its input. The key is the
/// buyer's own `authorized_keys` line (e.g. `~/.ssh/id_ed25519.pub`);
/// it is trimmed to one line, and the protocol validates it before it is
/// signed into the lease terms.
fn read_ssh_key(path: &str) -> anyhow::Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("could not read the ssh public key at {path}"))?;
    let key = raw.trim().to_string();
    anyhow::ensure!(!key.is_empty(), "the ssh public key file {path} is empty");
    Ok(key)
}

/// The terminal rendering of per-token log probabilities: one line per
/// generated token, the token quoted so leading spaces and newlines read,
/// then its log probability, then the alternatives the buyer asked for
/// (`--logprobs n`) when any. The full structure, `bytes` included, is one
/// `--json` away; this is the summary a person scans.
fn render_logprobs(tokens: &[covenant_compute_protocol::TokenLogprob]) -> String {
    let mut out = String::from("logprobs:\n");
    for t in tokens {
        out.push_str(&format!("  {:?} {:.3}", t.token, t.logprob));
        if !t.top_logprobs.is_empty() {
            let alts = t
                .top_logprobs
                .iter()
                .map(|a| format!("{:?} {:.3}", a.token, a.logprob))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("  (alternatives: {alts})"));
        }
        out.push('\n');
    }
    out
}

/// The terminal rendering of a tool-calling reply: any prose the model
/// returned first, then the calls it asked for, one `name(arguments)` per
/// line. `arguments` is the model's own JSON string, printed as attested.
/// The `--json` form is built inline in [`render_outcome`]; this is the
/// form a person reads.
fn render_tool_calls(reply: &AssistantReply) -> String {
    let mut out = String::new();
    if !reply.text.is_empty() {
        out.push_str(&reply.text);
        out.push_str("\n\n");
    }
    out.push_str(if reply.tool_calls.len() == 1 {
        "tool call:\n"
    } else {
        "tool calls:\n"
    });
    for call in &reply.tool_calls {
        out.push_str(&format!(
            "  {}({})\n",
            call.function.name, call.function.arguments
        ));
    }
    out
}

fn output_to_string(output: &[Content]) -> String {
    output
        .iter()
        .map(|c| match c {
            Content::Text { text } => text.clone(),
            Content::Json { value } => value.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compute_home() -> anyhow::Result<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_COMPUTE_MCP_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant-compute-mcp"))
}

/// The resolved home, created if absent — the point past which the
/// buyer identity can be loaded.
fn prepared_home() -> anyhow::Result<PathBuf> {
    let home = compute_home()?;
    std::fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;
    Ok(home)
}

/// The same key file, under the same seed string, the MCP server loads —
/// so a person and their agent are one buyer.
fn load_identity(home: &Path) -> anyhow::Result<LocalIdentity> {
    LocalIdentity::load_or_create(&home.join("identity.json"), "buyer@compute")
        .context("load or create buyer identity")
}

/// `--flag` present anywhere in the args, removed in place.
fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    if let Some(i) = args.iter().position(|a| a == name) {
        args.remove(i);
        return true;
    }
    false
}

fn positional(args: &[String], index: usize, name: &str) -> anyhow::Result<String> {
    args.get(index)
        .filter(|v| !v.trim().is_empty())
        .cloned()
        .with_context(|| format!("missing <{name}> — run `--help` for usage"))
}

fn parse_job_id(args: &[String], index: usize) -> anyhow::Result<Uuid> {
    positional(args, index, "job-id")?
        .parse()
        .context("<job-id> must be a job UUID")
}

/// `--name value` or `--name=value`, removed in place. A following token
/// that itself looks like a flag is not consumed as the value.
fn take_flag_value(args: &mut Vec<String>, name: &str) -> Option<String> {
    let eq = format!("{name}=");
    if let Some(i) = args.iter().position(|a| a.starts_with(&eq)) {
        return Some(args.remove(i)[eq.len()..].to_string());
    }
    let i = args.iter().position(|a| a == name)?;
    args.remove(i);
    if i < args.len() && !args[i].starts_with("--") {
        return Some(args.remove(i));
    }
    Some(String::new())
}

fn take_price(args: &mut Vec<String>) -> anyhow::Result<Option<u64>> {
    take_flag_value(args, "--price")
        .map(|v| {
            v.parse()
                .context("--price must be a whole number of micro-USDC")
        })
        .transpose()
}

/// `--gpu-class value`, trimmed. A bare flag (no value) is refused here
/// rather than passed on: an empty class names no hardware and would
/// otherwise surface as "no operator serving" instead of the real slip.
fn take_gpu_class(args: &mut Vec<String>) -> anyhow::Result<Option<String>> {
    match take_flag_value(args, "--gpu-class") {
        Some(c) if c.trim().is_empty() => {
            anyhow::bail!("--gpu-class needs a value like rtx-4090, h100, or cpu (see `capacity`)")
        }
        Some(c) => Ok(Some(c.trim().to_string())),
        None => Ok(None),
    }
}

/// A numeric flag, parsed to `T` or a loud error naming what the flag
/// wants — a mistyped sampling knob on a paid call must fail here, not
/// reach an operator as junk input.
fn take_number<T>(args: &mut Vec<String>, name: &str, wants: &str) -> anyhow::Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    take_flag_value(args, name)
        .map(|v| {
            v.parse::<T>()
                .map_err(|e| anyhow::anyhow!("{name} must be {wants} (got {v:?}): {e}"))
        })
        .transpose()
}

/// Every occurrence of a repeatable flag, in order, removed in place —
/// `--stop A --stop B` collects both.
fn take_flag_values(args: &mut Vec<String>, name: &str) -> Vec<String> {
    let mut values = Vec::new();
    while let Some(v) = take_flag_value(args, name) {
        values.push(v);
    }
    values
}

/// The sampling knobs for an `infer` buy, packed into one signed
/// generation block through the same `generation_input` the MCP and
/// native `compute.infer` surfaces use — so `--temperature 0` means the
/// same thing at the terminal as it does to an agent. `Ok(None)` when the
/// buyer set none (the operator runs at backend defaults); an
/// out-of-range knob fails here, before any key touch or dispatch.
fn infer_generation(args: &mut Vec<String>) -> anyhow::Result<Option<Content>> {
    let stop = take_flag_values(args, "--stop");
    let response_format = take_flag_value(args, "--response-format")
        .map(|v| parse_response_format(&v))
        .transpose()?;
    let params = GenerationParams {
        temperature: take_number(args, "--temperature", "a number in 0..=2")?,
        top_p: take_number(args, "--top-p", "a number in (0, 1]")?,
        max_tokens: take_number(args, "--max-tokens", "a whole number of tokens")?,
        seed: take_number(args, "--seed", "a whole number")?,
        presence_penalty: take_number(args, "--presence-penalty", "a number in -2..=2")?,
        frequency_penalty: take_number(args, "--frequency-penalty", "a number in -2..=2")?,
        stop: (!stop.is_empty()).then_some(stop),
        response_format,
        logprobs: take_number(args, "--logprobs", "a whole number of alternatives 0..=20")?,
    };
    if params.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        generation_input(params).map_err(|e| anyhow::anyhow!(e))?,
    ))
}

/// The tools an `infer` buy offers the model, packed into one signed tools
/// block through the same `tools_input` the MCP and native `compute.infer`
/// surfaces use. `--tools` names a JSON file of tool definitions (OpenAI's
/// function shape); `--tool-choice` sets whether the model may, must, or
/// must not call one, or names the single function to force. `Ok(None)` when
/// the buyer offered none; a `--tool-choice` that forces a call with no
/// tools to choose from fails here, before any key touch or dispatch.
fn infer_tools(args: &mut Vec<String>) -> anyhow::Result<Option<Content>> {
    let tools_path = take_flag_value(args, "--tools");
    let tool_choice = take_flag_value(args, "--tool-choice")
        .map(|v| parse_tool_choice(&v))
        .transpose()?;
    let Some(path) = tools_path else {
        if matches!(
            tool_choice,
            Some(ToolChoice::Mode(ToolChoiceMode::Required)) | Some(ToolChoice::Named(_))
        ) {
            anyhow::bail!(
                "--tool-choice forces a tool call but --tools offered none; add --tools <file> \
                 or drop --tool-choice"
            );
        }
        return Ok(None);
    };
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("--tools file {path:?} could not be read"))?;
    let tools: Vec<ToolDefinition> = serde_json::from_str(&raw).with_context(|| {
        format!("--tools file {path:?} is not a JSON array of tool definitions")
    })?;
    Ok(Some(
        tools_input(tools, tool_choice).map_err(|e| anyhow::anyhow!(e))?,
    ))
}

/// Parses `--tool-choice`: the bare modes `auto`, `none`, or `required`, or
/// a function name to force that one tool (OpenAI's
/// `{"type":"function",...}` object, spelled as just the name here).
fn parse_tool_choice(value: &str) -> anyhow::Result<ToolChoice> {
    let trimmed = value.trim();
    anyhow::ensure!(
        !trimmed.is_empty(),
        "--tool-choice was empty — drop it, or name a mode (auto|none|required) or a function"
    );
    Ok(match trimmed.to_ascii_lowercase().as_str() {
        "auto" => ToolChoice::Mode(ToolChoiceMode::Auto),
        "none" => ToolChoice::Mode(ToolChoiceMode::None),
        "required" => ToolChoice::Mode(ToolChoiceMode::Required),
        _ => ToolChoice::Named(NamedToolChoice {
            kind: ToolKind::Function,
            function: NamedFunction {
                name: trimmed.to_string(),
            },
        }),
    })
}

/// `--response-format json` asks the model for any valid JSON; a path to a
/// JSON schema file asks it to conform to that schema (structured output),
/// named after the file. The most common human need is "give me JSON", so
/// the bare `json` keyword is the ergonomic case.
fn parse_response_format(value: &str) -> anyhow::Result<ResponseFormat> {
    if value.eq_ignore_ascii_case("json") || value.eq_ignore_ascii_case("json_object") {
        return Ok(ResponseFormat::JsonObject);
    }
    let raw = std::fs::read_to_string(value).map_err(|e| {
        anyhow::anyhow!(
            "--response-format takes 'json' or a path to a JSON schema file \
             (reading {value:?}: {e})"
        )
    })?;
    let schema: Value = serde_json::from_str(&raw).map_err(|e| {
        anyhow::anyhow!("--response-format schema file {value:?} is not valid JSON: {e}")
    })?;
    let name = std::path::Path::new(value)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("schema")
        .to_string();
    Ok(ResponseFormat::JsonSchema {
        name,
        schema,
        strict: Some(true),
    })
}

/// The job input for an `infer` buy: a full conversation from
/// `--messages-file`, or the bare prompt, or a two-turn conversation when
/// `--system` set a system prompt to steer the model, plus the signed
/// generation block from any sampling knob. Packed through the same
/// `chat_input`/`generation_input` the MCP and native surfaces use, so the
/// terminal reaches an operator with the same input shape an agent does.
fn infer_input(
    system: Option<String>,
    messages_file: Option<String>,
    prompt: Option<String>,
    images: Vec<String>,
    generation: Option<Content>,
) -> anyhow::Result<Vec<Content>> {
    let mut input = match messages_file {
        Some(path) => {
            anyhow::ensure!(
                system.is_none(),
                "--messages-file already carries the whole conversation — put the system turn \
                 in the file instead of passing --system"
            );
            anyhow::ensure!(
                prompt.is_none(),
                "--messages-file already carries the whole conversation — drop the positional prompt"
            );
            anyhow::ensure!(
                images.is_empty(),
                "--messages-file already carries the whole conversation — attach images to a \
                 message inside the file instead of passing --image"
            );
            chat_input(read_messages(&path)?)
        }
        None => {
            let prompt =
                resolve_text(prompt.context("missing <prompt> — run `infer --help` for usage")?)?;
            let user = if images.is_empty() {
                ChatMessage::user(prompt.clone())
            } else {
                ChatMessage::user_with_images(prompt.clone(), images)
            };
            match system {
                Some(system) => {
                    let system = system.trim();
                    anyhow::ensure!(
                        !system.is_empty(),
                        "--system was empty — drop it or give it text"
                    );
                    chat_input(vec![ChatMessage::system(system), user])
                }
                // A plain prompt with no system turn and no images stays the
                // raw-prompt block, byte-identical to before vision existed.
                None if user.images.is_empty() => vec![Content::Text { text: prompt }],
                None => chat_input(vec![user]),
            }
        }
    };
    input.extend(generation);
    Ok(input)
}

/// Reads an image file for `infer --image` and base64-encodes it, the form
/// the signed job and every backend consume. An unreadable or empty file
/// fails here, before any key touch or dispatch.
fn read_image(path: &str) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read the image file {path:?}"))?;
    anyhow::ensure!(!bytes.is_empty(), "image file {path:?} is empty");
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Reads an audio file for `transcribe --audio` and base64-encodes it, the
/// form the transcription input carries over the wire. An unreadable or
/// empty file fails here, before any key touch or dispatch.
fn read_audio(path: &str) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read the audio file {path:?}"))?;
    anyhow::ensure!(!bytes.is_empty(), "audio file {path:?} is empty");
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Reads a full conversation for `infer --messages-file`: a JSON array of
/// `{"role","content"}` turns (role is system, user, or assistant), the
/// same shape the MCP `messages` argument takes, so a scripted multi-turn
/// buy reaches an operator identically whether it comes from the terminal
/// or an agent. `-` reads the array from stdin.
fn read_messages(path: &str) -> anyhow::Result<Vec<ChatMessage>> {
    let raw = if path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("read the conversation from stdin")?;
        buf
    } else {
        std::fs::read_to_string(path)
            .with_context(|| format!("read the conversation file {path:?}"))?
    };
    messages_from_str(&raw, if path == "-" { "stdin" } else { path })
}

fn messages_from_str(raw: &str, source: &str) -> anyhow::Result<Vec<ChatMessage>> {
    let messages: Vec<ChatMessage> = serde_json::from_str(raw.trim()).with_context(|| {
        format!(
            "{source} must be a JSON array of {{\"role\", \"content\"}} messages \
             (role is system, user, or assistant)"
        )
    })?;
    anyhow::ensure!(
        !messages.is_empty(),
        "{source} carries an empty conversation — add at least one message"
    );
    Ok(messages)
}

/// The positionals left after flags are stripped, joined into one
/// argument — so a prompt or command can be typed unquoted.
fn rest_joined(args: &[String], name: &str) -> anyhow::Result<String> {
    let joined = args.join(" ").trim().to_string();
    anyhow::ensure!(
        !joined.is_empty(),
        "missing <{name}> — run `--help` for usage"
    );
    Ok(joined)
}

/// Guards an `infer` prompt or `embed` text against a mistyped flag.
/// Once the known flags are stripped, a leftover `--foo` is a typo, and
/// folding it into a PAID job bills the buyer for corrupted input.
/// Refuse it. A leading `--` ends option parsing so the text may
/// legitimately start with dashes; `-` (stdin) is the other escape.
/// `run`'s command is arbitrary shell where flags belong, so it keeps
/// the permissive join.
fn reject_stray_flags(args: &mut Vec<String>) -> anyhow::Result<()> {
    if args.first().map(String::as_str) == Some("--") {
        args.remove(0);
        return Ok(());
    }
    if let Some(flag) = args.iter().find(|a| a.starts_with("--")) {
        anyhow::bail!(
            "unknown flag {flag:?} — to include it in the text, quote the whole argument or \
             read it from stdin with `-`"
        );
    }
    Ok(())
}

/// A lone `-` reads the prompt or command from stdin, the usual CLI
/// convention for input too long or multi-line to sit on the argv.
fn resolve_text(text: String) -> anyhow::Result<String> {
    if text != "-" {
        return Ok(text);
    }
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("read from stdin")?;
    let buf = buf.trim().to_string();
    anyhow::ensure!(!buf.is_empty(), "stdin was empty — nothing to buy");
    Ok(buf)
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Reads a numeric knob: unset (or blank) takes `default`, a set but
/// unparseable value fails loudly instead of silently falling back — a
/// mistyped `COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC` must not quietly
/// become the permissive default and remove the spend cap the operator
/// meant to set. `hint` reads into "{key} {hint}".
fn env_or<T: std::str::FromStr>(key: &str, default: T, hint: &str) -> anyhow::Result<T>
where
    T::Err: std::fmt::Display,
{
    match env_opt(key) {
        Some(v) => v
            .parse()
            .map_err(|e| anyhow::anyhow!("{key} {hint} (got {v:?}): {e}")),
        None => Ok(default),
    }
}

fn usdc(micro: u64) -> String {
    format!("{micro} micro-USDC (${:.6})", micro as f64 / 1e6)
}

fn field_u64(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn kind_label(kind: JobKind) -> &'static str {
    match kind {
        JobKind::InferenceCall => "inference_call",
        JobKind::BatchJob => "batch_job",
        JobKind::LeaseSession => "lease_session",
        JobKind::Embedding => "embedding",
        JobKind::Transcription => "transcription",
        JobKind::SpeechSynthesis => "speech_synthesis",
        JobKind::AgentTask => "agent_task",
        JobKind::AgentCheck => "agent_check",
    }
}

/// Names a job's constraints for a "nothing serves this" refusal, so the
/// buyer sees exactly which of kind, model, and hardware went unmet.
fn describe_request(
    kind: JobKind,
    model: Option<&str>,
    gpu_class: Option<&str>,
    min_vram_gb: Option<u32>,
) -> String {
    let mut desc = match model {
        Some(m) => format!("model {m:?} for {}", kind_label(kind)),
        None => format!("{} jobs", kind_label(kind)),
    };
    if let Some(class) = gpu_class {
        desc.push_str(&format!(" on gpu-class {class:?}"));
    }
    if let Some(vram) = min_vram_gb {
        desc.push_str(&format!(" with >={vram}GB VRAM"));
    }
    desc
}

/// Neutralizes control characters (to spaces) in operator-authored text
/// before it reaches the terminal. A capacity row's model name and
/// gpu-class labels come from an operator's own registration, which
/// anyone holding a keypair can send — without this a declared model
/// carrying an ANSI escape could rewrite or forge lines on a buyer's
/// screen when they run `capacity`. A real model id never contains a
/// control character, so this only ever touches hostile input.
fn flatten_controls(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn unit_label(unit: PriceUnit) -> &'static str {
    match unit {
        PriceUnit::PerMillionTokens => "per Mtok",
        PriceUnit::PerGpuSecond => "per gpu-s",
        PriceUnit::PerLeaseHour => "per hr",
        PriceUnit::PerJob => "per job",
    }
}

#[cfg(test)]
mod tests {
    use super::with_funding_hint;
    use covenant_compute_buyer::BuyerError;

    #[test]
    fn an_underfunded_refusal_names_the_deposit_path() {
        let hinted = with_funding_hint(BuyerError::SubmitRefused {
            status: 402,
            body: "buyer X has insufficient funds: hold needs 1000 micro-USDC, 0 available".into(),
        })
        .to_string();
        // The original shortfall survives, and the fix the rest of the
        // CLI documents is appended so the buyer knows what to do next.
        assert!(hinted.contains("insufficient funds"), "{hinted}");
        assert!(hinted.contains("covenant-compute balance"), "{hinted}");
        assert!(hinted.contains("deposit"), "{hinted}");
    }

    #[test]
    fn other_refusals_pass_through_without_a_deposit_hint() {
        let plain = with_funding_hint(BuyerError::SubmitRefused {
            status: 400,
            body: "job input malformed".into(),
        })
        .to_string();
        assert!(plain.contains("job input malformed"), "{plain}");
        assert!(!plain.contains("covenant-compute balance"), "{plain}");
    }

    #[test]
    fn a_re_read_failed_job_reads_as_refunded_never_payout_pending() {
        // A failed job carries a receipt (so it reaches this line) AND a
        // refund reason: it must read as refunded, never as a payout the
        // sweep still owes.
        let failed = super::outcome_line(Some("execution_failed"), None);
        assert!(failed.contains("refunded"), "{failed}");
        assert!(!failed.contains("pending"), "{failed}");
        // A completed job the sweep still owes stays "pending".
        let owed = super::outcome_line(None, None);
        assert!(owed.contains("pending"), "{owed}");
        // A paid completed job shows its transfer.
        let paid = super::outcome_line(None, Some((1_000, Some("5sIg"))));
        assert!(paid.contains("5sIg") && paid.contains("payout"), "{paid}");
        let offchain = super::outcome_line(None, Some((1_000, None)));
        assert!(offchain.contains("off-chain record"), "{offchain}");
    }

    #[test]
    fn the_receipt_block_reports_the_charged_offer_not_the_receipt_claim() {
        use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
        use covenant_compute_buyer::DispatchOutcome;
        use covenant_compute_protocol::{
            CapabilityRequirement, JobEnvelopePayload, JobKind, JobMeter, SignedJobEnvelope,
            SignedWorkReceipt, WorkReceiptPayload,
        };
        use covenant_identity::LocalIdentity;
        use covenant_mcp::Content;
        use uuid::Uuid;

        let buyer = LocalIdentity::generate("buyer@test");
        let operator = LocalIdentity::generate("operator@test");
        let job_id = Uuid::new_v4();

        // The buyer offered 60_000 and is charged that in full; the operator
        // signed a receipt claiming only 16_496 (verify_receipt allows a
        // receipt below the offer). The block must show the charge.
        let envelope = SignedJobEnvelope::sign(
            JobEnvelopePayload {
                job_id,
                buyer: buyer.agent_id(),
                kind: JobKind::InferenceCall,
                capability_requirement: CapabilityRequirement {
                    gpu_class: None,
                    min_vram_gb: None,
                    model_id: None,
                    kind: JobKind::InferenceCall,
                    max_duration_secs: 60,
                    min_reputation_bps: None,
                },
                input: vec![Content::text("hi")],
                price_micro_usdc: 60_000,
                deadline_ms: 60_000,
                idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "rcpt-test"),
                issued_at_ms: 0,
                referral_code: None,
                stream: false,
            },
            &buyer,
        )
        .expect("sign envelope");

        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "aa".repeat(32),
                result_hash_hex: "bb".repeat(32),
                meter: JobMeter {
                    wall_ms: 1_000,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 16_496,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 0,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .expect("sign receipt");

        let outcome = DispatchOutcome {
            envelope,
            receipt,
            output: vec![Content::text("hi")],
            payout: None,
        };

        let mut buf = Vec::new();
        super::write_receipt_block(&mut buf, &outcome).expect("write");
        let rendered = String::from_utf8(buf).unwrap();
        assert!(
            rendered.contains("price:     60000 micro-USDC"),
            "reports the charged offer: {rendered}"
        );
        assert!(
            !rendered.contains("16496"),
            "must not report the receipt's lower claim: {rendered}"
        );
    }

    #[test]
    fn the_receipt_block_names_a_non_clean_finish() {
        use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
        use covenant_compute_buyer::DispatchOutcome;
        use covenant_compute_protocol::{
            CapabilityRequirement, FinishReason, JobEnvelopePayload, JobKind, JobMeter,
            SignedJobEnvelope, SignedWorkReceipt, WorkReceiptPayload,
        };
        use covenant_identity::LocalIdentity;
        use covenant_mcp::Content;
        use uuid::Uuid;

        let buyer = LocalIdentity::generate("buyer@test");
        let operator = LocalIdentity::generate("operator@test");
        let job_id = Uuid::new_v4();

        let render = |finish: Option<FinishReason>| {
            let envelope = SignedJobEnvelope::sign(
                JobEnvelopePayload {
                    job_id,
                    buyer: buyer.agent_id(),
                    kind: JobKind::InferenceCall,
                    capability_requirement: CapabilityRequirement {
                        gpu_class: None,
                        min_vram_gb: None,
                        model_id: None,
                        kind: JobKind::InferenceCall,
                        max_duration_secs: 60,
                        min_reputation_bps: None,
                    },
                    input: vec![Content::text("hi")],
                    price_micro_usdc: 60_000,
                    deadline_ms: 60_000,
                    idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "finish-test"),
                    issued_at_ms: 0,
                    referral_code: None,
                    stream: false,
                },
                &buyer,
            )
            .expect("sign envelope");
            let receipt = SignedWorkReceipt::sign(
                WorkReceiptPayload {
                    job_id,
                    operator: operator.agent_id(),
                    job_hash_hex: "aa".repeat(32),
                    result_hash_hex: "bb".repeat(32),
                    meter: JobMeter {
                        wall_ms: 1_000,
                        tokens_in: None,
                        tokens_out: None,
                        gpu_seconds: None,
                        finish_reason: finish,
                    },
                    price_micro_usdc: 60_000,
                    status: A2ATaskStatus::Ok,
                    executed_at_ms: 0,
                    node_audit_root_hex: "cc".repeat(32),
                },
                &operator,
            )
            .expect("sign receipt");
            let outcome = DispatchOutcome {
                envelope,
                receipt,
                output: vec![Content::text("as far as I can")],
                payout: None,
            };
            let mut buf = Vec::new();
            super::write_receipt_block(&mut buf, &outcome).expect("write");
            String::from_utf8(buf).unwrap()
        };

        // A content-filtered stop is named, so a partial is not read as done.
        assert!(
            render(Some(FinishReason::ContentFilter)).contains("finish:    content_filter"),
            "a filtered stop must be named"
        );
        // A clean stop and a non-generation job (no reason) stay quiet.
        assert!(!render(Some(FinishReason::Stop)).contains("finish:"));
        assert!(!render(None).contains("finish:"));
    }

    #[test]
    fn capacity_render_neutralizes_hostile_operator_text() {
        // An operator can declare any model string; ANSI escapes and other
        // control bytes must not reach the buyer's terminal raw when they
        // run `capacity`.
        let flat = super::flatten_controls("gpt-4\x1b\x07:latest\n");
        assert!(
            !flat.chars().any(|c| c.is_control()),
            "a control char survived: {flat:?}"
        );
        assert_eq!(flat, "gpt-4  :latest ");
        // A real model id passes through untouched.
        assert_eq!(
            super::flatten_controls("qwen2.5-coder:7b"),
            "qwen2.5-coder:7b"
        );
    }

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn infer_generation_reads_every_sampling_knob() {
        use covenant_compute_protocol::{parse_generation_params, GenerationParams};
        let mut args = argv(
            "--temperature 0.7 --top-p 0.9 --max-tokens 128 --seed 42 \
             --presence-penalty 1.2 --frequency-penalty -0.6 --stop END --stop STOP \
             --logprobs 3",
        );
        let block = super::infer_generation(&mut args)
            .expect("valid knobs")
            .expect("a block is present");
        assert!(
            args.is_empty(),
            "the sampling flags were left unconsumed: {args:?}"
        );
        let parsed = parse_generation_params(&[block])
            .expect("well-formed")
            .expect("present");
        assert_eq!(
            parsed,
            GenerationParams {
                temperature: Some(0.7),
                top_p: Some(0.9),
                max_tokens: Some(128),
                seed: Some(42),
                presence_penalty: Some(1.2),
                frequency_penalty: Some(-0.6),
                stop: Some(vec!["END".into(), "STOP".into()]),
                response_format: None,
                logprobs: Some(3),
            }
        );
    }

    #[test]
    fn infer_with_no_sampling_knobs_carries_no_generation_block() {
        let mut args = argv("");
        assert!(super::infer_generation(&mut args).expect("ok").is_none());
    }

    #[test]
    fn response_format_json_packs_json_object_mode() {
        use covenant_compute_protocol::{parse_generation_params, ResponseFormat};
        let mut args = argv("--response-format json");
        let block = super::infer_generation(&mut args)
            .expect("valid")
            .expect("a block is present");
        assert!(args.is_empty(), "the flag was left unconsumed: {args:?}");
        let parsed = parse_generation_params(&[block])
            .expect("well-formed")
            .expect("present");
        assert_eq!(parsed.response_format, Some(ResponseFormat::JsonObject));
    }

    #[test]
    fn response_format_reads_a_json_schema_file_named_after_it() {
        use covenant_compute_protocol::{parse_generation_params, ResponseFormat};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weather.json");
        std::fs::write(&path, r#"{"type":"object","properties":{}}"#).unwrap();
        let mut args = argv("");
        args.push("--response-format".into());
        args.push(path.to_str().unwrap().into());
        let block = super::infer_generation(&mut args)
            .expect("valid")
            .expect("a block is present");
        let parsed = parse_generation_params(&[block])
            .expect("well-formed")
            .expect("present");
        assert_eq!(
            parsed.response_format,
            Some(ResponseFormat::JsonSchema {
                name: "weather".into(),
                schema: serde_json::json!({ "type": "object", "properties": {} }),
                strict: Some(true),
            })
        );
    }

    #[test]
    fn a_missing_response_format_schema_file_names_the_flag() {
        let mut args = argv("--response-format /no/such/schema.json");
        let err = super::infer_generation(&mut args).expect_err("unreadable file");
        assert!(err.to_string().contains("--response-format"), "{err}");
    }

    #[test]
    fn an_out_of_range_sampling_knob_is_refused_before_dispatch() {
        // The buyer learns their knob is bad locally, not after a network
        // round-trip and never as a paid operator-side failure.
        let mut args = argv("--temperature 3");
        let err = super::infer_generation(&mut args).expect_err("out of range");
        assert!(err.to_string().contains("outside 0..=2"), "{err}");
    }

    #[test]
    fn a_non_numeric_sampling_knob_names_the_flag() {
        let mut args = argv("--seed banana");
        let err = super::infer_generation(&mut args).expect_err("not a number");
        assert!(err.to_string().contains("--seed must be"), "{err}");
    }

    #[test]
    fn no_tools_offered_carries_no_tools_block() {
        let mut args = argv("");
        assert!(super::infer_tools(&mut args).expect("ok").is_none());
    }

    #[test]
    fn tool_choice_modes_and_a_named_function_parse() {
        use covenant_compute_protocol::{
            NamedFunction, NamedToolChoice, ToolChoice, ToolChoiceMode, ToolKind,
        };
        assert_eq!(
            super::parse_tool_choice("auto").unwrap(),
            ToolChoice::Mode(ToolChoiceMode::Auto)
        );
        // Case-folded, so `NONE` and `none` mean the same.
        assert_eq!(
            super::parse_tool_choice("NONE").unwrap(),
            ToolChoice::Mode(ToolChoiceMode::None)
        );
        assert_eq!(
            super::parse_tool_choice("required").unwrap(),
            ToolChoice::Mode(ToolChoiceMode::Required)
        );
        assert_eq!(
            super::parse_tool_choice("get_weather").unwrap(),
            ToolChoice::Named(NamedToolChoice {
                kind: ToolKind::Function,
                function: NamedFunction {
                    name: "get_weather".into()
                },
            })
        );
    }

    #[test]
    fn a_tools_file_packs_a_signed_tools_block_with_its_choice() {
        use covenant_compute_protocol::{parse_tools_input, ToolChoice, ToolChoiceMode};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weather_tools.json");
        std::fs::write(
            &path,
            r#"[{"type":"function","function":{"name":"get_weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}]"#,
        )
        .unwrap();
        let mut args = argv("--tool-choice required");
        args.push("--tools".into());
        args.push(path.to_str().unwrap().into());
        let block = super::infer_tools(&mut args)
            .expect("valid")
            .expect("a block is present");
        assert!(args.is_empty(), "the flags were left unconsumed: {args:?}");
        let request = parse_tools_input(&[block])
            .expect("well-formed")
            .expect("present");
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].function.name, "get_weather");
        assert_eq!(
            request.tool_choice,
            Some(ToolChoice::Mode(ToolChoiceMode::Required))
        );
    }

    #[test]
    fn a_tool_choice_that_forces_a_call_needs_tools() {
        // The same refusal the shared InferArgs makes, caught at the terminal
        // before any key touch or dispatch.
        let mut args = argv("--tool-choice required");
        let err = super::infer_tools(&mut args).expect_err("forcing with no tools is refused");
        assert!(err.to_string().contains("--tools"), "{err}");
    }

    #[test]
    fn a_missing_tools_file_names_the_flag() {
        let mut args = argv("--tools /no/such/tools.json");
        let err = super::infer_tools(&mut args).expect_err("unreadable file");
        assert!(err.to_string().contains("--tools"), "{err}");
    }

    fn call(name: &str, arguments: &str) -> covenant_compute_protocol::ToolCall {
        use covenant_compute_protocol::{FunctionCall, ToolCall, ToolCallKind};
        ToolCall {
            id: "call_0".into(),
            kind: ToolCallKind::Function,
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }

    #[test]
    fn one_tool_call_renders_as_a_single_named_call() {
        use covenant_compute_protocol::AssistantReply;
        let reply = AssistantReply {
            text: String::new(),
            tool_calls: vec![call("get_weather", r#"{"city":"Paris"}"#)],
            logprobs: None,
        };
        assert_eq!(
            super::render_tool_calls(&reply),
            "tool call:\n  get_weather({\"city\":\"Paris\"})\n"
        );
    }

    #[test]
    fn several_tool_calls_render_one_per_line_under_a_plural_header() {
        use covenant_compute_protocol::AssistantReply;
        let reply = AssistantReply {
            text: String::new(),
            tool_calls: vec![
                call("get_weather", r#"{"city":"Paris"}"#),
                call("get_time", r#"{"tz":"CET"}"#),
            ],
            logprobs: None,
        };
        assert_eq!(
            super::render_tool_calls(&reply),
            "tool calls:\n  get_weather({\"city\":\"Paris\"})\n  get_time({\"tz\":\"CET\"})\n"
        );
    }

    fn token(tok: &str, lp: f64, alts: &[(&str, f64)]) -> covenant_compute_protocol::TokenLogprob {
        use covenant_compute_protocol::{TokenLogprob, TopLogprob};
        TokenLogprob {
            token: tok.into(),
            logprob: lp,
            bytes: None,
            top_logprobs: alts
                .iter()
                .map(|(t, l)| TopLogprob {
                    token: (*t).into(),
                    logprob: *l,
                    bytes: None,
                })
                .collect(),
        }
    }

    #[test]
    fn logprobs_render_one_line_per_token_with_the_token_quoted() {
        let rendered =
            super::render_logprobs(&[token(" Paris", -0.023, &[]), token("!", -1.5, &[])]);
        assert_eq!(rendered, "logprobs:\n  \" Paris\" -0.023\n  \"!\" -1.500\n");
    }

    #[test]
    fn logprobs_render_the_alternatives_the_buyer_asked_for() {
        let rendered =
            super::render_logprobs(&[token(" Paris", -0.02, &[(" Lyon", -3.1), (" Nice", -4.0)])]);
        assert_eq!(
            rendered,
            "logprobs:\n  \" Paris\" -0.020  (alternatives: \" Lyon\" -3.100, \" Nice\" -4.000)\n"
        );
    }

    #[test]
    fn prose_alongside_a_tool_call_prints_first() {
        use covenant_compute_protocol::AssistantReply;
        let reply = AssistantReply {
            text: "Let me check that for you.".into(),
            tool_calls: vec![call("get_weather", r#"{"city":"Paris"}"#)],
            logprobs: None,
        };
        assert_eq!(
            super::render_tool_calls(&reply),
            "Let me check that for you.\n\ntool call:\n  get_weather({\"city\":\"Paris\"})\n"
        );
    }

    #[test]
    fn a_system_prompt_packs_a_two_turn_conversation() {
        use covenant_compute_protocol::{parse_chat_input, ChatMessage};
        let input = super::infer_input(
            Some("be terse".into()),
            None,
            Some("what color is the sky?".into()),
            Vec::new(),
            None,
        )
        .expect("valid");
        let messages = parse_chat_input(&input)
            .expect("well-formed")
            .expect("chat-shaped");
        assert_eq!(
            messages,
            vec![
                ChatMessage::system("be terse"),
                ChatMessage::user("what color is the sky?"),
            ]
        );
    }

    #[test]
    fn no_system_prompt_stays_a_bare_text_prompt() {
        use covenant_compute_protocol::parse_chat_input;
        let input = super::infer_input(None, None, Some("summarize this".into()), Vec::new(), None)
            .expect("valid");
        assert_eq!(input, vec![covenant_mcp::Content::text("summarize this")]);
        assert_eq!(parse_chat_input(&input).expect("well-formed"), None);
    }

    #[test]
    fn a_mistyped_flag_is_refused_not_billed_as_prompt() {
        // `infer --temperatuer 0.7 hello` must not pay to run the literal
        // prompt "--temperatuer 0.7 hello".
        let mut args = argv("--temperatuer 0.7 hello");
        let err = super::reject_stray_flags(&mut args).expect_err("typo");
        assert!(err.to_string().contains("--temperatuer"), "{err}");
    }

    #[test]
    fn a_plain_prompt_passes_the_flag_guard() {
        let mut args = argv("explain quantum tunneling");
        super::reject_stray_flags(&mut args).expect("no flags");
        assert_eq!(args, argv("explain quantum tunneling"));
    }

    #[test]
    fn a_leading_double_dash_lets_a_prompt_start_with_dashes() {
        let mut args = argv("-- --verbose is my prompt");
        super::reject_stray_flags(&mut args).expect("separator");
        assert_eq!(args, argv("--verbose is my prompt"));
    }

    #[test]
    fn an_empty_system_prompt_is_refused() {
        let err = super::infer_input(
            Some("   ".into()),
            None,
            Some("hi".into()),
            Vec::new(),
            None,
        )
        .expect_err("empty");
        assert!(err.to_string().contains("--system was empty"), "{err}");
    }

    #[test]
    fn a_messages_file_packs_the_whole_conversation() {
        use covenant_compute_protocol::{parse_chat_input, ChatMessage};
        let path =
            std::env::temp_dir().join(format!("covenant-compute-conv-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"[{"role":"system","content":"be terse"},
                {"role":"user","content":"hi"},
                {"role":"assistant","content":"hello"},
                {"role":"user","content":"and now?"}]"#,
        )
        .expect("write temp conversation");
        let input = super::infer_input(
            None,
            Some(path.to_string_lossy().into_owned()),
            None,
            Vec::new(),
            None,
        )
        .expect("valid");
        let _ = std::fs::remove_file(&path);
        let messages = parse_chat_input(&input)
            .expect("well-formed")
            .expect("chat-shaped");
        assert_eq!(
            messages,
            vec![
                ChatMessage::system("be terse"),
                ChatMessage::user("hi"),
                ChatMessage::assistant("hello"),
                ChatMessage::user("and now?"),
            ]
        );
    }

    #[test]
    fn a_messages_file_refuses_a_competing_system_prompt() {
        let err = super::infer_input(
            Some("be terse".into()),
            Some("/no/such/conversation.json".into()),
            None,
            Vec::new(),
            None,
        )
        .expect_err("conflict");
        assert!(err.to_string().contains("--messages-file"), "{err}");
    }

    #[test]
    fn a_messages_file_refuses_a_competing_prompt() {
        let err = super::infer_input(
            None,
            Some("/no/such/conversation.json".into()),
            Some("stray prompt".into()),
            Vec::new(),
            None,
        )
        .expect_err("conflict");
        assert!(
            err.to_string().contains("drop the positional prompt"),
            "{err}"
        );
    }

    #[test]
    fn a_multi_turn_conversation_round_trips_through_chat_input() {
        use covenant_compute_protocol::{chat_input, parse_chat_input, ChatMessage};
        let messages = super::messages_from_str(
            r#"[{"role":"user","content":"a"},{"role":"assistant","content":"b"}]"#,
            "test",
        )
        .expect("valid");
        let packed = chat_input(messages);
        assert_eq!(
            parse_chat_input(&packed)
                .expect("well-formed")
                .expect("chat-shaped"),
            vec![ChatMessage::user("a"), ChatMessage::assistant("b")]
        );
    }

    #[test]
    fn an_empty_conversation_file_is_refused() {
        let err = super::messages_from_str("[]", "conv.json").expect_err("empty");
        assert!(err.to_string().contains("empty conversation"), "{err}");
    }

    #[test]
    fn a_malformed_conversation_is_refused_not_billed() {
        let err = super::messages_from_str(r#"[{"role":"boss","content":"hi"}]"#, "conv.json")
            .expect_err("unknown role");
        assert!(err.to_string().contains("JSON array"), "{err}");
    }

    #[test]
    fn a_system_prompt_composes_with_a_generation_block() {
        use covenant_compute_protocol::{parse_chat_input, parse_generation_params};
        let mut knobs = argv("--seed 7");
        let generation = super::infer_generation(&mut knobs).expect("valid");
        let input = super::infer_input(
            Some("be terse".into()),
            None,
            Some("hi".into()),
            Vec::new(),
            generation,
        )
        .expect("valid");
        // Both the conversation and the signed knobs ride in the one input.
        assert!(parse_chat_input(&input).expect("well-formed").is_some());
        assert_eq!(
            parse_generation_params(&input)
                .expect("well-formed")
                .and_then(|p| p.seed),
            Some(7)
        );
    }

    #[test]
    fn an_image_makes_the_prompt_a_vision_message() {
        use covenant_compute_protocol::{parse_chat_input, ChatMessage};
        let input = super::infer_input(
            None,
            None,
            Some("what is this?".into()),
            vec!["aGVsbG8=".into()],
            None,
        )
        .expect("valid");
        let messages = parse_chat_input(&input)
            .expect("well-formed")
            .expect("an image makes it a chat job");
        assert_eq!(
            messages,
            vec![ChatMessage::user_with_images(
                "what is this?",
                vec!["aGVsbG8=".into()]
            )]
        );
    }

    #[test]
    fn an_image_with_a_messages_file_is_refused() {
        let err = super::infer_input(
            None,
            Some("/no/such/conversation.json".into()),
            None,
            vec!["aGVsbG8=".into()],
            None,
        )
        .expect_err("conflict");
        assert!(err.to_string().contains("--image"), "{err}");
    }
}
