//! Per-project allow/deny list for memory auto-ingestion. Closes the
//! `.covenantignore` pin in `00_spec.md` §11.
//!
//! Format mirrors `.gitignore`:
//! - blank lines and `#` comments are skipped
//! - leading `!` negates a previous match
//! - leading `/` anchors the glob to the start of the candidate string;
//!   without it the pattern matches at any offset (substring semantics)
//! - `*` matches any run of non-`/` chars; `**` matches anything; `?` matches
//!   exactly one non-`/` char
//!
//! Matching is "last rule wins" — same precedence as gitignore.

use std::path::Path;

#[derive(Debug, Clone)]
pub struct IgnorePattern {
    raw: String,
    negate: bool,
    anchored: bool,
    glob: String,
}

impl IgnorePattern {
    fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        let (negate, rest) = match trimmed.strip_prefix('!') {
            Some(r) => (true, r),
            None => (false, trimmed),
        };
        let (anchored, glob) = match rest.strip_prefix('/') {
            Some(r) => (true, r.to_string()),
            None => (false, rest.to_string()),
        };
        if glob.is_empty() {
            return None;
        }
        Some(Self {
            raw: line.to_string(),
            negate,
            anchored,
            glob,
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn negate(&self) -> bool {
        self.negate
    }

    fn matches(&self, text: &str) -> bool {
        let pat = self.glob.as_bytes();
        let txt = text.as_bytes();
        if self.anchored {
            return glob_match_prefix(pat, txt);
        }
        (0..=txt.len()).any(|start| glob_match_prefix(pat, &txt[start..]))
    }
}

#[derive(Debug, Clone, Default)]
pub struct IgnoreSet {
    patterns: Vec<IgnorePattern>,
}

impl IgnoreSet {
    pub fn parse(content: &str) -> Self {
        let patterns = content.lines().filter_map(IgnorePattern::parse).collect();
        Self { patterns }
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Self::parse(&s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    pub fn patterns(&self) -> &[IgnorePattern] {
        &self.patterns
    }

    /// Verdict for `text` under last-rule-wins semantics. Returns the
    /// matching pattern (if any) so callers can surface *why*.
    pub fn check<'a>(&'a self, text: &str) -> IgnoreVerdict<'a> {
        let mut verdict = IgnoreVerdict::default();
        for p in &self.patterns {
            if p.matches(text) {
                verdict = IgnoreVerdict {
                    ignored: !p.negate,
                    matched: Some(p),
                };
            }
        }
        verdict
    }

    pub fn is_ignored(&self, text: &str) -> bool {
        self.check(text).ignored
    }
}

#[derive(Debug, Clone, Default)]
pub struct IgnoreVerdict<'a> {
    pub ignored: bool,
    pub matched: Option<&'a IgnorePattern>,
}

/// Greedy glob matcher with backtracking on `*`/`**`. Returns true when
/// `pat` matches some prefix of `txt`. Single `*` won't cross `/`; `**` does.
fn glob_match_prefix(pat: &[u8], txt: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star_p: Option<usize> = None;
    let mut star_t: usize = 0;
    let mut star_double = false;

    while p < pat.len() {
        if pat[p] == b'*' {
            let dbl = p + 1 < pat.len() && pat[p + 1] == b'*';
            p += if dbl { 2 } else { 1 };
            star_p = Some(p);
            star_t = t;
            star_double = dbl;
            continue;
        }
        let lit_ok = t < txt.len() && (pat[p] == txt[t] || (pat[p] == b'?' && txt[t] != b'/'));
        if lit_ok {
            p += 1;
            t += 1;
            continue;
        }
        if let Some(sp) = star_p {
            if star_t >= txt.len() {
                return false;
            }
            if !star_double && txt[star_t] == b'/' {
                return false;
            }
            star_t += 1;
            t = star_t;
            p = sp;
        } else {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_literals() {
        assert!(glob_match_prefix(b"foo", b"foo"));
        assert!(!glob_match_prefix(b"foo", b"bar"));
        // prefix-match: trailing chars in txt are allowed
        assert!(glob_match_prefix(b"foo", b"foobar"));
        assert!(!glob_match_prefix(b"foobar", b"foo"));
    }

    #[test]
    fn glob_star_no_slash() {
        assert!(glob_match_prefix(b"*.txt", b"a.txt"));
        assert!(glob_match_prefix(b"*.txt", b".txt"));
        assert!(!glob_match_prefix(b"*.txt", b"a/b.txt"));
        assert!(!glob_match_prefix(b"a*b", b"a/b"));
    }

    #[test]
    fn glob_double_star_crosses_slash() {
        assert!(glob_match_prefix(
            b"**/secrets.toml",
            b"deep/nested/secrets.toml"
        ));
        assert!(glob_match_prefix(b"a**b", b"a/x/y/b"));
    }

    #[test]
    fn glob_question() {
        assert!(glob_match_prefix(b"a?c", b"abc"));
        assert!(!glob_match_prefix(b"a?c", b"abbc"));
        assert!(!glob_match_prefix(b"a?c", b"a/c"));
    }

    #[test]
    fn parse_skips_blank_and_comments() {
        let s = IgnoreSet::parse("\n# comment\n  \n*.key\n");
        assert_eq!(s.len(), 1);
        assert_eq!(s.patterns()[0].raw().trim(), "*.key");
    }

    // The second None arm: '/', '!', and '!/' each reduce to an empty
    // glob once the anchor/negation prefix is stripped. They must be
    // dropped, not built into a pattern whose empty glob matches every
    // candidate at offset 0 (which would silently ignore — or, negated,
    // force-include — every memory file).
    #[test]
    fn parse_skips_anchor_or_negation_only_lines() {
        let s = IgnoreSet::parse("/\n!\n!/\n*.key\n");
        assert_eq!(s.len(), 1, "anchor/negation-only lines produce no pattern");
        assert_eq!(s.patterns()[0].raw().trim(), "*.key");
        assert!(s.is_ignored("private.key"));
        assert!(!s.is_ignored("notes.txt"));
    }

    #[test]
    fn negation_reverses_match() {
        let s = IgnoreSet::parse("*.log\n!keep.log\n");
        assert!(s.is_ignored("debug.log"));
        assert!(!s.is_ignored("keep.log"));
    }

    #[test]
    fn last_rule_wins() {
        let s = IgnoreSet::parse("!*.tmp\n*.tmp\n");
        assert!(s.is_ignored("scratch.tmp"));
    }

    #[test]
    fn substring_match_default() {
        let s = IgnoreSet::parse("id_rsa\n");
        assert!(s.is_ignored("summarize ~/.ssh/id_rsa for me"));
    }

    #[test]
    fn anchored_pattern_only_matches_start() {
        let s = IgnoreSet::parse("/.env\n");
        assert!(s.is_ignored(".env file"));
        assert!(!s.is_ignored("see .env file"));
    }

    #[test]
    fn empty_set_ignores_nothing() {
        let s = IgnoreSet::default();
        assert!(!s.is_ignored("anything at all"));
        assert!(s.is_empty());
    }

    #[test]
    fn check_returns_matched_pattern() {
        let s = IgnoreSet::parse("# header\n**/.env*\n");
        let v = s.check("path/to/.env.production");
        assert!(v.ignored);
        assert_eq!(v.matched.unwrap().raw().trim(), "**/.env*");
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let s = IgnoreSet::load(Path::new("/no/such/file/here-xyz")).unwrap();
        assert!(s.is_empty());
    }

    // Unlike a missing file, a real I/O error must surface, not degrade to empty.
    #[test]
    fn load_propagates_non_notfound_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = IgnoreSet::load(dir.path()).unwrap_err();
        assert_ne!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
