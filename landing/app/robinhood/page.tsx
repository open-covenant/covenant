import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Bounded agents on Robinhood Chain: Covenant";
const DESCRIPTION =
  "Two bounds an agent cannot break on Robinhood Chain. What it may spend: a budget, a per-call ceiling, a provider allowlist, and pay-only-for-good-output, enforced by the contract holding the money. What it may trade: every tokenized-equity order checked against the asset's price feed, a per-trade cap, and a daily budget. Both proven live on mainnet in real USDG.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/robinhood" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/robinhood",
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
const link =
  "underline decoration-neutral-700 underline-offset-4 transition-colors hover:text-neutral-50 hover:decoration-neutral-300";

const EXPLORER = "https://robinhoodchain.blockscout.com";
const ESCROW = "0x196b55CF36f5c5a0498A5C7ADE91B5E94dF4d309";
const USDG = "0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168";
const GUARD = "0x1c6cca8De094209DE79A12eD63477434Ec2621c0";
const EXECUTOR = "0xE94A70f8C864cA3CaE85c74F92Ab8783d2d039A3";
const AAPL = "0xaF3D76f1834A1d425780943C99Ea8A608f8a93f9";

type Card = { title: string; body: string };
type Proof = { tag: string; result: string; attempt: string; detail: string; tx: string };

const SPEND_GUARANTEES: Card[] = [
  {
    title: "Bounded spend",
    body: "The agent spends against an onchain grant: a total budget, a per-call ceiling, an allowlist of who it may pay, an expiry. A charge past any bound reverts at the contract. The limit is not a policy the agent is asked to respect, it is a rule the money enforces, which is what lets you let go of the wheel.",
  },
  {
    title: "Pay only for good output",
    body: "Each call's funds sit in escrow and release to the provider only when the result clears the spec. Fail the bar and the funds refund to the grant, in full. You pay for output that passed and nothing else, decided before payout, on every call.",
  },
];

const SPEND_PROOFS: Proof[] = [
  {
    tag: "Over the ceiling",
    result: "Reverted onchain",
    attempt: "The agent tried to charge 2.00 USDG against its 1.00 USDG per-call ceiling.",
    detail:
      "The chain rejected the transaction. Overspending is not a mistake the agent can make; the ceiling holds even when it tries.",
    tx: "0x862036d191728a0497c061bcc8ad8fd3d5634c75fddc3a9008f0f8bcd3b1a31a",
  },
  {
    tag: "Good output",
    result: "Provider paid",
    attempt: "A call returned a result that cleared the spec.",
    detail: "The escrow released 0.45 USDG to the provider, 0.50 less the 10 percent protocol fee.",
    tx: "0x2759a7759db0c789c7fa9cb659d7c1595a0173010e5003a020bf44fd03266dd1",
  },
  {
    tag: "Junk output",
    result: "Refunded in full",
    attempt: "A call returned a result that failed the spec.",
    detail: "The held funds refunded to the grant. The provider was paid nothing.",
    tx: "0xedc35d380d6550800f2f3924892271a1e940f1e9ecc44d5e295b3cd83473e74d",
  },
];

const TRADE_GUARANTEES: Card[] = [
  {
    title: "Priced against the asset's own oracle",
    body: "Every fill is checked against the Chainlink price for that Stock Token, and refused if it sits outside the band or if the price has gone stale. A tokenized stock trades around the clock while its feed only prints when the exchange does, so the overnight gap between the two is exactly where an unattended agent gets filled at a price nobody would accept.",
  },
  {
    title: "Bounded per trade and per day",
    body: "A cap bounds any single trade. A daily budget bounds the hundred trades after it, and refills gradually rather than resetting on a boundary, so waiting for a clock tick does not buy a second full allowance. Both the entry and the exit count against it.",
  },
  {
    title: "It can leave, not just enter",
    body: "Selling runs through the same checks as buying. A position an agent can enter and cannot exit is not a bounded position, so the exit is judged before the token moves, same oracle band, same budget.",
  },
];

