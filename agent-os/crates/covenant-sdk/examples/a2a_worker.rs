//! An a2a worker agent: drain delegated tasks, do the work, post each result.
//!
//! ```bash
//! cargo run -p covenant-sdk --example a2a_worker
//! ```
//!
//! Another agent (or the daemon's intent dispatch) leases tasks to this
//! worker's identity. The worker receives them, returns a result keyed by the
//! task id, and the daemon routes the result back to the sender. This is the
//! receiving half of a2a; sending a task needs the caller's own public key and
//! an `a2a.send` capability and is not wrapped by the SDK yet.

use covenant_sdk::{A2ATaskResult, Client, Content, SdkError};

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let mut client = Client::connect_default().await?;
    println!("worker {} draining its a2a mailbox", client.identity());

    // Drain everything queued right now. A long-running worker would instead
    // poll on an interval and keep the connection open between rounds.
    while let Some(task) = client.try_recv_a2a_task().await? {
        println!("task {}: {}", task.id, task.intent_text);

        // `intent_text` is untrusted input from the delegating agent. Treat it
        // as opaque data — never feed it to a shell, eval, or file path.
        let result = match handle(&task.intent_text) {
            Ok(answer) => A2ATaskResult::ok(task.id, vec![Content::text(answer)]),
            Err(reason) => A2ATaskResult::error(task.id, reason),
        };
        client.post_a2a_result(result).await?;
    }

    println!("mailbox empty");
    Ok(())
}

/// Stand-in for the worker's real work. Returns `Err` to report a task-level
/// failure back to the sender as an error result rather than crashing the loop.
fn handle(intent_text: &str) -> Result<String, String> {
    if intent_text.trim().is_empty() {
        return Err("empty task".into());
    }
    Ok(format!("processed: {intent_text}"))
}
