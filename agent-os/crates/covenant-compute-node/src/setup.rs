//! First-run onboarding: `covenant-compute-node setup` walks a new
//! operator from nothing to a configured node — identity minted, model
//! backend probed, trust anchors collected and validated — and writes
//! `node.env` in the node home, which every subsequent boot loads
//! (process environment wins, the file is defaults not policy). One
//! command to configure, one to run.
//!
//! The wizard itself lives here rather than in `main.rs` so every step
//! is testable: prompts run over injected `BufRead`/`Write`, the ollama
//! probe over an injected base URL, and the env-file logic is pure
//! (`main.rs` owns the actual `set_var` loop).

use std::io::{BufRead, Write};
use std::path::Path;

use covenant_identity::LocalIdentity;

use crate::gpu::DetectedGpu;
use crate::tts::DEFAULT_SAY_BIN;
use crate::whisper::DEFAULT_WHISPER_BIN;
use crate::{ollama, openai_compat};

pub const ENV_FILE: &str = "node.env";

/// The OCI image the container executor runs each job in when the
/// operator doesn't name one. `alpine:3.20` is small, has a shell, and
/// is what the node.env hint documents; boot refuses to serve without an
/// image, so setup always pins one.
pub const DEFAULT_CONTAINER_IMAGE: &str = "alpine:3.20";

/// The per-job ask a node advertises when the operator sets no price.
/// Setup offers it as the prompt default and pins whatever is chosen, so
/// the interactive path and a bare boot always agree on the number.
pub const DEFAULT_PRICE_MICRO_USDC: u64 = 1_000;

/// Parses `node.env` text: `KEY=VALUE` lines, `#` comments and blank
/// lines skipped. Only `COVENANT_` keys are accepted — a config file
/// must not be able to reshape the process environment (PATH, DYLD_*,
/// …), and anything else in here is a typo worth failing on.
pub fn parse_env_file(raw: &str) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "line {}: expected KEY=VALUE, got {line:?}",
                idx + 1
            ));
        };
        let key = key.trim();
        if !key.starts_with("COVENANT_") {
            return Err(format!(
                "line {}: refusing non-COVENANT_ variable {key:?}",
                idx + 1
            ));
        }
        pairs.push((key.to_string(), value.trim().to_string()));
    }
    Ok(pairs)
}

/// The subset of parsed pairs that should actually be applied: those
/// not already set in the (real) environment. Pure so tests never
/// touch `set_var` — mutating the process environment races sibling
/// tests.
pub fn unset_pairs(
    pairs: Vec<(String, String)>,
    already_set: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    pairs.into_iter().filter(|(k, _)| !already_set(k)).collect()
}

pub fn valid_url(value: &str) -> Result<(), String> {
    // The scheme check comes first so a bare host or a typo'd address
    // gets the actionable "add http://" hint rather than a parser's
    // "relative URL without a base".
    if !value.starts_with("http://") && !value.starts_with("https://") {
        return Err("must start with http:// or https://".into());
    }
    let url = reqwest::Url::parse(value).map_err(|e| format!("not a valid URL: {e}"))?;
    // A scheme with no host (`http://`, a stray space) parses to nothing
    // reachable, and the reachability probe only soft-warns — so without
    // this it would be written and the node would retry registration
    // forever.
    if url.host_str().is_none_or(str::is_empty) {
        return Err("must include a host, e.g. http://coordinator.example:8080".into());
    }
    Ok(())
}

/// A Solana-shaped address: base58 decoding to exactly 32 bytes. Both
/// the coordinator's pinned identity key and the operator's payout
/// address have this shape.
pub fn valid_pubkey(value: &str) -> Result<(), String> {
    match bs58::decode(value).into_vec() {
        Ok(bytes) if bytes.len() == 32 => Ok(()),
        Ok(bytes) => Err(format!("decodes to {} bytes, expected 32", bytes.len())),
        Err(e) => Err(format!("not base58: {e}")),
    }
}

/// An existing, readable file — the ggml model a whisper node serves. The
/// boot refuses to serve without one, so a missing or mistyped path is
/// caught here (with the path named) rather than at first serve.
pub fn valid_model_file(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if !path.exists() {
        return Err(format!(
            "no file at {value} — download a whisper.cpp ggml model (e.g. ggml-base.en.bin) \
             and give its path"
        ));
    }
    if !path.is_file() {
        return Err(format!("{value} is a directory, not a model file"));
    }
    Ok(())
}

#[derive(Debug, Default, PartialEq)]
pub struct SetupOptions {
    pub coordinator_url: Option<String>,
    pub coordinator_pubkey_b58: Option<String>,
    pub payout_address: Option<String>,
    /// `ollama` (default when the probe succeeds), `openai-compat`
    /// (explicit opt-in — any server speaking the OpenAI
    /// chat-completions API), or `subprocess` (explicit opt-in —
    /// running strangers' shell commands is a bigger trust decision
    /// than serving inference).
    pub executor: Option<String>,
    pub ollama_url: Option<String>,
    pub openai_url: Option<String>,
    pub openai_api_key: Option<String>,
    /// The rootfs the container executor runs each job in. Only read for
    /// `--executor container`; defaults to [`DEFAULT_CONTAINER_IMAGE`].
    pub container_image: Option<String>,
    /// The ggml model file a whisper (speech-to-text) node serves. Required
    /// for `--executor whisper` — the boot refuses to serve without it.
    pub whisper_model: Option<String>,
    /// The whisper.cpp CLI a whisper node runs. Only read for
    /// `--executor whisper`; defaults to [`DEFAULT_WHISPER_BIN`] on `PATH`.
    pub whisper_bin: Option<String>,
    /// The synthesizer a say (text-to-speech) node runs. Only read for
    /// `--executor say`; defaults to [`DEFAULT_SAY_BIN`] on `PATH`.
    pub say_bin: Option<String>,
    /// Which discovered models to register. Every claim is benchmarked
    /// at boot, so serving everything ollama has is rarely right — an
    /// embedding-only model cannot generate at all.
    pub models: Option<Vec<String>>,
    /// The job kinds this node advertises. Only meaningful for the
    /// inference backends (`inference_call`, `embedding`, or both);
    /// `subprocess`/`container` always serve `batch_job`. Unset lets the
    /// node default to `inference_call` for a model backend — set
    /// `embedding` here to stand up an embedding node.
    pub job_kinds: Option<Vec<String>>,
    pub price_micro_usdc: Option<u64>,
}

pub const SETUP_USAGE: &str = "usage: covenant-compute-node setup \
[--coordinator-url URL] [--coordinator-pubkey BASE58] [--payout-address BASE58] \
[--executor ollama|openai-compat|whisper|say|subprocess|container] [--ollama-url URL] \
[--openai-url URL] [--openai-api-key KEY] [--container-image IMAGE] \
[--whisper-model PATH] [--whisper-bin PATH] [--say-bin PATH] [--models a,b,c] \
[--job-kinds inference_call,embedding] [--price-micro-usdc N]\n\
Anything omitted is asked for interactively.";

pub fn parse_setup_args(args: &[String]) -> Result<SetupOptions, String> {
    let mut opts = SetupOptions::default();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{name} needs a value\n{SETUP_USAGE}"))
        };
        match flag.as_str() {
            "--coordinator-url" => opts.coordinator_url = Some(value("--coordinator-url")?),
            "--coordinator-pubkey" => {
                opts.coordinator_pubkey_b58 = Some(value("--coordinator-pubkey")?)
            }
            "--payout-address" => opts.payout_address = Some(value("--payout-address")?),
            "--executor" => {
                let v = value("--executor")?;
                if !matches!(
                    v.as_str(),
                    "ollama" | "openai-compat" | "whisper" | "say" | "subprocess" | "container"
                ) {
                    return Err(format!(
                        "--executor must be ollama, openai-compat, whisper, say, subprocess or \
                         container, got {v:?}"
                    ));
                }
                opts.executor = Some(v);
            }
            "--ollama-url" => opts.ollama_url = Some(value("--ollama-url")?),
            "--openai-url" => opts.openai_url = Some(value("--openai-url")?),
            "--openai-api-key" => opts.openai_api_key = Some(value("--openai-api-key")?),
            "--container-image" => opts.container_image = Some(value("--container-image")?),
            "--whisper-model" => opts.whisper_model = Some(value("--whisper-model")?),
            "--whisper-bin" => opts.whisper_bin = Some(value("--whisper-bin")?),
            "--say-bin" => opts.say_bin = Some(value("--say-bin")?),
            "--models" => {
                opts.models = Some(
                    value("--models")?
                        .split(',')
                        .map(|m| m.trim().to_string())
                        .filter(|m| !m.is_empty())
                        .collect(),
                )
            }
            "--job-kinds" => {
                opts.job_kinds = Some(
                    value("--job-kinds")?
                        .split(',')
                        .map(|k| k.trim().to_string())
                        .filter(|k| !k.is_empty())
                        .collect(),
                )
            }
            "--price-micro-usdc" => {
                let v = value("--price-micro-usdc")?;
                opts.price_micro_usdc = Some(
                    v.parse()
                        .map_err(|_| format!("--price-micro-usdc must be a u64, got {v:?}"))?,
                );
            }
            other => return Err(format!("unknown flag {other:?}\n{SETUP_USAGE}")),
        }
    }
    Ok(opts)
}

