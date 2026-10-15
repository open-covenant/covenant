//! A swarm order: a batch job whose operator does not run the work but
//! buys it from the IMD swarm (identity.md, api.imd.fun) and relays the
//! result. The buyer names an IMD action and the body IMD's own API takes;
//! the order rides the signed envelope as one JSON block, so what the
//! swarm is asked is part of the paid, attested input.
//!
//! Only the shape is checked here, by both sides: the buyer before it
//! signs (a refused body is a spent payment on IMD's side, which has no
//! refunds), and the operator before it pays. Launches and workflows are
//! not sold through a swarm node: the wallet that pays IMD owns what a
//! launch deploys, and that wallet is the operator's.

use covenant_mcp::Content;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sign::ProtocolError;

/// The model label a swarm node advertises for `batch_job`, and the one a
/// buyer requires so the order never lands on a shell-command node.
pub const SWARM_MODEL: &str = "imd-swarm";

/// The most IMD's Intake contract takes as a body.
pub const MAX_SWARM_BODY_BYTES: usize = 16 * 1024;

/// Fields that only make sense on a launch, a cross-chain delivery or a
/// continuation. IMD refuses a job carrying them after taking the payment.
const LAUNCH_ONLY_FIELDS: &[&str] = &[
    "onchain",
    "chainId",
    "pairWith",
    "economics",
    "owner",
    "launch",
    "launchKind",
    "deliver",
    "host",
    "hostLabel",
    "projectId",
    "deploymentLaunchId",
];

/// The actions a swarm node sells, with the Intake action id each one
/// maps to.
pub const SWARM_ACTIONS: &[(&str, &str)] = &[
    ("job.open", "job.open@job-1"),
    ("job.continue", "job.continue@job-1"),
    ("oracle.request", "oracle.request@oracle-1"),
];

/// What a buyer sends: an action and the body IMD documents for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmOrder {
    pub action: String,
    pub body: Value,
}

impl SwarmOrder {
    pub fn new(action: impl Into<String>, body: Value) -> Self {
        SwarmOrder {
            action: action.into(),
            body,
        }
    }

    /// The Intake action id (`job.open@job-1`) for this order's action,
    /// or `None` for an action a swarm node does not sell.
    pub fn action_id(&self) -> Option<&'static str> {
        SWARM_ACTIONS
            .iter()
            .find(|(name, _)| *name == self.action)
            .map(|(_, id)| *id)
    }

    /// The body as compact UTF-8 JSON, the bytes the Intake emits and IMD
    /// validates. Also what the order hashes to.
    pub fn compact_body(&self) -> Result<Vec<u8>, ProtocolError> {
        let bytes = serde_json::to_vec(&self.body)?;
        if bytes.is_empty() || bytes.len() > MAX_SWARM_BODY_BYTES {
            return Err(ProtocolError::Invalid(format!(
                "swarm body is {} bytes; the Intake takes 1 to {} bytes",
                bytes.len(),
                MAX_SWARM_BODY_BYTES
            )));
        }
        Ok(bytes)
    }

    /// Shape checks both sides run before money moves. IMD's own check
    /// route judges the content; this refuses what IMD would refuse for
    /// structure, and what a swarm node will not sell.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.action_id().is_none() {
            let sold: Vec<&str> = SWARM_ACTIONS.iter().map(|(name, _)| *name).collect();
            return Err(ProtocolError::Invalid(format!(
                "action {:?} is not sold by a swarm node; one of {}",
                self.action,
                sold.join(", ")
            )));
        }
        let body = self
            .body
            .as_object()
            .ok_or_else(|| ProtocolError::Invalid("swarm body must be a JSON object".into()))?;
        self.compact_body()?;
        // An oracle question names the chain it is about; a job naming one
        // is asking for a deployment. A cross-chain delivery is refused on
        // every action: the result must come back to this node.
        let refused: &[&str] = if self.action == "oracle.request" {
            &["deliver"]
        } else {
            LAUNCH_ONLY_FIELDS
        };
        for field in refused {
            if body.contains_key(*field) {
                return Err(ProtocolError::Invalid(format!(
                    "swarm body field {field:?} belongs to a launch or a delivery, which a \
                     swarm node does not sell"
                )));
            }
        }
        match self.action.as_str() {
            "job.open" => {
                if body.contains_key("parentJobId") {
                    return Err(ProtocolError::Invalid(
                        "parentJobId belongs to job.continue, not job.open".into(),
                    ));
                }
                require_text(body, "objective", 1, 8_000)?;
                let names_work = ["skill", "template", "steps", "contracts"]
                    .iter()
                    .any(|k| body.contains_key(*k));
                if !names_work {
                    return Err(ProtocolError::Invalid(
                        "a job.open body names a skill, a template, steps or contracts beside \
                         its objective"
                            .into(),
                    ));
                }
            }
            "job.continue" => {
                require_text(body, "parentJobId", 1, 128)?;
                require_text(body, "objective", 1, 8_000)?;
            }
            "oracle.request" => {
                if body.get("v").and_then(Value::as_u64) != Some(1) {
                    return Err(ProtocolError::Invalid(
                        "an oracle body carries \"v\": 1".into(),
                    ));
                }
                require_text(body, "question", 1, 2_000)?;
                for key in ["chainId", "panelSize", "quorum", "validForSeconds"] {
                    if body.get(key).and_then(Value::as_u64).is_none() {
                        return Err(ProtocolError::Invalid(format!(
                            "an oracle body carries a whole-number {key}"
                        )));
                    }
                }
                if !body.get("window").is_some_and(Value::is_object) {
                    return Err(ProtocolError::Invalid(
                        "an oracle body carries a window: {hours} or {fromBlock, toBlock}".into(),
                    ));
                }
                require_text(body, "answerType", 1, 32)?;
            }
            _ => unreachable!("action_id() admitted only the sold actions"),
        }
        Ok(())
    }
}

