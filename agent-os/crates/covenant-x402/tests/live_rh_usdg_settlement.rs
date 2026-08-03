//! Live proof that covenant-x402 settles a *generic* x402 payment in USDG on
//! Robinhood Chain (4663) — an EIP-3009 `transferWithAuthorization`, with no
//! escrow, spend grant, or provider allowlist on the path. This is the
//! settlement primitive generalized past `SpendGrantEscrow`: any x402 resource
//! priced in USDG is now payable on 4663 with the same signer the crate uses on
//! Base.
//!
//!   RH_MAINNET_RPC=... RH_MAINNET_DEPLOYER_KEY=... RH_MAINNET_PROD_ADMIN_ADDR=... \
//!   COVENANT_X402_EVM_RPC_URL="$RH_MAINNET_RPC" \
//!   COVENANT_X402_EVM_CHAIN_ID=4663 \
//!   COVENANT_X402_EVM_SECRET_HEX="$RH_MAINNET_DEPLOYER_KEY" \
//!   COVENANT_X402_USDG_RECIPIENT="$RH_MAINNET_PROD_ADMIN_ADDR" \
//!   cargo test -p covenant-x402 --features evm-rpc \
//!     --test live_rh_usdg_settlement -- --ignored live_
//!
//! The two halves of the crate's settlement path meet here on real state:
//!   1. [`EvmSigner`] signs an EIP-3009 authorization for `value` USDG to the
//!      payee — the exact `exact`-scheme payload a facilitator receives;
//!   2. the on-chain `transferWithAuthorization` is preflighted (it *reverts*
//!      when the signature is tampered — the settle enforces the covenant-x402
//!      signature, it is not a bare transfer);
//!   3. [`EvmRpc`] broadcasts it and, once mined, the payer's USDG falls and the
//!      payee's rises by exactly `value`. The balance deltas — not the receipt —
//!      are the proof the transfer settled, since a reverted tx still mines.
//!
//! Env-gated and `#[ignore]`d: it spends real USDG + ETH gas, neither in CI.
//! Amount defaults to 10000 (0.01 USDG); override with `COVENANT_X402_USDG_AMOUNT`.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};

use covenant_x402::{
    EvmRpc, EvmSigner, EvmTxSigner, PaymentRequirements, Signer, TxRequest, USDG_ROBINHOOD_MAINNET,
};

// transferWithAuthorization(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)
const SEL_TWA: [u8; 4] = [0xe3, 0xee, 0x16, 0x0e];
// balanceOf(address)
const SEL_BALANCE_OF: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn secret_bytes(hex: &str) -> [u8; 32] {
    let body = hex.strip_prefix("0x").unwrap_or(hex);
    assert_eq!(body.len(), 64, "secret hex must be 32 bytes");
    let mut out = [0u8; 32];
    for (i, pair) in body.as_bytes().chunks_exact(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16)
            .expect("secret hex must be hexadecimal");
    }
    out
}

