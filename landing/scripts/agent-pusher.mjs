// Streams the local covenant loop's real events to the deployed site's ingest
// endpoint, so the public HUD shows the live loop. Reuses the same bus tail as
// the in-process feeder — it just forwards every event over HTTP.
//
// Run on the loop host, alongside the supervisor:
//   AGENT_INGEST_TOKEN=<secret> SITE_URL=https://opencovenant.org \
//     node scripts/agent-pusher.mjs

import { bus } from "../lib/agentBus.mjs";

const SITE = (process.env.SITE_URL || "http://localhost:3001").replace(/\/$/, "");
const TOKEN = process.env.AGENT_INGEST_TOKEN;

if (!TOKEN) {
  console.error("agent-pusher: set AGENT_INGEST_TOKEN");
  process.exit(1);
}

const url = `${SITE}/api/agent/ingest`;

bus.startLocalTail();
bus.subscribe(async (e) => {
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify(e),
    });
    if (!res.ok) console.error(`agent-pusher: ${res.status} ${await res.text()}`);
  } catch (err) {
    console.error(`agent-pusher: ${err?.message || err}`);
  }
});

console.log(`agent-pusher → ${url} (tailing local loop)`);
setInterval(() => {}, 1 << 30);
