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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn read_capped_reads_a_body_at_the_exact_cap_and_rejects_one_byte_over() {
        // read_capped guards memory with `> max` on both the Content-Length
        // pre-check (http.rs:28) and the running accumulation check (http.rs:36),
        // so a body sized exactly at the cap fits and must read back whole. The
        // crate's only body-cap test (latest_blockhash_rejects_oversized_rpc_body)
        // serves a 4096-byte body against a 64-byte cap, where `> max` and
        // `>= max` agree, so a `> max -> >= max` slip on either guard survives it.
        // Serve a body of known length N: at cap N the original accepts (N is not
        // > N) while the mutant rejects, and at cap N-1 the body sits one byte
        // over, so the pre-check fires and the rejection names the cap.
        const BODY: &str = "covenant-x402 bounded-read at-cap inclusive boundary fixture";
        let n = BODY.len();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
            .mount(&server)
            .await;

        let resp = reqwest::get(server.uri())
            .await
            .expect("GET the at-cap body");
        let body = read_capped(resp, n, X402Error::Sign)
            .await
            .expect("a body sized exactly at the cap fits and must read back whole");
        assert_eq!(body, BODY);

        let resp = reqwest::get(server.uri())
            .await
            .expect("GET the over-cap body");
        let err = read_capped(resp, n - 1, X402Error::Sign)
            .await
            .expect_err("a body one byte over the cap must be rejected");
        assert!(
            matches!(&err, X402Error::Sign(m) if m.contains("cap")),
            "the one-byte-over rejection must surface the byte-cap error; got {err:?}",
        );
    }
}
