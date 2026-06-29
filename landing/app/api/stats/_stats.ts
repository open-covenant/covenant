export function parseMetrics(md: string) {
  return {
    tests: md.match(/([\d,]+)\s+source-discovered Rust tests/i)?.[1] ?? null,
    live: md.match(/([\d,]+)\s+live boundary tests/i)?.[1] ?? null,
    crates: md.match(/(\d+)\s+Rust crates/i)?.[1] ?? null,
  };
}

export function ghHeaders() {
  const token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN;
  return {
    "user-agent": "covenant-hud",
    accept: "application/vnd.github+json",
    ...(token ? { authorization: `Bearer ${token}` } : {}),
  };
}
