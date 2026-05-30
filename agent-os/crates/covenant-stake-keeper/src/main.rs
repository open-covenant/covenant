use std::sync::Arc;

use anyhow::Result;
use covenant_stake_keeper::{Keeper, KeeperConfig};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "covenant_stake_keeper=info".into()),
        )
        .init();

    let cfg = KeeperConfig::from_env()?;
    let keeper = Arc::new(Keeper::from_config(cfg)?);

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("sweep") => keeper.sweep_once().await,
        Some("accrue") => keeper.accrue_once().await,
        Some("run") | None => keeper.run().await,
        Some(other) => {
            eprintln!(
                "unknown subcommand: {other}. usage: covenant-stake-keeper [run|sweep|accrue]"
            );
            std::process::exit(2);
        }
    }
}
