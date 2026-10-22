//! Private, authenticated adapter to the loopback payment verifier. On Solana
//! the fully signed transaction is returned before broadcast and fsynced by the
//! gateway, and settlement retries those exact bytes. On Base the stored item is
//! the signed EIP-3009 authorization, whose nonce the token accepts only once.
use crate::{
    payment_payload, GatewayError, PaymentRail, PaymentTerms, Result, SettlementStatus,
    TokenDomain, VerifiedPayment,
};
use async_trait::async_trait;
use base64::Engine;
use covenant_compute_protocol::InferenceTariff;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

const MAX_READINESS_BYTES: usize = 65_536;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Readiness {
    schema_version: u32,
    observed_at_ms: u64,
    pricing_valid: bool,
    scheme: String,
    network: String,
    asset: String,
    asset_decimals: u8,
    pay_to: String,
    fee_payer: String,
    resource_url: String,
    legacy_price_atomic: String,
    retail_tariffs: Vec<InferenceTariff>,
    #[serde(default)]
    batch_enabled: bool,
}

impl Readiness {
    fn legacy_price(&self) -> Option<u64> {
        self.legacy_price_atomic.parse::<u64>().ok().filter(|n| {
            *n > 0 && *n <= MAX_SAFE_INTEGER && n.to_string() == self.legacy_price_atomic
        })
    }
    fn allows(&self, amount: u64, now: u64) -> bool {
        if Some(amount) == self.legacy_price() {
            return true;
        }
        self.retail_tariffs.iter().any(|tariff| {
            if !tariff.valid_at(now) || amount < tariff.base_micro_usdc {
                return false;
            }
            let variable = u128::from(amount - tariff.base_micro_usdc);
            let rate = u128::from(tariff.output_cap_micro_usdc_per_million);
            if rate == 0 {
                return variable == 0;
            }
            if variable == 0 {
                return false;
            }
            let first = (variable - 1) * 1_000_000 / rate + 1;
            let last = variable * 1_000_000 / rate;
            first <= last && first <= u128::from(tariff.max_output_tokens) && last >= 1
        })
    }
    fn allows_batch(&self, manifest: &crate::batch::BatchPricingManifest, now: u64) -> bool {
        self.batch_enabled
            && manifest.children.len() <= 2
            && manifest.total().is_ok()
            && manifest.children.iter().all(|child| {
                if child.model.trim().is_empty()
                    || child.model == "any"
                    || child.model.len() > 128
                    || child.model.chars().any(char::is_control)
                    || child.max_output_tokens > 32_768
                {
                    return false;
                }
                match child.pricing.as_deref() {
                    None => Some(child.amount_atomic) == self.legacy_price(),
                    Some("model_tariff_v1") => self.retail_tariffs.iter().any(|tariff| {
                        tariff.model == child.model
                            && tariff.valid_at(now)
                            && tariff.price(child.max_output_tokens).ok()
                                == Some(child.amount_atomic)
                    }),
                    Some(_) => false,
                }
            })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Solana,
    Base,
}

/// Base USDC on each supported chain, with the EIP-712 domain its contract
/// reports. The verifier holds the same table.
fn base_usdc(network: &str) -> Option<(&'static str, &'static str)> {
    match network {
        "eip155:8453" => Some(("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", "USD Coin")),
        "eip155:84532" => Some(("0x036CbD53842c5426634e7929541eC2318f3dCF7e", "USDC")),
        _ => None,
    }
}

/// Hex form only; the zero address is refused because USDC cannot pay it.
fn is_evm_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
        && value[2..].bytes().any(|b| b != b'0')
}

fn is_transaction_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Matches `evmAuthorizationId` in the verifier: one id per payer and nonce.
fn evm_authorization_id(from: &str, nonce: &str) -> String {
    Sha256::digest(format!(
        "eip3009:{}:{}",
        from.to_ascii_lowercase(),
        nonce.to_ascii_lowercase()
    ))
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect()
}

