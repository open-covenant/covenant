//! Explicitly configured, standalone inbound gateway. Running this executable
//! without all reserve/budget/rail settings fails closed; no default network.
use covenant_compute_buyer::{durable_budget::DurableBudget, BuyerConfig};
use covenant_compute_gateway::{bridge::SvmBridge, buyer::BuyerExecutor, Gateway};
use covenant_identity::LocalIdentity;
use std::{path::PathBuf, sync::Arc, time::Duration};

fn required(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name).map_err(|_| anyhow::anyhow!("{name} must be configured"))?;
    anyhow::ensure!(!value.is_empty(), "{name} must not be empty");
    Ok(value)
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        required("COVENANT_GATEWAY_ENABLED")? == "true",
        "gateway is disabled"
    );
    let directory = PathBuf::from(required("COVENANT_GATEWAY_HOME")?);
    anyhow::ensure!(directory.is_absolute(), "gateway home must be absolute");
    let identity = Arc::new(LocalIdentity::load_or_create(
        &directory.join("identity.key"),
        "compute-gateway@local",
    )?);
    let coordinator = required("COVENANT_COMPUTE_COORDINATOR_URL")?;
    let budget = DurableBudget::from_env(&identity.agent_id().pubkey_base58(), &coordinator)?
        .ok_or_else(|| {
            anyhow::anyhow!("COVENANT_COMPUTE_DAILY_BUDGET_MICRO_USDC must be configured")
        })?;
    let reserve: u64 = required("COVENANT_GATEWAY_RESERVE_MICRO_USDC")?.parse()?;
    let price: u64 = required("COVENANT_GATEWAY_PRICE_MICRO_USDC")?.parse()?;
    let offer: u64 = required("COVENANT_GATEWAY_OFFER_MICRO_USDC")?.parse()?;
    anyhow::ensure!(
        offer > 0 && price >= offer && reserve >= offer,
        "price/reserve must cover the compute offer"
    );
    let max_input_bytes: usize = required("COVENANT_GATEWAY_MAX_INPUT_BYTES")?.parse()?;
    anyhow::ensure!(
        (1..=65_536).contains(&max_input_bytes),
        "maximum input bytes must be between 1 and 65536"
    );
    let approved_operators = required("COVENANT_GATEWAY_APPROVED_OPERATOR_PUBKEYS")?
        .split(',')
        .map(|key| key.trim().to_owned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !approved_operators.is_empty()
            && approved_operators.len() <= 32
            && approved_operators.iter().all(|key| {
                bs58::decode(key)
                    .into_vec()
                    .is_ok_and(|bytes| bytes.len() == 32)
            }),
        "approved operators must be one to 32 comma-separated public keys"
    );
    let operator_namespace = approved_operators.join(",");
    let rail = Arc::new(SvmBridge::new(
        required("COVENANT_GATEWAY_BRIDGE_URL")?,
        required("COVENANT_GATEWAY_BRIDGE_SECRET")?,
        required("COVENANT_GATEWAY_NETWORK")?,
        required("COVENANT_GATEWAY_ASSET")?,
        required("COVENANT_GATEWAY_PAY_TO")?,
        required("COVENANT_GATEWAY_FEE_PAYER")?,
        required("COVENANT_GATEWAY_RESOURCE_URL")?,
    )?);
    let executor = Arc::new(BuyerExecutor {
        config: BuyerConfig {
            coordinator_url: coordinator.clone(),
            poll_interval: Duration::from_millis(500),
            referral_code: None,
            rpc_url: None,
        },
        identity: identity.clone(),
        http: covenant_compute_buyer::http_client(),
        budget,
        model: required("COVENANT_GATEWAY_MODEL")?,
        offer_micro_usdc: offer,
        max_tokens: required("COVENANT_GATEWAY_MAX_TOKENS")?.parse()?,
        max_input_bytes,
        approved_operators,
    });
    let namespace = format!(
        "{}\0{coordinator}\0{}\0{}\0{offer}\0{max_input_bytes}\0{operator_namespace}",
        identity.agent_id().pubkey_base58(),
        executor.model,
        executor.max_tokens
    );
    let gateway = Arc::new(
        Gateway::open_with_reserve_increase(
            &directory,
            &namespace,
            reserve,
            price,
            rail,
            executor,
            std::env::var("COVENANT_GATEWAY_ALLOW_RESERVE_INCREASE").as_deref() == Ok("true"),
        )?
        .with_admissions_enabled(
            std::env::var("COVENANT_GATEWAY_ADMISSIONS_ENABLED").as_deref() != Ok("false"),
        ),
    );
    let _retention = covenant_compute_gateway::spawn_retention(gateway.clone());
    let address =
        std::env::var("COVENANT_GATEWAY_LISTEN").unwrap_or_else(|_| "127.0.0.1:4042".into());
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, covenant_compute_gateway::router(gateway)).await?;
    Ok(())
}