fn decode_hex(s: &str) -> Vec<u8> {
    let body = s.strip_prefix("0x").unwrap_or(s);
    assert!(body.len().is_multiple_of(2), "odd-length hex: {s}");
    (0..body.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&body[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hexstr(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn addr20(s: &str) -> [u8; 20] {
    let v = decode_hex(s);
    assert_eq!(v.len(), 20, "address must be 20 bytes: {s}");
    v.try_into().unwrap()
}

fn bytes32(s: &str) -> [u8; 32] {
    let v = decode_hex(s);
    assert_eq!(v.len(), 32, "expected 32 bytes: {s}");
    v.try_into().unwrap()
}

fn word_u256(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn word_addr(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

#[allow(clippy::too_many_arguments)]
fn twa_calldata(
    from: &[u8; 20],
    to: &[u8; 20],
    value: u128,
    valid_after: u64,
    valid_before: u64,
    nonce: &[u8; 32],
    v: u8,
    r: &[u8; 32],
    s: &[u8; 32],
) -> Vec<u8> {
    let mut d = Vec::with_capacity(4 + 9 * 32);
    d.extend_from_slice(&SEL_TWA);
    d.extend_from_slice(&word_addr(from));
    d.extend_from_slice(&word_addr(to));
    d.extend_from_slice(&word_u256(value));
    d.extend_from_slice(&word_u256(valid_after as u128));
    d.extend_from_slice(&word_u256(valid_before as u128));
    d.extend_from_slice(nonce);
    d.extend_from_slice(&word_u256(v as u128));
    d.extend_from_slice(r);
    d.extend_from_slice(s);
    d
}

async fn eth_call(
    client: &reqwest::Client,
    rpc: &str,
    to: &str,
    data: &[u8],
    from: Option<&str>,
) -> Result<String, String> {
    let mut call = json!({ "to": to, "data": format!("0x{}", hexstr(data)) });
    if let Some(f) = from {
        call["from"] = json!(f);
    }
    let body =
        json!({ "jsonrpc": "2.0", "id": 1, "method": "eth_call", "params": [call, "latest"] });
    let resp: Value = client
        .post(rpc)
        .json(&body)
        .send()
        .await
        .expect("rpc reachable")
        .json()
        .await
        .expect("rpc returns json");
    if let Some(err) = resp.get("error") {
        return Err(err.to_string());
    }
    Ok(resp["result"].as_str().expect("result hex").to_string())
}

async fn balance_of(client: &reqwest::Client, rpc: &str, token: &str, who: &[u8; 20]) -> u128 {
    let mut data = SEL_BALANCE_OF.to_vec();
    data.extend_from_slice(&word_addr(who));
    let word = eth_call(client, rpc, token, &data, None)
        .await
        .expect("balanceOf");
    let bytes = decode_hex(&word);
    assert_eq!(bytes.len(), 32, "balanceOf returns one word");
    let mut lo = [0u8; 16];
    lo.copy_from_slice(&bytes[16..]);
    u128::from_be_bytes(lo)
}

#[tokio::test]
#[ignore = "live: spends real USDG on RH-Chain 4663 — needs a funded payer key + RPC"]
async fn live_rh_usdg_transfer_with_authorization_settles() {
    let (Some(rpc), Some(chain_hex), Some(secret_hex), Some(recipient)) = (
        env("COVENANT_X402_EVM_RPC_URL"),
        env("COVENANT_X402_EVM_CHAIN_ID"),
        env("COVENANT_X402_EVM_SECRET_HEX"),
        env("COVENANT_X402_USDG_RECIPIENT"),
    ) else {
        eprintln!(
            "skipping: set COVENANT_X402_EVM_RPC_URL, COVENANT_X402_EVM_CHAIN_ID, \
             COVENANT_X402_EVM_SECRET_HEX, COVENANT_X402_USDG_RECIPIENT to enable"
        );
        return;
    };

    let chain_id: u64 = chain_hex.parse().expect("chain id");
    let asset = env("COVENANT_X402_USDG_ASSET").unwrap_or_else(|| USDG_ROBINHOOD_MAINNET.into());
    let amount: u128 = env("COVENANT_X402_USDG_AMOUNT")
        .unwrap_or_else(|| "10000".into())
        .parse()
        .expect("amount");
    // The gas-paying relayer defaults to the payer (self-relayed): EIP-3009
    // authenticates by the authorization signature, not msg.sender, so who
    // broadcasts is orthogonal to the transfer.
    let relayer_hex = env("COVENANT_X402_EVM_RELAYER_HEX").unwrap_or_else(|| secret_hex.clone());

    let payer = EvmSigner::from_secret_bytes(&secret_bytes(&secret_hex))
        .expect("payer signer")
        .with_valid_for_secs(900);
    let recipient_addr = addr20(&recipient);
    assert_ne!(
        payer.address(),
        recipient_addr,
        "payer and payee must differ for the balance-delta assertions to be meaningful"
    );

    let requirement = PaymentRequirements {
        network: "robinhood".into(),
        asset: asset.clone(),
        amount: amount.to_string(),
        amount_usdc: amount as f64 / 1_000_000.0,
        pay_to: recipient.clone(),
        scheme: "exact".into(),
        extra: None, // no domain hint → signer falls back to Global Dollar v1 for USDG
    };

    let header = payer
        .build_payment(&requirement)
        .await
        .expect("build payment");
    let envelope: Value =
        serde_json::from_slice(&BASE64.decode(header).expect("base64")).expect("json");
    let auth = &envelope["payload"]["authorization"];

    let from = addr20(auth["from"].as_str().unwrap());
    let to = addr20(auth["to"].as_str().unwrap());
    let value: u128 = auth["value"].as_str().unwrap().parse().unwrap();
    let valid_after: u64 = auth["validAfter"].as_str().unwrap().parse().unwrap();
    let valid_before: u64 = auth["validBefore"].as_str().unwrap().parse().unwrap();
    let nonce = bytes32(auth["nonce"].as_str().unwrap());

    let sig = decode_hex(envelope["payload"]["signature"].as_str().unwrap());
    assert_eq!(sig.len(), 65, "signature is r‖s‖v");
    let r: [u8; 32] = sig[0..32].try_into().unwrap();
    let s: [u8; 32] = sig[32..64].try_into().unwrap();
    let v = sig[64];
    assert!(v == 27 || v == 28, "v must be 27/28, got {v}");

    assert_eq!(from, payer.address(), "authorization.from is the payer");
    assert_eq!(to, recipient_addr, "authorization.to is the payee");
    assert_eq!(value, amount, "authorization.value is the requested amount");

    let calldata = twa_calldata(
        &from,
        &to,
        value,
        valid_after,
        valid_before,
        &nonce,
        v,
        &r,
        &s,
    );

    let client = reqwest::Client::new();
    let relayer = EvmTxSigner::from_secret_bytes(&secret_bytes(&relayer_hex)).expect("relayer");
    let relayer_addr = relayer.address_hex();

    // Positive preflight: the authorized transfer would succeed at head state.
    eth_call(&client, &rpc, &asset, &calldata, Some(&relayer_addr))
        .await
        .expect("transferWithAuthorization preflight must succeed for a valid authorization");

    // Negative preflight: flip one byte of r; the settle must reject it. This is
    // what distinguishes a covenant-x402 settlement from a bare transfer — the
    // token verifies the EIP-712 authorization signature and reverts otherwise.
    let mut tampered = calldata.clone();
    let r_offset = 4 + 7 * 32;
    tampered[r_offset] ^= 0xff;
    let rejected = eth_call(&client, &rpc, &asset, &tampered, Some(&relayer_addr)).await;
    assert!(
        rejected.is_err(),
        "a tampered authorization must revert, got: {rejected:?}"
    );

    let from_before = balance_of(&client, &rpc, &asset, &from).await;
    let to_before = balance_of(&client, &rpc, &asset, &to).await;
    assert!(
        from_before >= value,
        "payer USDG {from_before} < amount {value}; fund the payer"
    );

    let submitter = EvmRpc::new(rpc.clone(), chain_id);
    let receipt = submitter
        .submit(&relayer, &TxRequest::call(addr20(&asset), calldata))
        .await
        .expect("submit + confirm transferWithAuthorization");

    eprintln!(
        "settled {value} USDG {} -> {}  |  tx 0x{}  block {}  gas {}",
        hexstr(&from),
        hexstr(&to),
        hexstr(&receipt.tx_hash),
        receipt.block_number,
        receipt.gas_used,
    );

    let from_after = balance_of(&client, &rpc, &asset, &from).await;
    let to_after = balance_of(&client, &rpc, &asset, &to).await;

    assert_eq!(
        from_before - from_after,
        value,
        "payer USDG must fall by exactly the settled value"
    );
    assert_eq!(
        to_after - to_before,
        value,
        "payee USDG must rise by exactly the settled value"
    );
}