const TRADE_PROOFS: Proof[] = [
  {
    tag: "Reckless size",
    result: "Refused onchain",
    attempt: "The agent tried to buy 100 AAPL, about 31,000 dollars, against a 250 dollar per-trade cap.",
    detail: "The transaction reverted in the guard and moved nothing. The order never reached the venue.",
    tx: "0x6cc39206e27a1ff853f7fdea7ed0d77a7e825e306e418091553d3e08407c2c94",
  },
  {
    tag: "Off the oracle",
    result: "Refused onchain",
    attempt: "A trade sized inside the cap, but quoted two percent away from the AAPL price feed.",
    detail: "Refused at 208 basis points against a 50 basis point band. Size alone is not a safe trade.",
    tx: "0xdfc241c269209a175d6b6120a48a65c681eb9101367f9e3bbecbe94755991f46",
  },
  {
    tag: "A trade within the rules",
    result: "Filled",
    attempt: "The agent bought AAPL for 0.30 USDG through the Uniswap v4 pool on Robinhood Chain.",
    detail: "0.000963 AAPL delivered, at the oracle price, charged against the day's budget.",
    tx: "0xae7522f584c5b0c802b4b12fa6b2486375cc602a6043a084ed96d2103bdb4b04",
  },
  {
    tag: "The exit",
    result: "Closed",
    attempt: "The agent sold the position back, under the same bounds it bought under.",
    detail: "0.299790 USDG returned. The trader ended the session flat, down only the venue's spread.",
    tx: "0x3aa75706aa5e88a6421005825b47a3ee76227e80e01cfaada906bf88b8adf5ac",
  },
  {
    tag: "Budget spent",
    result: "Refused onchain",
    attempt: "One more trade, well inside the per-trade cap, after the day's budget was used.",
    detail: "Refused before anything moved. A cap on one trade is not a cap on an agent; this is.",
    tx: "0x3df1e7fc8b2028a5719925e6bb03211b593722101a7d6c371dc43f6fc442c7a9",
  },
];

const short = (hash: string) => `${hash.slice(0, 10)}…${hash.slice(-8)}`;

function Cards({ cards, columns }: { cards: Card[]; columns: string }) {
  return (
    <section className={`mt-10 grid gap-4 ${columns}`}>
      {cards.map((c) => (
        <div key={c.title} className="rounded border border-neutral-800 bg-neutral-950/60 p-5">
          <h3 className="text-[13px] uppercase tracking-[0.22em] text-neutral-100">{c.title}</h3>
          <p className={`${paragraph} mt-3 text-neutral-400`}>{c.body}</p>
        </div>
      ))}
    </section>
  );
}

