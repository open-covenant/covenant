# Blockers

Real human-only blockers. Each entry includes the exact action required.

---

## [Low] Web-search API key (optional — makes the research agent's answers actually useful)

### Why this is here
LLM is no longer blocked: `qwen2.5:7b` running locally on Ollama returns coherent summaries end-to-end (verified 2026-05-05, ~11 s round-trip). What the local LLM doesn't have is a live search context — it falls back to honestly saying "I couldn't find relevant information in the provided search results." A Brave or SerpAPI key fixes that.

### Exact action required from human
Pick one and write to `~/.covenant/secrets.toml`:

```toml
[search]
provider = "brave"          # or "serpapi"
api_key  = "BSA-..."        # or "SERPAPI_KEY=..."
```

Brave gives 2,000 free queries/month at https://api.search.brave.com/. SerpAPI is paid but more comprehensive.

### Work that can continue
Everything. The agent already produces coherent local-LLM summaries; only the *quality* of summaries depends on real search context. No build paths block on this.

### Priority
Low. Convenience improvement, not a build blocker.

---

## Operator-driven events (not blockers, listed for visibility)

These don't block any build path. Listed so future autonomous sessions don't accidentally treat them as blockers.

| Event | Owner | Note |
|---|---|---|
| Token launch on pump.fun | Operator | Launch when the operator decides; the build does not depend on a launched token. Settlement on-chain wiring (Phase 5) will accept whatever SPL mint address gets created. |
| `achillewasque` / `iko-rane` / `nr00x` GitHub onboarding | Operator | Assumed done. When live, GitHub will retroactively attribute committed history to the right accounts via the email + verified-key match. The local hooks and rotation already work without it. |

---

## Resolved (kept for history)

- ~~**[Medium] Live LLM**~~ — resolved 2026-05-05. `qwen2.5:7b` via Ollama works locally; pulled models include `qwen2.5:7b`, `qwen2.5-coder:7b`, `hf.co/NousResearch/Hermes-4.3-36B-GGUF:Q4_K_M`, `hf.co/OBLITERATUS/gemma-4-E4B-it-OBLITERATED:Q5_K_M`. Default in `~/.covenant/secrets.toml` is `qwen2.5:7b`. Anthropic / OpenAI / DeepSeek paths still wired but not needed.
