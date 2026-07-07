//! CCIP-Read (EIP-3668) gateway for `*.agents.opencovenant.eth`.
//!
//! An `OffchainResolver` on L1 reverts `OffchainLookup` and defers here. This
//! service answers `addr(node, 501)`, the ENSIP-9 Solana record, for names it
//! knows, signs the response with the gateway key, and returns the blob the
//! resolver's `resolveWithProof` verifies against its signer allowlist. It only
//! ever signs `addr(node, 501)`; the signing core refuses any other selector or
//! coin type, so the key cannot be steered into signing an arbitrary binding.
//!
//! Config (env):
//! - `COVENANT_ENS_GATEWAY_KEY_HEX` (32-byte hex) or `COVENANT_ENS_GATEWAY_KEY`
//!   (key file path): the secp256k1 signer, allowlisted in the resolver.
//! - `COVENANT_ENS_RESOLVER`: the deployed `OffchainResolver` address. The
//!   response digest binds it, so a response is not replayable at another resolver.
//! - `COVENANT_ENS_NAMES`: optional JSON `{ "<name>": "<base58 solana>" }` merged
//!   over the seed name.
//! - `PORT`: listen port (Render injects it; default 8080).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use covenant_evm_signer::resolver::{parse_addr_request, ResolverGateway};
use covenant_identity::Secp256k1IssuerKey;
use serde_json::{json, Value};

/// Seconds a signed response stays valid. A record is a projection of Solana
/// state, so a short life just triggers a re-fetch, never a stale binding.
const RESPONSE_TTL_SECS: u64 = 300;

/// A valid CCIP request is a few hundred bytes; anything past this is malformed,
/// not a real query, so reject it before decoding.
const MAX_REQUEST_HEX: usize = 8192;

/// The name seeded at boot: the Covenant foundation identity.
const SEED_NAME: &str = "foundation.agents.opencovenant.eth";
const SEED_SOLANA: &str = "4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc";

struct AppState {
    gateway: ResolverGateway,
    names: HashMap<String, [u8; 32]>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let gateway = ResolverGateway::new(load_signer(), load_resolver());
    let names = load_names();
    tracing::info!(
        signer = %hex_0x(&gateway.signer_address()),
        names = names.len(),
        "ens-gateway starting"
    );
    let state = Arc::new(AppState { gateway, names });

    // A CCIP-Read gateway is fetched cross-origin by in-browser wallets, so the
    // browser needs CORS to read the response at all.
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/:sender/:data", get(resolve))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind listener");
    tracing::info!(%addr, "listening");
    axum::serve(listener, app).await.expect("serve");
}

async fn healthz(State(s): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "signer": hex_0x(&s.gateway.signer_address()),
        "names": s.names.len(),
    }))
}

/// EIP-3668 gateway endpoint: `GET /{sender}/{data}.json`. `data` is the ABI
/// `resolve(name, addr(node, 501))` request the resolver deferred.
async fn resolve(
    State(s): State<Arc<AppState>>,
    AxPath((_sender, data)): AxPath<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let data = data.strip_suffix(".json").unwrap_or(&data);
    let response = respond(&s, data)?;
    Ok(Json(
        json!({ "data": format!("0x{}", hex::encode(response)) }),
    ))
}

/// The signing path behind the HTTP handler, separated so it is testable without
/// a socket: decode the request, resolve the name to a known Solana address, and
/// return the ABI-encoded signed response.
fn respond(state: &AppState, data: &str) -> Result<Vec<u8>, StatusCode> {
    if data.len() > MAX_REQUEST_HEX {
        return Err(StatusCode::BAD_REQUEST);
    }
    let request = decode_hex(data).ok_or(StatusCode::BAD_REQUEST)?;
    let query = parse_addr_request(&request).map_err(|_| StatusCode::BAD_REQUEST)?;
    let name = dns_decode(&query.name).ok_or(StatusCode::BAD_REQUEST)?;
    let solana = state.names.get(&name).ok_or(StatusCode::NOT_FOUND)?;
    let expires = now() + RESPONSE_TTL_SECS;
    // The address is a known, non-zero config value and signing is infallible, so
    // the only reachable error here is a non-501 coin type: a client fault, 400.
    let resp = state
        .gateway
        .resolve_solana(&request, solana, expires)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(resp.abi_encode())
}

fn load_signer() -> Secp256k1IssuerKey {
    if let Ok(h) = std::env::var("COVENANT_ENS_GATEWAY_KEY_HEX") {
        let bytes = decode_hex(&h).expect("COVENANT_ENS_GATEWAY_KEY_HEX must be hex");
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .expect("gateway key must be 32 bytes");
        return Secp256k1IssuerKey::from_secret_bytes(&arr).expect("valid gateway key");
    }
    let path = std::env::var("COVENANT_ENS_GATEWAY_KEY")
        .expect("COVENANT_ENS_GATEWAY_KEY or COVENANT_ENS_GATEWAY_KEY_HEX required");
    Secp256k1IssuerKey::load_or_create(Path::new(&path)).expect("load gateway key")
}

fn load_resolver() -> [u8; 20] {
    let s = std::env::var("COVENANT_ENS_RESOLVER").expect("COVENANT_ENS_RESOLVER required");
    let bytes = decode_hex(&s).expect("COVENANT_ENS_RESOLVER must be hex");
    bytes
        .as_slice()
        .try_into()
        .expect("resolver address must be 20 bytes")
}

