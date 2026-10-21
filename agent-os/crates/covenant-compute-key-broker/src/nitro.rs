//! AWS KMS Recipient support. Only the enclave holds the CMS recipient key.
//! This module never retries an unknown result or falls back to plain Decrypt.
use std::{collections::BTreeMap, time::Duration};

use aws_sdk_kms::{
    Client,
    config::{BehaviorVersion, Credentials, Region, retry::RetryConfig, timeout::TimeoutConfig},
    primitives::Blob,
    types::{EncryptionAlgorithmSpec, KeyEncryptionMechanism, RecipientInfo},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use openssl::{cms::CmsContentInfo, pkey::PKey, rsa::Rsa};
use rand::RngCore;
use zeroize::Zeroizing;

/// Temporary instance-role credentials arrive through the parent relay. Their
/// authority is limited by KMS policy; they cannot change the enclave policy.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: String,
}

impl Drop for SessionCredentials {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.access_key_id.zeroize();
        self.secret_access_key.zeroize();
        self.session_token.zeroize();
    }
}

pub struct NitroKms {
    client: Client,
}

#[async_trait::async_trait]
impl covenant_compute_key_broker::KeyUnwrapper for NitroKms {
    async fn unwrap_data_key(
        &self,
        key: &covenant_compute_key_broker::KeyPolicy,
        context: &BTreeMap<String, String>,
    ) -> covenant_compute_key_broker::Result<covenant_compute_key_broker::DataKey> {
        self.decrypt(&key.kms_key_arn, &key.wrapped_key_base64, context)
            .await
            .map_err(|_| covenant_compute_key_broker::BrokerError::UnwrapFailed)
    }
}

impl NitroKms {
    pub fn new(region: &str, credentials: SessionCredentials) -> Result<Self, &'static str> {
        if region.is_empty()
            || !region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("invalid region");
        }
        if credentials.access_key_id.is_empty()
            || credentials.secret_access_key.is_empty()
            || credentials.session_token.is_empty()
            || credentials.access_key_id.len() > 128
            || credentials.secret_access_key.len() > 256
            || credentials.session_token.len() > 16384
        {
            return Err("invalid temporary credentials");
        }
        let config = aws_sdk_kms::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(region.to_owned()))
            .credentials_provider(Credentials::new(
                credentials.access_key_id.clone(),
                credentials.secret_access_key.clone(),
                Some(credentials.session_token.clone()),
                None,
                "covenant-parent-instance-role",
            ))
            .retry_config(RetryConfig::disabled())
            .timeout_config(
                TimeoutConfig::builder()
                    .operation_timeout(Duration::from_secs(15))
                    .operation_attempt_timeout(Duration::from_secs(15))
                    .connect_timeout(Duration::from_secs(5))
                    .read_timeout(Duration::from_secs(10))
                    .build(),
            )
            .build();
        Ok(Self {
            client: Client::from_conf(config),
        })
    }

    pub async fn decrypt(
        &self,
        key_arn: &str,
        wrapped_key_base64: &str,
        context: &BTreeMap<String, String>,
    ) -> Result<Zeroizing<[u8; 32]>, &'static str> {
        let ciphertext = STANDARD
            .decode(wrapped_key_base64)
            .map_err(|_| "invalid wrapped key")?;
        if ciphertext.is_empty() || ciphertext.len() > 6144 {
            return Err("invalid wrapped key size");
        }
        // A fresh RSA key for every unwrap also makes response substitution
        // across concurrent requests fail CMS decryption.
        let key = PKey::from_rsa(Rsa::generate(3072).map_err(|_| "recipient generation failed")?)
            .map_err(|_| "recipient generation failed")?;
        let public = key
            .public_key_to_der()
            .map_err(|_| "recipient encoding failed")?;
        let mut nonce = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let attestation = attest_binding(public, nonce, None)?;
        let response = self
            .client
            .decrypt()
            .key_id(key_arn)
            .ciphertext_blob(Blob::new(ciphertext))
            .encryption_algorithm(EncryptionAlgorithmSpec::SymmetricDefault)
            .set_encryption_context(Some(context.clone().into_iter().collect()))
            .recipient(
                RecipientInfo::builder()
                    .attestation_document(Blob::new(attestation))
                    .key_encryption_algorithm(KeyEncryptionMechanism::RsaesOaepSha256)
                    .build(),
            )
            .send()
            .await
            .map_err(|_| "attested KMS decrypt failed or outcome unknown")?;
        decode_response(response, key_arn, &key)
    }
}

