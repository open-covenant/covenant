import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Agent trading policy demo";
const DESCRIPTION =
  "A dry-run policy evaluation with Covenant-authored Solana records. It does not prove live brokerage enforcement, execution, or trading performance.";

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
  decision: "allowed" | "blocked";
  reason: string;
  tx: string;
  asset: string;
}[] = [
  {
    case_: "Buy $60 of BTC, within policy",
    decision: "allowed",
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
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-[88px] sm:px-8 sm:pt-[120px]">
        <p className={eyebrow}>
          dry run &middot; policy &middot; signed records
        </p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">
          Agent trading policy demo
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          This page documents a local dry-run policy demo. No brokerage account
          is attached and no real order is submitted. It is not a claim about
          any brokerage&apos;s current product or controls.
        </p>
        <p className={`${paragraph} mt-4 max-w-2xl`}>
          The demo evaluates four proposed orders against one local policy and
          publishes Covenant-authored records for those results. The records
          authenticate the publisher and bytes. They do not prove that a live
          venue was mediated, that the input history is complete, or that the
          decisions represent trading performance.
        </p>

        <section className="mt-12 grid gap-4 sm:grid-cols-3">
          {[
            {
              title: "The policy",
              body: "The demo evaluator checks per-order and daily caps, an allowlist, sides, order types, rate limits, a daily-loss stop, and an approval threshold. Another execution path can bypass it.",
            },
            {
              title: "The receipt",
              body: "Each sample result has a Covenant-authored signed record. A valid signature detects changed bytes and attributes the record; it does not prove a real order or policy enforcement.",
            },
            {
              title: "The record",
              body: "The four records are evidence from one configured producer. They are not an independently verified track record, portable reputation, or proof of profit and loss.",
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
          <p className={eyebrow}>example records on solana mainnet</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            The demo evaluated four proposals under the policy above: one
            allowed and three blocked. The linked Solana transactions and
            records show that Covenant published corresponding bytes. They do
            not show a brokerage order, venue-side enforcement, or an
            independent review of the policy result.
          </p>
          <div className="mt-4 space-y-3">
            {DECISIONS.map((d) => (
              <div key={d.asset} className="rounded border border-neutral-800 bg-neutral-950/60 p-4">
                <div className="flex flex-wrap items-baseline justify-between gap-2">
                  <p className="text-[13px] text-neutral-100">{d.case_}</p>
                  <p
                    className={`font-mono text-[11px] uppercase tracking-[0.22em] ${
                      d.decision === "allowed"
                        ? "text-emerald-400"
                        : "text-red-400"
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
            The historical experimental branch runs this dry-run with no
            brokerage credentials:
          </p>
          <code className={`${cmdBlock} mt-3`}>
            {`git clone https://github.com/open-covenant/covenant -b feat/robinhood
cd covenant/agent-os
cargo run -p covenant-robinhood --example governed_demo`}
          </code>
          <p className={`${paragraph} mt-2 text-neutral-500`}>
            The demo is not a production trading boundary. Live use would still
            require an isolated credential holder, exact final-order validation,
            one-use approval consumption, venue reconciliation, and tests
            showing that no alternate path bypasses policy.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>integration review</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            To review the policy contract or help build the missing signer and
            venue boundary, write{" "}
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
