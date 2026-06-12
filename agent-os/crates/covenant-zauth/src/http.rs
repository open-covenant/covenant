//! Bounded reads of the untrusted `api.zauth.inc` upstream.
//!
//! reqwest buffers an entire response into memory, so every read of an
//! untrusted body (the directory listing, the RepoScan loop) goes through a
//! cap to keep a hostile or compromised remote from exhausting a CLI process.

use crate::{Result, ZauthError};

/// Maximum response body any of this crate's untrusted reads will buffer.
/// 16 MiB sits far above a real zauth response yet stops a runaway stream —
/// the memory-axis sibling of the per-request timeouts on the same clients.
pub(crate) const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Read a response body into a string, refusing anything past `max`. The
/// `Content-Length` check rejects an oversized declared body before it is
/// streamed; the running accumulation check is the real guard, since the
/// header is optional and provider-controlled. Bodies here are JSON, so the
/// bounded bytes are decoded lossily rather than via charset-aware `.text()`.
/// `too_large` names the error for an over-cap body — the directory read and
/// the RepoScan loop surface different variants.
pub(crate) async fn read_capped(
    mut resp: reqwest::Response,
    max: usize,
    too_large: impl Fn(String) -> ZauthError,
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
