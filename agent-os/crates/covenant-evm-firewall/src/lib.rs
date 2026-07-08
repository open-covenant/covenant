//! Pre-execution firewall for a governed agent's Base transactions.
//!
//! The daemon already gates a Base *payment* (x402/EIP-3009) through capability,
//! budget, and audit. An arbitrary Base *transaction* (a contract call, a Spend
//! Permission draw, a transfer) needs one more guard before the signer sees it.
//! That guard allowlists the contracts and function selectors the agent may
//! touch, caps native value, and dry-runs the call against the chain so a
//! transaction that would revert or misbehave is never signed.
//!
//! This crate is the pure half of that guard. [`EvmPolicy::evaluate`] is a
//! fail-closed allowlist + value-cap check (default deny), and
//! [`eth_call_request`] / [`interpret_eth_call`] encode and read the JSON-RPC
//! `eth_call` a caller uses to simulate. The HTTP round-trip and the signing
//! stay with the daemon; the decision logic lives here so it is testable in
//! isolation and shared by every Base action path.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

/// A proposed Base transaction the firewall judges before it is signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvmCall {
    pub to: [u8; 20],
    /// Native value in wei. A plain USDC payment carries none (value lives in the
    /// calldata); a native transfer carries value and empty data.
    pub value: u128,
    pub data: Vec<u8>,
}

impl EvmCall {
    /// The 4-byte function selector, or `None` for a call with no calldata (a
    /// bare value transfer).
    pub fn selector(&self) -> Option<[u8; 4]> {
        if self.data.is_empty() {
            return None;
        }
        self.data.get(..4).and_then(|s| s.try_into().ok())
    }
}

