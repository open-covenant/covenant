import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Governed agentic trading";
const DESCRIPTION =
  "Brokerages are opening to AI agents with no real limits. Covenant is the policy gate in front of the order: caps enforced before anything is placed, every decision signed and anchored onchain, and a track record an agent can prove.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/trading" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/trading",
    title: TITLE,
    description: DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: TITLE,
    description: DESCRIPTION,
  },
};

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const cmdBlock =
  "block overflow-x-auto whitespace-pre rounded border border-neutral-800 bg-neutral-950 px-4 py-3 font-mono text-[12.5px] leading-relaxed text-neutral-100 sm:text-[13px]";
const link =
  "underline decoration-neutral-700 underline-offset-4 transition-colors hover:text-neutral-50 hover:decoration-neutral-300";

const DECISIONS: {
  case_: string;
  decision: "executed" | "blocked";
  reason: string;
  tx: string;
  asset: string;
}[] = [
  {
    case_: "Buy $60 of BTC, within policy",
    decision: "executed",
    reason: "inside every cap, allowed universe",
    tx: "5jQ1Pt33QpVnohcGtXUStVSuDZxSn3EnYe6CgbpdQRJTDSD4MHsuRY8gzKmK1CgXgd73EyizriRyE1E1JmTm93cR",
    asset: "J1hxSfgUTqy9x39f2CT7YwWKcGyHY1hsdBoCRDdixXaX",
  },
  {
    case_: "Buy $1,200 of BTC",
    decision: "blocked",
    reason: "per-order cap exceeded (1200.00 > 500.00)",
    tx: "nTkBuP1tXTDXGT8ufKjWsdf64NkUeDDhSTtTGq6fvKzH8rdQxSW4d2ZmmA7xYPhopU38nTimFwCuRu4fygqLhPP",
    asset: "9W55tEUh3vPrMNgJtzkaFCzr4pi889rEYS98wLGWYBHU",
  },
  {
    case_: "Buy 1,000 DOGE",
    decision: "blocked",
    reason: "symbol DOGE-USD is not on the allow list",
    tx: "5Cuy7pNySPPo8MFHxj4BU8Ri8nrzwc7qoqztQmP74hjA8ziFethHCrd2ni9QRDc6oGCs5D6e561962cAwYBGLn4U",
    asset: "3MnEVtpbMr4WWuz51BhbYxo4vLJ1kgThhxpuDLuW7MsV",
  },
  {
    case_: "Buy $450 of BTC, over the approval line",
    decision: "blocked",
    reason: "approval required and not granted",
    tx: "44DGtp58dqXW8VtsozSwWmyHBcw3EKxAyCJM9TMuuU62HswjG6aBcnVvHXzQJgWAH9joQvW6a3LWQYqrisB71fgw",
    asset: "HF9ybagk3mzms2DWL3DD7Qyp7L3XwdB2pdmkRejLy6XC",
  },
];

const short = (s: string) => `${s.slice(0, 8)}…${s.slice(-6)}`;

