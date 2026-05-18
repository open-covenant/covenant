//! Fixture agent binary for budget hard-preempt live coverage. Reads
//! one Intent JSON line on stdin, sleeps long enough for the daemon's
//! budget projection-tick driver to flag the in-flight subprocess and
//! dispatch a SIGTERM via the process group, then emits a placeholder
//! result if the SIGTERM never arrived (no preempt → test failure).
//!
//! The fixture exists because no production agent currently sleeps long
//! enough for the projection-tick driver (250ms cadence default) to
//! observe a budget-exhausted intent before the agent finishes its
//! normal work. Three seconds of std::thread::sleep covers ~12 ticks at
//! the default cadence and stays well under the manifest's
//! `cpu_ms_per_task` (10s in the live test), so the runner-side
//! timeout never races the preempt path.

use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    thread::sleep(Duration::from_secs(3));
    let result = r#"{"text":"preempt fixture finished without being preempted","sources":[]}"#;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(result.as_bytes())?;
    stdout.write_all(b"\n")?;
    Ok(())
}
