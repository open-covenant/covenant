import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Bounded tokenized-equity trading on Robinhood Chain: Covenant";
const DESCRIPTION =
  "A tokenized stock trades around the clock; the price feed behind it does not. Covenant bounds an agent's equity trading onchain: every fill checked against the asset's oracle, a per-trade cap, a daily budget, and an exit under the same rules. Proven live on Robinhood Chain mainnet.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/equities" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/equities",
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
const GUARD = "0x1c6cca8De094209DE79A12eD63477434Ec2621c0";
const EXECUTOR = "0xE94A70f8C864cA3CaE85c74F92Ab8783d2d039A3";
const AAPL = "0xaF3D76f1834A1d425780943C99Ea8A608f8a93f9";

const GUARANTEES: { title: string; body: string }[] = [
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

const PROOFS: { tag: string; result: string; attempt: string; detail: string; tx: string }[] = [
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

export default function EquitiesPage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-[88px] sm:px-8 sm:pt-[120px]">
        <p className={eyebrow}>robinhood chain &middot; stock tokens &middot; live onchain</p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">
          Bounded equity trading
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Hand an agent a wallet of tokenized equities and the rules it trades under hold onchain.
          Every fill is checked against the asset&apos;s own price feed, sized against a per-trade
          cap, and charged to a daily budget that covers the exit as well as the entry. A trade that
          breaks a bound reverts before it reaches the venue, so the failure lands at the boundary
          instead of in the position.
        </p>

        <section className="mt-12 grid gap-4 sm:grid-cols-3">
          {GUARANTEES.map((g) => (
            <div key={g.title} className="rounded border border-neutral-800 bg-neutral-950/60 p-5">
              <h2 className="text-[13px] uppercase tracking-[0.22em] text-neutral-100">{g.title}</h2>
              <p className={`${paragraph} mt-3 text-neutral-400`}>{g.body}</p>
            </div>
          ))}
        </section>

        <section className="mt-14">
          <p className={eyebrow}>one session &middot; robinhood chain mainnet</p>
          <p className={`${paragraph} mt-3 max-w-2xl text-neutral-400`}>
            Five transactions, one policy, one sitting on 25 August 2026, in real USDG against the
            live AAPL Stock Token. Two refusals, a round trip, and a refusal on the budget. The
            refusals moved nothing; the round trip cost the venue&apos;s spread.
          </p>
          <ul className="mt-6 space-y-3">
            {PROOFS.map((p) => (
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
        </section>

        <section className="mt-14">
          <p className={eyebrow}>the contracts</p>
          <dl className="mt-4 divide-y divide-neutral-800/70 border-y border-neutral-800/70">
            <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-3">
              <dt className="font-mono text-[11px] uppercase tracking-[0.24em] text-neutral-500">Guard</dt>
              <dd>
                <a className={`font-mono text-[12px] text-neutral-300 ${link}`} href={`${EXPLORER}/address/${GUARD}`}>
                  {GUARD} &#8599;
                </a>
              </dd>
            </div>
            <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-3">
              <dt className="font-mono text-[11px] uppercase tracking-[0.24em] text-neutral-500">Executor</dt>
              <dd>
                <a className={`font-mono text-[12px] text-neutral-300 ${link}`} href={`${EXPLORER}/address/${EXECUTOR}`}>
                  {EXECUTOR} &#8599;
                </a>
              </dd>
            </div>
            <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-3">
              <dt className="font-mono text-[11px] uppercase tracking-[0.24em] text-neutral-500">Asset</dt>
              <dd>
                <a className={`font-mono text-[12px] text-neutral-300 ${link}`} href={`${EXPLORER}/address/${AAPL}`}>
                  AAPL {AAPL} &#8599;
                </a>
              </dd>
            </div>
            <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-3">
              <dt className="font-mono text-[11px] uppercase tracking-[0.24em] text-neutral-500">Chain</dt>
              <dd className="font-mono text-[12px] text-neutral-300">Robinhood Chain &middot; 4663</dd>
            </div>
          </dl>
          <p className={`${paragraph} mt-3 text-neutral-500`}>
            Deployed and source-verified on mainnet. The pilot runs at small size against one asset;
            the caps, the band and the budget are configuration, and each asset is registered against
            one specific price feed, so a look-alike token with the right ticker is not tradeable.
          </p>
        </section>

        <section className="mt-14">
          <p className={eyebrow}>enforced, not observed</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            A spend cap counts dollars. It cannot tell whether the price you are filling at is real,
            whether the feed behind it printed an hour ago or last Friday, or whether the same
            in-cap trade is about to run a hundred more times. Those are the bounds an equity
            position actually needs, and they hold at the contract, before the order reaches the
            venue.
          </p>
          <p className={`${paragraph} mt-6`}>
            Pairs with{" "}
            <a className={link} href="/robinhood">
              bounded agent spend
            </a>{" "}
            on the same chain, and{" "}
            <a className={link} href="/guard">
              Covenant Guard
            </a>
            .
          </p>
        </section>
      </main>
      <SiteFooter />
    </>
  );
}
