use std::sync::Arc;

use crate::capability::{CircuitCapability, SpendLedger};
use crate::payer::{CircPayer, TokenTransfer};
use crate::{circ, CircuitError, Result};

/// The classic SPL Token program, for quotes that name it instead of Token-2022.
const CLASSIC_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// A parsed settlement quote — either Circuit's default `payment` block (CIRC) or a chosen
/// `acceptedTokens` entry (e.g. $CVNT).
#[derive(Debug, Clone)]
pub struct PaymentQuote {
    pub recipient: String,
    pub amount_raw: u64,
    pub amount_display: String,
    pub token: String,
    pub token_program: String,
    pub token_decimals: u8,
    pub network: Option<String>,
}

impl PaymentQuote {
    /// Select the quote to settle. When `pay_mint` names a mint listed under
    /// `acceptedTokens`, that entry is used (so we pay in $CVNT and Circuit auto-settles to
    /// CIRC); otherwise the default `payment` block is used. `None` if neither yields a
    /// recipient and a usable amount.
    pub fn select(body: &serde_json::Value, pay_mint: Option<&str>) -> Option<Self> {
        if let Some(mint) = pay_mint {
            if let Some(q) = Self::from_accepted(body, mint) {
                return Some(q);
            }
        }
        Self::parse(body)
    }

    /// Parse the default `payment` block; `None` if it lacks a recipient or a usable amount.
    /// `amountRaw` arrives as either a JSON number or a string, so both are accepted.
    pub fn parse(body: &serde_json::Value) -> Option<Self> {
        let pay = body.get("payment")?;
        let recipient = pay.get("recipient")?.as_str()?.to_string();
        let amount_raw = amount_field(pay.get("amountRaw")?)?;
        Some(Self {
            recipient,
            amount_raw,
            amount_display: pay
                .get("amountDisplay")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| format!("{amount_raw} CIRC")),
            token: pay
                .get("token")
                .and_then(|v| v.as_str())
                .unwrap_or(circ::MINT)
                .to_string(),
            token_program: token_program_id(pay.get("tokenProgram").and_then(|v| v.as_str())),
            token_decimals: pay
                .get("tokenDecimals")
                .and_then(|v| v.as_u64())
                .unwrap_or(circ::DECIMALS as u64) as u8,
            network: pay
                .get("network")
                .and_then(|v| v.as_str())
                .map(String::from),
        })
    }

    /// Build a quote from the `acceptedTokens` entry whose `mint` equals `pay_mint`.
    fn from_accepted(body: &serde_json::Value, pay_mint: &str) -> Option<Self> {
        let entry = body
            .get("acceptedTokens")?
            .as_array()?
            .iter()
            .find(|t| t.get("mint").and_then(|v| v.as_str()) == Some(pay_mint))?;
        let recipient = entry.get("recipient")?.as_str()?.to_string();
        let amount_raw = amount_field(entry.get("amountRaw")?)?;
        let decimals = entry
            .get("decimals")
            .and_then(|v| v.as_u64())
            .unwrap_or(circ::DECIMALS as u64) as u8;
        let symbol = entry
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or(pay_mint);
        Some(Self {
            recipient,
            amount_raw,
            amount_display: format!("{amount_raw} {symbol}"),
            token: pay_mint.to_string(),
            token_program: token_program_id(entry.get("tokenProgram").and_then(|v| v.as_str())),
            token_decimals: decimals,
            network: body
                .get("payment")
                .and_then(|p| p.get("network"))
                .and_then(|v| v.as_str())
                .map(String::from),
        })
    }
}

/// Normalize a 402 `tokenProgram` field to a program id. Circuit names the default block's
/// program by full id but the `acceptedTokens` entries by the short label `token2022`.
fn token_program_id(s: Option<&str>) -> String {
    match s {
        Some("token2022") | Some("token-2022") => circ::TOKEN_PROGRAM.to_string(),
        Some("spl-token") | Some("token") => CLASSIC_TOKEN_PROGRAM.to_string(),
        Some(id) => id.to_string(),
        None => circ::TOKEN_PROGRAM.to_string(),
    }
}

