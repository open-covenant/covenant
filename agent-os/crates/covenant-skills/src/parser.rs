//! Parse `SKILL.md` (Anthropic Agent Skill frontmatter + body) and walk
//! a skill directory's `references/**` tree to compute a deterministic
//! content digest.
//!
//! Digest contract: `sha256(`for each tracked file, sorted by relative
//! path: `"{relpath}\t{sha256(normalized_content)}\n"`). Normalization
//! strips `\r`, trims trailing whitespace per line, and trims trailing
//! newlines from the file. Reordering directory listings or rewriting a
//! file with only line-ending / trailing-whitespace differences leaves
//! the digest unchanged; any other edit changes it.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default, rename = "user-invocable")]
    pub user_invocable: Option<bool>,
    #[serde(default)]
    pub metadata: Option<SkillFrontmatterMetadata>,
    #[serde(default, rename = "allowed-tools")]
    pub allowed_tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillFrontmatterMetadata {
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMd {
    pub frontmatter: SkillFrontmatter,
    pub body: String,
}

#[derive(Debug, Error)]
pub enum SkillParseError {
    #[error("SKILL.md is missing the leading `---` frontmatter delimiter")]
    MissingOpenDelimiter,
    #[error("SKILL.md frontmatter is not terminated by a `---` delimiter")]
    MissingCloseDelimiter,
    #[error("SKILL.md frontmatter is not valid YAML: {0}")]
    InvalidYaml(#[from] serde_yaml::Error),
    #[error("filesystem error reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub fn parse_skill_md(content: &str) -> Result<SkillMd, SkillParseError> {
    let normalized = content.replace("\r\n", "\n");
    let mut lines = normalized.split_inclusive('\n');
    let first = lines.next().ok_or(SkillParseError::MissingOpenDelimiter)?;
    if first.trim_end() != "---" {
        return Err(SkillParseError::MissingOpenDelimiter);
    }
    let mut frontmatter_buf = String::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        frontmatter_buf.push_str(line);
    }
    if !closed {
        return Err(SkillParseError::MissingCloseDelimiter);
    }
    let frontmatter: SkillFrontmatter = serde_yaml::from_str(&frontmatter_buf)?;
    let body: String = lines.collect();
    Ok(SkillMd { frontmatter, body })
}

pub fn parse_skill_md_path(path: &Path) -> Result<SkillMd, SkillParseError> {
    let content = fs::read_to_string(path).map_err(|source| SkillParseError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_skill_md(&content)
}

/// Compute the deterministic content digest for a skill rooted at
/// `skill_dir`. The digest hashes `SKILL.md` plus every regular file
/// under `references/**` (recursive); subdirectories outside
/// `references/` are ignored so build artifacts cannot perturb the
/// hash.
pub fn skill_content_digest(skill_dir: &Path) -> Result<String, SkillParseError> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let skill_md_path = skill_dir.join("SKILL.md");
    let skill_md_bytes = fs::read(&skill_md_path).map_err(|source| SkillParseError::Io {
        path: skill_md_path.clone(),
        source,
    })?;
    entries.push(("SKILL.md".to_string(), skill_md_bytes));
    for (rel, path) in reference_files(skill_dir)? {
        let bytes = fs::read(&path).map_err(|source| SkillParseError::Io {
            path: path.clone(),
            source,
        })?;
        entries.push((rel, bytes));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut composite = Sha256::new();
    for (relpath, bytes) in &entries {
        let normalized = normalize_bytes(bytes);
        let mut file_hash = Sha256::new();
        file_hash.update(&normalized);
        let file_hex = hex_encode(&file_hash.finalize());
        composite.update(relpath.as_bytes());
        composite.update(b"\t");
        composite.update(file_hex.as_bytes());
        composite.update(b"\n");
    }
    Ok(format!("sha256:{}", hex_encode(&composite.finalize())))
}

/// Relative paths (`references/...`, forward-slashed, sorted) of every file
/// under `skill_dir/references/**`. This is the bounded set of references a
/// skill may progressively disclose at run-time; [`crate::install_skill`] pins
/// it into the manifest so a load can be refused unless the path was declared
/// at install. Returns an empty vec when the skill ships no `references/` tree.
pub fn reference_paths(skill_dir: &Path) -> Result<Vec<String>, SkillParseError> {
    Ok(reference_files(skill_dir)?
        .into_iter()
        .map(|(rel, _)| rel)
        .collect())
}

/// Walk `skill_dir/references/**` and return `(relative_path, absolute_path)`
/// for every regular file, sorted by relative path. Relative paths are
/// `references/...` with forward slashes so the digest and the pinned manifest
/// set agree byte-for-byte across platforms.
fn reference_files(skill_dir: &Path) -> Result<Vec<(String, PathBuf)>, SkillParseError> {
    let references_dir = skill_dir.join("references");
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    if !references_dir.is_dir() {
        return Ok(out);
    }
    let strip_root = references_dir.parent().unwrap_or(&references_dir);
    let mut stack = vec![references_dir.clone()];
    while let Some(dir) = stack.pop() {
        let read = fs::read_dir(&dir).map_err(|source| SkillParseError::Io {
            path: dir.clone(),
            source,
        })?;
        for entry in read {
            let entry = entry.map_err(|source| SkillParseError::Io {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| SkillParseError::Io {
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let rel = path
                .strip_prefix(strip_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn normalize_bytes(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out.into_bytes()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(hex_char(b >> 4));
        out.push(hex_char(b & 0x0f));
    }
    out
}

fn hex_char(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + nibble - 10) as char,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const VALID_SKILL_MD: &str = "---\nname: covenant\ndescription: verify agent execution on Solana\nlicense: Apache-2.0\nmetadata:\n  author: open-covenant\n  version: 0.1.0\n---\n\n# covenant\n\nbody text here\n";

    #[test]
    fn parses_valid_frontmatter_and_body() {
        let parsed = parse_skill_md(VALID_SKILL_MD).expect("parse");
        assert_eq!(parsed.frontmatter.name, "covenant");
        assert_eq!(
            parsed.frontmatter.description,
            "verify agent execution on Solana"
        );
        assert_eq!(parsed.frontmatter.license.as_deref(), Some("Apache-2.0"));
        let metadata = parsed.frontmatter.metadata.expect("metadata");
        assert_eq!(metadata.author.as_deref(), Some("open-covenant"));
        assert_eq!(metadata.version.as_deref(), Some("0.1.0"));
        assert!(parsed.body.contains("body text here"));
    }

    #[test]
    fn rejects_missing_open_delimiter() {
        let bad = "name: covenant\n---\nbody\n";
        let err = parse_skill_md(bad).unwrap_err();
        assert!(matches!(err, SkillParseError::MissingOpenDelimiter));
    }

    #[test]
    fn rejects_unterminated_frontmatter() {
        let bad = "---\nname: covenant\nbody never closes\n";
        let err = parse_skill_md(bad).unwrap_err();
        assert!(matches!(err, SkillParseError::MissingCloseDelimiter));
    }

    #[test]
    fn rejects_invalid_yaml_cleanly() {
        let bad = "---\nname: : : :\n---\nbody\n";
        let err = parse_skill_md(bad).unwrap_err();
        assert!(matches!(err, SkillParseError::InvalidYaml(_)));
    }

    #[test]
    fn digest_is_stable_across_filesystem_listing_order() {
        let a = TempDir::new().expect("tempdir");
        let b = TempDir::new().expect("tempdir");
        for dir in [a.path(), b.path()] {
            fs::write(dir.join("SKILL.md"), VALID_SKILL_MD).unwrap();
            fs::create_dir(dir.join("references")).unwrap();
            fs::write(dir.join("references/z-last.md"), "zzz\n").unwrap();
            fs::write(dir.join("references/a-first.md"), "aaa\n").unwrap();
            fs::write(dir.join("references/m-mid.md"), "mmm\n").unwrap();
        }
        let digest_a = skill_content_digest(a.path()).expect("digest a");
        let digest_b = skill_content_digest(b.path()).expect("digest b");
        assert_eq!(
            digest_a, digest_b,
            "digest must not depend on directory entry ordering",
        );
        assert!(
            digest_a.starts_with("sha256:") && digest_a.len() == "sha256:".len() + 64,
            "digest must be sha256:<64-hex>",
        );
    }

    #[test]
    fn digest_ignores_crlf_and_trailing_whitespace() {
        let a = TempDir::new().expect("tempdir");
        let b = TempDir::new().expect("tempdir");
        fs::write(a.path().join("SKILL.md"), VALID_SKILL_MD).unwrap();
        let crlf = VALID_SKILL_MD.replace('\n', "\r\n");
        let with_trailing_ws = crlf.replace("body text here", "body text here   ");
        fs::write(b.path().join("SKILL.md"), with_trailing_ws).unwrap();
        let digest_a = skill_content_digest(a.path()).expect("digest a");
        let digest_b = skill_content_digest(b.path()).expect("digest b");
        assert_eq!(
            digest_a, digest_b,
            "line-ending + trailing-whitespace differences must not change the digest",
        );
    }

    #[test]
    fn digest_changes_when_real_content_changes() {
        let a = TempDir::new().expect("tempdir");
        let b = TempDir::new().expect("tempdir");
        fs::write(a.path().join("SKILL.md"), VALID_SKILL_MD).unwrap();
        let mutated = VALID_SKILL_MD.replace("body text here", "different body text");
        fs::write(b.path().join("SKILL.md"), mutated).unwrap();
        let digest_a = skill_content_digest(a.path()).expect("digest a");
        let digest_b = skill_content_digest(b.path()).expect("digest b");
        assert_ne!(
            digest_a, digest_b,
            "a real content change must change the digest",
        );
    }

    #[test]
    fn digest_walks_nested_references() {
        let root = TempDir::new().expect("tempdir");
        fs::write(root.path().join("SKILL.md"), VALID_SKILL_MD).unwrap();
        fs::create_dir_all(root.path().join("references/deep/inner")).unwrap();
        fs::write(root.path().join("references/top.md"), "top\n").unwrap();
        fs::write(root.path().join("references/deep/inner/leaf.md"), "leaf\n").unwrap();
        let digest = skill_content_digest(root.path()).expect("digest");
        assert!(digest.starts_with("sha256:"));
        let no_leaf = TempDir::new().expect("tempdir");
        fs::write(no_leaf.path().join("SKILL.md"), VALID_SKILL_MD).unwrap();
        fs::create_dir(no_leaf.path().join("references")).unwrap();
        fs::write(no_leaf.path().join("references/top.md"), "top\n").unwrap();
        let digest_no_leaf = skill_content_digest(no_leaf.path()).expect("digest");
        assert_ne!(
            digest, digest_no_leaf,
            "adding a nested reference must change the digest",
        );
    }

    #[test]
    fn reference_paths_lists_sorted_relpaths() {
        let dir = TempDir::new().expect("tempdir");
        fs::write(dir.path().join("SKILL.md"), VALID_SKILL_MD).unwrap();
        fs::create_dir_all(dir.path().join("references/deep")).unwrap();
        fs::write(dir.path().join("references/z.md"), "z\n").unwrap();
        fs::write(dir.path().join("references/a.md"), "a\n").unwrap();
        fs::write(dir.path().join("references/deep/b.md"), "b\n").unwrap();
        let paths = reference_paths(dir.path()).expect("reference paths");
        assert_eq!(
            paths,
            vec!["references/a.md", "references/deep/b.md", "references/z.md"],
            "reference paths must be `references/`-rooted, forward-slashed, and sorted",
        );
    }

    #[test]
    fn reference_paths_empty_without_references_dir() {
        let dir = TempDir::new().expect("tempdir");
        fs::write(dir.path().join("SKILL.md"), VALID_SKILL_MD).unwrap();
        assert!(
            reference_paths(dir.path())
                .expect("reference paths")
                .is_empty(),
            "a skill with no references/ tree declares no references",
        );
    }
}
