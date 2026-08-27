import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Bounded agent spend on Robinhood Chain: Covenant";
const DESCRIPTION =
  "Hand a funded agent a wallet on Robinhood Chain and walk away. Its spend cap, provider allowlist, and pay-only-for-good-output rule are enforced onchain by the contract that holds the money, proven live on mainnet in real USDG.";

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

const GUARANTEES: { title: string; body: string }[] = [
  {
    title: "Bounded spend",
    body: "The agent spends against an onchain grant: a total budget, a per-call ceiling, an allowlist of who it may pay, an expiry. A charge past any bound reverts at the contract. The limit is not a policy the agent is asked to respect, it is a rule the money enforces, which is what lets you let go of the wheel.",
  },
  {
    title: "Pay only for good output",
    body: "Each call's funds sit in escrow and release to the provider only when the result clears the spec. Fail the bar and the funds refund to the grant, in full. You pay for output that passed and nothing else, decided before payout, on every call.",
  },
];

const PROOFS: { tag: string; result: string; attempt: string; detail: string; tx: string }[] = [
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

const short = (hash: string) => `${hash.slice(0, 10)}…${hash.slice(-8)}`;

export default function RobinhoodPage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-[88px] sm:px-8 sm:pt-[120px]">
        <p className={eyebrow}>robinhood chain &middot; usdg &middot; live onchain</p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">
          Bounded agent spend
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Hand a funded agent a wallet on Robinhood Chain and walk away. Its spending limit is
          enforced by the contract that holds the money: a total cap, a per-call ceiling, an
          allowlist of who it may pay, an expiry. A rogue or buggy agent cannot exceed the cap, pay a
          stranger, or overpay for junk output. The bound holds onchain, before a dollar moves, not
          reconciled after.
        </p>

        <section className="mt-12 grid gap-4 sm:grid-cols-2">
          {GUARANTEES.map((g) => (
            <div key={g.title} className="rounded border border-neutral-800 bg-neutral-950/60 p-5">
              <h2 className="text-[13px] uppercase tracking-[0.22em] text-neutral-100">{g.title}</h2>
              <p className={`${paragraph} mt-3 text-neutral-400`}>{g.body}</p>
            </div>
          ))}
        </section>

        <section className="mt-14">
          <p className={eyebrow}>proven onchain &middot; robinhood chain mainnet</p>
          <p className={`${paragraph} mt-3 max-w-2xl text-neutral-400`}>
            Three transactions on Robinhood Chain mainnet, settled in real USDG. Each one is the
            enforcement doing its job, not a description of it.
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
                <a className={`mt-3 inline-block font-mono text-[12px] text-neutral-500 ${link}`} href={`${EXPLORER}/tx/${p.tx}`}>
                  {short(p.tx)} &#8599;
                </a>
              </li>
            ))}
          </ul>
        </section>

        <section className="mt-14">
          <p className={eyebrow}>the contract</p>
          <dl className="mt-4 divide-y divide-neutral-800/70 border-y border-neutral-800/70">
            <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-3">
              <dt className="font-mono text-[11px] uppercase tracking-[0.24em] text-neutral-500">Escrow</dt>
              <dd>
                <a className={`font-mono text-[12px] text-neutral-300 ${link}`} href={`${EXPLORER}/address/${ESCROW}`}>
                  {ESCROW} &#8599;
                </a>
              </dd>
            </div>
            <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-3">
              <dt className="font-mono text-[11px] uppercase tracking-[0.24em] text-neutral-500">Asset</dt>
              <dd>
                <a className={`font-mono text-[12px] text-neutral-300 ${link}`} href={`${EXPLORER}/address/${USDG}`}>
                  USDG {USDG} &#8599;
                </a>
              </dd>
            </div>
            <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-3">
              <dt className="font-mono text-[11px] uppercase tracking-[0.24em] text-neutral-500">Chain</dt>
              <dd className="font-mono text-[12px] text-neutral-300">Robinhood Chain &middot; 4663</dd>
            </div>
          </dl>
          <p className={`${paragraph} mt-3 text-neutral-500`}>
            Deployed, source-verified, and unpaused on mainnet.
          </p>
        </section>

        <section className="mt-14">
          <p className={eyebrow}>enforced, not observed</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            The guarantee is a property of the contract that custodies the funds, not a receipt
            written after the fact. There is no step where you trust Covenant, the agent, or the
            provider to have behaved: the limit holds because the money cannot move any other way.
            That is the difference between spending you can audit and spending you can walk away from.
          </p>
          <p className={`${paragraph} mt-6`}>
            Pairs with{" "}
            <a className={link} href="/equities">
              bounded equity trading
            </a>{" "}
            on the same chain, where the bound is the price and size of a trade rather
            than the size of a payment, and{" "}
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