/// Asks until the answer validates (three strikes), or fails with a
/// pointer at the flag equivalent when stdin runs dry — so a script
/// that forgot a flag gets a usable error instead of a hang-then-EOF
/// mystery.
fn prompt(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    question: &str,
    flag: &str,
    validate: impl Fn(&str) -> Result<(), String>,
) -> anyhow::Result<String> {
    for _ in 0..3 {
        write!(out, "{question}: ")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            anyhow::bail!("stdin closed before {question:?} was answered — pass {flag} instead");
        }
        let value = line.trim().to_string();
        if value.is_empty() {
            writeln!(out, "  a value is required")?;
            continue;
        }
        match validate(&value) {
            Ok(()) => return Ok(value),
            Err(e) => writeln!(out, "  {e}")?,
        }
    }
    anyhow::bail!("three invalid answers for {question:?} — pass {flag} instead")
}

fn resolve(
    given: Option<String>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    question: &str,
    flag: &str,
    validate: impl Fn(&str) -> Result<(), String>,
) -> anyhow::Result<String> {
    match given {
        Some(value) => {
            validate(&value).map_err(|e| anyhow::anyhow!("{flag} {value:?}: {e}"))?;
            Ok(value)
        }
        None => prompt(input, out, question, flag, validate),
    }
}

/// The API key for an openai-compat backend. A `--openai-api-key` flag
/// wins (the scripted path); otherwise the operator is asked, so an
/// interactive setup never has to put the secret on the command line
/// where `ps` and shell history keep it. Optional: a blank answer or a
/// closed stdin means the backend needs no key, which the probe then
/// confirms. No validation loop — the backend, not a regex, decides
/// whether a key is right.
fn resolve_openai_api_key(
    given: Option<String>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> anyhow::Result<Option<String>> {
    if let Some(key) = given {
        return Ok(Some(key));
    }
    write!(
        out,
        "API key for the backend (leave blank if it needs none): "
    )?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let key = line.trim().to_string();
    Ok((!key.is_empty()).then_some(key))
}

/// The per-job ask, in micro-USDC. A `--price-micro-usdc` flag wins;
/// otherwise the operator is prompted with the boot default one Enter
/// away. Unlike the required anchors, a closed stdin here takes the
/// default rather than failing — pricing has a sane default, so a
/// scripted `setup` with no flag must not block on it. Three unparseable
/// answers still bail, pointing at the flag.
fn resolve_price(
    given: Option<u64>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> anyhow::Result<u64> {
    if let Some(price) = given {
        anyhow::ensure!(
            price > 0,
            "--price-micro-usdc must be at least 1 — a node asking 0 serves every job for free"
        );
        return Ok(price);
    }
    for _ in 0..3 {
        write!(
            out,
            "price to charge per job in micro-USDC (1 USDC = 1_000_000) \
             [{DEFAULT_PRICE_MICRO_USDC}]: "
        )?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(DEFAULT_PRICE_MICRO_USDC);
        }
        let answer = line.trim();
        if answer.is_empty() {
            return Ok(DEFAULT_PRICE_MICRO_USDC);
        }
        // Accept the `1_000_000` grouping the prompt itself shows.
        match answer.replace('_', "").parse::<u64>() {
            Ok(0) => writeln!(
                out,
                "  0 would serve every job for free — enter at least 1, \
                 or press Enter for {DEFAULT_PRICE_MICRO_USDC}"
            )?,
            Ok(price) => return Ok(price),
            Err(_) => writeln!(out, "  a whole number of micro-USDC, e.g. 1000")?,
        }
    }
    anyhow::bail!("three invalid prices — pass --price-micro-usdc instead")
}

/// The job kinds a given executor can serve, as the strings the profile
/// env uses. Kept in step with the node's own boot coherence check so
/// setup refuses an impossible pairing early rather than writing a
/// node.env that fails at first boot.
fn servable_kind_labels(executor: &str) -> &'static [&'static str] {
    match executor {
        "ollama" | "openai-compat" => &["inference_call", "embedding"],
        "whisper" => &["transcription"],
        "say" => &["speech_synthesis"],
        _ => &["batch_job"],
    }
}

/// Validates the operator's declared `--job-kinds` against what the
/// resolved executor serves, returning them de-duplicated in declaration
/// order. An unknown or unservable kind is refused by name.
fn validate_job_kinds(executor: &str, kinds: &[String]) -> Result<Vec<String>, String> {
    let servable = servable_kind_labels(executor);
    let mut chosen: Vec<String> = Vec::new();
    for kind in kinds {
        if !servable.contains(&kind.as_str()) {
            return Err(format!(
                "--job-kinds: the {executor} executor can't serve {kind:?} (it serves {}); \
                 drop it or choose a matching executor",
                servable.join(", ")
            ));
        }
        if !chosen.contains(kind) {
            chosen.push(kind.clone());
        }
    }
    if chosen.is_empty() {
        return Err("--job-kinds names no kinds".into());
    }
    Ok(chosen)
}

/// Which of the available models to register. Every registered claim
/// is benchmarked at boot — an embedding-only model cannot serve
/// generation at all, and a huge model proves painfully slow — so the
/// operator chooses here, with "all" one Enter away.
fn resolve_models(
    given: Option<Vec<String>>,
    available: &[String],
    embedding_only: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> anyhow::Result<Vec<String>> {
    let check = |chosen: &[String]| -> Result<(), String> {
        match chosen.iter().find(|m| !available.contains(m)) {
            Some(missing) => Err(format!("{missing:?} is not among the available models")),
            None if chosen.is_empty() => Err("at least one model is required".into()),
            None => Ok(()),
        }
    };
    if let Some(chosen) = given {
        check(&chosen).map_err(|e| anyhow::anyhow!("--models: {e}"))?;
        return Ok(chosen);
    }
    // The hazard the boot benchmark catches, said where the choice is
    // made. For an inference node an embedding-only model can't generate;
    // for an embedding node every chosen model is proven against the
    // real `/api/embed` at first boot instead.
    if embedding_only {
        writeln!(
            out,
            "  note: these models will serve embeddings; each claim is proven against the \
             backend at first boot"
        )?;
    } else {
        writeln!(
            out,
            "  note: only text-generation models can serve inference; an embedding-only \
             model will fail the check at first boot"
        )?;
    }
    for _ in 0..3 {
        write!(out, "models to serve (comma-separated) [all]: ")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            anyhow::bail!("stdin closed before models were chosen — pass --models instead");
        }
        let answer = line.trim();
        if answer.is_empty() || answer == "all" {
            return Ok(available.to_vec());
        }
        let chosen: Vec<String> = answer
            .split(',')
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
        match check(&chosen) {
            Ok(()) => return Ok(chosen),
            Err(e) => writeln!(out, "  {e}")?,
        }
    }
    anyhow::bail!("three invalid answers for the model list — pass --models instead")
}