fn require_text(
    body: &serde_json::Map<String, Value>,
    key: &str,
    min: usize,
    max: usize,
) -> Result<(), ProtocolError> {
    match body.get(key).and_then(Value::as_str) {
        Some(text) if (min..=max).contains(&text.chars().count()) => Ok(()),
        Some(_) => Err(ProtocolError::Invalid(format!(
            "{key} must be {min} to {max} characters"
        ))),
        None => Err(ProtocolError::Invalid(format!(
            "a {key} string is required"
        ))),
    }
}

/// The one JSON block a swarm order rides in.
pub fn swarm_input(order: &SwarmOrder) -> Vec<Content> {
    vec![Content::json(serde_json::json!({
        "swarm": { "action": order.action, "body": order.body },
    }))]
}

/// Reads a swarm order back out of a job's input: the first JSON block
/// with a `swarm` object, or a text block holding the same JSON. Anything
/// else is not an order, and a swarm node refuses it before paying.
pub fn parse_swarm_input(input: &[Content]) -> Result<SwarmOrder, ProtocolError> {
    for block in input {
        let value = match block {
            Content::Json { value } => value.clone(),
            Content::Text { text } => match serde_json::from_str::<Value>(text) {
                Ok(value) => value,
                Err(_) => continue,
            },
        };
        let Some(swarm) = value.get("swarm") else {
            continue;
        };
        let action = swarm
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| ProtocolError::Invalid("swarm order names no action".into()))?;
        let body = swarm
            .get("body")
            .cloned()
            .ok_or_else(|| ProtocolError::Invalid("swarm order carries no body".into()))?;
        let order = SwarmOrder::new(action, body);
        order.validate()?;
        return Ok(order);
    }
    Err(ProtocolError::Invalid(
        "job input carries no swarm order: expected a JSON block {\"swarm\": {\"action\", \
         \"body\"}}"
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn research() -> SwarmOrder {
        SwarmOrder::new(
            "job.open",
            json!({
                "objective": "Write a sourced report on deterministic EVM testing.",
                "skill": "research-report",
                "outputs": [{"name": "report", "path": "artifacts/report.md", "mediaType": "text/markdown"}],
                "github": false
            }),
        )
    }

    #[test]
    fn a_research_job_round_trips_through_the_input_block() {
        let order = research();
        order.validate().expect("a well-formed job");
        assert_eq!(order.action_id(), Some("job.open@job-1"));
        let parsed = parse_swarm_input(&swarm_input(&order)).unwrap();
        assert_eq!(parsed, order);
        // The same JSON as text parses too.
        let text =
            serde_json::to_string(&json!({"swarm": {"action": "job.open", "body": order.body}}))
                .unwrap();
        assert_eq!(parse_swarm_input(&[Content::text(text)]).unwrap(), order);
    }

    #[test]
    fn a_plain_command_is_not_an_order() {
        let err = parse_swarm_input(&[Content::text("echo hi")]).unwrap_err();
        assert!(err.to_string().contains("no swarm order"), "{err}");
        assert!(parse_swarm_input(&[]).is_err());
    }

    #[test]
    fn launches_and_deliveries_are_refused_before_any_payment() {
        for action in ["launch.open", "workflow.open", "schedule.create", "bogus"] {
            let err = SwarmOrder::new(action, json!({"objective": "x", "skill": "y"}))
                .validate()
                .unwrap_err();
            assert!(err.to_string().contains("not sold"), "{action}: {err}");
        }
        let mut order = research();
        order.body["onchain"] = json!("custom_token");
        assert!(order
            .validate()
            .unwrap_err()
            .to_string()
            .contains("onchain"));
        let mut order = research();
        order.body["deliver"] = json!({"chainId": 1});
        assert!(order
            .validate()
            .unwrap_err()
            .to_string()
            .contains("deliver"));
        let mut order = research();
        order.body["parentJobId"] = json!("abc");
        assert!(order
            .validate()
            .unwrap_err()
            .to_string()
            .contains("job.continue"));
    }

    #[test]
    fn a_job_needs_an_objective_and_work_to_do() {
        let err = SwarmOrder::new("job.open", json!({"skill": "research-report"}))
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("objective"), "{err}");
        let err = SwarmOrder::new("job.open", json!({"objective": "only words"}))
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("skill"), "{err}");
        let long = "x".repeat(8_001);
        let err = SwarmOrder::new("job.open", json!({"objective": long, "skill": "s"}))
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("8000"), "{err}");
        assert!(SwarmOrder::new("job.open", json!("not an object"))
            .validate()
            .is_err());
    }

    #[test]
    fn a_continuation_names_its_parent() {
        let ok = SwarmOrder::new(
            "job.continue",
            json!({"parentJobId": "0b6f8f5e-4a7c-4d1e-9b2a-3c5d7e9f1a2b", "objective": "Add a claim page.", "skill": "build-website"}),
        );
        ok.validate().unwrap();
        assert_eq!(ok.action_id(), Some("job.continue@job-1"));
        let err = SwarmOrder::new("job.continue", json!({"objective": "Add a claim page."}))
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("parentJobId"), "{err}");
    }

    #[test]
    fn an_oracle_question_carries_its_panel_terms() {
        let ok = SwarmOrder::new(
            "oracle.request",
            json!({
                "v": 1, "question": "Did the pool trade above 1 ETH?", "chainId": 1,
                "window": {"hours": 24}, "answerType": "bool", "evidence": "panel",
                "panelSize": 5, "quorum": 4, "validForSeconds": 86400
            }),
        );
        ok.validate().unwrap();
        for missing in [
            "v",
            "question",
            "chainId",
            "window",
            "answerType",
            "panelSize",
            "quorum",
            "validForSeconds",
        ] {
            let mut body = ok.body.clone();
            body.as_object_mut().unwrap().remove(missing);
            let err = SwarmOrder::new("oracle.request", body)
                .validate()
                .unwrap_err();
            assert!(!err.to_string().is_empty(), "{missing} is required");
        }
    }

    #[test]
    fn the_body_is_bounded_by_what_the_intake_takes() {
        let mut order = research();
        order.body["padding"] = json!("p".repeat(MAX_SWARM_BODY_BYTES));
        let err = order.validate().unwrap_err();
        assert!(err.to_string().contains("16384"), "{err}");
        assert!(research().compact_body().unwrap().len() < 400);
    }
}