function Proofs({ proofs }: { proofs: Proof[] }) {
  return (
    <ul className="mt-6 space-y-3">
      {proofs.map((p) => (
        <li key={p.tag} className="rounded border border-neutral-800 bg-neutral-950/60 p-5">
          <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
            <span className="font-mono text-[11px] uppercase tracking-[0.24em] text-neutral-500">
              {p.tag}
            </span>
            <span className="font-mono text-[11px] uppercase tracking-[0.24em] text-neutral-200">
              {p.result}
            </span>
          </div>
          <p className={`${paragraph} mt-3`}>{p.attempt}</p>
          <p className={`${paragraph} mt-1 text-neutral-500`}>{p.detail}</p>
          <a
            className={`mt-3 inline-block font-mono text-[12px] text-neutral-500 ${link}`}
            href={`${EXPLORER}/tx/${p.tx}`}
          >
            {short(p.tx)} &#8599;
          </a>
        </li>
      ))}
    </ul>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-3">
      <dt className="font-mono text-[11px] uppercase tracking-[0.24em] text-neutral-500">{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

export default function RobinhoodPage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-[88px] sm:px-8 sm:pt-[120px]">
        <p className={eyebrow}>robinhood chain &middot; usdg &middot; live onchain</p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">
          Bounded agents
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Hand an agent a wallet on Robinhood Chain and walk away. Two bounds hold it, both
          enforced by the contracts rather than reconciled afterwards: what it may spend, and
          what it may trade. A rogue or buggy agent cannot exceed either, and the failure lands
          at the boundary instead of in your balance.
        </p>

        <section className="mt-16">
          <p className={eyebrow}>what it may spend</p>
          <h2 className="mt-3 text-xl font-extralight tracking-[0.16em] text-neutral-50">
            Bounded agent spend
          </h2>
          <p className={`${paragraph} mt-4 max-w-2xl`}>
            The agent spends against an onchain grant: a total cap, a per-call ceiling, an
            allowlist of who it may pay, an expiry. A rogue agent cannot exceed the cap, pay a
            stranger, or overpay for junk output.
          </p>
          <Cards cards={SPEND_GUARANTEES} columns="sm:grid-cols-2" />
          <p className={`${eyebrow} mt-12`}>proven onchain</p>
          <p className={`${paragraph} mt-3 max-w-2xl text-neutral-400`}>
            Three transactions in real USDG. Each one is the enforcement doing its job, not a
            description of it.
          </p>
          <Proofs proofs={SPEND_PROOFS} />
          <dl className="mt-8 divide-y divide-neutral-800/70 border-y border-neutral-800/70">
            <Row label="Escrow">
              <a className={`font-mono text-[12px] text-neutral-300 ${link}`} href={`${EXPLORER}/address/${ESCROW}`}>
                {ESCROW} &#8599;
              </a>
            </Row>
            <Row label="Asset">
              <a className={`font-mono text-[12px] text-neutral-300 ${link}`} href={`${EXPLORER}/address/${USDG}`}>
                USDG {USDG} &#8599;
              </a>
            </Row>
          </dl>
        </section>

        <section className="mt-20">
          <p className={eyebrow}>what it may trade</p>
          <h2 className="mt-3 text-xl font-extralight tracking-[0.16em] text-neutral-50">
            Bounded equity trading
          </h2>
          <p className={`${paragraph} mt-4 max-w-2xl`}>
            A spending limit counts dollars. It cannot tell whether the price an agent is filling
            at is real, whether the feed behind it printed an hour ago or last Friday, or whether
            the same in-cap trade is about to run a hundred more times. Tokenized equities need
            those bounds, and they hold before the order reaches the venue.
          </p>
          <Cards cards={TRADE_GUARANTEES} columns="sm:grid-cols-3" />
          <p className={`${eyebrow} mt-12`}>one session, proven onchain</p>
          <p className={`${paragraph} mt-3 max-w-2xl text-neutral-400`}>
            Five transactions, one policy, one sitting against the live AAPL Stock Token. Two
            refusals, a round trip, and a refusal on the day&apos;s budget. The refusals moved
            nothing; the round trip cost the venue&apos;s spread.
          </p>
          <Proofs proofs={TRADE_PROOFS} />
          <dl className="mt-8 divide-y divide-neutral-800/70 border-y border-neutral-800/70">
            <Row label="Guard">
              <a className={`font-mono text-[12px] text-neutral-300 ${link}`} href={`${EXPLORER}/address/${GUARD}`}>
                {GUARD} &#8599;
              </a>
            </Row>
            <Row label="Executor">
              <a className={`font-mono text-[12px] text-neutral-300 ${link}`} href={`${EXPLORER}/address/${EXECUTOR}`}>
                {EXECUTOR} &#8599;
              </a>
            </Row>
            <Row label="Asset">
              <a className={`font-mono text-[12px] text-neutral-300 ${link}`} href={`${EXPLORER}/address/${AAPL}`}>
                AAPL {AAPL} &#8599;
              </a>
            </Row>
          </dl>
          <p className={`${paragraph} mt-3 text-neutral-500`}>
            The pilot runs at small size against one asset; the caps, the band and the budget are
            configuration, and each asset is registered against one specific price feed, so a
            look-alike token with the right ticker is not tradeable.
          </p>
        </section>

        <section className="mt-20">
          <p className={eyebrow}>enforced, not observed</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            Both guarantees are properties of the contracts that hold the funds, not receipts
            written after the fact. There is no step where you trust Covenant, the agent, or the
            provider to have behaved: the limits hold because the money cannot move any other
            way. That is the difference between activity you can audit and activity you can walk
            away from.
          </p>
          <p className={`${paragraph} mt-3 text-neutral-500`}>
            Contracts are deployed and source-verified on Robinhood Chain, chain 4663.
          </p>
          <p className={`${paragraph} mt-6`}>
            Pairs with{" "}
            <a className={link} href="/guard">
              Covenant Guard
            </a>
            , the trust layer your agent checks before it pays another agent at all.
          </p>
        </section>
      </main>
      <SiteFooter />
    </>
  );
}
