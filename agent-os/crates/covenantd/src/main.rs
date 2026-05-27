//! Covenantd binary entrypoint. Wires runtime + router + memory + settlement
//! + server and hands them a Unix listener at `$COVENANT_HOME/sock`.

#![deny(unsafe_code)]

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::net::UnixListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("covenantd {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!(
                    "covenantd {version}\n\
                     local control plane for Covenant agents\n\n\
                     usage:\n  \
                       covenantd                 start the daemon (config from $COVENANT_HOME)\n  \
                       covenantd --version       print the version and exit\n  \
                       covenantd --help          print this help and exit\n",
                    version = env!("CARGO_PKG_VERSION"),
                );
                return Ok(());
            }
            other => {
                eprintln!("covenantd: unrecognised argument '{other}'. Try --help.");
                std::process::exit(2);
            }
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "covenantd=info".into()),
        )
        .init();

    let home = covenantd::covenant_home()?;
    std::fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;

    let agents_dir = home.join("agents");
    let cards = covenant_router::load_agents_from_dir(&agents_dir)
        .with_context(|| format!("load agents from {}", agents_dir.display()))?;
    info!(
        agents_dir = %agents_dir.display(),
        registered = cards.len(),
        "agents loaded"
    );
    let router = Arc::new(covenant_router::Router::from_cards(cards));
    let runtime_config = covenantd::runtime_runner_config_from_env(&home)?;
    let hermes_config = covenantd::hermes_gateway_config_from_env();
    let subprocess_tracker = Arc::new(covenant_runtime::SubprocessTracker::new());
    // Live audit step-trail (opt-in via COVENANT_LIVE_TRACE=1): the Hermes
    // runner streams each tool trace through this channel and the drainer
    // writes them into the chain as they arrive. Off by default — the proven
    // path folds the runner's runtime_events into the chain when the run ends.
    let live_trace = std::env::var("COVENANT_LIVE_TRACE").ok().as_deref() == Some("1");
    let (runtime_event_tx, runtime_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let runner = covenantd::runtime_runner_composite(
        &runtime_config,
        hermes_config.as_ref(),
        subprocess_tracker.clone(),
        live_trace.then_some(runtime_event_tx),
    );
    info!(
        backend = runtime_config.backend_name(),
        hermes = hermes_config.is_some(),
        "runtime runner ready"
    );

    // Probe the Hermes gateway up front when configured. A failed probe
    // or missing feature flag is non-fatal — we log loud so the operator
    // can fix it, then continue. Boot-time blocking on a remote service
    // would make a transient Hermes outage break Covenant restarts.
    if let Some(cfg) = hermes_config.as_ref() {
        match covenant_runtime::HermesRunner::new(cfg.base_url.clone(), cfg.api_key.clone()) {
            Ok(probe_runner) => match probe_runner.probe_capabilities().await {
                Some(caps) if caps.covers_runner() => {
                    info!(
                        base_url = %cfg.base_url,
                        sse = caps.run_events_sse,
                        stop = caps.run_stop,
                        approval = caps.run_approval_response,
                        "hermes gateway features confirmed",
                    );
                }
                Some(caps) => {
                    tracing::warn!(
                        base_url = %cfg.base_url,
                        run_submission = caps.run_submission,
                        run_events_sse = caps.run_events_sse,
                        run_stop = caps.run_stop,
                        "hermes gateway missing required features — dispatches may fail; upgrade to hermes-agent >= v0.12 or run with hermes disabled",
                    );
                }
                None => {
                    tracing::warn!(
                        base_url = %cfg.base_url,
                        "hermes capabilities probe failed (gateway unreachable or auth invalid) — hermes runtime will surface errors on first dispatch",
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    base_url = %cfg.base_url,
                    "hermes capabilities probe skipped — runner init failed",
                );
            }
        }
    }

    let memory_path = home.join("memory.db");
    let memory = Arc::new(
        covenant_memory::SqliteStore::open(&memory_path)
            .with_context(|| format!("open memory store at {}", memory_path.display()))?,
    );
    info!(path = %memory_path.display(), "memory store open");

    let receipts_path = home.join("receipts").join("working.jsonl");
    let settlement = Arc::new(
        covenant_settlement::JsonlReceiptStore::open(receipts_path.clone())
            .await
            .with_context(|| format!("open receipt log at {}", receipts_path.display()))?,
    );
    info!(path = %receipts_path.display(), "settlement receipt log open");

    let identity_path = home.join("identity").join("local.key");
    let identity = Arc::new(
        covenant_identity::LocalIdentity::load_or_create(&identity_path, "user@local")
            .with_context(|| format!("load/create identity at {}", identity_path.display()))?,
    );
    info!(
        display = %identity.display(),
        pubkey = %covenant_types::AgentId { display: String::new(), pubkey: identity.pubkey_bytes() }.pubkey_base58(),
        "local identity ready"
    );

    let audit_path = home.join("audit").join("events.jsonl");
    let audit = Arc::new(
        covenant_audit::JsonlAuditLog::open(audit_path.clone())
            .await
            .with_context(|| format!("open audit log at {}", audit_path.display()))?,
    );
    info!(path = %audit_path.display(), "audit log open");

    let capabilities_path = home.join("capabilities").join("granted.jsonl");
    let capabilities = Arc::new(
        covenant_permissions::JsonlCapabilityStore::open(capabilities_path.clone())
            .await
            .with_context(|| format!("open capability store at {}", capabilities_path.display()))?,
    );
    info!(path = %capabilities_path.display(), "capability store open");

    let secrets_path = home.join("secrets.toml");
    let embedder: Arc<dyn covenant_llm::Embedder> =
        Arc::from(covenant_llm::pick_embedder(&secrets_path).await);
    info!(embedder = %embedder.name(), "embedder ready");

    let ignore_path = home.join(".covenantignore");
    if !ignore_path.exists() {
        if let Err(e) = std::fs::write(&ignore_path, default_ignorefile()) {
            tracing::warn!(error = %e, path = %ignore_path.display(), "could not seed default .covenantignore");
        } else {
            info!(path = %ignore_path.display(), "seeded default .covenantignore");
        }
    }
    let ignore = Arc::new(
        covenant_memory::IgnoreSet::load(&ignore_path)
            .with_context(|| format!("load {}", ignore_path.display()))?,
    );
    info!(rules = ignore.len(), "ignore set loaded");

    let mut tools_vec: Vec<Arc<dyn covenant_mcp::Tool>> = vec![
        Arc::new(covenant_mcp::native::EchoTool),
        Arc::new(covenant_mcp::native::ClockTool),
    ];
    let mcp_cfg = covenant_mcp::config::McpConfigFile::from_path(&secrets_path)
        .with_context(|| format!("parse mcp config in {}", secrets_path.display()))?;
    for srv in mcp_cfg.servers() {
        if !srv.enabled {
            info!(server = %srv.name, "mcp server disabled");
            continue;
        }
        match covenant_mcp::transport::StdioMcpClient::spawn_with_env(
            &srv.command,
            &srv.args,
            &srv.env,
        )
        .await
        {
            Ok(client) => {
                let client_dyn: Arc<dyn covenant_mcp::transport::McpClient> = client;
                let options = covenant_mcp::external::RemoteToolOptions {
                    tool_prefix: srv.tool_prefix.clone(),
                    include: srv.include.clone(),
                    exclude: srv.exclude.clone(),
                };
                match covenant_mcp::external::bootstrap_remote_tools_with_options(
                    client_dyn, options,
                )
                .await
                {
                    Ok(remote) => {
                        info!(server = %srv.name, count = remote.len(), "mcp server ready");
                        tools_vec.extend(remote);
                    }
                    Err(e) => {
                        tracing::warn!(server = %srv.name, error = %e, "mcp bootstrap failed; skipping server");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(server = %srv.name, command = %srv.command, error = %e, "mcp spawn failed; skipping server");
            }
        }
    }
    let tools = Arc::new(covenant_mcp::ToolRegistry::from_tools(tools_vec));
    info!(count = tools.len(), tools = ?tools.names(), "tool registry ready");

    let mailbox_path = home.join("a2a").join("events.jsonl");
    let mailbox: Arc<dyn covenant_a2a::Mailbox> = Arc::new(
        covenant_a2a::JsonlMailbox::open(mailbox_path.clone())
            .await
            .with_context(|| format!("open a2a mailbox at {}", mailbox_path.display()))?,
    );
    info!(path = %mailbox_path.display(), "a2a mailbox open");

    let peers_path = home.join("peers").join("registry.jsonl");
    let peers: Arc<dyn covenant_peer_auth::PeerRegistry> = Arc::new(
        covenant_peer_auth::JsonlPeerRegistry::open(peers_path.clone())
            .await
            .with_context(|| format!("open peer registry at {}", peers_path.display()))?,
    );
    info!(path = %peers_path.display(), "peer registry open");

    bootstrap_operator_token(&home, &peers, &identity).await?;

    let budget_path = home.join("budget").join("ledger.jsonl");
    let budget: Arc<dyn covenant_budget::BudgetLedger> = Arc::new(
        covenant_budget::JsonlLedger::open(budget_path.clone())
            .await
            .with_context(|| format!("open budget ledger at {}", budget_path.display()))?,
    );
    info!(path = %budget_path.display(), "budget ledger open");

    let budget_checkpoints_path = home.join("budget").join("checkpoints.jsonl");
    let budget_checkpoints = Arc::new(
        covenant_budget::JsonlPauseCheckpointStore::open(budget_checkpoints_path.clone())
            .await
            .with_context(|| {
                format!(
                    "open budget checkpoint log at {}",
                    budget_checkpoints_path.display()
                )
            })?,
    );
    info!(path = %budget_checkpoints_path.display(), "budget checkpoint log open");

    // SAP bridge — opt-in, off by default. Building the bridge is
    // cheap (no network) and we log the resolved status either way so
    // operators can see at a glance whether the on-chain path is live.
    let sap_config = covenantd::sap_bridge_config_from_env();
    let sap_bridge =
        covenant_sap_bridge::SapBridge::new(sap_config.clone()).context("build SAP bridge")?;
    info!(
        enabled = sap_config.enabled,
        cluster = sap_config.cluster.as_str(),
        program_id = %sap_config.program_id,
        rpc_url = %sap_config.rpc_url,
        "sap bridge ready"
    );

    let server = covenantd::Server::new(
        router,
        runner,
        memory,
        settlement,
        audit,
        capabilities,
        embedder,
        identity,
        ignore,
        tools,
        mailbox,
        peers,
        budget,
    )
    .with_home(home.clone())
    .with_budget_checkpoints(budget_checkpoints)
    .with_subprocess_tracker(subprocess_tracker)
    .with_sap_bridge(sap_bridge);

    server
        .register_agent_budgets()
        .await
        .context("seed agent budget capacities from manifests")?;

    let a2a_retry_scheduler = covenantd::a2a_auto_retry_scheduler_config_from_env()?;
    let a2a_retry_scheduler_handle = if a2a_retry_scheduler.enabled {
        info!(
            interval_ms = a2a_retry_scheduler.interval_ms,
            min_lease_age_ms = a2a_retry_scheduler.policy.min_lease_age_ms,
            max_attempts = a2a_retry_scheduler.policy.max_attempts,
            max_requeues = a2a_retry_scheduler.policy.max_requeues,
            scan_limit = a2a_retry_scheduler.policy.scan_limit,
            "a2a auto retry scheduler enabled"
        );
        Some(covenantd::spawn_a2a_auto_retry_scheduler(
            server.clone(),
            a2a_retry_scheduler,
        ))
    } else {
        info!("a2a auto retry scheduler disabled");
        None
    };

    let projection_tick = covenantd::projection_tick_config_from_env()?;
    info!(
        period_ms = projection_tick.period_ms,
        grace_ms = projection_tick.grace_ms,
        "budget projection tick driver enabled"
    );
    let projection_tick_handle =
        covenantd::spawn_projection_tick_driver(server.clone(), projection_tick);

    // Fold live Hermes runtime traces into the audit chain as they stream in
    // (only when COVENANT_LIVE_TRACE=1; otherwise traces fold at run end).
    let runtime_event_drainer_handle =
        live_trace.then(|| covenantd::spawn_runtime_event_drainer(server.clone(), runtime_event_rx));

    // HTTP gateway for browser UIs. Every protected route requires
    // `Authorization: Bearer <token>` resolved via the same peer
    // registry the Unix socket uses; only `/health` is open. Operator
    // / web UI reads the token from `$COVENANT_HOME/peers/operator.token`.
    let http_port: u16 = std::env::var("COVENANT_HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8421);
    let http_bind: std::net::IpAddr = std::env::var("COVENANT_HTTP_BIND_ADDR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]));
    let http_addr = std::net::SocketAddr::new(http_bind, http_port);
    let http_state = covenantd::http::HttpState {
        server: server.clone(),
    };
    let http_router = covenantd::http::router(http_state);
    let http_listener = tokio::net::TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("bind http {}", http_addr))?;
    info!(addr = %http_addr, "http gateway listening (bearer-auth enforced)");
    let http_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(http_listener, http_router).await {
            tracing::warn!(error = %e, "http gateway exited");
        }
    });

    let sock_path = home.join("sock");
    if sock_path.exists() {
        std::fs::remove_file(&sock_path)
            .with_context(|| format!("remove stale socket at {}", sock_path.display()))?;
    }
    let listener =
        UnixListener::bind(&sock_path).with_context(|| format!("bind {}", sock_path.display()))?;
    info!(path = %sock_path.display(), "covenantd listening");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("shutdown requested");
            let saved = server.save_shutdown_budget_checkpoints().await;
            info!(saved, "shutdown budget checkpoints saved");
        }
        res = server.serve(listener) => {
            res?;
        }
    }

    http_handle.abort();
    if let Some(handle) = a2a_retry_scheduler_handle {
        handle.abort();
    }
    projection_tick_handle.abort();
    if let Some(h) = runtime_event_drainer_handle {
        h.abort();
    }
    if sock_path.exists() {
        let _ = std::fs::remove_file(&sock_path);
    }
    Ok(())
}