fn load_names() -> HashMap<String, [u8; 32]> {
    let mut names = HashMap::new();
    if let Some(seed) = base58_decode_32(SEED_SOLANA) {
        names.insert(SEED_NAME.to_string(), seed);
    }
    if let Ok(raw) = std::env::var("COVENANT_ENS_NAMES") {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) {
            for (name, value) in map {
                if let Some(addr) = value.as_str().and_then(base58_decode_32) {
                    names.insert(name.to_lowercase(), addr);
                }
            }
        }
    }
    names
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    hex::decode(s.strip_prefix("0x").unwrap_or(s)).ok()
}

fn hex_0x(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// Decode a DNS wire-format name (`\x05agent\x0copencovenant\x03eth\x00`) to a
/// lowercase dotted string. `None` on any malformed length or non-UTF-8 label.
fn dns_decode(name: &[u8]) -> Option<String> {
    let mut labels = Vec::new();
    let mut i = 0;
    while i < name.len() {
        let len = name[i] as usize;
        if len == 0 {
            break;
        }
        i += 1;
        let label = name.get(i..i + len)?;
        labels.push(std::str::from_utf8(label).ok()?);
        i += len;
    }
    if labels.is_empty() {
        return None;
    }
    Some(labels.join(".").to_lowercase())
}

/// Base58 (Bitcoin alphabet) decode of a Solana address, right-aligned into 32
/// bytes. `None` if it decodes to more than 32 bytes or contains a non-alphabet
/// character.
fn base58_decode_32(s: &str) -> Option<[u8; 32]> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut acc: Vec<u8> = Vec::new();
    for c in s.bytes() {
        let mut carry = ALPHABET.iter().position(|&a| a == c)? as u32;
        for b in acc.iter_mut() {
            let v = (*b as u32) * 58 + carry;
            *b = (v & 0xff) as u8;
            carry = v >> 8;
        }
        while carry > 0 {
            acc.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    for c in s.bytes() {
        if c == b'1' {
            acc.push(0);
        } else {
            break;
        }
    }
    acc.reverse();
    if acc.len() > 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out[32 - acc.len()..].copy_from_slice(&acc);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base58_decodes_the_foundation_identity() {
        // Cross-checked against the addr(501) bytes computed for opencovenant.eth.
        let got = base58_decode_32(SEED_SOLANA).unwrap();
        assert_eq!(
            hex::encode(got),
            "347cb1af926f368342eb8c64f6e4ec7e31a1eb45c5d41a04ea996dad85bc690f"
        );
    }

    #[test]
    fn respond_signs_a_known_name_and_404s_an_unknown_one() {
        use covenant_evm_signer::resolver::encode_solana_addr_request;
        let gateway = ResolverGateway::new(
            Secp256k1IssuerKey::from_secret_bytes(&[7u8; 32]).unwrap(),
            [0x11u8; 20],
        );
        let mut names = HashMap::new();
        names.insert("agent.opencovenant.eth".to_string(), [0x33u8; 32]);
        let state = AppState { gateway, names };
        let node = [0x22u8; 32];

        let known = encode_solana_addr_request(b"\x05agent\x0copencovenant\x03eth\x00", &node);
        let resp = respond(&state, &hex::encode(&known)).unwrap();
        assert!(
            resp.windows(32).any(|w| w == [0x33u8; 32]),
            "the resolved Solana address must appear in the signed response"
        );

        let unknown = encode_solana_addr_request(b"\x07unknown\x0copencovenant\x03eth\x00", &node);
        assert_eq!(
            respond(&state, &hex::encode(&unknown)),
            Err(StatusCode::NOT_FOUND)
        );
    }

    #[test]
    fn respond_400s_a_non_solana_coin_type_and_oversized_input() {
        use covenant_evm_signer::resolver::{encode_addr_call, encode_resolve_call};
        let gateway = ResolverGateway::new(
            Secp256k1IssuerKey::from_secret_bytes(&[7u8; 32]).unwrap(),
            [0x11u8; 20],
        );
        let mut names = HashMap::new();
        names.insert("agent.opencovenant.eth".to_string(), [0x33u8; 32]);
        let state = AppState { gateway, names };

        // A known name, but coinType 60 (ETH) instead of 501 (Solana).
        let eth_coin = encode_resolve_call(
            b"\x05agent\x0copencovenant\x03eth\x00",
            &encode_addr_call(&[0x22u8; 32], 60),
        );
        assert_eq!(
            respond(&state, &hex::encode(&eth_coin)),
            Err(StatusCode::BAD_REQUEST)
        );

        assert_eq!(
            respond(&state, &"0".repeat(MAX_REQUEST_HEX + 2)),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn dns_decode_reads_a_wire_name() {
        assert_eq!(
            dns_decode(b"\x0afoundation\x06agents\x0copencovenant\x03eth\x00").as_deref(),
            Some("foundation.agents.opencovenant.eth")
        );
        assert_eq!(dns_decode(b"\x05agent").as_deref(), Some("agent"));
        assert_eq!(dns_decode(b"").as_deref(), None);
        assert_eq!(dns_decode(b"\x06agent").as_deref(), None); // length past end
    }
}
