//! Live integration tests against a real local Ollama server.
//!
//! Requires `ollama serve` running on `http://localhost:11434` with the
//! `nomic-embed-text` model pulled. `#[ignore]`d by default. Run with
//! `cargo test -p covenant-llm -- --ignored live_`.

use covenant_llm::{ChatMessage, Embedder, OllamaEmbedder, OllamaProvider, Provider};
use serde_json::Value;

async fn assert_model_available(model: &str) {
    let tags: Value = reqwest::get("http://localhost:11434/api/tags")
        .await
        .unwrap_or_else(|e| panic!("ollama unreachable at http://localhost:11434: {e}"))
        .json()
        .await
        .unwrap_or_else(|e| panic!("ollama tags returned invalid json: {e}"));
    let models = tags["models"]
        .as_array()
        .unwrap_or_else(|| panic!("ollama tags response missing models array: {tags:?}"));
    let want = if model.contains(':') {
        model.to_string()
    } else {
        format!("{model}:latest")
    };
    let found = models.iter().any(|m| {
        m.get("name")
            .and_then(|v| v.as_str())
            .is_some_and(|name| name == want || name == model)
            || m.get("model")
                .and_then(|v| v.as_str())
                .is_some_and(|name| name == want || name == model)
    });
    assert!(
        found,
        "ollama model {want:?} is not available; run `ollama pull {want}` first"
    );
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[tokio::test]
#[ignore = "live: needs Ollama running with nomic-embed-text"]
async fn live_ollama_embeds_real_text() {
    assert_model_available("nomic-embed-text").await;
    let e = OllamaEmbedder::local("nomic-embed-text");
    let v = e
        .embed("agent memory and retrieval")
        .await
        .expect("ollama embedding");
    assert!(!v.is_empty(), "embedding should not be empty");
    // nomic-embed-text v1 ships 768-d vectors. Size check is a defensive
    // smoke; the real assertion is that the vector is non-trivial.
    assert!(v.len() >= 256, "embedding too short: {} dims", v.len());
    let nz = v.iter().filter(|x| **x != 0.0).count();
    assert!(nz > v.len() / 2, "embedding looks zero-padded");
}

#[tokio::test]
#[ignore = "live: needs Ollama running with nomic-embed-text"]
async fn live_ollama_semantic_similarity_holds() {
    assert_model_available("nomic-embed-text").await;
    let e = OllamaEmbedder::local("nomic-embed-text");
    let q1 = e.embed("agent memory retrieval").await.unwrap();
    let q2 = e.embed("how do agents recall information").await.unwrap();
    let q3 = e.embed("the price of avocados in chile").await.unwrap();

    let close = cosine(&q1, &q2);
    let far = cosine(&q1, &q3);
    // Real backend: related queries should cosine-correlate higher than the
    // unrelated one. The threshold is loose because the model + tokenizer
    // can shift between Ollama versions.
    assert!(
        close > far,
        "expected semantic ordering: close={close:.3} > far={far:.3}",
    );
    assert!(
        close > 0.3,
        "related queries should be reasonably similar (got {close:.3})",
    );
}

#[tokio::test]
#[ignore = "live: needs Ollama running with qwen2.5:7b"]
async fn live_ollama_chat_completes() {
    assert_model_available("qwen2.5:7b").await;
    let p = OllamaProvider::local("qwen2.5:7b");
    let r = p
        .complete(&[
            ChatMessage::system("Reply in exactly one short sentence."),
            ChatMessage::user("What is 2+2? Reply with just the number."),
        ])
        .await
        .expect("ollama chat completion");
    assert!(!r.trim().is_empty(), "completion should have content");
    // The model is non-deterministic; assert only that the answer mentions
    // "4" somewhere. If qwen ever stops getting basic arithmetic right,
    // we have larger problems than test breakage.
    assert!(r.contains('4'), "expected '4' in response, got: {r:?}");
}