/// Mint an operator token on first start (or read the existing one) and
/// register it in the peer registry under the daemon's local identity.
/// The token is the bootstrap credential the CLI and Web UI use to
/// authenticate; it lives at `$COVENANT_HOME/peers/operator.token` with
/// mode `0600` so only the running user can read it.
async fn bootstrap_operator_token(
    home: &std::path::Path,
    peers: &Arc<dyn covenant_peer_auth::PeerRegistry>,
    identity: &Arc<covenant_identity::LocalIdentity>,
) -> Result<()> {
    let token_path = home.join("peers").join("operator.token");
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let token = if token_path.exists() {
        // The mode of the existing file is the operator's exposure.
        // If anything other than 0600, refuse to trust it — silently
        // regenerating would still leak the prior token to whoever
        // could read it. Loud failure forces operator action.
        covenantd::require_operator_token_mode_0600(&token_path).with_context(|| {
            format!(
                "operator token at {} has insecure permissions",
                token_path.display()
            )
        })?;
        let s = std::fs::read_to_string(&token_path)
            .with_context(|| format!("read operator token at {}", token_path.display()))?;
        match covenant_peer_auth::PeerToken::from_b58(s.trim()) {
            Ok(t) => {
                // If the registry already resolves it, nothing to do.
                if peers.resolve(&t).await?.is_some() {
                    info!(path = %token_path.display(), "operator token reused");
                    return Ok(());
                }
                t
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %token_path.display(), "operator token unreadable; regenerating");
                covenant_peer_auth::PeerToken::generate()
            }
        }
    } else {
        covenant_peer_auth::PeerToken::generate()
    };

    covenantd::write_operator_token_0600(&token_path, &token.to_b58())
        .with_context(|| format!("write operator token at {}", token_path.display()))?;

    let entry = covenant_peer_auth::PeerEntry {
        token,
        agent_id: covenant_types::AgentId::new(identity.display(), identity.pubkey_bytes()),
        registered_at: epoch_ms(),
    };
    peers.register(entry).await?;
    info!(path = %token_path.display(), display = %identity.display(), "operator token minted and registered");
    Ok(())
}

