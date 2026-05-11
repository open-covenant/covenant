//! Intent router for Covenant.
//!
//! Routes incoming intents to registered agent capability cards via
//! keyword-overlap matching. Reserved capability namespaces in agent
//! manifests drive the keyword tables; new agents are picked up by
//! placing an `agent.toml` under `$COVENANT_HOME/agents/`.

#![deny(unsafe_code)]

use covenant_manifest::Manifest;
use std::path::{Path, PathBuf};
use tracing::debug;

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
                for kw in capability_keywords(cap) {
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
}