pub struct Bridge {
    http: reqwest::Client,
    url: String,
    secret: String,
    family: Family,
    network: String,
    asset: String,
    pay_to: String,
    fee_payer: String,
    resource_url: String,
}
impl Bridge {
    fn checked_client(url: &str, secret: &str, resource_url: &str) -> Result<reqwest::Client> {
        let parsed = reqwest::Url::parse(url)
            .map_err(|_| GatewayError::Invalid("invalid bridge URL".into()))?;
        if parsed.scheme() != "http"
            || parsed.host_str() != Some("127.0.0.1")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || secret.len() < 32
        {
            return Err(GatewayError::Invalid(
                "bridge must use authenticated loopback HTTP".into(),
            ));
        }
        let resource = reqwest::Url::parse(resource_url)
            .map_err(|_| GatewayError::Invalid("invalid resource URL".into()))?;
        if resource.scheme() != "https"
            || !resource.username().is_empty()
            || resource.password().is_some()
            || resource.path() != "/v1/chat/completions"
            || resource.query().is_some()
            || resource.fragment().is_some()
        {
            return Err(GatewayError::Invalid("invalid resource URL".into()));
        }
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| GatewayError::Unavailable(e.to_string()))
    }
    /// USDC on Base, paid with an EIP-3009 authorization. The facilitator pays
    /// the gas, so there is no fee payer.
    pub fn base(
        url: String,
        secret: String,
        network: String,
        asset: String,
        pay_to: String,
        resource_url: String,
    ) -> Result<Self> {
        let http = Self::checked_client(&url, &secret, &resource_url)?;
        if base_usdc(&network).map(|(usdc, _)| usdc) != Some(asset.as_str()) {
            return Err(GatewayError::Invalid(
                "rail asset configuration mismatch".into(),
            ));
        }
        if !is_evm_address(&pay_to) {
            return Err(GatewayError::Invalid("invalid payment address".into()));
        }
        Ok(Self {
            http,
            url: url.trim_end_matches('/').into(),
            secret,
            family: Family::Base,
            network,
            asset,
            pay_to,
            fee_payer: String::new(),
            resource_url,
        })
    }
    /// USDC on Solana, with Covenant's fee payer sponsoring the transaction.
    pub fn solana(
        url: String,
        secret: String,
        network: String,
        asset: String,
        pay_to: String,
        fee_payer: String,
        resource_url: String,
    ) -> Result<Self> {
        let http = Self::checked_client(&url, &secret, &resource_url)?;
        let asset_expected = match network.as_str() {
            "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp" => {
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
            }
            "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1" => {
                "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"
            }
            _ => return Err(GatewayError::Invalid("unsupported Solana network".into())),
        };
        if asset != asset_expected {
            return Err(GatewayError::Invalid(
                "rail asset configuration mismatch".into(),
            ));
        }
        for key in [&pay_to, &fee_payer] {
            if bs58::decode(key)
                .into_vec()
                .map_or(true, |bytes| bytes.len() != 32)
            {
                return Err(GatewayError::Invalid("invalid payment address".into()));
            }
        }
        Ok(Self {
            http,
            url: url.trim_end_matches('/').into(),
            secret,
            family: Family::Solana,
            network,
            asset,
            pay_to,
            fee_payer,
            resource_url,
        })
    }
    async fn readiness(&self, now: u64) -> Result<(Readiness, u64)> {
        let started = Instant::now();
        let unavailable =
            || GatewayError::Unavailable("payment bridge readiness unavailable".into());
        let mut response = self
            .http
            .get(format!("{}{}/readiness", self.url, self.prefix()))
            .bearer_auth(&self.secret)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|n| n > MAX_READINESS_BYTES as u64)
        {
            return Err(unavailable());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if chunk.len() > MAX_READINESS_BYTES.saturating_sub(body.len()) {
                return Err(unavailable());
            }
            body.extend_from_slice(&chunk);
        }
        let snapshot: Readiness = serde_json::from_slice(&body).map_err(|_| unavailable())?;
        let now = crate::after_wait(now, started);
        if snapshot.schema_version != 1
            || !snapshot.pricing_valid
            || snapshot.observed_at_ms.abs_diff(now) > 5_000
            || snapshot.scheme != "exact"
            || snapshot.asset_decimals != 6
            || snapshot.network != self.network
            || snapshot.asset != self.asset
            || snapshot.pay_to != self.pay_to
            || snapshot.fee_payer != self.fee_payer
            || snapshot.resource_url != self.resource_url
            || snapshot.legacy_price().is_none()
            || snapshot.retail_tariffs.len() > 32
        {
            return Err(unavailable());
        }
        let mut models = BTreeSet::new();
        for tariff in &snapshot.retail_tariffs {
            if tariff.validate().is_err()
                || tariff.max_output_tokens > 32_768
                || !models.insert(&tariff.model)
                || [
                    tariff.revision,
                    tariff.base_micro_usdc,
                    tariff.output_cap_micro_usdc_per_million,
                    tariff.valid_from_ms,
                    tariff.valid_until_ms,
                    tariff.price(tariff.max_output_tokens).unwrap_or(u64::MAX),
                ]
                .iter()
                .any(|n| *n > MAX_SAFE_INTEGER)
            {
                return Err(unavailable());
            }
        }
        Ok((snapshot, now))
    }
    fn prefix(&self) -> &'static str {
        match self.family {
            Family::Solana => "",
            Family::Base => "/evm",
        }
    }
    async fn call(&self, path: &str, body: Value) -> Result<Value> {
        let response = self
            .http
            .post(format!("{}{}{path}", self.url, self.prefix()))
            .bearer_auth(&self.secret)
            .json(&body)
            .send()
            .await
            .map_err(|_| GatewayError::Unavailable("payment bridge unavailable".into()))?;
        if response.status().is_client_error() {
            return Err(GatewayError::Payment(
                "authorization rejected by payment verifier".into(),
            ));
        }
        if !response.status().is_success() {
            return Err(GatewayError::Unavailable(
                "payment bridge unavailable".into(),
            ));
        }
        response
            .json()
            .await
            .map_err(|_| GatewayError::Unavailable("invalid payment bridge response".into()))
    }
}

