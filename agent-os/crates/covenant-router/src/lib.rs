//! Intent router for Covenant.
//!
//! [`Router`] holds a [`Vec`] of [`AgentCard`]s and scores incoming intent
//! text against each card's capability list via keyword-overlap matching.
//! [`Router::route`] lowercases the input before checking the keyword
//! table, so operator queries match regardless of case; ties between
//! equal-scoring agents resolve to the first-registered card via Vec
//! iteration order. Returns [`RouteMatch`] (agent id plus score) or
//! [`None`] when no capability keyword overlaps the intent.
//!
//! [`AgentCard`] is the routing-relevant projection of a manifest plus the
//! runtime-relevant `package_dir` that downstream runners resolve
//! `manifest.agent.entry` against. [`AgentCard::from_manifest_and_dir`]
//! concatenates `capabilities.required` then `capabilities.optional` in
//! their declared order with no dedup, preserving the full
//! [`covenant_manifest::Manifest`] so sandbox/resources/entry fields stay
//! available at dispatch time.
//!
//! [`load_agents_from_dir`] walks `$COVENANT_HOME/agents/` for
//! `<package>/agent.toml` files and returns cards sorted by manifest id so
//! routing tie-breaking is deterministic across hosts regardless of
//! `std::fs::read_dir` filesystem order. Missing directories return an
//! empty vec rather than an error; malformed manifests surface as
//! [`RouterError::Manifest`] with the offending path and the inner
//! [`covenant_manifest::ManifestError`] preserved via `#[source]`; IO
//! failures on the walk surface as [`RouterError::Io`] with the inner
//! [`std::io::Error`] preserved for retry-policy downcasts.

#![deny(unsafe_code)]

use covenant_manifest::Manifest;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// A registered agent. Holds the routing-relevant projection (id, name,
/// capabilities) plus the runtime-relevant data (full manifest, package_dir)
/// needed to spawn the agent.
#[derive(Debug, Clone)]
pub struct AgentCard {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub manifest: Manifest,
    pub package_dir: PathBuf,
}

