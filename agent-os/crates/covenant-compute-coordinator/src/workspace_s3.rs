//! Path-style S3 SigV4 grants. PUT requires an S3-verified SHA-256 and a
//! signed If-None-Match: * condition: an issued URL cannot replace an object.

use std::collections::BTreeMap;

use async_trait::async_trait;
use base64::Engine;
use covenant_compute_protocol::workspace::{ObjectGrant, WorkspaceUpload};
use hmac::{Hmac, Mac};
use sha2_10::{Digest, Sha256};

use super::{ObjectMetadata, WorkspaceError, WorkspaceObjects, GRANT_TTL_MS};

#[derive(Clone)]
pub struct S3WorkspaceConfig {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
}

pub struct S3WorkspaceObjects {
    config: S3WorkspaceConfig,
    http: reqwest::Client,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn hmac(key: &[u8], bytes: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key size");
    mac.update(bytes);
    mac.finalize().into_bytes().to_vec()
}

fn digest_b64(hex_digest: &str) -> Result<String, WorkspaceError> {
    if !covenant_compute_protocol::workspace::valid_digest(hex_digest) {
        return Err(WorkspaceError::BadRequest(
            "invalid ciphertext SHA-256".into(),
        ));
    }
    let bytes: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex_digest[i * 2..i * 2 + 2], 16).expect("validated hex"))
        .collect();
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

impl S3WorkspaceObjects {
    pub fn new(config: S3WorkspaceConfig) -> Result<Self, WorkspaceError> {
        let url = reqwest::Url::parse(&config.endpoint)
            .map_err(|_| WorkspaceError::Config("invalid S3 endpoint".into()))?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(WorkspaceError::Config(
                "S3 endpoint must be an HTTPS origin (HTTP only for loopback tests)".into(),
            ));
        }
        if config.bucket.is_empty()
            || !config
                .bucket
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
            || config.region.is_empty()
            || !config
                .region
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || config.access_key.is_empty()
            || config.secret_key.is_empty()
            || config
                .prefix
                .split('/')
                .any(|part| part == "." || part == "..")
        {
            return Err(WorkspaceError::Config(
                "invalid S3 bucket, region, prefix or credentials".into(),
            ));
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| WorkspaceError::Config(e.to_string()))?;
        Ok(Self { config, http })
    }

    pub(super) fn presign(
        &self,
        method: &str,
        key: &str,
        headers: BTreeMap<String, String>,
        now_ms: u64,
    ) -> Result<ObjectGrant, WorkspaceError> {
        self.presign_with_ttl(method, key, headers, now_ms, GRANT_TTL_MS)
    }

    fn presign_with_ttl(
        &self,
        method: &str,
        key: &str,
        mut headers: BTreeMap<String, String>,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<ObjectGrant, WorkspaceError> {
        if !(1000..=GRANT_TTL_MS).contains(&ttl_ms) {
            return Err(WorkspaceError::Config(
                "invalid object grant lifetime".into(),
            ));
        }
        let endpoint = reqwest::Url::parse(&self.config.endpoint).expect("validated endpoint");
        let host = match endpoint.port() {
            Some(port) => format!("{}:{port}", endpoint.host_str().expect("origin has host")),
            None => endpoint.host_str().expect("origin has host").to_owned(),
        };
        let now = time::OffsetDateTime::from_unix_timestamp((now_ms / 1000) as i64)
            .map_err(|_| WorkspaceError::Config("clock outside S3 signing range".into()))?;
        let format = time::format_description::parse("[year][month][day]T[hour][minute][second]Z")
            .expect("static date format");
        let timestamp = now
            .format(&format)
            .map_err(|e| WorkspaceError::Config(e.to_string()))?;
        let date = &timestamp[..8];
        let scope = format!("{date}/{}/s3/aws4_request", self.config.region);
        headers.insert("host".into(), host);
        let signed_headers = headers.keys().cloned().collect::<Vec<_>>().join(";");
        let canonical_headers = headers
            .iter()
            .map(|(k, v)| format!("{k}:{}\n", v.trim()))
            .collect::<String>();
        let mut query = BTreeMap::from([
            ("X-Amz-Algorithm".to_owned(), "AWS4-HMAC-SHA256".to_owned()),
            (
                "X-Amz-Credential".into(),
                format!("{}/{}", self.config.access_key, scope),
            ),
            ("X-Amz-Date".into(), timestamp.clone()),
            ("X-Amz-Expires".into(), (ttl_ms / 1000).to_string()),
            ("X-Amz-SignedHeaders".into(), signed_headers.clone()),
        ]);
        if let Some(token) = &self.config.session_token {
            query.insert("X-Amz-Security-Token".into(), token.clone());
        }
        let query = query
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let key = if self.config.prefix.trim_matches('/').is_empty() {
            key.to_owned()
        } else {
            format!("{}/{}", self.config.prefix.trim_matches('/'), key)
        };
        let path = format!(
            "/{}/{}",
            encode(&self.config.bucket),
            key.split('/').map(encode).collect::<Vec<_>>().join("/")
        );
        let canonical = format!(
            "{method}\n{path}\n{query}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD"
        );
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
            hex(&Sha256::digest(canonical.as_bytes()))
        );
        let date_key = hmac(
            format!("AWS4{}", self.config.secret_key).as_bytes(),
            date.as_bytes(),
        );
        let region_key = hmac(&date_key, self.config.region.as_bytes());
        let service_key = hmac(&region_key, b"s3");
        let signing_key = hmac(&service_key, b"aws4_request");
        let signature = hex(&hmac(&signing_key, to_sign.as_bytes()));
        headers.remove("host");
        Ok(ObjectGrant {
            method: method.into(),
            url: format!(
                "{}{path}?{query}&X-Amz-Signature={signature}",
                self.config.endpoint.trim_end_matches('/')
            ),
            headers,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        })
    }

    async fn request(&self, grant: ObjectGrant) -> Result<reqwest::Response, WorkspaceError> {
        let method = reqwest::Method::from_bytes(grant.method.as_bytes()).expect("internal method");
        let mut request = self.http.request(method, grant.url);
        for (key, value) in grant.headers {
            request = request.header(key, value);
        }
        // Presigned URLs contain credentials. Never format reqwest errors,
        // which include that URL, into logs or the public API.
        request
            .send()
            .await
            .map_err(|_| WorkspaceError::Storage("S3 request failed".into()))
    }
}

