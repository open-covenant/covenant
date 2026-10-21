// Covenant patch: preserve the original Intel-signed payload, without Value reserialization.
use alloc::{boxed::Box, string::String};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct TcbInfoResponse {
    #[serde(rename = "tcbInfo")]
    pub(crate) tcb_info: Box<serde_json::value::RawValue>,
    pub(crate) signature: String,
}

#[derive(Deserialize)]
pub(crate) struct QeIdentityResponse {
    #[serde(rename = "enclaveIdentity")]
    pub(crate) enclave_identity: Box<serde_json::value::RawValue>,
    pub(crate) signature: String,
}

#[cfg(test)]
mod tests {
    #[test]
    fn signed_collateral_preserves_exact_json_bytes() -> Result<(), serde_json::Error> {
        // These payload bytes, including whitespace/escapes/order, are signed by Intel.
        let tcb = r#"{ "z":1, "a": {"number":1e0,"escaped":"\u0041"} }"#;
        let raw = format!(r#"{{"tcbInfo":{tcb},"signature":"1234"}}"#);
        let parsed: super::TcbInfoResponse = serde_json::from_str(&raw)?;
        assert_eq!(parsed.tcb_info.get(), tcb);
        let qe = r#"{ "z": [2, 1], "a":"\u0042" }"#;
        let raw = format!(r#"{{"enclaveIdentity":{qe},"signature":"5678"}}"#);
        let parsed: super::QeIdentityResponse = serde_json::from_str(&raw)?;
        assert_eq!(parsed.enclave_identity.get(), qe);
        assert_ne!(
            serde_json::from_str::<serde_json::Value>(tcb)?.to_string(),
            tcb
        );
        assert_ne!(
            serde_json::from_str::<serde_json::Value>(qe)?.to_string(),
            qe
        );
        Ok(())
    }
}
