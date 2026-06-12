//! Bounded response-body reads shared across the x402 client and signers.

use reqwest::Response;

use crate::{Result, X402Error};

/// Maximum response body any x402 path buffers into memory. A remote that
/// answers a paid request — the 402 facilitator that returns the challenge or
/// the Solana RPC a signer hits for a blockhash — controls its response, so an
/// unbounded read lets a malicious one exhaust a worker's memory. 16 MiB sits
/// far above any real requirements array or RPC result yet stops a runaway
/// stream; it is the memory-axis sibling of the client's request timeout.
pub(crate) const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Reads a response body into a string, refusing anything past `max`. The
/// `Content-Length` check rejects an oversized declared body before it is
/// streamed; the running accumulation check is the real guard, since the header
/// is optional and remote-controlled. Bodies here are JSON, so the bounded
/// bytes are decoded lossily rather than via charset-aware `.text()`. The
/// `too_large` constructor lets each caller surface the cap breach as its own
/// error variant.
pub(crate) async fn read_capped(
    mut resp: Response,
    max: usize,
    too_large: impl Fn(String) -> X402Error,
) -> Result<String> {
    if let Some(len) = resp.content_length() {
        if len > max as u64 {
            return Err(too_large(format!(
                "response body of {len} bytes exceeds the {max}-byte cap"
            )));
        }
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() + chunk.len() > max {
            return Err(too_large(format!(
                "response body exceeds the {max}-byte cap"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}
