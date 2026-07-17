//! Request types for Invoica's invoice API.
//!
//! Only the request body is typed. Invoice responses are returned to the agent
//! as raw JSON: Invoica's published `@invoica/sdk` and its live backend
//! disagree on the response shape (the SDK documents `number`/`customerId`,
//! the backend returns `invoiceNumber`/`customerEmail`), so pinning a struct
//! to either would break against the other. The request fields below track the
//! backend's `openapi.json` (`POST /v1/invoices`), which is the contract the
//! live API actually serves.

use serde::Serialize;

/// Body for `POST /v1/invoices`. `amount` is a major-unit fiat value (the
/// backend stores it via `parseFloat`, so `100.0` is 100.00, not cents).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInvoiceRequest {
    pub amount: f64,
    pub customer_email: String,
    pub customer_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buyer_country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buyer_state_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_camel_case_and_omits_empty_optionals() {
        let req = CreateInvoiceRequest {
            amount: 100.0,
            customer_email: "buyer@acme.test".into(),
            customer_name: "Acme Inc".into(),
            currency: Some("USD".into()),
            chain: Some("solana".into()),
            buyer_country_code: None,
            buyer_state_code: None,
            company_id: None,
        };
        let out = serde_json::to_value(&req).unwrap();
        assert_eq!(
            out,
            json!({
                "amount": 100.0,
                "customerEmail": "buyer@acme.test",
                "customerName": "Acme Inc",
                "currency": "USD",
                "chain": "solana"
            })
        );
    }
}