fn amount_field(v: &serde_json::Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// A single paid request: how to build it and where it goes.
pub struct PaidRequest {
    pub method: reqwest::Method,
    pub url: String,
    pub json_body: Option<serde_json::Value>,
    pub headers: Vec<(String, String)>,
}

impl PaidRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: reqwest::Method::GET,
            url: url.into(),
            json_body: None,
            headers: Vec::new(),
        }
    }

    pub fn post_json(url: impl Into<String>, body: serde_json::Value) -> Self {
        Self {
            method: reqwest::Method::POST,
            url: url.into(),
            json_body: Some(body),
            headers: Vec::new(),
        }
    }

    pub fn header(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

/// The JSON body of a paid call plus the settling signature (`None` when the endpoint
/// answered without a charge, e.g. a free data endpoint).
pub struct Paid {
    pub body: serde_json::Value,
    pub payment_tx: Option<String>,
    pub quote: Option<PaymentQuote>,
}

/// The explicit x402 pay-and-retry engine. Composed into [`crate::Inference`] and
/// [`crate::DataClient`] so one payer, capability, and process-local ledger bind every
/// call. This engine does not provide a durable prepayment reservation, crash-safe
/// idempotency, or daemon authorization; callers must not treat it as production daemon
/// enforcement.
#[derive(Clone)]
pub struct X402 {
    http: reqwest::Client,
    payer: Arc<dyn CircPayer>,
    cap: CircuitCapability,
    ledger: Arc<SpendLedger>,
    /// Mint to settle in. `None` = Circuit's default (CIRC); set to the $CVNT mint to pay
    /// from the 402's `acceptedTokens` list instead.
    pay_mint: Option<String>,
}

impl X402 {
    pub fn new(
        payer: Arc<dyn CircPayer>,
        cap: CircuitCapability,
        ledger: Arc<SpendLedger>,
    ) -> Self {
        Self::with_client_builder(reqwest::Client::builder(), payer, cap, ledger)
            .expect("static Circuit HTTP client configuration must be valid")
    }

    /// Construct with caller-selected HTTP settings while forcing redirects
    /// off. The x402 challenge and paid retry must stay on the capability-
    /// checked URL; following a redirect could escape the allowed origin or
    /// forward payment material to a different endpoint.
    pub fn with_client_builder(
        builder: reqwest::ClientBuilder,
        payer: Arc<dyn CircPayer>,
        cap: CircuitCapability,
        ledger: Arc<SpendLedger>,
    ) -> Result<Self> {
        let http = builder
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            payer,
            cap,
            ledger,
            pay_mint: None,
        })
    }

    /// Settle in the token with this mint when the 402 lists it under `acceptedTokens`.
    pub fn with_pay_mint(mut self, mint: impl Into<String>) -> Self {
        self.pay_mint = Some(mint.into());
        self
    }

    pub fn ledger(&self) -> &Arc<SpendLedger> {
        &self.ledger
    }

    pub fn capability(&self) -> &CircuitCapability {
        &self.cap
    }

    fn build(&self, req: &PaidRequest, pay_sig: Option<&str>) -> reqwest::RequestBuilder {
        let mut rb = self.http.request(req.method.clone(), &req.url);
        for (k, v) in &req.headers {
            rb = rb.header(k, v);
        }
        if let Some(sig) = pay_sig {
            rb = rb.header(circ::PAYMENT_HEADER, sig);
        }
        if let Some(body) = &req.json_body {
            rb = rb.json(body);
        }
        rb
    }

    /// Run one explicit request through the loop: hit the endpoint, and on a 402 enforce
    /// the library-local capability, settle the CIRC, and retry with the payment signature.
    pub async fn send(&self, req: PaidRequest) -> Result<Paid> {
        // Host allowlist is checked before the first byte leaves — the grant names the
        // hosts it may talk to at all, paid or free.
        let host = reqwest::Url::parse(&req.url)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .ok_or_else(|| CircuitError::BadUrl(req.url.clone()))?;
        self.cap.check_host(&host)?;

        let resp = self.build(&req, None).send().await?;
        if resp.status() != reqwest::StatusCode::PAYMENT_REQUIRED {
            return finish(resp, None, None).await;
        }

        // Read the 402 payment block.
        let text = resp.text().await?;
        let body: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| CircuitError::Decode(format!("{e}: 402 body={text}")))?;
        let quote = PaymentQuote::select(&body, self.pay_mint.as_deref())
            .ok_or(CircuitError::NoPaymentRequirements)?;

        // Capability gate. Every check runs before a lamport moves; the recipient and
        // amount came from the untrusted endpoint.
        self.cap.check_per_call(quote.amount_raw)?;
        self.cap.check_recipient(&quote.recipient)?;
        self.ledger
            .reserve(quote.amount_raw, self.cap.max_total_spend_raw)?;

        // Settle, releasing the budget reservation if the on-chain send fails.
        let transfer = TokenTransfer {
            mint: &quote.token,
            token_program: &quote.token_program,
            decimals: quote.token_decimals,
            recipient: &quote.recipient,
            amount_raw: quote.amount_raw,
        };
        let sig = match self.payer.pay(&transfer).await {
            Ok(sig) => sig,
            Err(e) => {
                self.ledger.refund(quote.amount_raw);
                return Err(e);
            }
        };

        // Send the paid request exactly once. A retry after the transfer needs
        // an endpoint-defined idempotency contract; this generic client has no
        // such contract and must not duplicate side effects speculatively.
        let resp = self.build(&req, Some(&sig)).send().await?;
        finish(resp, Some(sig), Some(quote)).await
    }
}

async fn finish(
    resp: reqwest::Response,
    payment_tx: Option<String>,
    quote: Option<PaymentQuote>,
) -> Result<Paid> {
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(CircuitError::UnexpectedStatus {
            status: status.as_u16(),
            body: text,
        });
    }
    let body = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&text)
            .map_err(|e| CircuitError::Decode(format!("{e}: body={text}")))?
    };
    Ok(Paid {
        body,
        payment_tx,
        quote,
    })
}