/// The whole wizard. Returns the `node.env` path it wrote.
///
/// `detected_gpu` is the caller's hardware probe (the binary runs it;
/// tests inject a value), kept out of the wizard so every step here
/// stays hermetic. A detected GPU is reported and pinned into
/// `node.env`; its absence advertises CPU-only and leaves the hardware
/// knobs as commented hints.
pub async fn run_setup(
    home: &Path,
    opts: SetupOptions,
    detected_gpu: Option<DetectedGpu>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> anyhow::Result<std::path::PathBuf> {
    writeln!(
        out,
        "covenant-compute-node setup — home: {}",
        home.display()
    )?;

    let identity = LocalIdentity::load_or_create(&home.join("identity.json"), "operator@compute")?;
    let operator_pubkey = bs58::encode(identity.pubkey_bytes()).into_string();
    writeln!(
        out,
        "operator identity: {operator_pubkey}\n  (persisted in the node home — earnings and \
         reputation accrue against this key)"
    )?;

    // Model backend before trust anchors: if this machine can't serve
    // anything yet, say so first — the operator fixes one thing at a
    // time.
    let ollama_url = opts
        .ollama_url
        .clone()
        .unwrap_or_else(|| ollama::DEFAULT_OLLAMA_URL.to_string());
    let openai_url = opts
        .openai_url
        .clone()
        .unwrap_or_else(|| openai_compat::DEFAULT_OPENAI_COMPAT_URL.to_string());
    let container_image = opts
        .container_image
        .clone()
        .unwrap_or_else(|| DEFAULT_CONTAINER_IMAGE.to_string());
    let mut resolved_openai_api_key: Option<String> = None;
    // A whisper node's ggml model file and an optional non-default CLI for
    // whisper/say, captured in the executor arm and written into node.env
    // below — the same pattern the openai-compat key and container image take.
    let mut whisper_model: Option<String> = None;
    let mut whisper_bin: Option<String> = None;
    let mut say_bin: Option<String> = None;
    // An embedding node picks its models against the embedding probe, not
    // the generation one, so the model note below inverts. Read from the
    // declared kinds before the executor is resolved; the kinds are
    // validated against the resolved executor once it is known.
    let embedding_only = opts
        .job_kinds
        .as_ref()
        .map(|k| !k.is_empty() && k.iter().all(|s| s == "embedding"))
        .unwrap_or(false);
    let (executor, models) = match opts.executor.as_deref() {
        Some("subprocess") => {
            writeln!(
                out,
                "executor: subprocess. Buyers' commands run directly on this host with no \
                 filesystem or network isolation (env scrubbed, scratch cwd, output capped, \
                 killed at the deadline, nothing more). Serve strangers' work this way only \
                 on a machine you can expose; `--executor container` (docker or podman) puts \
                 each job behind container walls instead."
            )?;
            ("subprocess".to_string(), Vec::new())
        }
        Some("container") => {
            writeln!(
                out,
                "executor: container. Each buyer's command runs inside an OCI container built \
                 from {container_image} (default-deny egress, read-only rootfs, dropped \
                 capabilities, killed at the deadline) — the isolation the subprocess executor \
                 lacks, and the right default for serving strangers' work. Needs docker or \
                 podman on this host; override the image with --container-image."
            )?;
            ("container".to_string(), Vec::new())
        }
        Some("whisper") => {
            let binary = opts
                .whisper_bin
                .clone()
                .unwrap_or_else(|| DEFAULT_WHISPER_BIN.into());
            writeln!(
                out,
                "executor: whisper. This node transcribes audio through the whisper.cpp CLI \
                 ({binary}; override with --whisper-bin) against a local ggml model, and \
                 advertises the whisper-1 model id. No model server to run — point \
                 --whisper-model at the model file it should serve."
            )?;
            let model = resolve(
                opts.whisper_model.clone(),
                input,
                out,
                "path to the whisper ggml model file (e.g. ggml-base.en.bin)",
                "--whisper-model",
                valid_model_file,
            )?;
            whisper_model = Some(model);
            whisper_bin = opts.whisper_bin.clone();
            ("whisper".to_string(), Vec::new())
        }
        Some("say") => {
            let binary = opts
                .say_bin
                .clone()
                .unwrap_or_else(|| DEFAULT_SAY_BIN.into());
            writeln!(
                out,
                "executor: say. This node synthesizes speech through {binary} (macOS ships \
                 `say`; point --say-bin at a compatible synthesizer elsewhere) and advertises \
                 the say-1 model id. Nothing to download, no server to run."
            )?;
            say_bin = opts.say_bin.clone();
            ("say".to_string(), Vec::new())
        }
        Some("openai-compat") => {
            let api_key = resolve_openai_api_key(opts.openai_api_key.clone(), input, out)?;
            match openai_compat::list_models(&openai_url, api_key.as_deref()).await {
                Ok(available) if !available.is_empty() => {
                    writeln!(
                        out,
                        "backend reachable at {openai_url} — available: {}",
                        available.join(", ")
                    )?;
                    let models = resolve_models(
                        opts.models.clone(),
                        &available,
                        embedding_only,
                        input,
                        out,
                    )?;
                    resolved_openai_api_key = api_key;
                    ("openai-compat".to_string(), models)
                }
                Ok(_) => anyhow::bail!(
                    "the backend at {openai_url} serves no models yet — load one, then re-run \
                     setup"
                ),
                Err(e) => anyhow::bail!(
                    "no OpenAI-compatible server at {openai_url} ({e}).\n\
                     Point --openai-url at a running vLLM / llama.cpp / LM Studio endpoint \
                     (Ollama serves one at http://127.0.0.1:11434/v1), pass --openai-api-key \
                     if it needs one, and re-run setup"
                ),
            }
        }
        _ => match ollama::list_models(&ollama_url).await {
            Ok(available) if !available.is_empty() => {
                writeln!(
                    out,
                    "ollama reachable at {ollama_url} — available: {}",
                    available.join(", ")
                )?;
                let models =
                    resolve_models(opts.models.clone(), &available, embedding_only, input, out)?;
                ("ollama".to_string(), models)
            }
            Ok(_) => anyhow::bail!(
                "ollama at {ollama_url} serves no models yet — pull one first \
                 (e.g. `ollama pull qwen2.5:7b`), then re-run setup"
            ),
            Err(e) => anyhow::bail!(
                "no ollama server at {ollama_url} ({e}).\n\
                 Install it from https://ollama.com/download, `ollama pull qwen2.5:7b`, and \
                 re-run setup. Already running another local model server (LM Studio, vLLM, \
                 llama.cpp)? Serve from it with `--executor openai-compat --openai-url URL`. \
                 To serve shell jobs instead of inference, pass `--executor subprocess`."
            ),
        },
    };

    // The subprocess and container executors serve any job — a shell
    // command run to completion — not a named model, so a --models list
    // means nothing to them. Say so rather than discarding it in silence.
    if opts.models.is_some()
        && matches!(
            executor.as_str(),
            "subprocess" | "container" | "whisper" | "say"
        )
    {
        writeln!(
            out,
            "note: --models is ignored for the {executor} executor — it serves a fixed \
             capability (a shell command, or one local model), not a chosen model list."
        )?;
    }

    // Declared job kinds are validated against the resolved executor here,
    // the same coherence the node enforces at boot — so an impossible
    // pairing (embedding on a subprocess node) is refused before a
    // node.env is written, not at first serve.
    let job_kinds = match &opts.job_kinds {
        Some(kinds) => Some(validate_job_kinds(&executor, kinds).map_err(|e| anyhow::anyhow!(e))?),
        None => None,
    };

    let coordinator_url = resolve(
        opts.coordinator_url,
        input,
        out,
        "coordinator URL",
        "--coordinator-url",
        valid_url,
    )?;
    let coordinator_pubkey = resolve(
        opts.coordinator_pubkey_b58,
        input,
        out,
        "coordinator pubkey (base58, from the network operator)",
        "--coordinator-pubkey",
        valid_pubkey,
    )?;
    let payout_address = resolve(
        opts.payout_address,
        input,
        out,
        "payout address (base58 Solana address earnings pay to)",
        "--payout-address",
        valid_pubkey,
    )?;

    // Best-effort reachability, never a failure: registration retries
    // with backoff at boot anyway, and onboarding may legitimately
    // happen before the coordinator is up. A typo'd URL still gets
    // caught here instead of at first boot.
    match reqwest::Client::new()
        .get(format!(
            "{}/federation/fees",
            coordinator_url.trim_end_matches('/')
        ))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            match resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["fee_bps"].as_u64())
            {
                Some(fee_bps) => writeln!(
                    out,
                    "coordinator reachable — disclosed marketplace fee: {fee_bps} bps"
                )?,
                // Don't report a fee we couldn't read as 0 — an operator
                // who prices against a phantom 0% keeps less than they
                // planned. Say the disclosure was unreadable instead.
                None => writeln!(
                    out,
                    "coordinator reachable, but its fee disclosure was unreadable — \
                     confirm the marketplace fee before pricing your capacity"
                )?,
            }
        }
        Ok(resp) => writeln!(
            out,
            "warning: coordinator answered {} — double-check the URL if this persists",
            resp.status()
        )?,
        Err(e) => {
            let why = if e.is_timeout() {
                "timed out"
            } else if e.is_connect() {
                "connection refused"
            } else {
                "no response"
            };
            writeln!(
                out,
                "warning: coordinator at {} not reachable right now ({why}) — the node will \
                 keep retrying at boot",
                coordinator_url.trim_end_matches('/')
            )?
        }
    }

    // After the fee disclosure, so the operator prices against it. A
    // node with no price set silently ships the boot default, and
    // pricing is the direct lever on earnings — so surface it here, at
    // the one moment the operator is deciding to run.
    let price_micro_usdc = resolve_price(opts.price_micro_usdc, input, out)?;
    writeln!(
        out,
        "asking {price_micro_usdc} micro-USDC per job (change it any time in node.env)"
    )?;

    let mut env = format!(
        "# covenant-compute-node configuration (written by `covenant-compute-node setup`)\n\
         # Loaded at every boot; variables already set in the environment win.\n\
         COVENANT_COMPUTE_COORDINATOR_URL={coordinator_url}\n\
         COVENANT_COMPUTE_COORDINATOR_PUBKEY={coordinator_pubkey}\n\
         COVENANT_COMPUTE_PAYOUT_ADDRESS={payout_address}\n\
         COVENANT_COMPUTE_NODE_EXECUTOR={executor}\n"
    );
    if !models.is_empty() {
        env.push_str(&format!(
            "COVENANT_COMPUTE_NODE_MODELS={}\n",
            models.join(",")
        ));
        writeln!(
            out,
            "will serve: {} (each claim is proven against the backend at boot)",
            models.join(", ")
        )?;
    }
    if let Some(kinds) = &job_kinds {
        env.push_str(&format!(
            "COVENANT_COMPUTE_NODE_JOB_KINDS={}\n",
            kinds.join(",")
        ));
        writeln!(out, "advertising job kinds: {}", kinds.join(", "))?;
    }
    if executor == "ollama" && ollama_url != ollama::DEFAULT_OLLAMA_URL {
        env.push_str(&format!("COVENANT_COMPUTE_OLLAMA_URL={ollama_url}\n"));
    }
    if executor == "openai-compat" {
        if openai_url != openai_compat::DEFAULT_OPENAI_COMPAT_URL {
            env.push_str(&format!("COVENANT_COMPUTE_OPENAI_URL={openai_url}\n"));
        }
        if let Some(key) = &resolved_openai_api_key {
            env.push_str(&format!("COVENANT_COMPUTE_OPENAI_API_KEY={key}\n"));
        }
    }
    if executor == "container" {
        env.push_str(&format!(
            "COVENANT_COMPUTE_NODE_CONTAINER_IMAGE={container_image}\n"
        ));
        // A detected GPU is advertised to the matcher; pass it into the
        // container as well, or GPU jobs would run CPU-only behind the
        // walls and miss their deadline.
        if detected_gpu.is_some() {
            env.push_str("COVENANT_COMPUTE_NODE_CONTAINER_GPUS=all\n");
        }
    }
    if executor == "whisper" {
        // Required: the boot refuses a whisper node with no model file.
        // Only pin a non-default CLI, so the file stays the whole config.
        if let Some(model) = &whisper_model {
            env.push_str(&format!("COVENANT_COMPUTE_WHISPER_MODEL={model}\n"));
        }
        if let Some(bin) = whisper_bin.as_deref().filter(|b| *b != DEFAULT_WHISPER_BIN) {
            env.push_str(&format!("COVENANT_COMPUTE_WHISPER_BIN={bin}\n"));
        }
    }
    if executor == "say" {
        // Nothing is required — `say` is the default. Only pin a non-default
        // synthesizer, so a stock macOS node.env carries no backend line.
        if let Some(bin) = say_bin.as_deref().filter(|b| *b != DEFAULT_SAY_BIN) {
            env.push_str(&format!("COVENANT_COMPUTE_SAY_BIN={bin}\n"));
        }
    }
    env.push_str(&format!(
        "COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC={price_micro_usdc}\n"
    ));
    match &detected_gpu {
        Some(gpu) => {
            writeln!(
                out,
                "detected {} ({} GB VRAM); advertising it so GPU jobs match this node",
                gpu.model, gpu.vram_gb
            )?;
            env.push_str(&format!(
                "# GPU detected at setup and pinned below, so this node advertises it even\n\
                 # where the GPU probe can't run at boot. Re-run setup after a hardware\n\
                 # change, or edit these two lines.\n\
                 COVENANT_COMPUTE_NODE_HARDWARE={}\n\
                 COVENANT_COMPUTE_NODE_VRAM_GB={}\n",
                gpu.hardware_env_value(),
                gpu.vram_gb
            ));
        }
        None => {
            writeln!(
                out,
                "no GPU detected; advertising CPU-only (edit node.env to declare \
                 other hardware)"
            )?;
            env.push_str(
                "# No GPU detected at setup. To advertise one, uncomment and edit:\n\
                 # COVENANT_COMPUTE_NODE_HARDWARE=consumer:rtx4090\n\
                 # COVENANT_COMPUTE_NODE_VRAM_GB=24\n",
            );
        }
    }
    // The commented "switch to the container" hint only helps an operator
    // who isn't on it yet — omit it once container is the chosen executor,
    // where the image and GPU passthrough are already written as active
    // lines above. A GPU node that switches needs to pass the device in, or
    // it keeps advertising the GPU while running jobs CPU-only; only shown
    // when a GPU was detected, so a CPU host isn't told to wire hardware it
    // doesn't have.
    let container_switch_hint = if executor == "container" {
        String::new()
    } else {
        let gpu_hint = if detected_gpu.is_some() {
            "# and pass the detected GPU in, or container jobs run CPU-only while\n\
             # this node still advertises the GPU:\n\
             # COVENANT_COMPUTE_NODE_CONTAINER_GPUS=all\n"
        } else {
            ""
        };
        format!(
            "# Run strangers' jobs inside a container instead of a raw subprocess\n\
             # (default-deny egress, read-only rootfs; needs docker or podman):\n\
             # COVENANT_COMPUTE_NODE_EXECUTOR=container\n\
             # COVENANT_COMPUTE_NODE_CONTAINER_IMAGE=alpine:3.20\n\
             {gpu_hint}"
        )
    };
    env.push_str(&format!(
        "# How often paid-out earnings reconcile against the coordinator books\n\
         # (seconds, default 60, 0 disables):\n\
         # COVENANT_COMPUTE_NODE_PAYOUT_POLL_SECS=60\n\
         {container_switch_hint}\
         # Or serve any OpenAI-compatible server (vLLM, llama.cpp, LM Studio,\n\
         # hosted; Ollama exposes one at /v1 too) — set the key only if the\n\
         # endpoint requires one:\n\
         # COVENANT_COMPUTE_NODE_EXECUTOR=openai-compat\n\
         # COVENANT_COMPUTE_OPENAI_URL=http://127.0.0.1:8000/v1\n\
         # COVENANT_COMPUTE_OPENAI_API_KEY=\n\
         # Your own Solana RPC endpoint for `earnings verify` — proves each\n\
         # paid row on-chain without trusting the coordinator's books:\n\
         # COVENANT_COMPUTE_NODE_RPC_URL=https://api.devnet.solana.com\n"
    ));

    let path = home.join(ENV_FILE);
    let existed = path.exists();
    // The wizard regenerates node.env from scratch, so a re-run (after a
    // new GPU, say) would silently drop hand edits the file invites — an
    // uncommented RPC URL, a tuned price. Keep the current version as a
    // recoverable backup first, written through the same atomic 0600 path
    // node.env itself takes: the backup can hold the same API key, so it
    // must not be briefly world-readable the way a plain copy's
    // create-then-chmod leaves it. Best-effort — a backup that can't be
    // saved is not a reason to block reconfiguration.
    let backup = if existed {
        let dest = home.join(format!("{ENV_FILE}.bak"));
        match std::fs::read_to_string(&path) {
            Ok(prev) if write_env_owner_only(&dest, &prev).is_ok() => Some(dest),
            _ => None,
        }
    } else {
        None
    };
    // node.env can carry an API key (the openai-compat backend), so it is
    // written owner-only and atomically: created 0600 before any bytes
    // land, then renamed into place. A secret is never briefly
    // world-readable the way a create-then-chmod leaves it, and a re-run
    // can't truncate a working config on a crash or a full disk.
    write_env_owner_only(&path, &env)?;
    match &backup {
        Some(backup) => writeln!(
            out,
            "\nrewrote {} (previous config saved to {})",
            path.display(),
            backup.display()
        )?,
        None => writeln!(
            out,
            "\n{} {}",
            if existed { "rewrote" } else { "wrote" },
            path.display()
        )?,
    }
    writeln!(
        out,
        "\nsetup complete. serve jobs in this terminal:\n  covenant-compute-node\nor keep \
         earning across logouts and reboots (runs in the background, so don't also run it \
         above):\n  covenant-compute-node service install\ncheck standing and earnings any \
         time:\n  covenant-compute-node status\n  covenant-compute-node earnings"
    )?;
    Ok(path)
}