fn decode_response(
    response: aws_sdk_kms::operation::decrypt::DecryptOutput,
    key_arn: &str,
    key: &openssl::pkey::PKeyRef<openssl::pkey::Private>,
) -> Result<Zeroizing<[u8; 32]>, &'static str> {
    if response.plaintext().is_some()
        || response.key_id() != Some(key_arn)
        || response.encryption_algorithm() != Some(&EncryptionAlgorithmSpec::SymmetricDefault)
    {
        return Err("unexpected KMS response");
    }
    let cms = response
        .ciphertext_for_recipient()
        .ok_or("missing recipient ciphertext")?;
    if cms.as_ref().len() > 65536 {
        return Err("oversized recipient ciphertext");
    }
    // The CMS bytes come exclusively from the authenticated AWS TLS
    // response, not a parent-supplied CMS/decryption endpoint. AWS emits
    // CMS with no recipient certificate; its attested RSA key identifies it.
    let plaintext = Zeroizing::new(
        CmsContentInfo::from_der(cms.as_ref())
            .and_then(|cms| cms.decrypt_without_cert_check(key))
            .map_err(|_| "recipient decryption failed")?,
    );
    if plaintext.len() != 32 {
        return Err("unexpected data key length");
    }
    let mut value = Zeroizing::new([0u8; 32]);
    value.copy_from_slice(&plaintext);
    Ok(value)
}

#[cfg(target_os = "linux")]
pub fn attest_binding(
    public: Vec<u8>,
    nonce: [u8; 32],
    user_data: Option<Vec<u8>>,
) -> Result<Vec<u8>, &'static str> {
    use aws_nitro_enclaves_nsm_api::{
        api::{Request, Response},
        driver,
    };
    let fd = driver::nsm_init();
    if fd < 0 {
        return Err("Nitro Secure Module unavailable");
    }
    let response = driver::nsm_process_request(
        fd,
        Request::Attestation {
            public_key: Some(public.into()),
            nonce: Some(nonce.to_vec().into()),
            user_data: user_data.map(Into::into),
        },
    );
    driver::nsm_exit(fd);
    match response {
        Response::Attestation { document } if !document.is_empty() && document.len() <= 262144 => {
            Ok(document)
        }
        _ => Err("Nitro attestation failed"),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn attest_binding(
    _: Vec<u8>,
    _: [u8; 32],
    _: Option<Vec<u8>>,
) -> Result<Vec<u8>, &'static str> {
    Err("Nitro Secure Module requires Linux enclave hardware")
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_kms::operation::decrypt::DecryptOutput;
    use openssl::{
        asn1::Asn1Time,
        cms::CMSOptions,
        hash::MessageDigest,
        stack::Stack,
        symm::Cipher,
        x509::{X509, X509NameBuilder},
    };
    const ARN: &str =
        "arn:aws:kms:eu-north-1:000000000000:key/00000000-0000-0000-0000-000000000001";

    fn fixture(plaintext: &[u8]) -> (PKey<openssl::pkey::Private>, DecryptOutput) {
        // Synthetic CMS only. This exercises actual OpenSSL parsing/decryption;
        // it does not assert AWS attestation or AWS's exact CMS construction.
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", "offline-test").unwrap();
        let name = name.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        cert.set_subject_name(&name).unwrap();
        cert.set_issuer_name(&name).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        let mut recipients = Stack::new().unwrap();
        recipients.push(cert.build()).unwrap();
        let cms = CmsContentInfo::encrypt(
            &recipients,
            plaintext,
            Cipher::aes_256_cbc(),
            CMSOptions::BINARY,
        )
        .unwrap();
        let response = DecryptOutput::builder()
            .key_id(ARN)
            .encryption_algorithm(EncryptionAlgorithmSpec::SymmetricDefault)
            .ciphertext_for_recipient(Blob::new(cms.to_der().unwrap()))
            .build();
        (key, response)
    }

    #[test]
    fn cms_data_key_roundtrip_and_wrong_recipient_rejection() {
        let (key, response) = fixture(&[42; 32]);
        assert_eq!(
            *decode_response(response.clone(), ARN, &key).unwrap(),
            [42; 32]
        );
        let wrong = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        assert!(decode_response(response, ARN, &wrong).is_err());
    }

    #[test]
    fn rejects_plaintext_fallback_and_other_key_or_bad_length() {
        let (key, valid) = fixture(&[42; 31]);
        assert!(decode_response(valid.clone(), ARN, &key).is_err());
        assert!(decode_response(valid, "different-root", &key).is_err());
        let plain = DecryptOutput::builder()
            .key_id(ARN)
            .encryption_algorithm(EncryptionAlgorithmSpec::SymmetricDefault)
            .plaintext(Blob::new([42; 32]))
            .build();
        assert!(decode_response(plain, ARN, &key).is_err());
        let absent = DecryptOutput::builder()
            .key_id(ARN)
            .encryption_algorithm(EncryptionAlgorithmSpec::SymmetricDefault)
            .build();
        assert!(decode_response(absent, ARN, &key).is_err());
    }
}
