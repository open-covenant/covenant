//! WURK submission view (free, secret-authed) and winner selection.
//!
//! `GET /{network}/agenttohuman[advanced]?action=view&secret=…&page=N`
//! returns the job's submissions. Selection is `POST
//! /api/agenttohumanadvanced/choose-winners {secret, submissionIds}`.
//! Neither needs a payment.

use serde::Deserialize;

/// One worker submission. `winner` is `0`/`1` in the wire form.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Submission {
    pub id: String,
    #[serde(default)]
    pub content_text: String,
    #[serde(default)]
    pub attachment_urls: Option<Vec<String>>,
    #[serde(default)]
    pub winner: i64,
}

impl Submission {
    pub fn is_winner(&self) -> bool {
        self.winner != 0
    }
}

/// A page of a WURK job view (only the fields we consume).
#[derive(Debug, Clone, Deserialize)]
pub struct JobView {
    #[serde(default)]
    pub ok: bool,
    #[serde(rename = "jobId", default)]
    pub job_id: String,
    #[serde(default)]
    pub submissions: Vec<Submission>,
    #[serde(default)]
    pub total: u64,
    #[serde(rename = "totalPages", default)]
    pub total_pages: u64,
    #[serde(rename = "nextPageUrl", default)]
    pub next_page_url: Option<String>,
}

/// Parse a WURK view response body.
pub fn parse_view(body: &str) -> crate::Result<JobView> {
    serde_json::from_str(body).map_err(|e| crate::WurkError::View(format!("decode view: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed but faithful copy of the live view response for the
    /// Covenant selection job (job c27eb389), captured from production.
    const LIVE_VIEW: &str = r#"{
        "ok": true,
        "jobId": "c27eb389",
        "network": "solana",
        "submissions": [
            {
                "id": "ba5db22d",
                "content_text": "1: no - included the price and missed the BPA-free requirement\n2: no - inaccurately claimed it is open every day instead of just weekdays\n3: yes - accurately summarized the statement in one sentence under 20 words",
                "attachment_urls": ["https://ik.imagekit.io/wurk/IMG_5784_KozBmOmsT.png"],
                "winner": 0
            },
            { "id": "7f207879", "content_text": "Wurk Done!", "attachment_urls": null, "winner": 0 },
            { "id": "963098fd", "content_text": "Yes", "attachment_urls": null, "winner": 0 }
        ],
        "page": 1,
        "pageSize": 50,
        "total": 3,
        "totalPages": 1,
        "nextPageUrl": null
    }"#;

    #[test]
    fn parses_live_view_with_submissions() {
        let view = parse_view(LIVE_VIEW).expect("parse view");
        assert!(view.ok);
        assert_eq!(view.job_id, "c27eb389");
        assert_eq!(view.total, 3);
        assert_eq!(view.submissions.len(), 3);
        let first = &view.submissions[0];
        assert_eq!(first.id, "ba5db22d");
        assert!(first.content_text.contains("1: no"));
        assert_eq!(first.attachment_urls.as_deref().map(|a| a.len()), Some(1));
        assert!(!first.is_winner());
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        let view = parse_view(r#"{"ok":true,"submissions":[{"id":"x"}]}"#).expect("parse");
        assert_eq!(view.submissions[0].id, "x");
        assert_eq!(view.submissions[0].content_text, "");
        assert!(view.submissions[0].attachment_urls.is_none());
        assert!(view.next_page_url.is_none());
    }

    #[test]
    fn rejects_malformed_view() {
        assert!(matches!(
            parse_view("{not json"),
            Err(crate::WurkError::View(m)) if m.contains("decode view")
        ));
    }
}