/// Writes `contents` to `path` owner-only and atomically. A temp file in
/// the target's own directory is created at 0600, filled, flushed, then
/// renamed over `path`. The 0600 mode is in force before any bytes land,
/// so the API key `node.env` can hold is never briefly world-readable
/// the way a plain create-then-chmod leaves it; the rename is atomic, so
/// a crash or a full disk mid-write can never truncate an existing
/// config on a re-run. This is the owner-only posture `covenant-identity`
/// takes for the identity key.
fn write_env_owner_only(path: &Path, contents: &str) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".node.env-")
        .tempfile_in(dir)
        .map_err(|e| anyhow::anyhow!("stage {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| anyhow::anyhow!("restrict {} to owner-only: {e}", path.display()))?;
    }
    tmp.write_all(contents.as_bytes())
        .map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| anyhow::anyhow!("flush {}: {e}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("finalize {}: {e}", path.display()))?;
    Ok(())
}

/// Repairs `node.env` to owner-only in place when a looser mode slipped
/// in — a `node.env` an older setup created before it tightened the mode,
/// or a copy/restore that widened it. Boot calls this whenever it reads
/// the file, the same self-heal `covenant-identity` performs on the
/// identity key at load. A missing file, a symlink, or a non-Unix host
/// is a silent no-op.
pub fn ensure_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_file() && meta.permissions().mode() & 0o777 != 0o600 {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::{Json, Router};
    use std::io::Cursor;

    #[cfg(unix)]
    #[test]
    fn node_env_is_written_owner_only_and_atomically() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ENV_FILE);

        // A fresh secret file is owner-only from the moment it exists —
        // no create-at-0644-then-chmod window a co-tenant could read.
        write_env_owner_only(&path, "COVENANT_COMPUTE_OPENAI_API_KEY=sk-secret\n").unwrap();
        let mode = std::fs::symlink_metadata(&path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "a fresh secret file must be owner-only, got {mode:#o}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "COVENANT_COMPUTE_OPENAI_API_KEY=sk-secret\n"
        );

        // A re-run overwrites in place, stays owner-only, and leaves no
        // stray temp file behind (the rename is atomic).
        write_env_owner_only(&path, "COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC=2000\n").unwrap();
        let mode = std::fs::symlink_metadata(&path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "a rewritten file must stay owner-only, got {mode:#o}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC=2000\n"
        );
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .filter(|n| n != ENV_FILE)
            .collect();
        assert!(
            strays.is_empty(),
            "atomic write left a stray temp file: {strays:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_owner_only_repairs_a_world_readable_secret() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ENV_FILE);
        // A node.env an older setup left world-readable (the create-then-
        // chmod bug, or a copy that widened it).
        std::fs::write(&path, "COVENANT_COMPUTE_OPENAI_API_KEY=sk-secret\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        ensure_owner_only(&path);
        let mode = std::fs::symlink_metadata(&path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "boot must repair a loose secret, got {mode:#o}"
        );

        // An absent file (a node not yet set up) is a silent no-op.
        ensure_owner_only(&dir.path().join("absent.env"));
    }

    #[tokio::test]
    async fn models_passed_to_a_modelless_executor_are_flagged_not_dropped() {
        let home = tempfile::tempdir().unwrap();
        // subprocess is hermetic (no model-server probe) and ignores
        // --models; the operator must hear that, not have it vanish.
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("subprocess".into()),
            price_micro_usdc: Some(1_000),
            models: Some(vec!["qwen2.5:7b".into()]),
            ..SetupOptions::default()
        };
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("subprocess setup writes node.env");
        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.contains("--models is ignored for the subprocess executor"),
            "the operator must be told --models was ignored; got: {printed}"
        );
    }

    #[test]
    fn validate_job_kinds_checks_against_the_executor() {
        // An inference backend serves inference and embedding.
        assert_eq!(
            validate_job_kinds("ollama", &["embedding".into()]).unwrap(),
            vec!["embedding".to_string()]
        );
        assert_eq!(
            validate_job_kinds(
                "openai-compat",
                &["inference_call".into(), "embedding".into()],
            )
            .unwrap(),
            vec!["inference_call".to_string(), "embedding".to_string()]
        );
        // De-duplicates in declaration order.
        assert_eq!(
            validate_job_kinds("ollama", &["embedding".into(), "embedding".into()]).unwrap(),
            vec!["embedding".to_string()]
        );
        // A subprocess node can't embed; unknown and unservable kinds are
        // refused by name.
        let err = validate_job_kinds("subprocess", &["embedding".into()]).unwrap_err();
        assert!(err.contains("can't serve"), "{err}");
        assert!(validate_job_kinds("ollama", &["lease_session".into()]).is_err());
        assert!(validate_job_kinds("ollama", &["gibberish".into()]).is_err());
    }

    #[test]
    fn parse_setup_args_reads_job_kinds() {
        let opts =
            parse_setup_args(&["--job-kinds".into(), "inference_call, embedding".into()]).unwrap();
        assert_eq!(
            opts.job_kinds,
            Some(vec!["inference_call".to_string(), "embedding".to_string()])
        );
    }

    #[tokio::test]
    async fn setup_pins_declared_job_kinds_into_node_env() {
        let home = tempfile::tempdir().unwrap();
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("subprocess".into()),
            price_micro_usdc: Some(1_000),
            job_kinds: Some(vec!["batch_job".into()]),
            ..SetupOptions::default()
        };
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("subprocess setup writes node.env");
        let env = std::fs::read_to_string(&path).unwrap();
        assert!(
            env.contains("COVENANT_COMPUTE_NODE_JOB_KINDS=batch_job"),
            "declared kinds are pinned into node.env: {env}"
        );
    }

    #[tokio::test]
    async fn setup_refuses_a_job_kind_the_executor_cannot_serve() {
        let home = tempfile::tempdir().unwrap();
        // An embedding claim on a shell-command node can never hold; catch
        // it at setup, before any node.env is written.
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("subprocess".into()),
            price_micro_usdc: Some(1_000),
            job_kinds: Some(vec!["embedding".into()]),
            ..SetupOptions::default()
        };
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let err = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect_err("embedding on a subprocess node is refused");
        assert!(err.to_string().contains("can't serve"), "{err}");
        assert!(
            !home.path().join(ENV_FILE).exists(),
            "a refused setup writes no node.env"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn re_running_setup_backs_up_the_previous_node_env() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        // The subprocess executor needs no model-server probe, so this
        // stays hermetic; every anchor is supplied, so empty stdin is
        // enough.
        let opts = || SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("subprocess".into()),
            price_micro_usdc: Some(1_000),
            ..SetupOptions::default()
        };

        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let path = run_setup(home.path(), opts(), None, &mut input, &mut out)
            .await
            .expect("first setup writes node.env");
        assert!(
            !home.path().join("node.env.bak").exists(),
            "a first run has nothing to back up"
        );

        // The operator hand-edits the file, exactly what its commented
        // hints invite.
        let edited = format!(
            "{}COVENANT_COMPUTE_NODE_RPC_URL=https://my.rpc.example\n",
            std::fs::read_to_string(&path).unwrap()
        );
        std::fs::write(&path, &edited).unwrap();

        // Re-running setup preserves that edited version as a backup.
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        run_setup(home.path(), opts(), None, &mut input, &mut out)
            .await
            .expect("re-run rewrites node.env");
        let backup = home.path().join("node.env.bak");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            edited,
            "the backup is the operator's edited config, recoverable after the rewrite"
        );
        let mode = std::fs::symlink_metadata(&backup)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the backup is owner-only; it can hold an API key"
        );
        assert!(
            String::from_utf8_lossy(&out).contains("node.env.bak"),
            "the rewrite tells the operator where the backup went"
        );
    }

    #[test]
    fn env_file_parses_values_comments_and_blanks() {
        let pairs = parse_env_file(
            "# comment\n\nCOVENANT_A=1\nCOVENANT_B = with = equals \n  # indented comment\n",
        )
        .expect("parses");
        assert_eq!(
            pairs,
            vec![
                ("COVENANT_A".into(), "1".into()),
                ("COVENANT_B".into(), "with = equals".into()),
            ]
        );
    }

    #[test]
    fn env_file_rejects_non_covenant_keys_and_malformed_lines() {
        let err = parse_env_file("PATH=/tmp/evil").expect_err("non-covenant key");
        assert!(err.contains("non-COVENANT_"), "got: {err}");
        let err = parse_env_file("COVENANT_NO_VALUE").expect_err("malformed");
        assert!(err.contains("KEY=VALUE"), "got: {err}");
    }

    #[test]
    fn already_set_variables_win_over_the_file() {
        let pairs = vec![
            ("COVENANT_A".to_string(), "file".to_string()),
            ("COVENANT_B".to_string(), "file".to_string()),
        ];
        let apply = unset_pairs(pairs, |k| k == "COVENANT_A");
        assert_eq!(apply, vec![("COVENANT_B".into(), "file".into())]);
    }

    #[test]
    fn setup_args_parse_and_reject_unknowns() {
        let opts = parse_setup_args(&[
            "--payout-address".into(),
            "abc".into(),
            "--executor".into(),
            "subprocess".into(),
            "--price-micro-usdc".into(),
            "2500".into(),
        ])
        .expect("parses");
        assert_eq!(opts.payout_address.as_deref(), Some("abc"));
        assert_eq!(opts.executor.as_deref(), Some("subprocess"));
        assert_eq!(opts.price_micro_usdc, Some(2500));

        let opts = parse_setup_args(&[
            "--executor".into(),
            "openai-compat".into(),
            "--openai-url".into(),
            "http://127.0.0.1:1234/v1".into(),
            "--openai-api-key".into(),
            "k".into(),
        ])
        .expect("parses");
        assert_eq!(opts.executor.as_deref(), Some("openai-compat"));
        assert_eq!(opts.openai_url.as_deref(), Some("http://127.0.0.1:1234/v1"));
        assert_eq!(opts.openai_api_key.as_deref(), Some("k"));

        let err = parse_setup_args(&["--wat".into()]).expect_err("unknown flag");
        assert!(err.contains("unknown flag"), "got: {err}");
        let err =
            parse_setup_args(&["--executor".into(), "docker".into()]).expect_err("bad executor");
        assert!(err.contains("subprocess or container"), "got: {err}");

        let opts = parse_setup_args(&[
            "--executor".into(),
            "container".into(),
            "--container-image".into(),
            "ubuntu:24.04".into(),
        ])
        .expect("parses");
        assert_eq!(opts.executor.as_deref(), Some("container"));
        assert_eq!(opts.container_image.as_deref(), Some("ubuntu:24.04"));

        let err = parse_setup_args(&["--coordinator-url".into()]).expect_err("missing value");
        assert!(err.contains("needs a value"), "got: {err}");
    }

    #[test]
    fn pubkey_validation_wants_32_base58_bytes() {
        let ok = bs58::encode([7u8; 32]).into_string();
        assert!(valid_pubkey(&ok).is_ok());
        assert!(valid_pubkey("shortkey").is_err());
        assert!(valid_pubkey("not-base58-0OIl").is_err());
        assert!(valid_url("http://x").is_ok());
        assert!(valid_url("https://coordinator.example:8080").is_ok());
        assert!(valid_url("ftp://x").is_err());
        // A scheme with no host parses but is unreachable — reject it so
        // it isn't written and retried forever.
        assert!(valid_url("http://").is_err());
        assert!(valid_url("https://").is_err());
        assert!(
            valid_url("coordinator.example:8080").is_err(),
            "a bare host has no scheme"
        );
    }

    async fn ollama_stub(models: serde_json::Value) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new().route(
            "/api/tags",
            get(move || {
                let models = models.clone();
                async move { Json(serde_json::json!({ "models": models })) }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn full_setup_writes_a_loadable_env_file() {
        let home = tempfile::tempdir().unwrap();
        let ollama_url = ollama_stub(serde_json::json!([{ "name": "qwen2.5:7b" }])).await;
        let payout = bs58::encode([9u8; 32]).into_string();
        let coord_pk = bs58::encode([4u8; 32]).into_string();

        // Model list and coordinator URL arrive via prompt — empty
        // answer takes all models, the URL is invalid once then valid
        // — the rest via flags.
        let mut input = Cursor::new("\nnot-a-url\nhttp://127.0.0.1:1\n".to_string());
        let mut out = Vec::new();
        let opts = SetupOptions {
            coordinator_pubkey_b58: Some(coord_pk.clone()),
            payout_address: Some(payout.clone()),
            ollama_url: Some(ollama_url.clone()),
            price_micro_usdc: Some(1234),
            ..SetupOptions::default()
        };
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("setup succeeds");

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("qwen2.5:7b"), "probe report missing: {text}");
        assert!(
            text.contains("must start with http"),
            "invalid answer not re-prompted: {text}"
        );
        assert!(
            text.contains("not reachable right now"),
            "unreachable coordinator must warn, not fail: {text}"
        );
        assert!(
            text.contains("service install"),
            "the closing guidance must point at the way to keep earning across reboots: {text}"
        );

        let pairs = parse_env_file(&std::fs::read_to_string(&path).unwrap()).expect("loadable");
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(
            get("COVENANT_COMPUTE_COORDINATOR_URL"),
            "http://127.0.0.1:1"
        );
        assert_eq!(get("COVENANT_COMPUTE_COORDINATOR_PUBKEY"), coord_pk);
        assert_eq!(get("COVENANT_COMPUTE_PAYOUT_ADDRESS"), payout);
        assert_eq!(get("COVENANT_COMPUTE_NODE_EXECUTOR"), "ollama");
        assert_eq!(
            get("COVENANT_COMPUTE_NODE_MODELS"),
            "qwen2.5:7b",
            "the chosen model set must be pinned for the boot-time benchmark"
        );
        assert_eq!(get("COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC"), "1234");
        assert_eq!(
            get("COVENANT_COMPUTE_OLLAMA_URL"),
            ollama_url,
            "non-default ollama url must persist"
        );

        // Identity persisted — a re-run keeps the operator's key.
        assert!(home.path().join("identity.json").exists());
    }

    #[tokio::test]
    async fn setup_prompts_for_price_and_pins_the_answer_or_the_default() {
        let home = tempfile::tempdir().unwrap();
        let ollama_url = ollama_stub(serde_json::json!([{ "name": "qwen2.5:7b" }])).await;
        let anchors = || SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            ollama_url: Some(ollama_url.clone()),
            models: Some(vec!["qwen2.5:7b".into()]),
            ..SetupOptions::default()
        };
        let price_of = |path: &std::path::Path| {
            parse_env_file(&std::fs::read_to_string(path).unwrap())
                .unwrap()
                .into_iter()
                .find(|(k, _)| k == "COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC")
                .map(|(_, v)| v)
        };

        // The anchors all arrive by flag, so the price is the only prompt:
        // a typed value is pinned verbatim, and the operator was asked.
        let mut input = Cursor::new("2500\n".to_string());
        let mut out = Vec::new();
        let path = run_setup(home.path(), anchors(), None, &mut input, &mut out)
            .await
            .expect("typed price");
        assert_eq!(price_of(&path).as_deref(), Some("2500"));
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("price to charge per job"),
            "the operator must be asked what to charge, not silently shipped the default"
        );

        // Closed stdin with no flag pins the boot default rather than
        // failing — a scripted setup must not block on an optional knob.
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let path = run_setup(home.path(), anchors(), None, &mut input, &mut out)
            .await
            .expect("no price, no flag: takes the default");
        assert_eq!(price_of(&path), Some(DEFAULT_PRICE_MICRO_USDC.to_string()));
    }

    #[test]
    fn resolve_price_refuses_a_free_ask_and_reads_grouped_digits() {
        // A flagged 0 fails outright — a node asking 0 serves for free.
        let err = resolve_price(Some(0), &mut Cursor::new(String::new()), &mut Vec::new())
            .expect_err("a zero price must be refused");
        assert!(err.to_string().contains("at least 1"), "{err}");

        // Typed 0 re-prompts (not the same as an empty line, which takes
        // the default) and the next real answer, grouped, is accepted.
        let mut out = Vec::new();
        let price = resolve_price(None, &mut Cursor::new("0\n1_000_000\n"), &mut out)
            .expect("a real price after the zero");
        assert_eq!(price, 1_000_000);
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("serve every job for free"),
            "the operator is told why 0 was rejected"
        );

        // Three zeros exhaust the retries and bail toward the flag.
        let err = resolve_price(None, &mut Cursor::new("0\n0\n0\n"), &mut Vec::new())
            .expect_err("three zeros bail");
        assert!(err.to_string().contains("--price-micro-usdc"), "{err}");
    }

    #[tokio::test]
    async fn setup_pins_a_detected_gpu_into_the_env_file() {
        let home = tempfile::tempdir().unwrap();
        let ollama_url = ollama_stub(serde_json::json!([{ "name": "qwen2.5:7b" }])).await;
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            ollama_url: Some(ollama_url),
            models: Some(vec!["qwen2.5:7b".into()]),
            ..SetupOptions::default()
        };
        let gpu = DetectedGpu {
            model: "NVIDIA GeForce RTX 4090".into(),
            vram_gb: 24,
        };
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let path = run_setup(home.path(), opts, Some(gpu), &mut input, &mut out)
            .await
            .expect("setup succeeds");

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("NVIDIA GeForce RTX 4090") && text.contains("advertising it"),
            "the operator must see the detected GPU during setup: {text}"
        );

        let raw = std::fs::read_to_string(&path).unwrap();
        // Pinned as ACTIVE lines the boot path reads back, not a comment,
        // so the advertisement survives a boot without nvidia-smi.
        assert!(
            raw.contains("\nCOVENANT_COMPUTE_NODE_HARDWARE=consumer:NVIDIA GeForce RTX 4090\n"),
            "hardware must be pinned active: {raw}"
        );
        assert!(
            raw.contains("\nCOVENANT_COMPUTE_NODE_VRAM_GB=24\n"),
            "vram must be pinned active: {raw}"
        );
        assert!(
            !raw.contains("# COVENANT_COMPUTE_NODE_HARDWARE"),
            "no commented placeholder once a real GPU is pinned: {raw}"
        );

        // What was pinned loads back to exactly the detected class.
        let pairs = parse_env_file(&raw).expect("loadable");
        let hardware = pairs
            .iter()
            .find(|(k, _)| k == "COVENANT_COMPUTE_NODE_HARDWARE")
            .map(|(_, v)| v.clone());
        assert_eq!(
            hardware.as_deref(),
            Some("consumer:NVIDIA GeForce RTX 4090")
        );
        assert_eq!(
            crate::gpu::parse_hardware(hardware.as_deref().unwrap()),
            DetectedGpu {
                model: "NVIDIA GeForce RTX 4090".into(),
                vram_gb: 24
            }
            .hardware_class()
        );

        // The container hint carries the GPU passthrough knob, so an
        // operator who switches to the container keeps delivering the GPU
        // this node advertises instead of silently running CPU-only.
        assert!(
            raw.contains("# COVENANT_COMPUTE_NODE_CONTAINER_GPUS=all"),
            "a detected GPU must add the container passthrough hint: {raw}"
        );
    }

    #[tokio::test]
    async fn setup_without_a_gpu_advertises_cpu_only_and_keeps_the_hint() {
        let home = tempfile::tempdir().unwrap();
        let ollama_url = ollama_stub(serde_json::json!([{ "name": "qwen2.5:7b" }])).await;
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            ollama_url: Some(ollama_url),
            models: Some(vec!["qwen2.5:7b".into()]),
            ..SetupOptions::default()
        };
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("setup succeeds");

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("no GPU detected"),
            "a GPU-less host must be told it serves CPU-only: {text}"
        );

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("# COVENANT_COMPUTE_NODE_HARDWARE=consumer:rtx4090"),
            "the hardware knob stays a commented hint: {raw}"
        );
        // Nothing active pins hardware, so boot-time GPU detection still
        // runs — a GPU added later is picked up without re-running setup.
        let pairs = parse_env_file(&raw).expect("loadable");
        assert!(
            !pairs
                .iter()
                .any(|(k, _)| k == "COVENANT_COMPUTE_NODE_HARDWARE"),
            "no active hardware pin when no GPU was detected: {raw}"
        );
        // No GPU, so no container passthrough hint — a CPU host isn't told
        // to wire a device it doesn't have.
        assert!(
            !raw.contains("COVENANT_COMPUTE_NODE_CONTAINER_GPUS"),
            "no GPU passthrough hint on a CPU-only host: {raw}"
        );
    }

    #[tokio::test]
    async fn setup_fails_actionably_without_ollama_unless_subprocess_is_explicit() {
        let home = tempfile::tempdir().unwrap();
        let payout = bs58::encode([9u8; 32]).into_string();
        let coord_pk = bs58::encode([4u8; 32]).into_string();
        let base = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(coord_pk),
            payout_address: Some(payout),
            // Unroutable: the probe must fail fast.
            ollama_url: Some("http://127.0.0.1:1".into()),
            ..SetupOptions::default()
        };

        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let err = run_setup(home.path(), base, None, &mut input, &mut out)
            .await
            .expect_err("no ollama");
        let msg = err.to_string();
        assert!(msg.contains("ollama.com/download"), "got: {msg}");
        assert!(
            msg.contains("openai-compat") && msg.contains("subprocess"),
            "an operator already running LM Studio/vLLM must be pointed at openai-compat, not \
             just ollama-or-subprocess: {msg}"
        );

        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("subprocess".into()),
            ..SetupOptions::default()
        };
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("subprocess opt-in works without ollama");
        let pairs = parse_env_file(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(pairs.contains(&(
            "COVENANT_COMPUTE_NODE_EXECUTOR".to_string(),
            "subprocess".to_string()
        )));
        assert!(
            !pairs
                .iter()
                .any(|(k, _)| k == "COVENANT_COMPUTE_NODE_MODELS"),
            "a subprocess node pins no model list"
        );
        let warning = String::from_utf8(out).unwrap();
        assert!(
            warning.contains("no filesystem or network isolation") && warning.contains("container"),
            "choosing subprocess must warn it runs strangers' commands unwalled, and name the \
             container alternative: {warning}"
        );
    }

    #[tokio::test]
    async fn container_setup_pins_the_default_image_and_passes_a_detected_gpu() {
        let home = tempfile::tempdir().unwrap();
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        // No --container-image, no ollama/openai backend reachable: the
        // container executor serves shell jobs, so setup must neither probe
        // a model server nor leave the boot-required image unset.
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("container".into()),
            ..SetupOptions::default()
        };
        let gpu = DetectedGpu {
            model: "NVIDIA GeForce RTX 4090".into(),
            vram_gb: 24,
        };
        let path = run_setup(home.path(), opts, Some(gpu), &mut input, &mut out)
            .await
            .expect("container opt-in needs no model backend");

        let raw = std::fs::read_to_string(&path).unwrap();
        let pairs = parse_env_file(&raw).expect("loadable");
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            get("COVENANT_COMPUTE_NODE_EXECUTOR").as_deref(),
            Some("container")
        );
        assert_eq!(
            get("COVENANT_COMPUTE_NODE_CONTAINER_IMAGE").as_deref(),
            Some(DEFAULT_CONTAINER_IMAGE),
            "boot refuses to serve without an image, so setup pins the default: {raw}"
        );
        assert_eq!(
            get("COVENANT_COMPUTE_NODE_CONTAINER_GPUS").as_deref(),
            Some("all"),
            "a container node advertising a detected GPU must pass the device in: {raw}"
        );
        assert!(
            !pairs
                .iter()
                .any(|(k, _)| k == "COVENANT_COMPUTE_NODE_MODELS"),
            "a container node serves `any`, pinning no model list: {raw}"
        );
        // Active lines, not the commented switch-to-container hint that only
        // helps an operator who hasn't chosen it yet.
        assert!(
            !raw.contains("# COVENANT_COMPUTE_NODE_EXECUTOR=container"),
            "no redundant switch hint once container is the chosen executor: {raw}"
        );
        assert!(
            !raw.contains("# COVENANT_COMPUTE_NODE_CONTAINER_GPUS=all"),
            "the GPU passthrough is written active, not as a comment: {raw}"
        );
        let msg = String::from_utf8(out).unwrap();
        assert!(
            msg.contains(DEFAULT_CONTAINER_IMAGE)
                && msg.to_lowercase().contains("docker or podman"),
            "container setup names the image and the runtime it needs: {msg}"
        );
    }

    #[tokio::test]
    async fn container_setup_honors_an_explicit_image_and_skips_gpu_without_one() {
        let home = tempfile::tempdir().unwrap();
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("container".into()),
            container_image: Some("ubuntu:24.04".into()),
            ..SetupOptions::default()
        };
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("container opt-in works on a CPU host");
        let raw = std::fs::read_to_string(&path).unwrap();
        let pairs = parse_env_file(&raw).expect("loadable");
        assert!(
            pairs.contains(&(
                "COVENANT_COMPUTE_NODE_CONTAINER_IMAGE".to_string(),
                "ubuntu:24.04".to_string()
            )),
            "an explicit --container-image must persist: {raw}"
        );
        assert!(
            !pairs
                .iter()
                .any(|(k, _)| k == "COVENANT_COMPUTE_NODE_CONTAINER_GPUS"),
            "no GPU detected: nothing to pass through: {raw}"
        );
    }

    #[test]
    fn parse_setup_args_reads_whisper_and_say() {
        let opts = parse_setup_args(&[
            "--executor".into(),
            "whisper".into(),
            "--whisper-model".into(),
            "/models/ggml-base.en.bin".into(),
            "--whisper-bin".into(),
            "/opt/whisper-cli".into(),
        ])
        .expect("parses whisper");
        assert_eq!(opts.executor.as_deref(), Some("whisper"));
        assert_eq!(
            opts.whisper_model.as_deref(),
            Some("/models/ggml-base.en.bin")
        );
        assert_eq!(opts.whisper_bin.as_deref(), Some("/opt/whisper-cli"));

        let opts = parse_setup_args(&[
            "--executor".into(),
            "say".into(),
            "--say-bin".into(),
            "/opt/say".into(),
        ])
        .expect("parses say");
        assert_eq!(opts.executor.as_deref(), Some("say"));
        assert_eq!(opts.say_bin.as_deref(), Some("/opt/say"));
    }

    #[test]
    fn servable_labels_and_job_kinds_cover_say_and_whisper() {
        // The regression this guards: `say` once fell through to the
        // batch_job default, so a speech node's own coherence check rejected
        // the one kind it serves and waved through one it can't.
        assert_eq!(servable_kind_labels("say"), &["speech_synthesis"]);
        assert_eq!(servable_kind_labels("whisper"), &["transcription"]);
        assert_eq!(
            validate_job_kinds("say", &["speech_synthesis".into()]).unwrap(),
            vec!["speech_synthesis".to_string()]
        );
        assert_eq!(
            validate_job_kinds("whisper", &["transcription".into()]).unwrap(),
            vec!["transcription".to_string()]
        );
        let err = validate_job_kinds("say", &["transcription".into()]).unwrap_err();
        assert!(err.contains("can't serve"), "{err}");
        assert!(validate_job_kinds("say", &["batch_job".into()]).is_err());
    }

    #[tokio::test]
    async fn say_setup_needs_no_backend_and_advertises_the_say_id() {
        let home = tempfile::tempdir().unwrap();
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        // No model server, no model file, no stdin: a say node onboards from
        // flags alone — the zero-config text-to-speech path on macOS.
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("say".into()),
            price_micro_usdc: Some(1_000),
            ..SetupOptions::default()
        };
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("a say node needs no backend probe");
        let raw = std::fs::read_to_string(&path).unwrap();
        let pairs = parse_env_file(&raw).expect("loadable");
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            get("COVENANT_COMPUTE_NODE_EXECUTOR").as_deref(),
            Some("say")
        );
        assert!(
            !pairs
                .iter()
                .any(|(k, _)| k == "COVENANT_COMPUTE_NODE_MODELS"),
            "a say node serves the fixed say-1 id, pinning no model list: {raw}"
        );
        assert!(
            !pairs.iter().any(|(k, _)| k == "COVENANT_COMPUTE_SAY_BIN"),
            "a stock macOS node.env carries no backend line — the default `say` is left implicit: {raw}"
        );
        let msg = String::from_utf8(out).unwrap();
        assert!(
            msg.contains("say-1") && msg.to_lowercase().contains("macos"),
            "say setup names the model id and the OS tool it needs: {msg}"
        );
    }

    #[tokio::test]
    async fn say_setup_pins_a_non_default_synthesizer() {
        let home = tempfile::tempdir().unwrap();
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("say".into()),
            say_bin: Some("/opt/tts/say".into()),
            price_micro_usdc: Some(1_000),
            ..SetupOptions::default()
        };
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("say setup honors a custom synthesizer");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("COVENANT_COMPUTE_SAY_BIN=/opt/tts/say"),
            "an explicit --say-bin persists so boot runs it: {raw}"
        );
    }

    #[tokio::test]
    async fn whisper_setup_pins_the_model_file_and_an_optional_bin() {
        let home = tempfile::tempdir().unwrap();
        let model = home.path().join("ggml-base.en.bin");
        std::fs::write(&model, b"not a real model, just a file that exists").unwrap();
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("whisper".into()),
            whisper_model: Some(model.to_string_lossy().into_owned()),
            whisper_bin: Some("/opt/whisper/whisper-cli".into()),
            price_micro_usdc: Some(1_000),
            ..SetupOptions::default()
        };
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("a whisper node onboards against a present model file");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(&format!(
                "COVENANT_COMPUTE_WHISPER_MODEL={}",
                model.display()
            )),
            "the model file boot requires is pinned: {raw}"
        );
        assert!(
            raw.contains("COVENANT_COMPUTE_WHISPER_BIN=/opt/whisper/whisper-cli"),
            "an explicit --whisper-bin persists: {raw}"
        );
        assert!(
            !raw.contains("COVENANT_COMPUTE_NODE_MODELS"),
            "a whisper node serves the fixed whisper-1 id, pinning no model list: {raw}"
        );
    }

    #[tokio::test]
    async fn whisper_setup_refuses_a_missing_model_file() {
        let home = tempfile::tempdir().unwrap();
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("whisper".into()),
            whisper_model: Some(
                home.path()
                    .join("does-not-exist.bin")
                    .to_string_lossy()
                    .into_owned(),
            ),
            price_micro_usdc: Some(1_000),
            ..SetupOptions::default()
        };
        let err = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect_err("a whisper node with no model file boots to nothing, so setup refuses it");
        assert!(err.to_string().contains("no file at"), "{err}");
        assert!(
            !home.path().join(ENV_FILE).exists(),
            "a refused setup writes no node.env"
        );
    }

    async fn openai_stub(ids: &[&str]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let data: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| serde_json::json!({ "id": id, "object": "model" }))
            .collect();
        let router = Router::new().route(
            "/models",
            get(move || {
                let data = data.clone();
                async move { Json(serde_json::json!({ "object": "list", "data": data })) }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn openai_compat_setup_probes_the_backend_and_persists_its_config() {
        let home = tempfile::tempdir().unwrap();
        let openai_url = openai_stub(&["coder-7b", "chat-8b"]).await;

        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("openai-compat".into()),
            openai_url: Some(openai_url.clone()),
            openai_api_key: Some("op-key-1".into()),
            models: Some(vec!["coder-7b".into()]),
            ..SetupOptions::default()
        };
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("setup succeeds");

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("coder-7b"), "probe report missing: {text}");

        let pairs = parse_env_file(&std::fs::read_to_string(&path).unwrap()).expect("loadable");
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(get("COVENANT_COMPUTE_NODE_EXECUTOR"), "openai-compat");
        assert_eq!(
            get("COVENANT_COMPUTE_NODE_MODELS"),
            "coder-7b",
            "the chosen model set must be pinned for the boot-time benchmark"
        );
        assert_eq!(
            get("COVENANT_COMPUTE_OPENAI_URL"),
            openai_url,
            "non-default backend url must persist"
        );
        assert_eq!(
            get("COVENANT_COMPUTE_OPENAI_API_KEY"),
            "op-key-1",
            "the key must persist or every boot fails auth"
        );
    }

    #[tokio::test]
    async fn openai_compat_setup_prompts_for_the_api_key_instead_of_forcing_a_flag() {
        let home = tempfile::tempdir().unwrap();
        let openai_url = openai_stub(&["coder-7b"]).await;

        // No --openai-api-key flag: an interactive operator types the key
        // at the prompt, so it never lands in `ps` output or shell
        // history. The model set is a flag, so the only stdin read is the
        // key.
        let mut input = Cursor::new("prompted-key\n".to_string());
        let mut out = Vec::new();
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("openai-compat".into()),
            openai_url: Some(openai_url),
            models: Some(vec!["coder-7b".into()]),
            ..SetupOptions::default()
        };
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("setup succeeds with a prompted key");
        assert!(
            String::from_utf8_lossy(&out).contains("API key for the backend"),
            "an interactive setup must offer to enter the key"
        );
        let pairs = parse_env_file(&std::fs::read_to_string(&path).unwrap()).expect("loadable");
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "COVENANT_COMPUTE_OPENAI_API_KEY" && v == "prompted-key"),
            "the prompted key must persist for boot-time auth: {pairs:?}"
        );
    }

    #[tokio::test]
    async fn openai_compat_setup_takes_a_blank_key_as_no_key() {
        let home = tempfile::tempdir().unwrap();
        let openai_url = openai_stub(&["coder-7b"]).await;

        // A keyless backend: the operator hits Enter at the key prompt.
        let mut input = Cursor::new("\n".to_string());
        let mut out = Vec::new();
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("openai-compat".into()),
            openai_url: Some(openai_url),
            models: Some(vec!["coder-7b".into()]),
            ..SetupOptions::default()
        };
        let path = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect("setup succeeds with no key");
        let pairs = parse_env_file(&std::fs::read_to_string(&path).unwrap()).expect("loadable");
        assert!(
            !pairs
                .iter()
                .any(|(k, _)| k == "COVENANT_COMPUTE_OPENAI_API_KEY"),
            "a blank key must not write an empty key line: {pairs:?}"
        );
    }

    #[tokio::test]
    async fn openai_compat_setup_fails_actionably_when_the_backend_is_down() {
        let home = tempfile::tempdir().unwrap();
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let opts = SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            executor: Some("openai-compat".into()),
            // Unroutable: the probe must fail fast.
            openai_url: Some("http://127.0.0.1:1".into()),
            ..SetupOptions::default()
        };
        let err = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect_err("no backend");
        assert!(
            err.to_string().contains("--openai-url"),
            "the failure must point at the fix: {err}"
        );
    }

    #[tokio::test]
    async fn model_selection_validates_against_whats_available() {
        let home = tempfile::tempdir().unwrap();
        let ollama_url =
            ollama_stub(serde_json::json!([{ "name": "gen-model" }, { "name": "embed-model" }]))
                .await;
        let anchors = |models: Option<Vec<String>>| SetupOptions {
            coordinator_url: Some("http://127.0.0.1:1".into()),
            coordinator_pubkey_b58: Some(bs58::encode([4u8; 32]).into_string()),
            payout_address: Some(bs58::encode([9u8; 32]).into_string()),
            ollama_url: Some(ollama_url.clone()),
            models,
            ..SetupOptions::default()
        };

        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let err = run_setup(
            home.path(),
            anchors(Some(vec!["other".into()])),
            None,
            &mut input,
            &mut out,
        )
        .await
        .expect_err("unknown model via flag");
        assert!(err.to_string().contains("not among"), "got: {err}");

        // Interactive: an unknown name re-prompts, then a subset sticks.
        let mut input = Cursor::new("nope\ngen-model\n".to_string());
        let mut out = Vec::new();
        let path = run_setup(home.path(), anchors(None), None, &mut input, &mut out)
            .await
            .expect("subset selection succeeds");
        let pairs = parse_env_file(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(pairs.contains(&(
            "COVENANT_COMPUTE_NODE_MODELS".to_string(),
            "gen-model".to_string()
        )));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("not among"), "re-prompt missing: {text}");
        // With an embedding model on the backend, the operator is warned
        // at the prompt that it can't serve — before the "all" default
        // hands them a node that fails its first boot check.
        assert!(
            text.contains("only text-generation models can serve"),
            "the embedding-model caveat must show at the prompt: {text}"
        );
    }

    #[tokio::test]
    async fn stdin_running_dry_points_at_the_missing_flag() {
        let home = tempfile::tempdir().unwrap();
        let ollama_url = ollama_stub(serde_json::json!([{ "name": "m" }])).await;

        // Dry at the very first prompt (models).
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let opts = SetupOptions {
            ollama_url: Some(ollama_url.clone()),
            ..SetupOptions::default()
        };
        let err = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect_err("nothing to answer prompts");
        assert!(err.to_string().contains("--models"), "got: {err}");

        // Models supplied — dry at the next prompt, named accordingly.
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let opts = SetupOptions {
            ollama_url: Some(ollama_url),
            models: Some(vec!["m".into()]),
            ..SetupOptions::default()
        };
        let err = run_setup(home.path(), opts, None, &mut input, &mut out)
            .await
            .expect_err("nothing to answer prompts");
        assert!(err.to_string().contains("--coordinator-url"), "got: {err}");
    }
}