#[async_trait]
impl PaymentRail for Bridge {
    fn network(&self) -> &str {
        &self.network
    }
    fn asset(&self) -> &str {
        &self.asset
    }
    fn pay_to(&self) -> &str {
        &self.pay_to
    }
    fn fee_payer(&self) -> &str {
        &self.fee_payer
    }
    fn resource_url(&self) -> &str {
        &self.resource_url
    }
    fn token_domain(&self) -> Option<TokenDomain> {
        let (_, name) = base_usdc(&self.network).filter(|_| self.family == Family::Base)?;
        Some(TokenDomain {
            name: name.into(),
            version: "2".into(),
        })
    }
    async fn check_offers(
        &self,
        offers: &[crate::discovery::DiscoveryOffer],
        now: u64,
    ) -> Result<()> {
        let (snapshot, now) = self.readiness(now).await?;
        for offer in offers {
            let matches = match &offer.pricing {
                crate::discovery::DiscoveryPricing::Legacy {
                    retail_micro_usdc, ..
                } => Some(*retail_micro_usdc) == snapshot.legacy_price(),
                crate::discovery::DiscoveryPricing::ModelTariff { retail, .. } => {
                    retail.valid_at(now) && snapshot.retail_tariffs.contains(retail)
                }
            };
            if !matches {
                return Err(GatewayError::Unavailable(
                    "payment bridge pricing differs from advertised offer".into(),
                ));
            }
        }
        Ok(())
    }
    async fn check_price(&self, amount_atomic: u64, now: u64) -> Result<()> {
        let (snapshot, now) = self.readiness(now).await?;
        if !snapshot.allows(amount_atomic, now) {
            return Err(GatewayError::Unavailable(
                "payment amount is not configured for admission".into(),
            ));
        }
        Ok(())
    }
    async fn check_batch_price(
        &self,
        manifest: &crate::batch::BatchPricingManifest,
        now: u64,
    ) -> Result<()> {
        if self.family != Family::Solana {
            return Err(GatewayError::Unavailable(
                "batches require the Solana payment rail".into(),
            ));
        }
        let (snapshot, now) = self.readiness(now).await?;
        if !snapshot.allows_batch(manifest, now) {
            return Err(GatewayError::Unavailable(
                "batch pricing is not configured for admission".into(),
            ));
        }
        Ok(())
    }
    fn authorization_id(&self, header: &str) -> Result<String> {
        let payload = payment_payload(header)?;
        if self.family == Family::Base {
            let authorization = &payload["payload"]["authorization"];
            let (Some(from), Some(nonce)) = (
                authorization["from"].as_str(),
                authorization["nonce"].as_str(),
            ) else {
                return Err(GatewayError::Payment("authorization missing".into()));
            };
            if !is_evm_address(from)
                || nonce.len() != 66
                || !nonce.starts_with("0x")
                || !nonce[2..].bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(GatewayError::Payment("authorization invalid".into()));
            }
            return Ok(evm_authorization_id(from, nonce));
        }
        let encoded = payload["payload"]["transaction"]
            .as_str()
            .ok_or_else(|| GatewayError::Payment("transaction missing".into()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| GatewayError::Payment("transaction encoding invalid".into()))?;
        if bytes.len() > 1232 || bytes.is_empty() {
            return Err(GatewayError::Payment("transaction size invalid".into()));
        }
        Ok(Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }
    async fn verify(&self, header: &str, terms: &PaymentTerms) -> Result<VerifiedPayment> {
        let payload = payment_payload(header)?;
        if payload["x402Version"] != 2
            || payload["accepted"] != terms.accepted()
            || payload["resource"]["url"] != terms.resource_url
        {
            return Err(GatewayError::Payment(
                "payment requirements or resource changed".into(),
            ));
        }
        let result = self.call("/verify", json!({"paymentPayload":payload,"paymentRequirements":terms.accepted(),"resourceUrl":terms.resource_url})).await?;
        let payment: VerifiedPayment = serde_json::from_value(result)?;
        if payment.authorization_id != self.authorization_id(header)?
            || payment.transaction_id.is_empty()
            || payment.encoded_transaction.is_empty()
            || payment.payer.is_empty()
        {
            return Err(GatewayError::Unavailable(
                "payment bridge binding mismatch".into(),
            ));
        }
        Ok(payment)
    }
    async fn settle(
        &self,
        payment: &VerifiedPayment,
        terms: &PaymentTerms,
    ) -> Result<SettlementStatus> {
        let value = self
            .call(
                "/settle",
                json!({"payment":payment,"paymentRequirements":terms.accepted()}),
            )
            .await?;
        Ok(match value["status"].as_str() {
            // Solana's signature is known before broadcast; Base's transaction
            // hash only once the facilitator has submitted the authorization.
            Some("finalized")
                if self.family == Family::Solana
                    && value["transaction_id"] == payment.transaction_id =>
            {
                SettlementStatus::Finalized {
                    transaction_id: payment.transaction_id.clone(),
                }
            }
            Some("finalized") if self.family == Family::Base => {
                match value["transaction_id"].as_str() {
                    Some(hash) if is_transaction_hash(hash) => SettlementStatus::Finalized {
                        transaction_id: hash.into(),
                    },
                    _ => SettlementStatus::Pending,
                }
            }
            Some("failed") => SettlementStatus::Failed,
            _ => SettlementStatus::Pending,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{DiscoveryOffer, DiscoveryPricing};
    use axum::{extract::State, http::HeaderMap, routing::get, Json, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::sync::Mutex;

    const SECRET: &str = "readiness-test-secret-at-least-32-bytes";
    const NETWORK: &str = "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1";
    const ASSET: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
    const ADDRESS: &str = "11111111111111111111111111111111";
    const RESOURCE: &str = "https://example.com/v1/chat/completions";

    fn tariff() -> InferenceTariff {
        InferenceTariff {
            schema_version: 1,
            id: "retail".into(),
            revision: 1,
            model: "haiku".into(),
            base_micro_usdc: 12_000,
            output_cap_micro_usdc_per_million: 9_000_000,
            max_output_tokens: 2048,
            valid_from_ms: 1000,
            valid_until_ms: 200_000,
        }
    }
    fn snapshot() -> Value {
        json!({"schemaVersion":1,"observedAtMs":1000,"pricingValid":true,"scheme":"exact",
            "network":NETWORK,"asset":ASSET,"assetDecimals":6,"payTo":ADDRESS,"feePayer":ADDRESS,
            "resourceUrl":RESOURCE,"legacyPriceAtomic":"50000","retailTariffs":[tariff()]})
    }
    fn bridge(url: String, resource: &str) -> Result<Bridge> {
        Bridge::solana(
            url,
            SECRET.into(),
            NETWORK.into(),
            ASSET.into(),
            ADDRESS.into(),
            ADDRESS.into(),
            resource.into(),
        )
    }
    struct Fixture {
        value: Mutex<Value>,
        reads: AtomicUsize,
    }
    async fn serve(State(state): State<Arc<Fixture>>, headers: HeaderMap) -> Json<Value> {
        assert_eq!(headers["authorization"], format!("Bearer {SECRET}"));
        state.reads.fetch_add(1, Ordering::SeqCst);
        Json(state.value.lock().await.clone())
    }
    async fn fixture() -> (Bridge, Arc<Fixture>, tokio::task::JoinHandle<()>) {
        let state = Arc::new(Fixture {
            value: Mutex::new(snapshot()),
            reads: AtomicUsize::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/readiness", get(serve))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (bridge(url, RESOURCE).unwrap(), state, task)
    }
    fn offers() -> Vec<DiscoveryOffer> {
        vec![
            DiscoveryOffer {
                model: "haiku".into(),
                max_output_tokens: 2048,
                max_input_bytes: 4096,
                pricing: DiscoveryPricing::Legacy {
                    retail_micro_usdc: 50000,
                    native_micro_usdc: 30000,
                },
            },
            DiscoveryOffer {
                model: "haiku".into(),
                max_output_tokens: 256,
                max_input_bytes: 4096,
                pricing: DiscoveryPricing::ModelTariff {
                    retail: tariff(),
                    native: tariff(),
                },
            },
        ]
    }

    #[tokio::test]
    async fn readiness_binds_complete_offers_and_exact_integer_cap_prices_with_get_only() {
        let (bridge, state, task) = fixture().await;
        bridge.check_offers(&offers(), 1000).await.unwrap();
        assert_eq!(
            state.reads.load(Ordering::SeqCst),
            1,
            "one policy read for all offers"
        );
        for amount in [14_304, 30_432, 50_000] {
            bridge.check_price(amount, 1000).await.unwrap();
        }
        for amount in [0, 12_000, 14_305, 30_441, 50_001] {
            assert!(bridge.check_price(amount, 1000).await.is_err());
        }
        let mut changed = snapshot();
        changed["retailTariffs"][0]["revision"] = json!(2);
        *state.value.lock().await = changed;
        assert!(
            bridge.check_offers(&offers(), 1000).await.is_err(),
            "same price with changed definition must not advertise"
        );
        bridge.check_price(14_304, 1000).await.unwrap();
        *state.value.lock().await = snapshot();
        let mut changed_offers = offers();
        if let DiscoveryPricing::ModelTariff { retail, .. } = &mut changed_offers[1].pricing {
            retail.max_output_tokens = 256;
        }
        assert!(
            bridge.check_offers(&changed_offers, 1000).await.is_err(),
            "admitted cap must not replace signed tariff cap"
        );
        task.abort();
    }

    #[tokio::test]
    async fn batch_readiness_requires_opt_in_and_each_exact_child_price() {
        use crate::batch::{BatchPrice, BatchPricingManifest};
        let (bridge, state, task) = fixture().await;
        let manifest = BatchPricingManifest {
            version: 1,
            batch_id: uuid::Uuid::new_v4(),
            children: [256, 2048]
                .map(|cap| BatchPrice {
                    job_id: uuid::Uuid::new_v4(),
                    model: "haiku".into(),
                    max_output_tokens: cap,
                    pricing: Some("model_tariff_v1".into()),
                    amount_atomic: tariff().price(cap).unwrap(),
                })
                .into(),
        };
        assert!(bridge.check_batch_price(&manifest, 1000).await.is_err());
        state.value.lock().await["batchEnabled"] = json!(true);
        bridge.check_batch_price(&manifest, 1000).await.unwrap();
        let mut changed = manifest.clone();
        changed.children[0].amount_atomic += 1;
        changed.children[1].amount_atomic -= 1;
        assert_eq!(changed.total().unwrap(), manifest.total().unwrap());
        assert!(bridge.check_batch_price(&changed, 1000).await.is_err());
        for field in ["model", "max_output_tokens", "job_id", "amount_atomic"] {
            let mut changed = serde_json::to_value(&manifest).unwrap();
            changed["children"][0][field] = match field {
                "model" => json!("other"),
                "max_output_tokens" => json!(257),
                "job_id" => changed["children"][1][field].clone(),
                _ => json!(0),
            };
            let changed = serde_json::from_value(changed).unwrap();
            assert!(
                bridge.check_batch_price(&changed, 1000).await.is_err(),
                "accepted {field}"
            );
        }
        let mut legacy = manifest.clone();
        legacy.children[0].pricing = None;
        legacy.children[0].amount_atomic = 50_000;
        bridge.check_batch_price(&legacy, 1000).await.unwrap();
        assert_eq!(legacy.total().unwrap(), 80_432);
        task.abort();
    }

    #[tokio::test]
    async fn readiness_refuses_identity_drift_malformed_stale_or_oversized_policy() {
        let (bridge, state, task) = fixture().await;
        for (field, value) in [
            ("network", json!("other")),
            ("asset", json!("other")),
            ("payTo", json!("other")),
            ("feePayer", json!("other")),
            (
                "resourceUrl",
                json!("https://other.example/v1/chat/completions"),
            ),
            ("scheme", json!("upto")),
            ("assetDecimals", json!(9)),
            ("schemaVersion", json!(2)),
            ("pricingValid", json!(false)),
            ("observedAtMs", json!(20_000)),
            ("legacyPriceAtomic", json!("050000")),
            ("legacyPriceAtomic", json!("9007199254740992")),
            ("retailTariffs", json!([tariff(), tariff()])),
            ("extra", json!("x".repeat(MAX_READINESS_BYTES))),
        ] {
            let mut changed = snapshot();
            changed[field] = value;
            *state.value.lock().await = changed;
            assert!(
                bridge.check_price(50_000, 1000).await.is_err(),
                "accepted changed {field}"
            );
        }
        *state.value.lock().await = snapshot();
        assert!(
            bridge.check_offers(&offers(), 10_000).await.is_err(),
            "stale bridge clock must not advertise"
        );
        let mut expired = snapshot();
        expired["observedAtMs"] = json!(200_000);
        *state.value.lock().await = expired;
        assert!(bridge.check_price(14_304, 200_000).await.is_err());
        bridge.check_price(50_000, 200_000).await.unwrap();
        task.abort();
    }

    #[test]
    fn readiness_price_grid_covers_zero_rates_rounding_and_rejects_resource_credentials() {
        for rate in [0, 1, 500_000, 1_000_001, 1_500_000, 9_000_000] {
            let mut value = snapshot();
            value["retailTariffs"][0]["base_micro_usdc"] = json!(100);
            value["retailTariffs"][0]["output_cap_micro_usdc_per_million"] = json!(rate);
            let policy: Readiness = serde_json::from_value(value).unwrap();
            let tariff = &policy.retail_tariffs[0];
            let prices: BTreeSet<_> = (1..=2048).map(|cap| tariff.price(cap).unwrap()).collect();
            for amount in 99..=tariff.price(2048).unwrap() + 1 {
                assert_eq!(
                    policy.allows(amount, 1000),
                    prices.contains(&amount),
                    "rate {rate}, amount {amount}"
                );
            }
        }
        assert!(bridge(
            "http://127.0.0.1:4043".into(),
            "https://user:secret@example.com/v1/chat/completions"
        )
        .is_err());
        assert!(bridge(
            "http://127.0.0.1:4043".into(),
            "https://user@example.com/v1/chat/completions"
        )
        .is_err());
    }

    const BASE: &str = "eip155:84532";
    const BASE_USDC: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
    const MERCHANT: &str = "0x5fA1d0C0bfFE257a20027C523093F941834f5D66";

    fn base(url: String) -> Result<Bridge> {
        Bridge::base(
            url,
            SECRET.into(),
            BASE.into(),
            BASE_USDC.into(),
            MERCHANT.into(),
            RESOURCE.into(),
        )
    }
    fn evm_header(from: &str, nonce: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(
            json!({"x402Version":2,"payload":{"authorization":{"from":from,"nonce":nonce}}})
                .to_string(),
        )
    }

    #[test]
    fn base_authorization_id_matches_the_verifier_vector() {
        let bridge = base("http://127.0.0.1:1".into()).unwrap();
        let nonce = format!("0x{}2", "0".repeat(63));
        // The verifier's evm.test.mjs asserts the same value.
        let expected = "4d7d82ffcc60a251090b98c1a1be4dc739b50eab2d384fa083eba828bb25ad8e";
        for from in [
            "0xAbCdEf0000000000000000000000000000000001",
            "0xABCDEF0000000000000000000000000000000001",
        ] {
            assert_eq!(
                bridge.authorization_id(&evm_header(from, &nonce)).unwrap(),
                expected
            );
        }
        for (from, nonce) in [
            ("0x01", nonce.as_str()),
            ("0xAbCdEf0000000000000000000000000000000001", "0x02"),
        ] {
            assert!(bridge.authorization_id(&evm_header(from, nonce)).is_err());
        }
    }

    #[test]
    fn base_rail_names_usdc_and_its_domain_and_refuses_other_configuration() {
        let rail = base("http://127.0.0.1:1".into()).unwrap();
        assert_eq!(rail.network(), BASE);
        assert_eq!(rail.fee_payer(), "");
        assert_eq!(
            rail.token_domain(),
            Some(TokenDomain {
                name: "USDC".into(),
                version: "2".into()
            })
        );
        let solana = bridge("http://127.0.0.1:1".into(), RESOURCE).unwrap();
        assert_eq!(solana.token_domain(), None);
        let other = |network: &str, asset: &str, pay_to: &str| {
            Bridge::base(
                "http://127.0.0.1:1".into(),
                SECRET.into(),
                network.into(),
                asset.into(),
                pay_to.into(),
                RESOURCE.into(),
            )
            .is_err()
        };
        assert!(other("eip155:1", BASE_USDC, MERCHANT));
        assert!(other(
            BASE,
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            MERCHANT
        ));
        assert!(other(BASE, BASE_USDC, ADDRESS));
        assert!(other(
            BASE,
            BASE_USDC,
            "0x0000000000000000000000000000000000000000"
        ));
    }

    #[tokio::test]
    async fn base_settlement_finalizes_only_on_a_lowercase_transaction_hash() {
        let reply = Arc::new(Mutex::new(json!({})));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let served = reply.clone();
        let app = Router::new().route(
            "/evm/settle",
            axum::routing::post(move |headers: HeaderMap| {
                let served = served.clone();
                async move {
                    assert_eq!(headers["authorization"], format!("Bearer {SECRET}"));
                    Json(served.lock().await.clone())
                }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let bridge = base(url).unwrap();
        let payment = VerifiedPayment {
            payer: "0xAbCdEf0000000000000000000000000000000001".into(),
            authorization_id: "a".repeat(64),
            transaction_id: "reference".into(),
            encoded_transaction: "{}".into(),
        };
        let terms = PaymentTerms {
            batch_pricing: None,
            quote_id: uuid::Uuid::nil(),
            request_sha256: "a".repeat(64),
            network: BASE.into(),
            asset: BASE_USDC.into(),
            pay_to: MERCHANT.into(),
            amount_atomic: 1200,
            asset_decimals: 6,
            expires_at_ms: 90_000,
            fee_payer: String::new(),
            resource_url: RESOURCE.into(),
            memo_binding: crate::MemoBinding::V3,
            token_domain: bridge.token_domain(),
        };
        let hash = format!("0x{}", "ab".repeat(32));
        *reply.lock().await = json!({"status":"finalized","transaction_id":hash});
        assert_eq!(
            bridge.settle(&payment, &terms).await.unwrap(),
            SettlementStatus::Finalized {
                transaction_id: hash.clone()
            }
        );
        for bad in [
            hash.to_uppercase().replace("0X", "0x"),
            "0x12".into(),
            "reference".into(),
        ] {
            *reply.lock().await = json!({"status":"finalized","transaction_id":bad});
            assert_eq!(
                bridge.settle(&payment, &terms).await.unwrap(),
                SettlementStatus::Pending
            );
        }
        *reply.lock().await = json!({"status":"failed"});
        assert_eq!(
            bridge.settle(&payment, &terms).await.unwrap(),
            SettlementStatus::Failed
        );
        task.abort();
    }
}
