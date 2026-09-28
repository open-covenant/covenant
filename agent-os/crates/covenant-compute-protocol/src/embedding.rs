//! Typed embedding input/output over the envelope's `Vec<Content>`.
//!
//! An [`JobKind::Embedding`](crate::JobKind::Embedding) job's input is
//! its `Content::Text` blocks — the texts to embed, in order — and its
//! output is one `Content::Json` block carrying the vectors and the
//! model that produced them. Both sides live here, so a buyer packing a
//! request and the node answering it cannot drift on what an embedding
//! job means — the same contract [`crate::chat`] keeps for a
//! conversation.
//!
//! Vectors are `f32`: that is the precision every embedding model emits,
//! and the receipt binds `result_hash_hex` over the output's JSON form
//! (a `serde_json::Value`, f64-backed and round-trip stable), so the
//! hash a buyer re-checks is over the transmitted bytes, never a
//! re-serialization of these typed fields.

use covenant_mcp::Content;
use serde::{Deserialize, Serialize};

use crate::sign::ProtocolError;

/// The texts an embedding job asks the node to embed, read from the
/// input's `Content::Text` blocks in order. A `Content::Json` block is
/// skipped, so a job may still carry structured metadata alongside its
/// text. An input with no text at all is an error: an embedding job with
/// nothing to embed is malformed, and the node must fail it loudly
/// rather than bill an empty result.
pub fn embedding_texts(input: &[Content]) -> Result<Vec<String>, ProtocolError> {
    let texts: Vec<String> = input
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } if !text.is_empty() => Some(text.clone()),
            _ => None,
        })
        .collect();
    if texts.is_empty() {
        return Err(ProtocolError::Invalid(
            "embedding input carries no text to embed".into(),
        ));
    }
    Ok(texts)
}

/// A completed embedding job's result: the vectors, the model that
/// produced them, and their shared width. One `Content::Json` block of
/// this shape is the whole output — [`embedding_output`] packs it, a
/// buyer reads it with [`parse_embedding_output`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingResult {
    pub model: String,
    pub dimensions: usize,
    pub embeddings: Vec<Vec<f32>>,
}

/// Packs an embedding result into job output: one JSON block. Width is
/// taken from the first vector so a reader learns the dimensionality
/// without indexing into the data.
pub fn embedding_output(model: impl Into<String>, embeddings: Vec<Vec<f32>>) -> Content {
    let dimensions = embeddings.first().map(Vec::len).unwrap_or(0);
    let result = EmbeddingResult {
        model: model.into(),
        dimensions,
        embeddings,
    };
    let value =
        serde_json::to_value(result).expect("an embedding result serializes to JSON infallibly");
    Content::json(value)
}

/// Reads an embedding result back out of job output.
///
/// `Err` means the output is not embedding-shaped (no JSON block
/// carrying `embeddings`) or is malformed — a buyer that asked for
/// embeddings and got a text completion instead should see that plainly,
/// not a silently empty vector list.
pub fn parse_embedding_output(output: &[Content]) -> Result<EmbeddingResult, ProtocolError> {
    for content in output {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("embeddings").is_none() {
            continue;
        }
        let result: EmbeddingResult = serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("embedding output: {e}")))?;
        // A finite-but-huge JSON number deserializes to a non-finite f32
        // (`1e308 as f32` is infinity), which then serializes back to JSON
        // `null` — an invalid embedding a reader can't use. A real vector is
        // all finite, so reject any that isn't rather than pass it on.
        if result.embeddings.iter().flatten().any(|f| !f.is_finite()) {
            return Err(ProtocolError::Invalid(
                "embedding output carries a non-finite value".into(),
            ));
        }
        // `dimensions` is the vectors' shared width, and a reader reshapes
        // or indexes by it; a result whose vectors disagree with it (ragged
        // widths, or a width field that lies about them) would silently
        // misplace components, so reject it rather than pass it on.
        if result
            .embeddings
            .iter()
            .any(|v| v.len() != result.dimensions)
        {
            return Err(ProtocolError::Invalid(
                "embedding output has a vector whose width disagrees with its dimensions".into(),
            ));
        }
        return Ok(result);
    }
    Err(ProtocolError::Invalid(
        "job output carries no embedding block".into(),
    ))
}

