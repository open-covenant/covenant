//! Private, authenticated adapter to the pinned official x402 SVM verifier.
//! The fully signed transaction is returned before broadcast and fsynced by
//! the gateway. Settlement always retries those exact bytes and checks finality.
use crate::{
    payment_payload, GatewayError, PaymentRail, PaymentTerms, Result, SettlementStatus,
    VerifiedPayment,
};
use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub struct SvmBridge {
    http: reqwest::Client,
    url: String,
    secret: String,
    network: String,
    asset: String,
    pay_to: String,
    fee_payer: String,
    resource_url: String,
}
impl SvmBridge {
    pub fn new(
        url: String,
        secret: String,
        network: String,
        asset: String,
        pay_to: String,
        fee_payer: String,
        resource_url: String,
    ) -> Result<Self> {
        let parsed = reqwest::Url::parse(&url)
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
        let asset_expected = match network.as_str() {
            "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp" => {
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
            }
            "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1" => {
                "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"
            }
            _ => return Err(GatewayError::Invalid("unsupported Solana network".into())),
        };
        let resource = reqwest::Url::parse(&resource_url)
            .map_err(|_| GatewayError::Invalid("invalid resource URL".into()))?;
        if asset != asset_expected
            || resource.scheme() != "https"
            || resource.path() != "/v1/chat/completions"
            || resource.query().is_some()
            || resource.fragment().is_some()
        {
            return Err(GatewayError::Invalid(
                "rail asset/resource configuration mismatch".into(),
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
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| GatewayError::Unavailable(e.to_string()))?;
        Ok(Self {
            http,
            url: url.trim_end_matches('/').into(),
            secret,
            network,
            asset,
            pay_to,
            fee_payer,
            resource_url,
        })
    }
    async fn call(&self, path: &str, body: Value) -> Result<Value> {
        let response = self
            .http
            .post(format!("{}{path}", self.url))
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
impl PaymentRail for SvmBridge {
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
    fn authorization_id(&self, header: &str) -> Result<String> {
        let payload = payment_payload(header)?;
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
            Some("finalized") if value["transaction_id"] == payment.transaction_id => {
                SettlementStatus::Finalized {
                    transaction_id: payment.transaction_id.clone(),
                }
            }
            Some("failed") => SettlementStatus::Failed,
            _ => SettlementStatus::Pending,
        })
    }
}