#[async_trait]
impl WorkspaceObjects for S3WorkspaceObjects {
    fn put_grant(
        &self,
        key: &str,
        upload: &WorkspaceUpload,
        now_ms: u64,
    ) -> Result<ObjectGrant, WorkspaceError> {
        self.presign(
            "PUT",
            key,
            BTreeMap::from([
                ("if-none-match".into(), "*".into()),
                ("content-length".into(), upload.ciphertext_bytes.to_string()),
                (
                    "x-amz-checksum-sha256".into(),
                    digest_b64(&upload.ciphertext_sha256)?,
                ),
            ]),
            now_ms,
        )
    }

    fn get_grant(
        &self,
        key: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<ObjectGrant, WorkspaceError> {
        self.presign_with_ttl("GET", key, BTreeMap::new(), now_ms, ttl_ms)
    }

    async fn head(&self, key: &str, now_ms: u64) -> Result<ObjectMetadata, WorkspaceError> {
        let response = self
            .request(self.presign(
                "HEAD",
                key,
                BTreeMap::from([("x-amz-checksum-mode".into(), "ENABLED".into())]),
                now_ms,
            )?)
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(WorkspaceError::NotFound);
        }
        if !response.status().is_success() {
            return Err(WorkspaceError::Storage(format!(
                "S3 HEAD returned {}",
                response.status().as_u16()
            )));
        }
        let size = response
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.parse::<u64>().ok())
            .ok_or_else(|| WorkspaceError::Storage("S3 HEAD omitted object length".into()))?;
        let checksum = response
            .headers()
            .get("x-amz-checksum-sha256")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| base64::engine::general_purpose::STANDARD.decode(h).ok())
            .filter(|v| v.len() == 32)
            .ok_or_else(|| {
                WorkspaceError::Storage(
                    "S3 must verify and expose the SHA-256 object checksum".into(),
                )
            })?;
        Ok(ObjectMetadata {
            ciphertext_bytes: size,
            ciphertext_sha256: hex(&checksum),
        })
    }

    async fn delete(&self, key: &str, now_ms: u64) -> Result<(), WorkspaceError> {
        let response = self
            .request(self.presign("DELETE", key, BTreeMap::new(), now_ms)?)
            .await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(WorkspaceError::Storage(format!(
            "S3 DELETE returned {}",
            response.status().as_u16()
        )))
    }

    async fn fence(&self, key: &str, now_ms: u64) -> Result<(), WorkspaceError> {
        let empty_sha = hex(&Sha256::digest([]));
        // This coordinator-only unconditional write removes ciphertext and
        // leaves an existing key against which all client If-None-Match PUTs
        // fail. Never DELETE a fence or reuse its key.
        let response = self
            .request(self.presign(
                "PUT",
                key,
                BTreeMap::from([
                    ("content-length".into(), "0".into()),
                    ("x-amz-checksum-sha256".into(), digest_b64(&empty_sha)?),
                ]),
                now_ms,
            )?)
            .await?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(WorkspaceError::Storage(format!(
                "S3 fence PUT returned {}",
                response.status().as_u16()
            )));
        }
        let metadata = self.head(key, now_ms).await?;
        if metadata.ciphertext_bytes != 0 || metadata.ciphertext_sha256 != empty_sha {
            return Err(WorkspaceError::Storage(
                "S3 fence did not verify as empty".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_signs_write_once_checksum_size_and_expires() {
        let storage = S3WorkspaceObjects::new(S3WorkspaceConfig {
            endpoint: "https://s3.example.com".into(),
            region: "us-east-1".into(),
            bucket: "workspaces".into(),
            prefix: "snapshots".into(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "secret".into(),
            session_token: None,
        })
        .unwrap();
        let upload = WorkspaceUpload {
            upload_id: uuid::Uuid::nil(),
            expected_version: 0,
            plaintext_bytes: 0,
            ciphertext_bytes: 60,
            ciphertext_sha256: "ab".repeat(32),
        };
        let grant = storage
            .put_grant("owner/workspace/object", &upload, 1_700_000_000_000)
            .unwrap();
        assert_eq!(grant.headers["if-none-match"], "*");
        assert_eq!(grant.headers["content-length"], "60");
        assert!(grant.url.contains(
            "X-Amz-SignedHeaders=content-length%3Bhost%3Bif-none-match%3Bx-amz-checksum-sha256"
        ));
        assert!(grant.url.contains("X-Amz-Expires=300"));
        assert_eq!(grant.expires_at_ms, 1_700_000_300_000);
        assert_eq!(
            grant.url,
            "https://s3.example.com/workspaces/snapshots/owner/workspace/object?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEXAMPLE%2F20231114%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20231114T221320Z&X-Amz-Expires=300&X-Amz-SignedHeaders=content-length%3Bhost%3Bif-none-match%3Bx-amz-checksum-sha256&X-Amz-Signature=c334a1dcc05e204b2c0e4c933485c61f82607d689161df00b48a089c42aa9f91"
        );
    }
}
