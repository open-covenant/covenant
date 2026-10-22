//! Explicitly configured, standalone inbound gateway. Running this executable
//! without all reserve/budget/rail settings fails closed; no default network.
use covenant_compute_buyer::{durable_budget::DurableBudget, BuyerConfig};
use covenant_compute_gateway::{bridge::Bridge, buyer::BuyerExecutor, Gateway};
use covenant_identity::LocalIdentity;
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

fn tariff_configuration_latch(directory: &Path) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(directory.join("admissions.disabled")) {
        Ok(mut file) => {
            file.write_all(b"Invalid model tariff configuration; review before new admissions\n")?;
            file.sync_all()?;
            std::fs::File::open(directory)?.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

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
    let bridge_url = required("COVENANT_GATEWAY_BRIDGE_URL")?;
    let bridge_secret = required("COVENANT_GATEWAY_BRIDGE_SECRET")?;
    let resource_url = required("COVENANT_GATEWAY_RESOURCE_URL")?;
    let rail = Arc::new(Bridge::solana(
        bridge_url.clone(),
        bridge_secret.clone(),
        required("COVENANT_GATEWAY_NETWORK")?,
        required("COVENANT_GATEWAY_ASSET")?,
        required("COVENANT_GATEWAY_PAY_TO")?,
        required("COVENANT_GATEWAY_FEE_PAYER")?,
        resource_url.clone(),
    )?);
    // USDC on Base is offered beside Solana once a Base merchant is set. The
    // verifier must be configured for the same network and merchant.
    let base = match std::env::var("COVENANT_GATEWAY_BASE_PAY_TO") {
        Ok(pay_to) if !pay_to.is_empty() => Some(Arc::new(Bridge::base(
            bridge_url,
            bridge_secret,
            required("COVENANT_GATEWAY_BASE_NETWORK")?,
            required("COVENANT_GATEWAY_BASE_ASSET")?,
            pay_to,
            resource_url,
        )?)),
        Ok(_) | Err(std::env::VarError::NotPresent) => None,
        Err(_) => anyhow::bail!("COVENANT_GATEWAY_BASE_PAY_TO must be valid Unicode"),
    };
    let model = required("COVENANT_GATEWAY_MODEL")?;
    let max_tokens: u32 = required("COVENANT_GATEWAY_MAX_TOKENS")?.parse()?;
    let tariff_configuration = std::env::var("COVENANT_GATEWAY_TARIFFS_FILE")
        .ok()
        .map(|path| {
            let path = PathBuf::from(path);
            anyhow::ensure!(path.is_absolute(), "tariffs file must be absolute");
            let book = covenant_compute_gateway::pricing::TariffBook::load(&path)?;
            book.validate_single_provider(&model, max_tokens)?;
            Ok::<_, anyhow::Error>(Arc::new(book))
        })
        .transpose();
    let (tariffs, tariff_configuration_ok) = match tariff_configuration {
        Ok(tariffs) => (tariffs, true),
        Err(_) => {
            // Optional new pricing cannot strand already-admitted payments.
            // The durable latch also prevents the supervisor's paid node probe.
            tariff_configuration_latch(&directory)?;
            eprintln!("Invalid model tariff configuration; payment recovery only");
            (None, false)
        }
    };
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
        model,
        offer_micro_usdc: offer,
        max_tokens,
        max_input_bytes,
        approved_operators,
        tariffs,
    });
    let namespace = format!(
        "{}\0{coordinator}\0{}\0{}\0{offer}\0{max_input_bytes}\0{operator_namespace}",
        identity.agent_id().pubkey_base58(),
        executor.model,
        executor.max_tokens
    );
    let executor = Arc::new(covenant_compute_gateway::batch::BatchExecutor {
        inner: executor,
        directory: directory.join("batches"),
        enabled: std::env::var("COVENANT_GATEWAY_BATCH_ENABLED").as_deref() == Ok("true"),
    });
    let mut gateway = Gateway::open_with_reserve_increase(
        &directory,
        &namespace,
        reserve,
        price,
        rail,
        executor,
        std::env::var("COVENANT_GATEWAY_ALLOW_RESERVE_INCREASE").as_deref() == Ok("true"),
    )?
    .with_admissions_enabled(
        tariff_configuration_ok
            && std::env::var("COVENANT_GATEWAY_ADMISSIONS_ENABLED").as_deref() != Ok("false"),
    )
    .with_settlement_wait(covenant_compute_gateway::SETTLEMENT_WAIT);
    if let Some(base) = base {
        gateway = gateway.with_alternate_rail(base)?;
    }
    gateway
        .check_open_payments(covenant_compute_gateway::now_ms())
        .await?;
    let gateway = Arc::new(gateway);
    let _retention = covenant_compute_gateway::spawn_retention(gateway.clone());
    let address =
        std::env::var("COVENANT_GATEWAY_LISTEN").unwrap_or_else(|_| "127.0.0.1:4042".into());
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, covenant_compute_gateway::router(gateway)).await?;
    Ok(())
}
