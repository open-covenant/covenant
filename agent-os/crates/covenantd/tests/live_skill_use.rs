//! Live integration test: end-to-end governed skill run over the raw IPC
//! socket. Spawns covenantd against a tempdir HOME, installs a skill, and
//! drives `Request::SkillUse` to prove the governed audit chain — the use is
//! capability-gated (`skill.use.{name}`), the on-disk content digest is
//! re-verified against the install pin, and the chain records
//! `SkillContextInjected` -> `SkillInvoked` so a verifier can recompute which
//! instructions the agent ran under for the intent.
//!
//! Two paths are exercised against one daemon:
//!   1. Ungranted use is refused: `SkillUse` without `skill.use.covenant`
//!      returns an error, records `SkillRefused`, and emits neither
//!      `SkillContextInjected` nor `SkillInvoked` — the gate is in the run
//!      path, not bypassed.
//!   2. Granted use runs: with `skill.use.covenant` + `memory.write` granted,
//!      `SkillUse` returns `IntentResult`, the run output carries the injected
//!      `SKILL.md` body (the agent genuinely ran under it), and the chain
//!      carries `SkillContextInjected` (digest matching the install row, no
//!      references disclosed) strictly before `SkillInvoked` (joined to the
//!      dispatched intent_id).
//!
//! Hermetic and deterministic — a fresh tempdir HOME matches no agent card, so
//! dispatch takes the phase-0 echo path (no model, no subprocess, no network).
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_skill_use -- --ignored live_`.

use covenant_audit::AuditKind;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

const SKILL_MD: &str = "---\nname: covenant\ndescription: verifiable agent execution on Solana\nmetadata:\n  version: 0.1.0\n---\n\n# covenant\n\nPOLICY: never sign a transaction without explicit operator approval.\n";
const BODY_MARKER: &str = "never sign a transaction without explicit operator approval";

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &Path) -> String {
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

async fn spawn_daemon(home: &Path) -> Child {
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");
    if !wait_for_sock(&home.join("sock")).await {
        panic!("daemon never created its socket");
    }
    child
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(home: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(home.join("sock"))
        .await
        .expect("connect socket");
    let token = read_operator_token(home).await;
    match req(&mut stream, Request::Authenticate { token_b58: token }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
    stream
}

async fn recent_audit(stream: &mut UnixStream) -> Vec<covenant_audit::AuditEvent> {
    match req(
        stream,
        Request::RecentAudit {
            limit: 256,
            since_ms: None,
            prefer_stream: None,
        },
    )
    .await
    {
        Response::AuditEvents { events } => events,
        other => panic!("expected Response::AuditEvents, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives a governed Request::SkillUse run over the socket"]
async fn live_skill_use_governed_run_records_inject_then_invoke() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // Install the skill the run will be governed by. Operator-only — the
    // authenticated stream is the operator.
    let skill_src = tempfile::tempdir().expect("skill tempdir");
    std::fs::write(skill_src.path().join("SKILL.md"), SKILL_MD).unwrap();
    std::fs::create_dir(skill_src.path().join("references")).unwrap();
    std::fs::write(
        skill_src.path().join("references/signing.md"),
        "signing policy detail\n",
    )
    .unwrap();
    let installed_digest = match req(
        &mut stream,
        Request::SkillAdd {
            dir: skill_src.path().to_string_lossy().to_string(),
            url: "https://github.com/open-covenant/covenant-skill/tree/v0.1.0/skill".into(),
            tag: "v0.1.0".into(),
            commit: "0".repeat(40),
        },
    )
    .await
    {
        Response::Skill { skill } => {
            assert_eq!(skill.name, "covenant");
            assert!(
                !skill.references.is_empty(),
                "fixture declares a reference so the no-reference-disclosure assertion is meaningful"
            );
            skill.digest
        }
        other => panic!("expected Response::Skill from install, got {other:?}"),
    };
    let digest_hex = installed_digest
        .strip_prefix("sha256:")
        .expect("install digest is sha256:-prefixed")
        .to_string();

    // Path 1: ungranted use is refused in the run path.
    match req(
        &mut stream,
        Request::SkillUse {
            name: "covenant".into(),
            text: "summarise the audit log".into(),
        },
    )
    .await
    {
        Response::Error { message } => assert!(
            message.contains("skill.use.covenant"),
            "ungranted skill use must name the missing capability: {message}"
        ),
        other => panic!("ungranted SkillUse must be refused, got {other:?}"),
    }
    let after_deny = recent_audit(&mut stream).await;
    assert!(
        after_deny.iter().any(|e| matches!(
            &e.kind,
            AuditKind::SkillRefused { skill_name, .. } if skill_name == "covenant"
        )),
        "a refused skill use must record SkillRefused: {after_deny:?}"
    );
    assert!(
        !after_deny
            .iter()
            .any(|e| matches!(e.kind, AuditKind::SkillContextInjected { .. }))
            && !after_deny
                .iter()
                .any(|e| matches!(e.kind, AuditKind::SkillInvoked { .. })),
        "a refused skill use must inject nothing and invoke nothing: {after_deny:?}"
    );

    // Grant the gate capability plus memory.write (the run path's write gate).
    for action in ["skill.use.covenant", "memory.write"] {
        match req(
            &mut stream,
            Request::GrantCapability {
                action: action.into(),
                scope: None,
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted { .. } => {}
            other => panic!("granting {action} failed: {other:?}"),
        }
    }

    // Path 2: granted use runs under the skill.
    let intent_id = match req(
        &mut stream,
        Request::SkillUse {
            name: "covenant".into(),
            text: "summarise the audit log".into(),
        },
    )
    .await
    {
        Response::IntentResult {
            intent_id,
            status,
            text,
            ..
        } => {
            assert_eq!(status, "ok", "granted governed run must succeed");
            assert!(
                text.contains(BODY_MARKER),
                "the run output must carry the injected SKILL.md body — proof the agent ran under the skill, not beside it: {text}"
            );
            intent_id
        }
        other => panic!("granted SkillUse must return IntentResult, got {other:?}"),
    };

    let events = recent_audit(&mut stream).await;
    let injected_at = events.iter().position(|e| {
        matches!(
            &e.kind,
            AuditKind::SkillContextInjected {
                skill_name,
                skill_digest_hex,
                references,
            } if skill_name == "covenant"
                && *skill_digest_hex == digest_hex
                && references.is_empty()
        )
    });
    let invoked_at = events.iter().position(|e| {
        matches!(
            &e.kind,
            AuditKind::SkillInvoked { skill_name, intent_id: id }
                if skill_name == "covenant" && *id == intent_id
        )
    });
    let dispatched = events.iter().any(|e| {
        matches!(
            &e.kind,
            AuditKind::IntentDispatched { intent_id: id, .. } if *id == intent_id
        )
    });

    let injected_at = injected_at.unwrap_or_else(|| {
        panic!("missing SkillContextInjected row (digest {digest_hex}, no references): {events:?}")
    });
    let invoked_at = invoked_at
        .unwrap_or_else(|| panic!("missing SkillInvoked row for intent {intent_id}: {events:?}"));
    assert!(
        dispatched,
        "the governed run must record an IntentDispatched row for {intent_id}: {events:?}"
    );
    assert!(
        injected_at < invoked_at,
        "SkillContextInjected must precede SkillInvoked in the chain (got {injected_at} >= {invoked_at}): {events:?}"
    );

    // Path 3: a post-install content swap is refused in the run path and
    // recorded, even though the capability is now granted — the digest pin
    // holds at use-time, not just at install.
    let invoked_before = events
        .iter()
        .filter(|e| matches!(e.kind, AuditKind::SkillInvoked { .. }))
        .count();
    std::fs::write(
        home.path().join("skills/covenant/SKILL.md"),
        "---\nname: covenant\ndescription: swapped\n---\n\nmalicious body\n",
    )
    .unwrap();
    match req(
        &mut stream,
        Request::SkillUse {
            name: "covenant".into(),
            text: "summarise the audit log".into(),
        },
    )
    .await
    {
        Response::Error { message } => assert!(
            message.contains("digest mismatch"),
            "a swapped skill must be refused with a digest-mismatch error: {message}"
        ),
        other => panic!("a swapped skill must be refused at use-time, got {other:?}"),
    }
    let after_swap = recent_audit(&mut stream).await;
    assert!(
        after_swap.iter().any(|e| matches!(
            &e.kind,
            AuditKind::SkillRefused { skill_name, reason }
                if skill_name == "covenant" && reason.contains("content digest mismatch")
        )),
        "the run-path digest mismatch must record SkillRefused: {after_swap:?}"
    );
    assert_eq!(
        after_swap
            .iter()
            .filter(|e| matches!(e.kind, AuditKind::SkillInvoked { .. }))
            .count(),
        invoked_before,
        "a refused swapped skill must not add a SkillInvoked row: {after_swap:?}"
    );

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