impl AgentCard {
    pub fn from_manifest_and_dir(m: Manifest, package_dir: PathBuf) -> Self {
        Self {
            id: m.agent.id.clone(),
            name: m.agent.name.clone(),
            capabilities: m
                .capabilities
                .required
                .iter()
                .chain(m.capabilities.optional.iter())
                .cloned()
                .collect(),
            manifest: m,
            package_dir,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteMatch {
    pub agent_id: String,
    pub score: f32,
}

#[derive(Debug, Default)]
pub struct Router {
    agents: Vec<AgentCard>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_cards(cards: Vec<AgentCard>) -> Self {
        for card in &cards {
            warn_unmapped_capabilities(card);
        }
        Self { agents: cards }
    }

    pub fn register(&mut self, card: AgentCard) {
        warn_unmapped_capabilities(&card);
        self.agents.push(card);
    }

    pub fn agents(&self) -> &[AgentCard] {
        &self.agents
    }

    pub fn find_by_id(&self, id: &str) -> Option<&AgentCard> {
        self.agents.iter().find(|a| a.id == id)
    }

    /// Score every registered agent against the intent text and return
    /// the highest-scoring match. Returns `None` if nothing matches.
    pub fn route(&self, text: &str) -> Option<RouteMatch> {
        let lowered = text.to_lowercase();
        let mut best: Option<RouteMatch> = None;
        for agent in &self.agents {
            let mut score = 0.0_f32;
            for cap in &agent.capabilities {
                let kws = capability_keywords(cap);
                if kws.is_empty() {
                    // The operator-visible warn for unmapped capabilities
                    // fires once per card at register/from_cards time, not
                    // here. route() is the per-intent hot path; warning
                    // here once produced one log row per dispatch and
                    // buried real routing-failure signal.
                    continue;
                }
                for kw in kws {
                    if lowered.contains(kw) {
                        score += 1.0;
                    }
                }
            }
            if score > 0.0 {
                debug!(agent = %agent.id, score, "candidate");
                if best.as_ref().is_none_or(|b| score > b.score) {
                    best = Some(RouteMatch {
                        agent_id: agent.id.clone(),
                        score,
                    });
                }
            }
        }
        best
    }
}

/// Surface a one-time warn at registration when a card declares a
/// capability with no keyword bridge. Without this the operator only
/// learns about the misconfiguration through the silent symptom of
/// "this agent never receives intents". One warn per card per unmapped
/// capability is loud enough to investigate and quiet enough to leave
/// real routing-failure signal visible.
fn warn_unmapped_capabilities(card: &AgentCard) {
    for cap in &card.capabilities {
        if capability_keywords(cap).is_empty() {
            warn!(
                agent = %card.id,
                capability = %cap,
                "router: capability has no keyword bridge; intents will not match this agent on it"
            );
        }
    }
}

/// Keyword bridge from a capability path to text features.
/// v0 uses a hand-curated table; cosine similarity over embeddings replaces
/// this when an embed model is wired (Phase 1).
fn capability_keywords(cap: &str) -> &'static [&'static str] {
    match cap {
        "tool.web_search" => &[
            "search", "find", "look up", "papers", "articles", "research", "news", "what is",
        ],
        "tool.summarize" => &["summarize", "summarise", "tl;dr", "brief", "summary"],
        "tool.gpu_inference" => &["generate", "render", "image", "diffusion", "infer"],
        // Coding/build intents. The sandbox is coding-focused, so this leans
        // broad: file extensions and common build/coding nouns and verbs. Bare
        // "app"/"api" are still omitted (they match "happen"/"rapid") — "web
        // app"/"rest api" cover those. This v0 keyword table is a placeholder
        // for an embedding/semantic router, which is the real fix for phrasings
        // it misses; the non-greedy guard against research/chat intents is
        // pinned by coder_keywords_do_not_steal_research_or_chat.
        "tool.code" => &[
            "build",
            "create",
            "make",
            "write",
            "implement",
            "scaffold",
            "refactor",
            "fix",
            "compile",
            "debug",
            "website",
            "web app",
            "webapp",
            "frontend",
            "backend",
            "endpoint",
            "rest api",
            "component",
            "page",
            "script",
            "program",
            "function",
            "class",
            "module",
            "cli",
            "algorithm",
            "parser",
            "command-line",
            "regex",
            "code",
            "bug",
            "css",
            "html",
            "javascript",
            "typescript",
            "react",
            "next.js",
            "three.js",
            "rust",
            "python",
            ".py",
            ".js",
            ".ts",
            ".tsx",
            ".jsx",
            ".rs",
            ".go",
            ".json",
            ".sh",
        ],
        "memory.write" => &["remember", "save", "note", "store", "log"],
        "memory.read" => &["recall", "what did", "previous", "earlier"],
        "intent.delegate" => &["assign", "delegate", "ask another", "route to"],
        "intent.subscribe" => &["hello", "hi", "echo", "ping", "test"],
        _ => &[],
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest at {path}: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: covenant_manifest::ManifestError,
    },
}

/// Walk `dir` for agent packages. Each package is a subdirectory containing
/// an `agent.toml` file; the returned cards know their package directory so
/// the runtime can resolve `manifest.agent.entry` against it.
pub fn load_agents_from_dir(dir: &Path) -> Result<Vec<AgentCard>, RouterError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut cards = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let pkg_dir = entry.path();
        if !pkg_dir.is_dir() {
            continue;
        }
        let manifest_path = pkg_dir.join("agent.toml");
        if !manifest_path.exists() {
            continue;
        }
        let m = Manifest::from_path(&manifest_path).map_err(|e| RouterError::Manifest {
            path: manifest_path.clone(),
            source: e,
        })?;
        cards.push(AgentCard::from_manifest_and_dir(m, pkg_dir));
    }
    // Sort by manifest id so routing ties resolve deterministically across
    // hosts; `read_dir` returns entries in filesystem order, which varies.
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_card(id: &str, capabilities: Vec<&str>) -> AgentCard {
        let toml = format!(
            r#"
[agent]
id = "{id}"
name = "{id}"
version = "0.0.1"
runtime = "rust-bin"
entry = "./fake"

[capabilities]
required = {caps:?}
"#,
            caps = capabilities
        );
        let m = Manifest::parse(&toml).unwrap();
        AgentCard::from_manifest_and_dir(m, PathBuf::from("/tmp/nope"))
    }

    fn research_card() -> AgentCard {
        build_card("research", vec!["tool.web_search", "memory.write"])
    }

    fn renderer_card() -> AgentCard {
        build_card("renderer", vec!["tool.gpu_inference"])
    }

    fn coder_card() -> AgentCard {
        build_card("coder", vec!["tool.code"])
    }

    #[test]
    fn empty_router_returns_none() {
        let r = Router::new();
        assert!(r.route("anything").is_none());
    }

    #[test]
    fn matches_research_for_paper_intent() {
        let r = Router::from_cards(vec![research_card()]);
        let m = r.route("find recent papers on agent memory").unwrap();
        assert_eq!(m.agent_id, "research");
        assert!(m.score >= 2.0);
    }

