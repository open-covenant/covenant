//! Bounded reads of the untrusted upstreams this crate talks to.
//!
//! reqwest buffers an entire response into memory, so every read of an
//! untrusted body (the paid loop, the manifest poll) goes through a cap to
//! keep a hostile or compromised remote from exhausting a daemon worker.

use crate::{HyreError, Result};

/// Maximum response body any of this crate's untrusted reads will buffer.
/// 16 MiB sits far above a real Hyre response yet stops a runaway stream —
/// the memory-axis sibling of the per-request timeouts on the same clients.
pub(crate) const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Read a response body into a string, refusing anything past `max`. The
/// `Content-Length` check rejects an oversized declared body before it is
/// streamed; the running accumulation check is the real guard, since the
/// header is optional and provider-controlled. Bodies here are JSON, so the
/// bounded bytes are decoded lossily rather than via charset-aware `.text()`.
/// `too_large` names the error for an over-cap body — the paid loop and the
/// manifest poll surface different variants.
pub(crate) async fn read_capped(
    mut resp: reqwest::Response,
    max: usize,
    too_large: impl Fn(String) -> HyreError,
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
        // pre-check (http.rs:27) and the running accumulation check (http.rs:35),
        // so a body sized exactly at the cap fits and must read back whole. The
        // existing cap tests (facilitator/x402/catalog) serve a 4096-byte body
        // against a 64-byte cap, where `> max` and `>= max` agree, so a
        // `> max -> >= max` slip on either guard survives them — and
        // facilitator.rs:426 concedes the accumulation guard is only
        // inspection-verified. Serve a body of known length N: at cap N the
        // original accepts (N is not > N) while the mutant rejects, and at cap
        // N-1 the body sits one byte over, so the pre-check fires and the
        // rejection names the cap.
        const BODY: &str = "covenant-hyre bounded-read at-cap inclusive boundary fixture";
        let n = BODY.len();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
            .mount(&server)
            .await;

        let resp = reqwest::get(server.uri())
            .await
            .expect("GET the at-cap body");
        let body = read_capped(resp, n, HyreError::Execute)
            .await
            .expect("a body sized exactly at the cap fits and must read back whole");
        assert_eq!(body, BODY);

        let resp = reqwest::get(server.uri())
            .await
            .expect("GET the over-cap body");
        let err = read_capped(resp, n - 1, HyreError::Execute)
            .await
            .expect_err("a body one byte over the cap must be rejected");
        assert!(
            matches!(&err, HyreError::Execute(m) if m.contains("cap")),
            "the one-byte-over rejection must surface the byte-cap error; got {err:?}",
        );
    }
}