fn epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn default_ignorefile() -> &'static str {
    "# .covenantignore — patterns matched against intent text and any other\n\
     # auto-ingested content. gitignore-style; '!' un-ignores; '/' anchors.\n\
     # Last matching rule wins.\n\
     #\n\
     # Default rules cover the obvious credential paths. Edit freely.\n\
     \n\
     **/.env\n\
     **/.env.*\n\
     **/secrets.toml\n\
     **/secrets.json\n\
     **/*.pem\n\
     **/*.key\n\
     **/id_rsa*\n\
     **/id_ed25519*\n\
     **/.ssh/**\n\
     **/credentials\n\
     **/credentials.json\n\
     **/.aws/credentials\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ignorefile_pins_each_credential_path_pattern_and_operator_comment_header() {
        // default_ignorefile is the security-critical bootstrap
        // content written to ~/.covenant/.covenantignore on first
        // daemon start (the std::fs::write call gated by
        // `if !ignore_path.exists()` near the daemon-home bootstrap
        // block). The function encodes the
        // documented floor of credential patterns the daemon must NOT
        // auto-ingest under any operator config — .env, secrets.toml,
        // secrets.json, *.pem, *.key, id_rsa*, id_ed25519*, .ssh/**,
        // credentials, credentials.json, .aws/credentials.
        //
        // The function has no direct test today; its content is
        // exercised only indirectly through the daemon's bootstrap
        // path. A refactor that dropped '**/.env' under a 'let
        // operators decide their own rules' rationale would silently
        // let every new daemon auto-ingest .env files containing API
        // keys; a refactor that renamed '**/id_rsa*' to '*id_rsa*'
        // would silently widen the glob (root-only vs any-depth) and
        // let id_rsa-style filenames slip through in nested locations.
        let body = default_ignorefile();

        for pattern in [
            "**/.env",
            "**/.env.*",
            "**/secrets.toml",
            "**/secrets.json",
            "**/*.pem",
            "**/*.key",
            "**/id_rsa*",
            "**/id_ed25519*",
            "**/.ssh/**",
            "**/credentials",
            "**/credentials.json",
            "**/.aws/credentials",
        ] {
            assert!(
                body.contains(pattern),
                "default_ignorefile must contain {pattern:?} as a substring \
                 — this is the documented floor that protects every new \
                 daemon from auto-ingesting credentials. A refactor that \
                 dropped or renamed this pattern would silently let \
                 matching files leak into the audit chain and memory \
                 store on first daemon start, before the operator has a \
                 chance to edit .covenantignore",
            );
        }

        assert!(
            body.starts_with('#'),
            "default_ignorefile must begin with a '#' comment line so \
             operators opening ~/.covenant/.covenantignore see a \
             documented file rather than raw glob patterns — the \
             leading comment block explains the gitignore-style \
             semantics ('!' un-ignores; '/' anchors; last matching \
             rule wins) that operators rely on when extending the \
             rule set",
        );

        assert!(
            body.contains("# .covenantignore"),
            "default_ignorefile must carry the documented header line \
             '# .covenantignore — ...' so the file is self-identifying \
             when opened in isolation (e.g., from a backup tarball or \
             a copied home directory); a refactor that dropped the \
             header would force operators to remember which daemon \
             owns the file",
        );

        assert!(
            body.contains("gitignore-style"),
            "default_ignorefile must surface the 'gitignore-style' \
             contract in the comment block so operators know the \
             match semantics (last rule wins, '!' un-ignore, '/' \
             anchor) WITHOUT consulting external docs — a refactor \
             that dropped this phrase would silently let operators \
             assume CSV or fnmatch semantics and introduce broken \
             un-ignore rules that mask credential leaks",
        );
    }
}