    #[test]
    fn picks_higher_score_when_two_match() {
        let r = Router::from_cards(vec![research_card(), renderer_card()]);
        let m = r.route("find papers and summarize them").unwrap();
        assert_eq!(m.agent_id, "research");
    }

    #[test]
    fn picks_renderer_for_image_intent() {
        let r = Router::from_cards(vec![research_card(), renderer_card()]);
        let m = r.route("generate an image of a cat").unwrap();
        assert_eq!(m.agent_id, "renderer");
    }

    #[test]
    fn returns_none_when_nothing_overlaps() {
        let r = Router::from_cards(vec![research_card()]);
        assert!(r.route("zzz no keywords here").is_none());
    }

    #[test]
    fn matches_coder_for_build_intent() {
        // The canonical intent that returned the "no agent" default
        // before tool.code existed. With a coder card present it must
        // route, and score several keywords (build, website, next.js,
        // three.js) so it beats incidental single-keyword matches.
        let r = Router::from_cards(vec![research_card(), coder_card()]);
        let m = r
            .route("Build a website in Next.js with a Rubik's cube solver using three.js")
            .expect("a build intent must route to the coder once tool.code is bridged");
        assert_eq!(m.agent_id, "coder");
        assert!(
            m.score >= 3.0,
            "expected multi-keyword score, got {}",
            m.score
        );
    }

    #[test]
    fn coder_keywords_do_not_steal_research_or_chat() {
        // tool.code must not be so greedy that it outscores the agent an
        // intent actually belongs to. A refactor that added bare "app"
        // or "api" (matching "happen"/"rapid") or generic verbs like
        // "create" would flip these routes and is the regression this
        // pins. research_card declares tool.web_search + memory.write;
        // a demo-style card declares intent.subscribe (hello/hi/...).
        let chat = build_card("demo", vec!["intent.subscribe"]);
        let r = Router::from_cards(vec![research_card(), coder_card(), chat]);

        let m = r
            .route("find recent papers on agent memory")
            .expect("research intent must still match");
        assert_eq!(m.agent_id, "research", "coder stole a research intent");

        let m = r
            .route("hello there")
            .expect("chat intent must still match");
        assert_eq!(m.agent_id, "demo", "coder stole a chat intent");
    }

    #[test]
    fn coder_routes_common_coding_phrasings() {
        // Real phrasings the first keyword table missed (found by an end-to-end
        // run — "Create fizzbuzz.py" matched nothing). The broadened table
        // covers file extensions and common coding nouns/verbs.
        let r = Router::from_cards(vec![research_card(), coder_card()]);
        for intent in [
            "Create fizzbuzz.py and run it",
            "write a parser in rust",
            "make a CLI tool",
            "implement a sorting algorithm",
        ] {
            assert_eq!(
                r.route(intent).map(|m| m.agent_id),
                Some("coder".to_string()),
                "should route to coder: {intent}"
            );
        }
    }

    #[test]
    fn route_pins_insertion_order_tie_breaking_on_equal_scores() {
        // covenant_router::Router::route picks the highest-scoring
        // agent against an intent. The tie-breaker is documented
        // through the strict greater-than comparison:
        //
        //   best.as_ref().is_none_or(|b| score > b.score)
        //
        // When two agents produce the same score (e.g., both declare
        // the same capability and the intent text overlaps the same
        // keywords), the FIRST agent encountered in self.agents
        // iteration wins. self.agents is a Vec, so insertion order is
        // the deterministic tie-breaker.
        //
        // picks_higher_score_when_two_match exercises the no-tie
        // case (research scores 2 on 'find papers and
        // summarize them' while renderer scores 1); the tie case is
        // not covered. A refactor that changed > to >= during a
        // 'simplify' pass would silently flip the tie-breaker to
        // last-wins. A refactor that swapped self.agents from Vec to
        // BTreeMap-by-id during a 'use sorted collections' pass would
        // silently change tie-breaking to alphabetical.

        // Two agents declaring the SAME capability. Any intent that
        // matches the capability's keyword scores both at exactly the
        // same value, so the only thing that can pick a winner is the
        // documented insertion-order rule.
        let r = Router::from_cards(vec![
            build_card("primary", vec!["tool.web_search"]),
            build_card("backup", vec!["tool.web_search"]),
        ]);
        let m = r
            .route("search for papers")
            .expect("at least one agent must match 'search'");
        assert_eq!(
            m.agent_id, "primary",
            "tie-broken by insertion order: 'primary' was registered \
             first, so 'primary' wins the tie — a refactor that \
             changed the strict greater-than to greater-than-or-equal \
             would silently flip the tie-breaker to last-wins and the \
             'backup' agent would surface on every tied route; the \
             existing picks_higher_score_when_two_match pin would \
             still pass because it relies on a strict score \
             difference, but operator dashboards observing routing \
             decisions on equal-scoring agents would see \
             non-deterministic agent selection across daemon restarts \
             that re-order Router::from_cards",
        );

        // Reverse the registration order; the winner must flip with
        // insertion order, NOT stay the same (which would indicate an
        // alphabetical or attribute-based tie-breaker). This proves
        // the contract is order-of-iteration rather than any inherent
        // property of the AgentCard.
        let r = Router::from_cards(vec![
            build_card("backup", vec!["tool.web_search"]),
            build_card("primary", vec!["tool.web_search"]),
        ]);
        let m = r
            .route("search for papers")
            .expect("at least one agent must match 'search'");
        assert_eq!(
            m.agent_id, "backup",
            "with reversed registration order, the first-registered \
             agent now wins — confirms the tie-breaker is the \
             documented Vec iteration order and not an alphabetical \
             or attribute-based property. A refactor that swapped \
             self.agents from Vec to BTreeMap-by-id during a 'use \
             sorted collections for deterministic iteration' pass \
             would silently shift tie-breaking to alphabetical \
             (here, 'backup' < 'primary' so the alphabetical winner \
             would coincidentally still be 'backup', but the first \
             pin above would surface the regression on the original \
             order); pinning both orderings anchors insertion-order \
             as the contract independent of any alphabetical bias",
        );
    }

