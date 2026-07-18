use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::capability::{CircuitCapability, SpendLedger};
use crate::payer::CircPayer;
use crate::x402::{PaidRequest, X402};
use crate::{circ, CircuitError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatParams {
    pub messages: Vec<ChatMessage>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

impl ChatParams {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens: Option<u32>,
    #[serde(default)]
    pub total_tokens: Option<u32>,
}

#[derive(Debug)]
pub struct ChatResult {
    pub content: String,
    pub usage: Option<Usage>,
    /// The payment that settled this call, if it was charged.
    pub payment_tx: Option<String>,
    /// Raw base units this call cost, if it was charged.
    pub paid_raw: Option<u64>,
    /// The mint that settled the call (CIRC or $CVNT), if it was charged.
    pub token: Option<String>,
    pub raw: serde_json::Value,
}

/// Circuit inference: the decentralized 72B behind an OpenAI-compatible gateway, paid per
/// call in CIRC. A trusted co-located caller can set an internal key to skip payment.
pub struct Inference {
    x402: X402,
    base: String,
    model: String,
    internal_key: Option<String>,
}

impl Inference {
    pub fn new(
        http: reqwest::Client,
        payer: Arc<dyn CircPayer>,
        cap: CircuitCapability,
        ledger: Arc<SpendLedger>,
    ) -> Self {
        Self::from_x402(X402::new(http, payer, cap, ledger))
    }

    pub fn from_x402(x402: X402) -> Self {
        Self {
            x402,
            base: circ::INFERENCE_BASE.to_string(),
            model: circ::DEFAULT_MODEL.to_string(),
            internal_key: None,
        }
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base = base.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_internal_key(mut self, key: impl Into<String>) -> Self {
        self.internal_key = Some(key.into());
        self
    }

    /// One non-streaming chat completion, paying CIRC over x402 if the gateway charges.
    pub async fn chat(&self, params: ChatParams) -> Result<ChatResult> {
        let body = serde_json::json!({
            "model": params.model.as_deref().unwrap_or(&self.model),
            "messages": params.messages,
            "max_tokens": params.max_tokens.unwrap_or(512),
            "temperature": params.temperature.unwrap_or(0.7),
            "stream": false,
        });
        let mut req = PaidRequest::post_json(format!("{}/chat/completions", self.base), body);
        if let Some(key) = &self.internal_key {
            req = req.header(circ::INTERNAL_KEY_HEADER, key);
        }

        let paid = self.x402.send(req).await?;
        let content = paid
            .body
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CircuitError::Decode(format!("no choices[0].message.content in {}", paid.body))
            })?
            .to_string();
        let usage = paid
            .body
            .get("usage")
            .and_then(|u| serde_json::from_value(u.clone()).ok());

        Ok(ChatResult {
            content,
            usage,
            paid_raw: paid.quote.as_ref().map(|q| q.amount_raw),
            token: paid.quote.as_ref().map(|q| q.token.clone()),
            payment_tx: paid.payment_tx,
            raw: paid.body,
        })
    }
}