/// Reads an embedding result and requires one vector per input text.
/// `expected` is the number of texts the job asked to embed
/// ([`embedding_texts`] over the request input). A conforming operator
/// returns exactly that many vectors in input order; the receipt's
/// `result_hash_hex` binds *what* the operator returned, not *that* it
/// returned one vector per input, so a mismatched count is an upstream
/// fault — caught here before a caller pairs vectors to inputs by position
/// and silently misindexes them.
pub fn parse_embedding_output_expecting(
    output: &[Content],
    expected: usize,
) -> Result<EmbeddingResult, ProtocolError> {
    let result = parse_embedding_output(output)?;
    if result.embeddings.len() != expected {
        return Err(ProtocolError::Invalid(format!(
            "embedding output carries {} vectors for {expected} input texts",
            result.embeddings.len(),
        )));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real embedding carries floats whose f32 JSON form re-serializes
    // differently once parsed back as f64. The operator hashes this block
    // into its receipt and the coordinator re-hashes it after the wire to
    // release escrow, so the two must be byte-identical — the regression
    // behind embedding jobs that executed but never settled.
    #[test]
    fn an_embedding_block_hashes_the_same_after_a_wire_round_trip() {
        // A spread of messy components, the kind a real embedding model
        // returns: most f32 values re-serialize a low bit off once parsed
        // back as f64, which is exactly what the hash must survive.
        let vector: Vec<f32> = (0..64)
            .map(|i| (i as f32 * 0.17 + 0.03).sin() * 0.5)
            .collect();
        let embeddings = vec![vector];
        let output = vec![embedding_output("nomic-embed-text", embeddings)];
        let on_the_wire = serde_json::to_string(&output).unwrap();
        let received: Vec<Content> = serde_json::from_str(&on_the_wire).unwrap();
        assert_eq!(
            on_the_wire,
            serde_json::to_string(&received).unwrap(),
            "the block re-serialized to different bytes after the wire"
        );
        assert_eq!(
            crate::output_hash_hex(&output),
            crate::output_hash_hex(&received),
            "the wire round-trip changed the output hash, so settlement would reject it"
        );
    }

    #[test]
    fn collects_the_text_blocks_in_order() {
        let input = vec![
            Content::text("first"),
            Content::json(serde_json::json!({ "meta": 1 })),
            Content::text("second"),
        ];
        assert_eq!(embedding_texts(&input).unwrap(), vec!["first", "second"]);
    }

    #[test]
    fn an_input_with_no_text_is_malformed() {
        let input = vec![Content::json(serde_json::json!({ "meta": 1 }))];
        let err = embedding_texts(&input).expect_err("nothing to embed");
        assert!(err.to_string().contains("no text"), "got: {err}");
        assert!(
            embedding_texts(&[]).is_err(),
            "an empty input embeds nothing"
        );
        // A blank text block is not something to embed either.
        assert!(embedding_texts(&[Content::text("")]).is_err());
    }

    #[test]
    fn output_round_trips_and_reports_its_width() {
        let vectors = vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5, 0.6]];
        let output = vec![embedding_output("nomic-embed-text", vectors.clone())];
        let result = parse_embedding_output(&output).expect("embedding-shaped");
        assert_eq!(result.model, "nomic-embed-text");
        assert_eq!(result.dimensions, 3);
        assert_eq!(result.embeddings, vectors);
    }

    #[test]
    fn a_text_completion_is_not_an_embedding() {
        let output = vec![Content::text("the answer is blue")];
        let err = parse_embedding_output(&output).expect_err("not embedding-shaped");
        assert!(err.to_string().contains("no embedding block"), "got: {err}");
    }

    #[test]
    fn a_malformed_embedding_block_fails_loudly() {
        let output = vec![Content::json(serde_json::json!({
            "model": "m",
            "dimensions": 2,
            "embeddings": "not-a-list-of-vectors",
        }))];
        let err = parse_embedding_output(&output).expect_err("wrong embeddings type");
        assert!(err.to_string().contains("embedding output"), "got: {err}");
    }

    #[test]
    fn an_empty_result_reports_zero_width_without_panicking() {
        let output = vec![embedding_output("m", vec![])];
        let result = parse_embedding_output(&output).unwrap();
        assert_eq!(result.dimensions, 0);
        assert!(result.embeddings.is_empty());
    }

    #[test]
    fn a_non_finite_component_is_rejected() {
        // A huge finite JSON number deserializes to a non-finite f32, which
        // would re-encode to JSON null; catch it as malformed output.
        let output = vec![Content::json(serde_json::json!({
            "model": "m",
            "dimensions": 2,
            "embeddings": [[0.1, 1e308]],
        }))];
        let err = parse_embedding_output(&output).expect_err("non-finite component");
        assert!(err.to_string().contains("non-finite"), "got: {err}");
    }

    #[test]
    fn a_vector_whose_width_disagrees_with_dimensions_is_rejected() {
        // Ragged widths: dimensions comes from the first vector (3) but the
        // second is only 2 — a reader indexing by dimensions misplaces it.
        let ragged = vec![Content::json(serde_json::json!({
            "model": "m",
            "dimensions": 3,
            "embeddings": [[0.1, 0.2, 0.3], [0.4, 0.5]],
        }))];
        let err = parse_embedding_output(&ragged).expect_err("ragged widths");
        assert!(err.to_string().contains("width"), "got: {err}");
        // A dimensions field that lies about an otherwise-uniform width is
        // caught the same way.
        let mislabeled = vec![Content::json(serde_json::json!({
            "model": "m",
            "dimensions": 5,
            "embeddings": [[0.1, 0.2], [0.3, 0.4]],
        }))];
        let err = parse_embedding_output(&mislabeled).expect_err("width disagrees with dimensions");
        assert!(err.to_string().contains("width"), "got: {err}");
    }

    #[test]
    fn a_vector_count_that_disagrees_with_the_input_is_rejected() {
        let vectors = vec![vec![0.1, 0.2], vec![0.3, 0.4]];
        let output = vec![embedding_output("m", vectors)];
        // Two vectors for two inputs is fine.
        assert!(parse_embedding_output_expecting(&output, 2).is_ok());
        // Two vectors for three inputs — an operator dropped one — is a
        // fault, not a silent positional misindex.
        let err = parse_embedding_output_expecting(&output, 3).expect_err("count mismatch");
        assert!(
            err.to_string().contains("2 vectors for 3 input texts"),
            "got: {err}"
        );
        // More vectors than inputs is caught too.
        assert!(parse_embedding_output_expecting(&output, 1).is_err());
    }
}