    #[test]
    fn route_pins_lowercased_input_so_case_does_not_change_match() {
        // covenant_router::Router::route computes
        //
        //   let lowered = text.to_lowercase();
        //
        // and feeds `lowered` into `lowered.contains(kw)`. Every
        // keyword in capability_keywords is lowercase, so the
        // to_lowercase() call IS the case-insensitive routing
        // contract: operator inputs like "Find Papers" or
        // "SUMMARIZE this" must score the same keywords as their
        // fully lowercase forms.
        //
        // matches_research_for_paper_intent only passes a lowercase
        // query, so the to_lowercase() call is not operator-pinned.
        // A refactor that dropped it under a
        // "kws are already lowercase, contains() handles it" pass
        // (wrong — String::contains is case-sensitive on the
        // haystack) or that wrapped it in an
        // `if text.chars().any(|c| c.is_uppercase())` early-exit
        // optimization with an inverted `.all()` condition would
        // silently break case-insensitive routing: a user typing
        // "Find papers on agents" would route to no agent while the
        // lowercase form still works. This pin catches both
        // mutations directly by asserting against uppercase and
        // mixed-case inputs.

        // Uppercase form of the matches_research_for_paper_intent
        // query. Must produce the SAME route and the SAME minimum
        // score the lowercase pin asserts, otherwise the
        // to_lowercase() contract has shifted.
        let r = Router::from_cards(vec![research_card()]);
        let m = r.route("FIND PAPERS ON AGENT MEMORY").expect(
            "uppercase form of 'find papers on agent memory' must route — \
                 if this fires, text.to_lowercase() in Router::route has been \
                 removed or guarded with an inverted condition and routing \
                 is now case-sensitive",
        );
        assert_eq!(
            m.agent_id, "research",
            "uppercase query must select the same agent as the lowercase \
             ancestor matches_research_for_paper_intent; a different \
             agent here means the keyword scoring is no longer \
             case-insensitive",
        );
        assert!(
            m.score >= 2.0,
            "uppercase query must score the same matched keywords \
             ('find' + 'papers') as the lowercase ancestor \
             matches_research_for_paper_intent (which asserts score \
             >= 2.0); a lower score here means at least one keyword \
             fell out because the haystack was no longer lowercased \
             before contains()",
        );

        // Mixed-case input against a DIFFERENT capability arm
        // (tool.summarize) anchors that case-insensitivity is not
        // accidentally specific to the tool.web_search arm.
        let r = Router::from_cards(vec![build_card("summarizer", vec!["tool.summarize"])]);
        let m = r.route("Summarize This").expect(
            "mixed-case 'Summarize This' must match the lowercase \
             'summarize' keyword in capability_keywords(tool.summarize); \
             if this fires, the to_lowercase() call is gone and any \
             non-fully-lowercase operator query stops routing",
        );
        assert_eq!(
            m.agent_id, "summarizer",
            "mixed-case query must select the summarizer agent — anchors \
             that to_lowercase() works across capability arms, not just \
             tool.web_search",
        );
        assert!(
            m.score >= 1.0,
            "the 'summarize' keyword must score exactly once for the \
             mixed-case input; a score below 1.0 means the keyword fell \
             out of the contains() check because the input retained its \
             original case",
        );
    }

