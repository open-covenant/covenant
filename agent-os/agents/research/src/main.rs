//! Research agent.
//!
//! Reads one JSON line on stdin (`Intent`), writes one JSON line on stdout
//! (`{"text": ..., "sources": [...]}`). When `~/.covenant/secrets.toml`
//! configures an LLM and search provider, runs them; otherwise falls back
//! to canned text so the Phase 0 loop stays operational without keys.

use covenant_llm::{ChatMessage, Provider};
use covenant_tools::{SearchHit, SearchProvider};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct Intent {
    text: String,
}

#[derive(Debug, Serialize)]
struct AgentResult {
    text: String,
    sources: Vec<String>,
}

fn covenant_home() -> PathBuf {
    if let Ok(p) = std::env::var("COVENANT_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".covenant")
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    let intent: Intent = serde_json::from_str(buf.trim()).map_err(std::io::Error::other)?;

    let secrets = covenant_home().join("secrets.toml");
    let llm = covenant_llm::pick_provider(&secrets).await;
    let search = covenant_tools::pick_search(&secrets);

    let result = run(&intent.text, llm.as_ref(), search.as_ref()).await;
    emit(&result)
}

async fn run(text: &str, llm: &dyn Provider, search: &dyn SearchProvider) -> AgentResult {
    let hits = search.search(text, 5).await.unwrap_or_default();
    let llm_name = llm.name();
    let search_name = search.name();

    if llm_name == "mock" && search_name == "mock" {
        return canned(text, &hits);
    }

    let context = build_context(&hits);
    let messages = vec![
        ChatMessage::system(
            "You are a research agent. Summarise the search results into a concise answer to the user's question. Cite source URLs when relevant. If no search results are provided, answer from your own knowledge but flag the absence.",
        ),
        ChatMessage::user(format!("Question: {text}\n\n{context}")),
    ];

    match llm.complete(&messages).await {
        Ok(summary) => AgentResult {
            text: summary,
            sources: hits.into_iter().map(|h| h.url).collect(),
        },
        Err(e) => AgentResult {
            text: format!(
                "research agent fell back to canned response (llm '{llm_name}' failed: {e}): {text}"
            ),
            sources: hits.into_iter().map(|h| h.url).collect(),
        },
    }
}

fn build_context(hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return "No search results available.".into();
    }
    let mut s = String::from("Search results:\n");
    for (i, h) in hits.iter().enumerate() {
        s.push_str(&format!(
            "{}. {} ({})\n   {}\n",
            i + 1,
            h.title,
            h.url,
            h.snippet
        ));
    }
    s
}

fn canned(text: &str, hits: &[SearchHit]) -> AgentResult {
    AgentResult {
        text: format!("research stub processed: {text}"),
        sources: if hits.is_empty() {
            vec!["stub://no-real-search".into()]
        } else {
            hits.iter().map(|h| h.url.clone()).collect()
        },
    }
}

fn emit(result: &AgentResult) -> std::io::Result<()> {
    use std::io::Write;
    let line = serde_json::to_string(result).map_err(std::io::Error::other)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(line.as_bytes())?;
    stdout.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_llm::MockProvider;
    use covenant_tools::MockSearch;

    #[tokio::test]
    async fn falls_back_to_canned_when_both_providers_are_mock() {
        let llm = MockProvider::new("ignored");
        let search = MockSearch::stub();
        let r = run("find recent papers", &llm, &search).await;
        assert!(r.text.starts_with("research stub processed: "));
    }

    #[tokio::test]
    async fn uses_llm_when_provider_is_real() {
        // A non-"mock" LLM with a canned response: simulate a configured
        // provider so `run` takes the live path.
        struct FakeLive;
        #[async_trait::async_trait]
        impl Provider for FakeLive {
            fn name(&self) -> &'static str {
                "fake-live"
            }
            async fn complete(
                &self,
                _messages: &[ChatMessage],
            ) -> Result<String, covenant_llm::ProviderError> {
                Ok("a real-looking summary".into())
            }
        }
        let search = MockSearch::stub();
        let r = run("query", &FakeLive, &search).await;
        assert_eq!(r.text, "a real-looking summary");
        // Sources come from the (mock) search hits.
        assert_eq!(r.sources.len(), 1);
    }

    #[tokio::test]
    async fn falls_back_to_canned_when_llm_fails() {
        // A non-"mock" provider whose completion always errors: `run` takes
        // the live path and must surface the failure as a canned fallback
        // rather than panic or emit empty output during a provider outage.
        struct FailingLive;
        #[async_trait::async_trait]
        impl Provider for FailingLive {
            fn name(&self) -> &'static str {
                "failing-live"
            }
            async fn complete(
                &self,
                _messages: &[ChatMessage],
            ) -> Result<String, covenant_llm::ProviderError> {
                Err(covenant_llm::ProviderError::Empty)
            }
        }
        let search = MockSearch::stub();
        let r = run("what is x?", &FailingLive, &search).await;
        assert!(
            r.text.contains("fell back to canned response"),
            "expected fallback marker, got: {}",
            r.text
        );
        // The failing provider's name and the original question are surfaced.
        assert!(r.text.contains("failing-live"), "missing provider name: {}", r.text);
        assert!(r.text.contains("what is x?"), "missing original question: {}", r.text);
        // Sources still flow from the search hits so the caller isn't left empty.
        assert_eq!(r.sources.len(), 1, "expected the stub search hit as a source");
    }
}
