//! OpenAPI discovery for clients that sample a request before obtaining a real quote.
//! All availability and price bounds come from the same read-only live catalog.
use super::{DiscoveryOffer, DiscoveryPricing};
use crate::{Gateway, GatewayError, Result};
use axum::{extract::State, response::Response};
use serde_json::{json, Value};
use std::sync::Arc;

fn usd_amount(micro_usdc: u64) -> String {
    // Avoid floating point rounding, including at the full u64 boundary.
    format!("{}.{:06}", micro_usdc / 1_000_000, micro_usdc % 1_000_000)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn price_metadata(offers: &[DiscoveryOffer]) -> Result<Value> {
    let mut minimum = u64::MAX;
    let mut maximum = 0;
    for offer in offers {
        let (low, high) = match &offer.pricing {
            DiscoveryPricing::Legacy {
                retail_micro_usdc, ..
            } => (*retail_micro_usdc, *retail_micro_usdc),
            DiscoveryPricing::ModelTariff { retail, .. } => (
                retail
                    .price(1)
                    .map_err(|error| GatewayError::Invalid(error.to_string()))?,
                retail
                    .price(offer.max_output_tokens)
                    .map_err(|error| GatewayError::Invalid(error.to_string()))?,
            ),
        };
        minimum = minimum.min(low);
        maximum = maximum.max(high);
    }
    Ok(if minimum == maximum {
        json!({"mode":"fixed", "currency":"USD", "amount":usd_amount(minimum)})
    } else {
        json!({"mode":"dynamic", "currency":"USD", "min":usd_amount(minimum), "max":usd_amount(maximum)})
    })
}

fn request_schema(offers: &[DiscoveryOffer]) -> Result<(Value, Value)> {
    // The scanner samples only top-level properties and the first branch's
    // required fields. Keep those properties usable while retaining exact
    // per-model, per-pricing-mode constraints in the branches.
    let mut ordered = offers.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|offer| matches!(offer.pricing, DiscoveryPricing::Legacy { .. }));
    let preferred = ordered[0];
    let cap = preferred.max_output_tokens.min(256);
    let mut messages = json!([{"role":"user", "content":"Reply with a short greeting."}]);
    if serde_json::to_vec(&messages)?.len() > preferred.max_input_bytes {
        messages = json!([{"role":"user", "content":"x"}]);
    }
    if serde_json::to_vec(&messages)?.len() > preferred.max_input_bytes {
        return Err(GatewayError::Unavailable(
            "input limit cannot accommodate a discovery sample".into(),
        ));
    }
    let mut example = json!({"model":preferred.model, "messages":messages, "max_tokens":cap});
    if matches!(preferred.pricing, DiscoveryPricing::ModelTariff { .. }) {
        example["pricing"] = json!("model_tariff_v1");
    }
    let mut schema = preferred.input_schema();
    schema["required"] = json!(["model", "messages", "max_tokens"]);
    let models = offers
        .iter()
        .map(|offer| offer.model.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    schema["properties"]["model"] = if models.len() == 1 {
        json!({"type":"string", "const":preferred.model})
    } else {
        json!({"type":"string", "enum":models, "example":preferred.model})
    };
    schema["properties"]["messages"]["example"] = messages;
    schema["properties"]["max_tokens"]["maximum"] = json!(offers
        .iter()
        .map(|offer| offer.max_output_tokens)
        .max()
        .unwrap());
    schema["properties"]["max_tokens"]["example"] = json!(cap);
    schema["anyOf"] = json!(ordered
        .iter()
        .map(|offer| offer.input_schema())
        .collect::<Vec<_>>());
    schema["example"] = example.clone();
    Ok((schema, example))
}

pub(super) fn document(
    resource_url: &str,
    observed: u64,
    offers: &[DiscoveryOffer],
    with_base: bool,
) -> Result<Value> {
    let resource = reqwest::Url::parse(resource_url)
        .map_err(|_| GatewayError::Invalid("invalid discovery resource URL".into()))?;
    if resource.scheme() != "https"
        || resource.host_str().is_none()
        || !resource.username().is_empty()
        || resource.password().is_some()
        || resource.path() != "/v1/chat/completions"
        || resource.query().is_some()
        || resource.fragment().is_some()
    {
        return Err(GatewayError::Invalid(
            "invalid discovery resource URL".into(),
        ));
    }
    let paid_with = if with_base {
        "exact USDC on Solana or Base"
    } else {
        "exact Solana USDC"
    };
    let mut document = json!({
        "openapi":"3.1.0",
        "info":{
            "title":"Covenant Compute", "version":"1.0.0",
            "description":format!("Text inference paid with {paid_with} through x402 v2. Any x402 v2 client can pay; Covenant's client SDK also verifies the signed receipt. Discovery is read-only and reports current availability; it does not reserve capacity or execute inference.")
        },
        "externalDocs":{"description":"Covenant x402 client SDK, which verifies receipts","url":"https://github.com/open-covenant/covenant/tree/deploy/compute-coordinator/sdk/compute-x402-client"},
        "servers":[{"url":resource.origin().ascii_serialization()}],
        "security":[],
        "paths":{},
        "x-covenant-observed-at-ms":observed,
        "x-covenant-available":!offers.is_empty()
    });
    if offers.is_empty() {
        return Ok(document);
    }
    let (schema, example) = request_schema(offers)?;
    document["paths"]["/v1/chat/completions"] = json!({"post":{
        "operationId":"createChatCompletion",
        "summary":"Covenant Compute text inference",
        "description":"Submit a valid JSON request without Payment-Signature to obtain a fresh HTTP 402 quote. The runtime quote is authoritative and binds the exact request body, price, expiry, and payment destination. Submit the identical body with the quoted x402 payment authorization to execute. No API key or login is required. Prices are fixed upfront for the request; model_tariff_v1 prices depend on the requested output cap, not actual token use. Legacy requests omit pricing. Availability can change after discovery.",
        "security":[],
        "x-payment-info":{"protocols":["x402"],"price":price_metadata(offers)?},
        "parameters":[
            {"name":"Payment-Signature","in":"header","required":false,"description":"Base64 x402 v2 payment authorization for the exact quote and unchanged request body. Omit to obtain a quote.","schema":{"type":"string"}},
            {"name":"Idempotency-Key","in":"header","required":false,"description":"Optional UUID to reuse an unpaid quote for the identical request body.","schema":{"type":"string","format":"uuid"}}
        ],
        "requestBody":{"required":true,"content":{"application/json":{"schema":schema,"example":example}}},
        "responses":{
            "200":{
                "description":"Completed non-streaming chat response. choices[].message.content contains assistant text; covenant contains the job identity, signed envelope, verified operator receipt, and output. Payment-Response reports the settled x402 payment.",
                "headers":{"Payment-Response":{"description":"Base64 x402 settlement response.","schema":{"type":"string"}}},
                "content":{"application/json":{"schema":{
                    "type":"object", "required":["id","object","created","model","choices","covenant"],
                    "properties":{
                        "id":{"type":"string"},"object":{"const":"chat.completion"},"created":{"type":"integer"},"model":{"type":"string"},
                        "choices":{"type":"array","items":{"type":"object","properties":{"index":{"type":"integer"},"message":{"type":"object","properties":{"role":{"const":"assistant"},"content":{"type":"string"}}},"finish_reason":{"type":"string"}}}},
                        "covenant":{"type":"object","description":"Job envelope, signed receipt, and inference output."}
                    }
                }}}
            },
            "202":{"description":format!("Payment or execution is pending. A paid request is held up to {} s for payment finality before this response. After Retry-After, resend the identical request with the same Payment-Signature; payment is not repeated.", crate::SETTLEMENT_WAIT.as_secs())},
            "400":{"description":"Invalid request body, output cap, pricing mode, or idempotency key."},
            "402":{
                "description":"Fresh body-bound x402 v2 payment requirements. Inspect the runtime accepts array for exact atomic USDC amount, payment destination, quote identity, and expiry. A payment error can also return 402 without a payable challenge.",
                "headers":{"Payment-Required":{"description":"Base64 x402 v2 payment requirements for a fresh quote.","schema":{"type":"string"}}},
                "content":{"application/json":{"schema":{"type":"object","properties":{"x402Version":{"const":2},"resource":{"type":"object"},"accepts":{"type":"array","items":{"type":"object"}},"error":{"type":"string"}}}}}
            },
            "410":{"description":"The retained completed response expired; payment will not be repeated."},
            "422":{"description":"The retained execution failed."},
            "503":{"description":"No payable capacity, insufficient reserve, or discovery/execution is unavailable."}
        }
    }});
    Ok(document)
}

impl Gateway {
    pub async fn discovery_openapi(&self) -> Result<Value> {
        let live = self.live_discovery_offers(true).await?;
        document(
            self.rail.resource_url(),
            live.observed,
            &live.offers,
            !live.alternates.is_empty(),
        )
    }
}

pub async fn openapi_http(State(gateway): State<Arc<Gateway>>) -> Response {
    super::respond(gateway.discovery_openapi().await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_compute_protocol::InferenceTariff;

    const RESOURCE: &str = "https://compute.example:8443/v1/chat/completions";

    fn offers() -> Vec<DiscoveryOffer> {
        let tariff = InferenceTariff {
            schema_version: 1,
            id: "retail".into(),
            revision: 1,
            model: "example".into(),
            base_micro_usdc: 12_000,
            output_cap_micro_usdc_per_million: 9_000_000,
            max_output_tokens: 2048,
            valid_from_ms: 1,
            valid_until_ms: 600_000,
        };
        vec![
            DiscoveryOffer {
                model: "example".into(),
                max_output_tokens: 2048,
                max_input_bytes: 4096,
                pricing: DiscoveryPricing::Legacy {
                    retail_micro_usdc: 50_000,
                    native_micro_usdc: 30_000,
                },
            },
            DiscoveryOffer {
                model: "example".into(),
                max_output_tokens: 2048,
                max_input_bytes: 4096,
                pricing: DiscoveryPricing::ModelTariff {
                    retail: tariff.clone(),
                    native: tariff,
                },
            },
        ]
    }

    #[test]
    fn usd_conversion_is_exact_without_floating_point() {
        for (micro, expected) in [
            (0, "0"),
            (1, "0.000001"),
            (12_009, "0.012009"),
            (50_000, "0.05"),
            (1_000_000, "1"),
            (10_000_001, "10.000001"),
            (u64::MAX, "18446744073709.551615"),
        ] {
            assert_eq!(usd_amount(micro), expected);
        }
    }

    #[test]
    fn price_range_covers_both_modes_and_current_cap() {
        let mut offers = offers();
        assert_eq!(
            price_metadata(&offers).unwrap(),
            json!({"mode":"dynamic","currency":"USD","min":"0.012009","max":"0.05"})
        );
        assert_eq!(
            price_metadata(&offers[..1]).unwrap(),
            json!({"mode":"fixed","currency":"USD","amount":"0.05"})
        );
        offers[1].max_output_tokens = 17;
        assert_eq!(
            price_metadata(&offers[1..]).unwrap(),
            json!({"mode":"dynamic","currency":"USD","min":"0.012009","max":"0.012153"})
        );
        offers[1].max_output_tokens = 1;
        assert_eq!(
            price_metadata(&offers[1..]).unwrap(),
            json!({"mode":"fixed","currency":"USD","amount":"0.012009"})
        );
    }

    #[test]
    fn schema_samples_tariff_while_preserving_legacy_and_per_mode_limits() {
        let mut offers = offers();
        offers[1].max_output_tokens = 17;
        let (schema, example) = request_schema(&offers).unwrap();
        assert_eq!(
            example,
            json!({"model":"example","messages":[{"role":"user","content":"Reply with a short greeting."}],"max_tokens":17,"pricing":"model_tariff_v1"})
        );
        assert!(offers[1].matches_request(&example));
        assert_eq!(schema["properties"]["max_tokens"]["maximum"], 2048);
        assert_eq!(schema["properties"]["max_tokens"]["example"], 17);
        assert_eq!(
            schema["properties"]["messages"]["example"],
            example["messages"]
        );
        assert_eq!(schema["properties"]["pricing"]["const"], "model_tariff_v1");
        assert_eq!(
            schema["anyOf"][0]["properties"]["max_tokens"]["maximum"],
            17
        );
        assert_eq!(
            schema["anyOf"][1]["properties"]["max_tokens"]["maximum"],
            2048
        );
        assert!(schema["anyOf"][0]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("pricing")));
        assert!(schema["anyOf"][1]["properties"].get("pricing").is_none());
        assert_eq!(schema["anyOf"][1]["additionalProperties"], false);
        let (legacy_schema, legacy_example) = request_schema(&offers[..1]).unwrap();
        assert!(legacy_example.get("pricing").is_none());
        assert!(legacy_schema["properties"].get("pricing").is_none());
        assert!(offers[0].matches_request(&legacy_example));
    }

    #[test]
    fn sample_fits_small_normalized_input_limits_or_fails_closed() {
        let mut offers = offers();
        for limit in 31..=58 {
            for offer in &mut offers {
                offer.max_input_bytes = limit;
            }
            for modes in [&offers[..], &offers[..1]] {
                let (schema, example) = request_schema(modes).unwrap();
                assert_eq!(
                    example["messages"][0]["content"],
                    if limit == 58 {
                        "Reply with a short greeting."
                    } else {
                        "x"
                    }
                );
                assert!(serde_json::to_vec(&example["messages"]).unwrap().len() <= limit);
                assert_eq!(
                    schema["properties"]["messages"]["example"],
                    example["messages"]
                );
            }
        }
        for limit in 0..31 {
            for offer in &mut offers {
                offer.max_input_bytes = limit;
            }
            assert!(matches!(
                document(RESOURCE, 123, &offers, false),
                Err(GatewayError::Unavailable(_))
            ));
        }
    }

    #[test]
    fn openapi_uses_canonical_origin_and_real_payment_protocol() {
        let document = document(RESOURCE, 123, &offers(), false).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
        assert_eq!(document["info"]["title"], "Covenant Compute");
        assert_eq!(document["info"]["version"], "1.0.0");
        assert_eq!(
            document["servers"],
            json!([{"url":"https://compute.example:8443"}])
        );
        let operation = &document["paths"]["/v1/chat/completions"]["post"];
        assert_eq!(operation["security"], json!([]));
        assert_eq!(operation["x-payment-info"]["protocols"], json!(["x402"]));
        assert!(operation["responses"].get("402").is_some());
        assert!(document["paths"]["/v1/chat/completions"]
            .get("get")
            .is_none());
        for resource in [
            "http://compute.example/v1/chat/completions",
            "https://user:secret@compute.example/v1/chat/completions",
            "https://compute.example/other",
            "https://compute.example/v1/chat/completions?key=secret",
        ] {
            assert!(super::document(resource, 123, &offers(), false).is_err());
        }
    }

    #[test]
    fn no_capacity_has_no_paid_paths_or_sample() {
        let document = document(RESOURCE, 123, &[], false).unwrap();
        assert_eq!(document["paths"], json!({}));
        assert_eq!(document["x-covenant-available"], false);
        assert_eq!(document["x-covenant-observed-at-ms"], 123);
        assert_eq!(document["security"], json!([]));
    }
}