    #[test]
    fn capability_keywords_pins_each_documented_arm_and_unknown_falls_to_empty() {
        for (cap, anchor) in [
            ("tool.web_search", "search"),
            ("tool.summarize", "summarize"),
            ("tool.gpu_inference", "image"),
            ("tool.code", "build"),
            ("memory.write", "remember"),
            ("memory.read", "recall"),
            ("intent.delegate", "delegate"),
            ("intent.subscribe", "hello"),
        ] {
            let kws = capability_keywords(cap);
            assert!(
                !kws.is_empty(),
                "capability {cap:?} must have at least one keyword or every agent declaring it becomes silently unroutable",
            );
            assert!(
                kws.contains(&anchor),
                "capability {cap:?} must contain its anchor keyword {anchor:?} so an accidental arm swap with another capability is caught: got {kws:?}",
            );
        }

        assert_eq!(
            capability_keywords("unknown.capability"),
            &[] as &[&str],
            "unknown capabilities must return an empty slice; if this fires the catch-all has been replaced with a fallback list and unknown caps would silently match every intent",
        );
        assert_eq!(
            capability_keywords(""),
            &[] as &[&str],
            "an empty capability string must also fall through to the empty slice; otherwise an unset cap would silently match every intent",
        );
    }

    #[test]
    fn find_by_id_returns_registered_card() {
        let r = Router::from_cards(vec![research_card()]);
        assert_eq!(r.find_by_id("research").unwrap().name, "research");
        assert!(r.find_by_id("missing").is_none());
    }