/// Which function selectors are permitted on an allowlisted contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorRule {
    /// Any selector (and bare value transfers) are allowed on this contract.
    Any,
    /// Only these selectors are allowed. A bare value transfer (no selector) is
    /// allowed only if [`ContractPolicy::allow_bare_value`] is set.
    Only(BTreeSet<[u8; 4]>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractPolicy {
    pub selectors: SelectorRule,
    /// Per-contract value ceiling in wei. `None` falls back to the policy-wide cap.
    pub max_value_wei: Option<u128>,
    /// Whether a bare value transfer (empty calldata) is allowed when
    /// `selectors` is `Only(..)`. `Any` always allows it.
    pub allow_bare_value: bool,
}

impl ContractPolicy {
    /// A contract the agent may call with any selector, up to the policy-wide
    /// value cap.
    pub fn any() -> Self {
        Self {
            selectors: SelectorRule::Any,
            max_value_wei: None,
            allow_bare_value: true,
        }
    }

    /// A contract restricted to a fixed selector set, no bare value transfers.
    pub fn selectors(selectors: impl IntoIterator<Item = [u8; 4]>) -> Self {
        Self {
            selectors: SelectorRule::Only(selectors.into_iter().collect()),
            max_value_wei: None,
            allow_bare_value: false,
        }
    }

    pub fn with_max_value_wei(mut self, cap: u128) -> Self {
        self.max_value_wei = Some(cap);
        self
    }
}

/// The set of Base actions an agent is permitted. Default deny: a contract not
/// listed here is refused.
#[derive(Debug, Clone, Default)]
pub struct EvmPolicy {
    contracts: BTreeMap<[u8; 20], ContractPolicy>,
    /// Policy-wide value ceiling in wei. Defaults to 0 (no native value), so a
    /// value-bearing call is refused unless the cap is raised.
    max_value_wei: u128,
}

impl EvmPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_value_wei(mut self, cap: u128) -> Self {
        self.max_value_wei = cap;
        self
    }

    pub fn allow(mut self, contract: [u8; 20], policy: ContractPolicy) -> Self {
        self.contracts.insert(contract, policy);
        self
    }

    /// Judge a call against the policy. Fail-closed: an unlisted contract, a
    /// selector outside the allowlist, malformed calldata, or value over a cap
    /// is refused.
    pub fn evaluate(&self, call: &EvmCall) -> Result<(), Denial> {
        let contract = self
            .contracts
            .get(&call.to)
            .ok_or(Denial::ContractNotAllowed { to: call.to })?;

        let cap = contract.max_value_wei.unwrap_or(self.max_value_wei);
        if call.value > cap {
            return Err(Denial::ValueOverCap {
                value: call.value,
                cap,
            });
        }

        if call.data.is_empty() {
            let allowed =
                matches!(contract.selectors, SelectorRule::Any) || contract.allow_bare_value;
            if !allowed {
                return Err(Denial::BareValueNotAllowed { to: call.to });
            }
        } else if call.data.len() < 4 {
            return Err(Denial::MalformedCalldata {
                len: call.data.len(),
            });
        } else {
            let selector: [u8; 4] = call.data[..4].try_into().expect("len checked >= 4");
            if let SelectorRule::Only(allowed) = &contract.selectors {
                if !allowed.contains(&selector) {
                    return Err(Denial::SelectorNotAllowed {
                        to: call.to,
                        selector,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Why a call was refused. Every variant names the offending field so the
/// daemon can audit a precise reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denial {
    ContractNotAllowed { to: [u8; 20] },
    SelectorNotAllowed { to: [u8; 20], selector: [u8; 4] },
    BareValueNotAllowed { to: [u8; 20] },
    MalformedCalldata { len: usize },
    ValueOverCap { value: u128, cap: u128 },
}

impl std::fmt::Display for Denial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Denial::ContractNotAllowed { to } => {
                write!(f, "contract {} is not on the allowlist", hex0x(to))
            }
            Denial::SelectorNotAllowed { to, selector } => write!(
                f,
                "selector {} is not allowed on {}",
                hex0x(selector),
                hex0x(to)
            ),
            Denial::BareValueNotAllowed { to } => {
                write!(f, "a bare value transfer to {} is not allowed", hex0x(to))
            }
            Denial::MalformedCalldata { len } => {
                write!(f, "calldata of {len} bytes is too short to hold a selector")
            }
            Denial::ValueOverCap { value, cap } => {
                write!(f, "value {value} wei exceeds the cap of {cap} wei")
            }
        }
    }
}

impl std::error::Error for Denial {}

/// Build the `eth_call` JSON-RPC request to dry-run a call (`from` is the agent's
/// Base address). The caller POSTs this to a Base RPC and passes the reply to
/// [`interpret_eth_call`].
pub fn eth_call_request(call: &EvmCall, from: &[u8; 20], id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "eth_call",
        "params": [
            {
                "from": hex0x(from),
                "to": hex0x(&call.to),
                "value": format!("0x{:x}", call.value),
                "data": hex0x(&call.data),
            },
            "latest"
        ],
    })
}

/// The outcome of a simulated call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Simulation {
    /// The call executed cleanly; the returned bytes are the `eth_call` result.
    Ok(Vec<u8>),
    /// The call reverted; the string is the node's error message.
    Reverted(String),
    /// The response was not a well-formed JSON-RPC reply.
    Malformed(String),
}

/// Read an `eth_call` JSON-RPC reply into a [`Simulation`]. A revert (the guard
/// that catches a call that would fail or misbehave on chain) surfaces as
/// [`Simulation::Reverted`], not a clean result.
pub fn interpret_eth_call(response: &Value) -> Simulation {
    if let Some(err) = response.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("execution reverted")
            .to_string();
        return Simulation::Reverted(msg);
    }
    match response.get("result").and_then(Value::as_str) {
        Some(result) => match decode_hex(result) {
            Some(bytes) => Simulation::Ok(bytes),
            None => Simulation::Malformed(format!("result is not hex: {result}")),
        },
        None => Simulation::Malformed("reply has neither result nor error".into()),
    }
}

