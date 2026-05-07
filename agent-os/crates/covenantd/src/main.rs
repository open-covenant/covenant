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
    let runner = Arc::new(covenant_runtime::SubprocessRunner);

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
        match covenant_mcp::transport::StdioMcpClient::spawn(&srv.command, &srv.args).await {
            Ok(client) => {
                let client_dyn: Arc<dyn covenant_mcp::transport::McpClient> = client;
                match covenant_mcp::external::bootstrap_remote_tools(client_dyn).await {
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
    );

    // HTTP gateway for browser UIs.
    let http_port: u16 = std::env::var("COVENANT_HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8421);
    let http_addr = std::net::SocketAddr::from(([127, 0, 0, 1], http_port));
    let http_state = covenantd::http::HttpState {
        server: server.clone(),
    };
    let http_router = covenantd::http::router(http_state);
    let http_listener = tokio::net::TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("bind http {}", http_addr))?;
    info!(addr = %http_addr, "http gateway listening");
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
        }
        res = server.serve(listener) => {
            res?;
        }
    }

    http_handle.abort();
    if sock_path.exists() {
        let _ = std::fs::remove_file(&sock_path);
    }
    Ok(())
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
