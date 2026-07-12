//! Live integration test: a broken configured external MCP server is skipped at
//! bootstrap and the daemon stays available — serving its built-in tools and a
//! *healthy* external server's tools while the broken server contributes none.
//!
//! The daemon's external-MCP bootstrap loop (covenantd/src/main.rs) is fail-open
//! by design: each configured server is spawned and bootstrapped inside a match
//! whose error arms warn and continue, so one broken server cannot abort startup
//! or remove the others. This availability property — a misconfigured or
//! crashing MCP server degrades to "that server's tools are absent", not "the
//! daemon is down" — has no live coverage; every other MCP test configures only
//! healthy servers. A regression that propagated a bootstrap error instead of
//! skipping (turning a bad third-party server into a daemon-wide outage) would
//! pass every existing test.
//!
//! Two servers are registered with distinct prefixes: a healthy one
//! (`tool_prefix = "ok"`) and a broken one (`tool_prefix = "broken"`) launched
//! with `--exit-after-initialize`, which exits right after the initialize
//! handshake so the daemon's `tools/list` bootstrap hits a closed transport —
//! the `BootstrapError::Transport` arm that warns and skips (live_stdio.rs pins
//! `--exit-after-initialize` -> transport-closed bootstrap error). A socket that
//! never appears means the daemon aborted on the broken server, so the test
//! fails loudly there rather than swallowing it as a slow start.
//!
//! Hermetic — the fake servers read only their stdin and emit canned JSON-RPC.
//! `#[ignore]`'d and additionally skipped when the fixture binary is not built.
//! Run with:
//! `cargo test -p covenantd --test live_ipc_mcp_external_server_bootstrap_isolation -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &std::path::Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(sock: &std::path::Path, token_b58: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    let request = Request::Authenticate {
        token_b58: token_b58.to_string(),
    };
    match req(&mut stream, request).await {
        Response::Authenticated { .. } => stream,
        other => panic!("authentication failed: {other:?}"),
    }
}

/// Resolve the built `covenant-mcp-fake-server` binary. `CARGO_BIN_EXE_*` is
/// only exposed to tests in the binary's own crate, so a covenantd test must
/// find the workspace binary itself: it sits next to this test binary's
/// `target/<profile>/` directory (current_exe is `.../target/<profile>/deps/<name>`).
/// An explicit `COVENANT_LIVE_MCP_FAKE_SERVER` path overrides the search.
/// Returns `None` when the fixture is not built so a `-p covenantd`-only run
/// skips rather than fails; `cargo test --workspace` builds it because
/// covenant-mcp's own live_stdio test references it via CARGO_BIN_EXE.
fn fake_server_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COVENANT_LIVE_MCP_FAKE_SERVER") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let target_dir = exe.parent()?.parent()?; // deps -> target/<profile>
    let bin = target_dir.join("covenant-mcp-fake-server");
    bin.exists().then_some(bin)
}

/// TOML literal string ('...') so the resolved absolute path is written
/// verbatim with no escape processing. Workspace target paths contain no
/// single quotes; if one ever did, fail loudly rather than emit broken TOML.
fn toml_literal(path: &std::path::Path) -> String {
    let s = path.to_str().expect("fixture path is valid UTF-8");
    assert!(
        !s.contains('\''),
        "fixture path contains a single quote, cannot be a TOML literal string: {s}"
    );
    format!("'{s}'")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + healthy/broken covenant-mcp-fake-server subprocesses; opt-in via --ignored live_"]
async fn live_ipc_mcp_broken_external_server_is_skipped_and_daemon_stays_up() {
    let Some(fake_server) = fake_server_bin() else {
        eprintln!(
            "covenant-mcp-fake-server not built; skipping external-MCP bootstrap-isolation live \
             test (build it or set COVENANT_LIVE_MCP_FAKE_SERVER)"
        );
        return;
    };

    let home = tempfile::tempdir().expect("tempdir");

    // One healthy server and one that exits after the initialize handshake. The
    // broken server reaches the daemon's warn-and-skip bootstrap-failure arm
    // (its tools/list hits a closed transport). Distinct prefixes make
    // "mcp_ok_ping present, no mcp_broken_* present" unambiguous.
    let bin = toml_literal(&fake_server);
    let secrets = format!(
        "[embed]\n\
         provider = \"mock\"\n\
         \n\
         [[mcp.server]]\n\
         name = \"ok\"\n\
         command = {bin}\n\
         args = [\"--string-ids\"]\n\
         tool_prefix = \"ok\"\n\
         \n\
         [[mcp.server]]\n\
         name = \"broken\"\n\
         command = {bin}\n\
         args = [\"--exit-after-initialize\"]\n\
         tool_prefix = \"broken\"\n"
    );
    std::fs::write(home.path().join("secrets.toml"), secrets).expect("write secrets.toml");

    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    // The availability property: a broken external server must not abort
    // startup. A socket that never appears means the daemon aborted on the
    // broken server — surface it as an explicit failure, never a swallowed
    // timeout or a skip, because daemon-up-despite-broken-server is the point.
    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!(
            "daemon never created its socket at {} — a broken external MCP server must be skipped, \
             not abort startup",
            sock.display()
        );
    }

    let operator_token = read_operator_token(home.path()).await;
    let mut stream = authenticated_stream(&sock, &operator_token).await;

    match req(&mut stream, Request::ListTools).await {
        Response::ToolList { tools } => {
            let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
            assert!(
                names.contains(&"echo"),
                "the built-in echo tool must remain available after a broken server is skipped, got {names:?}"
            );
            assert!(
                names.contains(&"mcp_ok_ping"),
                "the healthy external server's tool must be registered alongside the skipped one, got {names:?}"
            );
            assert!(
                !names.iter().any(|n| n.starts_with("mcp_broken_")),
                "the broken external server must register no tools — no phantom or half-initialized tool, got {names:?}"
            );
        }
        other => panic!("ListTools must return a ToolList, got {other:?}"),
    }

    let _ = child.kill().await;
}