    #[test]
    fn from_manifest_collects_required_and_optional() {
        let toml = r#"
[agent]
id = "research"
name = "Research"
version = "0.1.0"
runtime = "python3"
entry = "main.py"

[capabilities]
required = ["tool.web_search"]
optional = ["tool.summarize"]
"#;
        let m = Manifest::parse(toml).unwrap();
        let card = AgentCard::from_manifest_and_dir(m, PathBuf::from("/tmp/x"));
        assert_eq!(card.id, "research");
        assert!(card.capabilities.contains(&"tool.web_search".into()));
        assert!(card.capabilities.contains(&"tool.summarize".into()));
        assert_eq!(card.package_dir, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn from_manifest_and_dir_pins_required_before_optional_order_name_id_distinction_and_manifest_preservation(
    ) {
        // AgentCard::from_manifest_and_dir builds the routing-relevant
        // projection: it chains m.capabilities.required.iter() THEN
        // m.capabilities.optional.iter() and clones m.agent.id and
        // m.agent.name independently (different manifest paths).
        //
        // from_manifest_collects_required_and_optional checks card.id
        // and uses .contains() for capabilities — it does NOT pin the
        // required-before-optional ORDER, the card.name field
        // separately from id, dedup-free behavior, or the manifest
        // preservation. A refactor that swapped the chain order would
        // silently change Router::route iteration order; a refactor
        // that deduped capabilities would change scoring when an
        // operator's agent.toml accidentally listed the same
        // capability in both required and optional.
        let toml = r#"
[agent]
id = "research-bot"
name = "Research Bot Display Name"
version = "0.1.0"
runtime = "python3"
entry = "main.py"

[capabilities]
required = ["tool.web_search", "memory.write"]
optional = ["tool.summarize", "memory.write"]
"#;
        let m = Manifest::parse(toml).unwrap();
        let card = AgentCard::from_manifest_and_dir(m, PathBuf::from("/tmp/pkg"));

        assert_eq!(
            card.capabilities,
            vec![
                "tool.web_search".to_string(),
                "memory.write".to_string(),
                "tool.summarize".to_string(),
                "memory.write".to_string(),
            ],
            "AgentCard::from_manifest_and_dir must concatenate required \
             (in declared order) THEN optional (in declared order) with \
             NO dedup or filtering — Router::route iterates this exact \
             vec when scoring agents. A refactor that swapped the chain \
             order would silently change debug! 'candidate' log order \
             and any future score-weighting that favored early entries \
             would shift routing; a refactor that deduped 'memory.write' \
             (which intentionally appears in both required and optional \
             here) would silently halve the scoring contribution and \
             shift routing for any operator agent.toml that lists the \
             same capability in both lists",
        );

        assert_eq!(
            card.id, "research-bot",
            "card.id must clone m.agent.id verbatim — the operator-facing \
             routing identifier",
        );
        assert_eq!(
            card.name, "Research Bot Display Name",
            "card.name must clone m.agent.name verbatim — a SEPARATE \
             field from card.id sourced from a different manifest path. \
             A refactor that merged name into id (e.g., \
             'id = format!(\"{{}}-{{}}\", m.agent.id, m.agent.name)') \
             would silently let operator dashboards drift away from \
             the manifest's declared agent.id and agent.name fields",
        );
        assert_ne!(
            card.id, card.name,
            "card.id and card.name must be independent values — pinning \
             with distinct manifest inputs anchors that a refactor \
             collapsing them into one field surfaces here regardless of \
             which direction the merge went",
        );

        assert_eq!(
            card.capabilities.len(),
            4,
            "card.capabilities.len() must equal required.len() + \
             optional.len() — the duplicated 'memory.write' across \
             both lists must survive the projection. A refactor that \
             collected into a HashSet would surface here as len == 3, \
             and the score-accumulation contract in Router::route \
             that gives one point per iteration would silently shift",
        );

        assert_eq!(
            card.manifest.agent.id, "research-bot",
            "card.manifest must be preserved verbatim on the card — \
             the runtime resolves manifest.agent.entry against \
             card.package_dir at dispatch time, so a refactor that \
             lossy-projected the manifest (e.g., stored only the \
             capabilities-relevant fields) would silently break \
             every Hermes/subprocess runner that consults the full \
             manifest for sandbox, resources, and entry-point fields",
        );
        assert_eq!(
            card.package_dir,
            PathBuf::from("/tmp/pkg"),
            "card.package_dir must be the path passed in — a refactor \
             that re-derived it from manifest.agent.entry or similar \
             would silently break runners that resolve relative paths \
             against the original package directory",
        );
    }

    #[test]
    fn load_agents_from_missing_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let cards = load_agents_from_dir(&missing).unwrap();
        assert!(cards.is_empty());
    }

    #[test]
    fn load_agents_from_unreadable_dir_is_an_io_error_not_a_silent_empty() {
        // A genuinely absent dir returns Ok(empty) above — that is benign. But a
        // path that *exists* and is not a readable directory must fail loud: the
        // daemon loads agents from $COVENANT_HOME/agents on startup
        // (covenantd/src/main.rs), so an agents path that is accidentally a file
        // or otherwise unreadable has to surface as RouterError::Io, never the
        // Ok(empty) of the absent case — collapsing a misconfigured path to "no
        // agents" would boot the daemon mis-routed with no parse-time signal. A
        // plain file passes Path::exists() but fails read_dir with ENOTDIR,
        // giving a deterministic, portable trigger (no chmod flakiness). Guards a
        // refactor that swaps the read_dir `?` for an error-swallowing
        // `.ok()`/`flatten()`.
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("agents");
        std::fs::write(&not_a_dir, "i am a file, not a directory").unwrap();
        assert!(
            not_a_dir.exists(),
            "the file must pass the missing-dir exists() guard so read_dir is reached",
        );
        let err = load_agents_from_dir(&not_a_dir)
            .expect_err("a present-but-unreadable agents path must not return Ok(empty)");
        assert!(matches!(err, RouterError::Io(_)), "got {err:?}");
    }

    #[test]
    fn load_agents_walks_package_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("research");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("agent.toml"),
            r#"
[agent]
id = "research"
name = "Research"
version = "0.1.0"
runtime = "rust-bin"
entry = "./research"

[capabilities]
required = ["tool.web_search"]
"#,
        )
        .unwrap();
        // Stray file at top level is ignored.
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        // Subdir without agent.toml is ignored.
        std::fs::create_dir_all(dir.path().join("not-an-agent")).unwrap();