export default function TradingPage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-4xl px-5 pb-24 pt-14 sm:px-8">
        <p className={eyebrow}>policy &middot; receipts &middot; track record</p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">
          Governed agentic trading
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Brokerages are opening to AI agents. Robinhood now lets any MCP agent read a portfolio and
          place real orders, and its safety model is a funded sub-account, a notification feed, and a
          disconnect button. There is no per-trade cap, no symbol allowlist, no daily-loss stop, and
          no audit a third party can check. The only limit on a looping agent is the account balance.
        </p>
        <p className={`${paragraph} mt-4 max-w-2xl`}>
          Covenant is the layer that governs the order before it leaves. A policy the operator writes
          is enforced ahead of the venue, every decision, placed or refused, becomes a signed receipt
          anchored onchain, and the receipt history compounds into a track record an agent can prove
          to anyone.
        </p>

        <section className="mt-12 grid gap-4 sm:grid-cols-3">
          {[
            {
              title: "The policy",
              body: "Per-order and daily caps, an allow/deny universe, permitted sides and order types, rate limits, a daily-loss stop, and a human-approval threshold. Enforced before the order is sent, not observed after.",
            },
            {
              title: "The receipt",
              body: "Every decision is signed ed25519 over a canonical hash and anchored onchain, refusals included. Change one field and verification fails. An activity feed inside a broker app can't do that.",
            },
            {
              title: "The record",
              body: "Receipts roll up into mandate adherence and activity: how often the gate had to refuse the agent, and what it actually did. A track record that travels between venues and can't be self-asserted.",
            },
          ].map((c) => (
            <div key={c.title} className="rounded border border-neutral-800 bg-neutral-950/60 p-5">
              <h2 className="text-[13px] uppercase tracking-[0.22em] text-neutral-100">{c.title}</h2>
              <p className={`${paragraph} mt-3 text-neutral-400`}>{c.body}</p>
            </div>
          ))}
        </section>

        <section className="mt-12">
          <p className={eyebrow}>the policy an operator writes</p>
          <code className={`${cmdBlock} mt-3`}>
            {`{
  "venue": "robinhood-crypto",
  "mode": "dry_run",
  "caps": { "per_order_usd": 500, "daily_notional_usd": 2000 },
  "risk": { "daily_loss_stop_usd": 300 },
  "universe": { "allow": ["BTC-USD", "ETH-USD"], "sides": ["buy"] },
  "order_types": ["market"],
  "rate": { "max_orders_per_min": 10 },
  "approvals": { "require_human_over_usd": 400 }
}`}
          </code>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>live decisions, anchored on solana mainnet</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            The same agent asked for four trades under the policy above. One executed, three were
            refused, and all four decisions are anchored as verifiable records on Solana mainnet,
            written by the Covenant attestation authority. The venue leg runs dry-run, no brokerage
            account attached; the policy, the signatures, and the anchors are real.
          </p>
          <div className="mt-4 space-y-3">
            {DECISIONS.map((d) => (
              <div key={d.asset} className="rounded border border-neutral-800 bg-neutral-950/60 p-4">
                <div className="flex flex-wrap items-baseline justify-between gap-2">
                  <p className="text-[13px] text-neutral-100">{d.case_}</p>
                  <p
                    className={`font-mono text-[11px] uppercase tracking-[0.22em] ${
                      d.decision === "executed" ? "text-emerald-400" : "text-red-400"
                    }`}
                  >
                    {d.decision}
                  </p>
                </div>
                <p className={`${paragraph} mt-1 text-neutral-500`}>{d.reason}</p>
                <p className="mt-2 font-mono text-[11.5px] text-neutral-500">
                  <a className={link} href={`https://solscan.io/tx/${d.tx}`}>
                    tx {short(d.tx)}
                  </a>
                  {"  ·  "}
                  <a className={link} href={`https://solscan.io/token/${d.asset}`}>
                    record {short(d.asset)}
                  </a>
                </p>
              </div>
            ))}
          </div>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>run it &middot; no keys, no account</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            The whole gate runs locally in dry-run with zero credentials, from the open-source crate:
          </p>
          <code className={`${cmdBlock} mt-3`}>
            {`git clone https://github.com/open-covenant/covenant -b feat/robinhood
cd covenant/agent-os
cargo run -p covenant-robinhood --example governed_demo`}
          </code>
          <p className={`${paragraph} mt-2 text-neutral-500`}>
            Live execution attaches your own API key on your own account, held in a signing sidecar
            the daemon never reads. Covenant is infrastructure you run, not a broker, adviser, or
            custodian, and it never holds funds.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>us beta testers wanted</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            We&apos;re extending the same gate to Robinhood&apos;s hosted agentic MCP, and that beta is
            US-only. If you&apos;re trading with an agent on it, we&apos;re looking for five testers to
            run the proxy and help pin down the connect flow. Write{" "}
            <a className={link} href="mailto:contact@opencovenant.org">
              contact@opencovenant.org
            </a>{" "}
            or DM{" "}
            <a className={link} href="https://x.com/OpenCovenant">
              @OpenCovenant
            </a>
            .
          </p>
        </section>

        <p className={`${paragraph} mt-14 text-[11.5px] text-neutral-600`}>
          Covenant is not affiliated with, endorsed by, or sponsored by Robinhood Markets, Inc.
          Nothing here is investment advice. Agentic trading involves significant risk.
        </p>
      </main>
      <SiteFooter />
    </>
  );
}