fn hex0x(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    let body = value.strip_prefix("0x").unwrap_or(value);
    if !body.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() / 2);
    let bytes = body.as_bytes();
    let nibble = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    for pair in bytes.chunks_exact(2) {
        out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC: [u8; 20] = [0x11; 20];
    const OTHER: [u8; 20] = [0x22; 20];
    const AGENT: [u8; 20] = [0x33; 20];
    const TRANSFER: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb]; // transfer(address,uint256)
    const APPROVE: [u8; 4] = [0x09, 0x5e, 0xa7, 0xb3];

    fn call(to: [u8; 20], value: u128, data: Vec<u8>) -> EvmCall {
        EvmCall { to, value, data }
    }

    fn policy() -> EvmPolicy {
        EvmPolicy::new().allow(USDC, ContractPolicy::selectors([TRANSFER]))
    }

    #[test]
    fn allows_an_allowlisted_selector() {
        let mut data = TRANSFER.to_vec();
        data.extend_from_slice(&[0u8; 64]);
        assert!(policy().evaluate(&call(USDC, 0, data)).is_ok());
    }

    #[test]
    fn refuses_an_unlisted_contract() {
        assert_eq!(
            policy().evaluate(&call(OTHER, 0, TRANSFER.to_vec())),
            Err(Denial::ContractNotAllowed { to: OTHER })
        );
    }

    #[test]
    fn refuses_a_selector_outside_the_allowlist() {
        let mut data = APPROVE.to_vec();
        data.extend_from_slice(&[0u8; 64]);
        assert_eq!(
            policy().evaluate(&call(USDC, 0, data)),
            Err(Denial::SelectorNotAllowed {
                to: USDC,
                selector: APPROVE
            })
        );
    }

    #[test]
    fn refuses_value_over_the_cap_by_default() {
        // Default policy-wide cap is 0, so any native value is refused.
        let mut data = TRANSFER.to_vec();
        data.extend_from_slice(&[0u8; 64]);
        assert_eq!(
            policy().evaluate(&call(USDC, 1, data)),
            Err(Denial::ValueOverCap { value: 1, cap: 0 })
        );
    }

    #[test]
    fn honors_a_raised_value_cap() {
        let p = EvmPolicy::new()
            .with_max_value_wei(1_000)
            .allow(USDC, ContractPolicy::any());
        assert!(p.evaluate(&call(USDC, 1_000, vec![])).is_ok());
        assert_eq!(
            p.evaluate(&call(USDC, 1_001, vec![])),
            Err(Denial::ValueOverCap {
                value: 1_001,
                cap: 1_000
            })
        );
    }

    #[test]
    fn refuses_a_bare_value_transfer_to_a_selector_only_contract() {
        let p = EvmPolicy::new()
            .with_max_value_wei(10)
            .allow(USDC, ContractPolicy::selectors([TRANSFER]));
        assert_eq!(
            p.evaluate(&call(USDC, 5, vec![])),
            Err(Denial::BareValueNotAllowed { to: USDC })
        );
    }

    #[test]
    fn refuses_malformed_calldata() {
        assert_eq!(
            policy().evaluate(&call(USDC, 0, vec![0xa9, 0x05])),
            Err(Denial::MalformedCalldata { len: 2 })
        );
    }

    #[test]
    fn eth_call_request_shape() {
        let req = eth_call_request(&call(USDC, 0x1f, TRANSFER.to_vec()), &AGENT, 7);
        assert_eq!(req["method"], "eth_call");
        assert_eq!(req["params"][0]["to"], hex0x(&USDC));
        assert_eq!(req["params"][0]["from"], hex0x(&AGENT));
        assert_eq!(req["params"][0]["value"], "0x1f");
        assert_eq!(req["params"][1], "latest");
    }

    #[test]
    fn interprets_revert_and_success() {
        let revert =
            json!({"jsonrpc":"2.0","id":1,"error":{"code":3,"message":"execution reverted: bad"}});
        assert_eq!(
            interpret_eth_call(&revert),
            Simulation::Reverted("execution reverted: bad".into())
        );
        let ok = json!({"jsonrpc":"2.0","id":1,"result":"0x0000000000000000000000000000000000000000000000000000000000000001"});
        assert!(
            matches!(interpret_eth_call(&ok), Simulation::Ok(b) if b.len() == 32 && b[31] == 1)
        );
    }
}