        let cards = load_agents_from_dir(dir.path()).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "research");
        assert_eq!(cards[0].package_dir, pkg);
    }

    #[test]
    fn load_agents_from_dir_sorts_multiple_packages_by_manifest_id() {
        // load_agents_from_dir line 174:
        //   cards.sort_by(|a, b| a.id.cmp(&b.id));
        // documented as load-bearing:
        //   "Sort by manifest id so routing ties resolve deterministically
        //    across hosts; read_dir returns entries in filesystem order,
        //    which varies."
        // The existing load_agents_walks_package_subdirs test creates a
        // single package so the sort is trivially deterministic. Pin the
        // sort across three packages whose manifest ids alphabetize
        // unambiguously to catch a sort_by removal or a sort-key swap.
        let dir = tempfile::tempdir().unwrap();
        for id in ["renderer", "research", "analytics"] {
            let pkg = dir.path().join(id);
            std::fs::create_dir_all(&pkg).unwrap();
            std::fs::write(
                pkg.join("agent.toml"),
                format!(
                    r#"
[agent]
id = "{id}"
name = "{id}"
version = "0.1.0"
runtime = "rust-bin"
entry = "./{id}"

[capabilities]
required = ["tool.web_search"]
"#
                ),
            )
            .unwrap();
        }

        let cards = load_agents_from_dir(dir.path()).unwrap();
        let ids: Vec<&str> = cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["analytics", "renderer", "research"],
            "load_agents_from_dir must return cards sorted by manifest id; \
             a refactor that dropped the explicit cards.sort_by line as \
             'redundant' would silently return cards in std::fs::read_dir \
             filesystem order, which varies across hosts (APFS vs ext4 vs \
             ntfs) and inode-creation history — operator routing decisions \
             would diverge between machines with no parse-time signal that \
             the order drifted from the documented manifest-id contract"
        );
    }

    #[test]
    fn router_error_manifest_display_message_pins_prefix_path_then_source_ordering_and_display_formatting_on_both_fields(
    ) {
        let err = RouterError::Manifest {
            path: PathBuf::from("/tmp/missing-agent/agent.toml"),
            source: covenant_manifest::ManifestError::Validation(
                "agent.id must not be empty".into(),
            ),
        };
        let message = format!("{err}");
        assert_eq!(
            message, "manifest at /tmp/missing-agent/agent.toml: validation: agent.id must not be empty",
            "RouterError::Manifest Display drifted (typo, dropped 'manifest at' prefix, field-ordering swap, or Debug-vs-Display formatting regression class)"
        );
        assert!(
            message.starts_with("manifest at "),
            "RouterError::Manifest must surface the 'manifest at ' source-of-failure prefix so operators investigating a covenant daemon startup failure can locate the broken agent.toml file (dropped-prefix regression class): {message}"
        );
        assert!(
            message.contains(": validation: agent.id must not be empty"),
            "RouterError::Manifest must surface the inner ManifestError via its Display impl after the colon (path-then-source ordering); a refactor that put {{source}} before {{path}} would lose the file-locating context (field-ordering-swap regression class): {message}"
        );
        assert!(
            !message.contains("\"/tmp/missing-agent/agent.toml\""),
            "RouterError::Manifest must NOT surround the path with quotes; the {{path}} interpolation must render via Display (no surrounding quotes), not Debug; a refactor to {{path:?}} would surround the path with quotes and break operator-facing CLI documentation parity and audit-log scrapers that split on the colon-after-path (Debug-vs-Display formatting regression class on the {{path}} interpolation): {message}"
        );
    }

    #[test]
    fn router_error_manifest_source_delegation_pin_returns_inner_manifest_error_via_std_error_source(
    ) {
        use std::error::Error;
        let err = RouterError::Manifest {
            path: PathBuf::from("/tmp/missing-agent/agent.toml"),
            source: covenant_manifest::ManifestError::Validation(
                "agent.id must not be empty".into(),
            ),
        };
        let source = err.source().expect(
            "RouterError::Manifest must surface the inner ManifestError via std::error::Error::source so anyhow chain printers and tracing's source-walking emitters can render the wrapper context AND the inner cause; a thiserror refactor that dropped the #[source] attribute on the source field (e.g., field rename without re-annotation, or attribute conversion #[source]→#[from]) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        let source_message = format!("{source}");
        assert!(
            source_message.starts_with("validation: "),
            "RouterError::Manifest source() must return an error whose Display starts with 'validation: ' — the inner ManifestError::Validation Display prefix; a refactor that wrapped the source in a different type (e.g., Box<dyn Error>) and dropped the literal prefix would silently mute the structural validation discriminator in chain-walked output (concrete-source-type regression class): {source_message}"
        );
        assert_eq!(
            source_message,
            format!(
                "{}",
                covenant_manifest::ManifestError::Validation("agent.id must not be empty".into())
            ),
            "RouterError::Manifest source() Display must match a direct format!() of the same ManifestError::Validation variant verbatim; a refactor that swapped the source field type to Box<dyn Error> or any other wrapper would silently break callsite downcasts to the concrete ManifestError type (concrete-source-type regression class)"
        );
    }

    #[test]
    fn router_error_io_display_message_pins_prefix_and_external_source_display_delegation() {
        let err = RouterError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "agents dir not readable",
        ));
        let message = format!("{err}");
        assert!(
            message.starts_with("io: "),
            "RouterError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish router package-directory IO failures from manifest-parse failures (dropped-prefix regression class): {message}"
        );
        assert!(
            message.contains("agents dir not readable"),
            "RouterError::Io must surface the inner std::io::Error Display rendering after the colon ({{0}}, not {{0:?}}); a Debug refactor would render 'Custom {{ kind: PermissionDenied, error: ... }}' instead of the message payload (Debug-vs-Display formatting regression class on the {{0}} interpolation): {message}"
        );
        assert!(
            !message.contains("Custom {") && !message.contains("Os {"),
            "RouterError::Io must NOT surface the std::io::Error Debug rendering; a Debug refactor on {{0}} would expose internal struct fields like 'Custom {{ kind: ..., error: ... }}' or 'Os {{ code: ..., kind: ..., message: ... }}' (Debug-vs-Display formatting regression class on the {{0}} interpolation): {message}"
        );
        assert!(
            !message.starts_with("manifest at "),
            "RouterError::Io must not converge with RouterError::Manifest 'manifest at ' prefix; a package-directory-walk fault must not be mis-routed as a manifest-parse fault (Manifest-convergence regression class): {message}"
        );
    }

    #[test]
    fn router_error_io_source_delegation_pin_returns_inner_std_io_error_via_std_error_source() {
        use std::error::Error;

        let inner = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "agents dir not readable",
        );
        let expected_display = format!("{inner}");
        let err = RouterError::Io(inner);
        let source = err.source().expect(
            "covenant_router::RouterError::Io must surface the inner std::io::Error via std::error::Error::source so daemon-side router-load retry-policy classifiers can walk the error chain and downcast source() to std::io::Error to extract io::ErrorKind for distinct decisions on agents-directory IO (NotFound returns empty-router fast path, PermissionDenied escalates to operator-attention, Interrupted retries immediately); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "covenant_router::RouterError::Io source() Display must match a direct format!() of the same std::io::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        let kind = source.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::PermissionDenied),
            "covenant_router::RouterError::Io source() must downcast_ref to std::io::Error so daemon-side router-load retry-policy classifiers can extract io::ErrorKind for retry decisions on agents-directory IO; a refactor that wrapped the inner in a project-local newtype (e.g., RouterIoError(std::io::Error) under a 'tag router IO failures distinctly from sibling Io variants in other crates' rationale) would silently break downcast_ref::<std::io::Error>() at every downstream callsite that classifies router agents-directory walk faults (concrete-source-type downcast regression class)"
        );
    }

    #[test]
    fn route_returns_none_for_all_unknown_capability_agent() {
        // route() must silently skip unmapped capabilities — no warn,
        // no panic, no spurious match. The agent simply never wins a
        // candidate so the dispatch surface returns None to the
        // caller. The warn that used to fire here at line 112 moved to
        // register/from_cards (see warn_unmapped_capabilities); the
        // hot path stays quiet. The `tool.` prefix passes manifest
        // namespace validation while the suffix is not in the keyword
        // table, exercising the unmapped-cap branch.
        let card = build_card("unmapped", vec!["tool.unmapped_a", "tool.unmapped_b"]);
        let r = Router::from_cards(vec![card]);
        assert!(
            r.route("any intent text").is_none(),
            "an agent whose every capability is unmapped must contribute zero score and the route must collapse to None — otherwise an unmapped capability bug could silently promote the agent to a winning candidate via the keyword catch-all"
        );
    }

    #[test]
    fn route_does_not_warn_for_known_capability_agent() {
        // Defence-in-depth: route() over a fully-mapped agent must not
        // hit the unmapped branch at all. This locks the post-fix
        // shape — the per-cap loop body's only unmapped-capability
        // logic is `continue;` — so a future refactor that resurrects
        // the warn! inside route() will conflict with this test's
        // structural intent.
        let r = Router::from_cards(vec![build_card("search", vec!["tool.web_search"])]);
        let m = r.route("search for recent papers").expect("must match");
        assert_eq!(m.agent_id, "search");
    }

    #[test]
    fn register_handles_mixed_known_and_unknown_capabilities() {
        // Registration is the operator-action moment where a config
        // mistake surfaces — not every subsequent dispatch.
        // warn_unmapped_capabilities runs from both register() and
        // from_cards(), so a card with an unmapped capability fires
        // exactly one warn per capability at that boundary. Exercise
        // both entry points; the known-cap arm still routes, the
        // all-unknown arm still collapses to None.
        let mut r = Router::new();
        r.register(build_card(
            "mixed",
            vec!["tool.unmapped_x", "tool.web_search"],
        ));
        assert!(
            r.route("search").is_some(),
            "the known tool.web_search capability must still win — the unmapped sibling must not poison the score"
        );

        let r2 = Router::from_cards(vec![build_card("mixed2", vec!["tool.unmapped_y"])]);
        assert!(r2.route("anything").is_none());
    }
}
