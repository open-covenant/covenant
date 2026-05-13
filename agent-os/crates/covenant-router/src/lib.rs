//! Intent router for Covenant.
//!
//! Routes incoming intents to registered agent capability cards via
//! keyword-overlap matching. Reserved capability namespaces in agent
//! manifests drive the keyword tables; new agents are picked up by
//! placing an `agent.toml` under `$COVENANT_HOME/agents/`.

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
        Self { agents: cards }
    }

    pub fn register(&mut self, card: AgentCard) {
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
                    // Capability has no entry in the keyword table — the
                    // agent declared it but routing can't match anything
                    // to it. Surface as warn! so the operator sees the
                    // silent drop instead of wondering why their agent
                    // never receives intents.
                    warn!(
                        agent = %agent.id,
                        capability = %cap,
                        "router: capability has no keyword bridge; intents will not match this agent on it"
                    );
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
    fn route_pins_insertion_order_tie_breaking_on_equal_scores() {
        // covenant_router::Router::route (line 78-115) picks the
        // highest-scoring agent against an intent. The tie-breaker is
        // documented through the strict greater-than comparison on
        // line 106:
        //
        //   best.as_ref().is_none_or(|b| score > b.score)
        //
        // When two agents produce the same score (e.g., both declare
        // the same capability and the intent text overlaps the same
        // keywords), the FIRST agent encountered in self.agents
        // iteration wins. self.agents is a Vec, so insertion order is
        // the deterministic tie-breaker.
        //
        // picks_higher_score_when_two_match (line 223) exercises the
        // no-tie case (research scores 2 on 'find papers and
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
        // covenant_router::Router::route (line 78-115) computes
        //
        //   let lowered = text.to_lowercase();
        //
        // on line 79 and feeds `lowered` into `lowered.contains(kw)`
        // on line 99. Every keyword in capability_keywords (line
        // 121-134) is lowercase, so the to_lowercase() call IS the
        // case-insensitive routing contract: operator inputs like
        // "Find Papers" or "SUMMARIZE this" must score the same
        // keywords as their fully lowercase forms.
        //
        // matches_research_for_paper_intent (line 216) only passes a
        // lowercase query, so the to_lowercase() call is not
        // operator-pinned. A refactor that dropped it under a
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
                 if this fires, text.to_lowercase() on line 79 has been \
                 removed or guarded with an inverted condition and routing \
                 is now case-sensitive",
        );
        assert_eq!(
            m.agent_id, "research",
            "uppercase query must select the same agent as the lowercase \
             ancestor matches_research_for_paper_intent (line 216); a \
             different agent here means the keyword scoring is no longer \
             case-insensitive",
        );
        assert!(
            m.score >= 2.0,
            "uppercase query must score the same matched keywords \
             ('find' + 'papers') as the lowercase ancestor (line 216 \
             asserts score >= 2.0); a lower score here means at least \
             one keyword fell out because the haystack was no longer \
             lowercased before contains()",
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
        // AgentCard::from_manifest_and_dir (line 27-41) builds the
        // routing-relevant projection: it chains
        // m.capabilities.required.iter() THEN
        // m.capabilities.optional.iter() and clones m.agent.id and
        // m.agent.name independently (different manifest paths).
        //
        // from_manifest_collects_required_and_optional (line 443-462)
        // checks card.id and uses .contains() for capabilities — it
        // does NOT pin the required-before-optional ORDER, the
        // card.name field separately from id, dedup-free behavior, or
        // the manifest preservation. A refactor that swapped the chain
        // order would silently change Router::route iteration order;
        // a refactor that deduped capabilities would change scoring
        // when an operator's agent.toml accidentally listed the same
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
    fn router_error_manifest_display_message_pins_prefix_path_then_source_ordering_and_display_formatting_on_both_fields() {
        let err = RouterError::Manifest {
            path: PathBuf::from("/tmp/missing-agent/agent.toml"),
            source: covenant_manifest::ManifestError::Validation("agent.id must not be empty".into()),
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
}
