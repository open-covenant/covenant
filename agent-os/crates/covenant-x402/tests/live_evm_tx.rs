//! Live check that [`covenant_x402::EvmRpc`] signs and broadcasts a real
//! EIP-1559 transaction end-to-end. `#[ignore]`d and env-gated: it needs a
//! funded key on an EVM chain and its RPC, neither in CI.
//!
//! ```bash
//! COVENANT_X402_EVM_RPC_URL="$RH_TESTNET_RPC" \
//! COVENANT_X402_EVM_CHAIN_ID=46630 \
//! COVENANT_X402_EVM_SECRET_HEX="$RH_TESTNET_DEPLOYER_KEY" \
//! cargo test -p covenant-x402 --features evm-rpc -- --ignored live_evm_tx
//! ```
//!
//! The tx is a 0-value self-transfer — the minimal, non-destructive call that
//! still exercises the whole submit path: chain-id verify, pending nonce, fee
//! resolution, gas estimate (revert preflight), raw broadcast, and receipt
//! poll. A green receipt proves covenantd can move its own funds in-process,
//! no `cast` in the loop.

use covenant_x402::{EvmRpc, EvmTxSigner, TxRequest};

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

#[tokio::test]
#[ignore = "live: needs a funded EVM key + RPC (RH Chain testnet)"]
async fn live_evm_tx_self_transfer_confirms() {
    let (Some(rpc_url), Some(chain_hex), Some(secret_hex)) = (
        env("COVENANT_X402_EVM_RPC_URL"),
        env("COVENANT_X402_EVM_CHAIN_ID"),
        env("COVENANT_X402_EVM_SECRET_HEX"),
    ) else {
        eprintln!(
            "skipping: set COVENANT_X402_EVM_RPC_URL, COVENANT_X402_EVM_CHAIN_ID, \
             COVENANT_X402_EVM_SECRET_HEX to enable"
        );
        return;
    };

    let chain_id: u64 = chain_hex.parse().expect("chain id");
    let signer = EvmTxSigner::from_secret_bytes(&secret_bytes(&secret_hex)).expect("signer");
    let rpc = EvmRpc::new(rpc_url, chain_id);

    let tx = TxRequest {
        to: signer.address(),
        value: 0,
        data: vec![],
        gas_limit: None,
    };

    eprintln!(
        "submitting 0-value self-transfer from {}",
        signer.address_hex()
    );
    let receipt = rpc.submit(&signer, &tx).await.expect("submit + confirm");
    eprintln!(
        "confirmed in block {} — gas {} — tx 0x{}",
        receipt.block_number,
        receipt.gas_used,
        receipt
            .tx_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    );
    assert!(
        receipt.block_number > 0,
        "receipt must carry a block number"
    );
}
