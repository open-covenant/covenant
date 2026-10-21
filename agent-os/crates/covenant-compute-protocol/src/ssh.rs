//! Structured SSH observations for lease-bound hardware evidence.
//! A discovered key does not authenticate a guest. Hardware verification must
//! bind it to the measured workload and its live lease before it earns trust.
use crate::sign::ProtocolError;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshHostKey {
    pub algorithm: String,
    pub key_base64: String,
}
impl SshHostKey {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if !["ssh-ed25519", "ecdsa-sha2-nistp256", "ssh-rsa"].contains(&self.algorithm.as_str())
            || self.key_base64.is_empty()
            || self.key_base64.len() > 16384
            || !self
                .key_base64
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
        {
            return Err(ProtocolError::Invalid(
                "invalid SSH host key encoding".into(),
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.key_base64)
            .map_err(|_| ProtocolError::Invalid("invalid SSH host key base64".into()))?;
        let mut wire = bytes.as_slice();
        let algorithm_matches = ssh_string(&mut wire) == Some(self.algorithm.as_bytes());
        let key_matches = match self.algorithm.as_str() {
            "ssh-ed25519" => ssh_string(&mut wire).is_some_and(|key| key.len() == 32),
            "ecdsa-sha2-nistp256" => {
                ssh_string(&mut wire) == Some(b"nistp256".as_slice())
                    && ssh_string(&mut wire).is_some_and(|point| point.len() == 65 && point[0] == 4)
            }
            "ssh-rsa" => {
                ssh_string(&mut wire).is_some_and(positive_mpint)
                    && ssh_string(&mut wire).is_some_and(positive_mpint)
            }
            _ => false,
        };
        if !algorithm_matches || !key_matches || !wire.is_empty() {
            return Err(ProtocolError::Invalid(
                "SSH host key bytes do not match the algorithm".into(),
            ));
        }
        Ok(())
    }
}
fn ssh_string<'a>(wire: &mut &'a [u8]) -> Option<&'a [u8]> {
    let length = u32::from_be_bytes(wire.get(..4)?.try_into().ok()?) as usize;
    let end = 4usize.checked_add(length)?;
    let value = wire.get(4..end)?;
    *wire = wire.get(end..)?;
    Some(value)
}
fn positive_mpint(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len() <= 1025
        && bytes[0] & 0x80 == 0
        && bytes.iter().any(|b| *b != 0)
        && (bytes[0] != 0 || bytes.get(1).is_some_and(|b| b & 0x80 != 0))
}
pub fn validate_ssh_host(host: &str) -> Result<(), ProtocolError> {
    if host.is_empty()
        || host.len() > 253
        || host.starts_with('-')
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b))
    {
        return Err(ProtocolError::Invalid(
            "invalid SSH hostname/address".into(),
        ));
    }
    Ok(())
}
/// An outer channel only; neither TLS nor an observed SSH key is hardware evidence.
/// The TLS certificate and SNI must identify the exact `SshAccess.host`, and the
/// TCP connection uses its `port`. There is no alternate dial target or command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SshTransport {
    Tls,
    /// HTTPS relay carries SSH ciphertext; session is the signed job UUID.
    OutboundRelay,
}
impl<'de> Deserialize<'de> for SshTransport {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // An internally tagged unit enum ignores extra fields in serde. Use a
        // strict object so command/alternate-host metadata cannot disappear.
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Kind {
            Tls,
            OutboundRelay,
        }
        struct Wire;
        impl<'de> serde::de::Visitor<'de> for Wire {
            type Value = SshTransport;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an SSH transport object containing only kind")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut kind = None;
                while let Some(field) = map.next_key::<String>()? {
                    if field != "kind" {
                        return Err(serde::de::Error::unknown_field(&field, &["kind"]));
                    }
                    if kind.is_some() {
                        return Err(serde::de::Error::duplicate_field("kind"));
                    }
                    kind = Some(map.next_value::<Kind>()?);
                }
                match kind {
                    Some(Kind::Tls) => Ok(SshTransport::Tls),
                    Some(Kind::OutboundRelay) => Ok(SshTransport::OutboundRelay),
                    None => Err(serde::de::Error::missing_field("kind")),
                }
            }
        }
        deserializer.deserialize_map(Wire)
    }
}
impl SshTransport {
    pub fn validate_host(&self, host: &str) -> Result<(), ProtocolError> {
        validate_ssh_host(host)?;
        if host.parse::<std::net::IpAddr>().is_ok()
            || host.ends_with('.')
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
        {
            return Err(ProtocolError::Invalid(
                "TLS SSH requires an exact lowercase DNS hostname".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshAccess {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub lease_generation: String,
    /// Absent means direct TCP, preserving legacy serialized/signed bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<SshTransport>,
    /// Unauthenticated discovery from the remote SSH service. Never an attestation.
    pub observed_host_keys: Vec<SshHostKey>,
}
impl SshAccess {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_ssh_host(&self.host)?;
        if let Some(transport) = self.transport {
            transport.validate_host(&self.host)?;
        }
        if self.port == 0
            || self.username.is_empty()
            || self.username.len() > 64
            || !self
                .username
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || self.lease_generation.is_empty()
            || self.lease_generation.len() > 128
            || self.lease_generation.chars().any(char::is_control)
            || self.observed_host_keys.len() > 8
        {
            return Err(ProtocolError::Invalid(
                "invalid structured SSH access".into(),
            ));
        }
        for key in &self.observed_host_keys {
            key.validate()?;
        }
        Ok(())
    }
    /// A host-key pin usable by OpenSSH after the caller chooses its trust policy.
    /// Formatting this does not authenticate an observed key.
    pub fn known_hosts(&self) -> Result<String, ProtocolError> {
        self.validate()?;
        let host = if self.port == 22 && !self.host.contains(':') {
            self.host.clone()
        } else {
            format!("[{}]:{}", self.host, self.port)
        };
        Ok(self
            .observed_host_keys
            .iter()
            .map(|key| format!("{host} {} {}\n", key.algorithm, key.key_base64))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access() -> SshAccess {
        let mut bytes = vec![0, 0, 0, 11];
        bytes.extend_from_slice(b"ssh-ed25519");
        bytes.extend_from_slice(&[0, 0, 0, 32]);
        bytes.extend_from_slice(&[7; 32]);
        SshAccess {
            host: "gpu.example".into(),
            port: 2222,
            username: "worker".into(),
            lease_generation: "instance-1".into(),
            transport: None,
            observed_host_keys: vec![SshHostKey {
                algorithm: "ssh-ed25519".into(),
                key_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            }],
        }
    }

    #[test]
    fn absent_transport_preserves_exact_legacy_json_and_tls_has_no_command() {
        let mut a = access();
        let old = format!(
            r#"{{"host":"gpu.example","port":2222,"username":"worker","lease_generation":"instance-1","observed_host_keys":[{{"algorithm":"ssh-ed25519","key_base64":"{}"}}]}}"#,
            a.observed_host_keys[0].key_base64
        );
        assert_eq!(serde_json::to_string(&a).unwrap(), old);
        assert_eq!(serde_json::from_str::<SshAccess>(&old).unwrap(), a);
        a.transport = Some(SshTransport::Tls);
        a.validate().unwrap();
        assert!(serde_json::to_string(&a)
            .unwrap()
            .contains(r#""transport":{"kind":"tls"}"#));
        assert_eq!(
            serde_json::from_str::<SshTransport>(r#"{"kind":"tls"}"#).unwrap(),
            SshTransport::Tls
        );
        for wire in [
            r#"{"kind":"tls","command":"evil"}"#,
            r#"{"kind":"tls","server_name":"another.example"}"#,
            r#"{"kind":"proxy","command":"evil"}"#,
            r#"{"kind":"tls","kind":"tls"}"#,
            r#"{"kind":null}"#,
            r#"{}"#,
            r#"["tls"]"#,
            r#""tls""#,
        ] {
            assert!(
                serde_json::from_str::<SshTransport>(wire).is_err(),
                "{wire}"
            );
        }
        for host in [
            "127.0.0.1",
            "::1",
            "Example.com",
            "example.com.",
            "a..com",
            "-a.com",
            "a-.com",
        ] {
            a.host = host.into();
            assert!(a.validate().is_err(), "{host}");
        }
    }

    #[test]
    fn known_hosts_preserves_exact_key_and_endpoint_without_claiming_trust() {
        let a = access();
        assert_eq!(
            a.known_hosts().unwrap(),
            format!(
                "[gpu.example]:2222 ssh-ed25519 {}\n",
                a.observed_host_keys[0].key_base64
            )
        );
        let mut ipv6 = a;
        ipv6.host = "2001:db8::1".into();
        ipv6.port = 22;
        assert!(ipv6.known_hosts().unwrap().starts_with("[2001:db8::1]:22 "));
    }

    #[test]
    fn endpoint_and_key_injection_or_malformed_wire_keys_are_rejected() {
        for host in [
            "-oProxyCommand=evil",
            "gpu\nother",
            "$(evil)",
            "gpu;evil",
            "user@host",
        ] {
            let mut a = access();
            a.host = host.into();
            assert!(a.known_hosts().is_err());
        }
        let mut a = access();
        a.observed_host_keys[0].algorithm = "ssh-rsa".into();
        assert!(a.validate().is_err());
        let mut a = access();
        a.observed_host_keys[0].key_base64 = "AAAA".into();
        assert!(a.validate().is_err());
        let mut a = access();
        a.lease_generation = "instance\nother".into();
        assert!(a.validate().is_err());
    }
}
