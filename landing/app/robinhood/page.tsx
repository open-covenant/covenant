import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Bounded agent spend on Robinhood Chain: Covenant";
const DESCRIPTION =
  "Funds deposited in a Robinhood Chain SpendGrantEscrow grant are bounded onchain by a total cap, per-call ceiling, provider allowlist, and expiry. Optional quality-gated payout follows a configured attestor’s signed verdict.";

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
    body: "Funds deposited in a grant are subject to a total budget, per-call ceiling, provider allowlist, and expiry. A charge past those bounds reverts. These controls cover funds held by this escrow contract; they do not constrain assets the agent controls elsewhere.",
  },
  {
    title: "Attestor-gated payout",
    body: "When a grant enables its quality gate, held funds release on a configured attestor's signed pass verdict. A signed fail permits an immediate return to the grant; after the call deadline, anyone may trigger a return. The contract verifies the signature and bound call fields; the attestor remains trusted for the semantic judgment.",
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
    tag: "Attestor-signed pass",
    result: "Provider paid",
    attempt:
      "releaseCallAttested accepted a configured-attestor signature over the call, result hash, spec ID, pass verdict, and deadline.",
    detail:
      "The escrow transferred 0.45 USDG to the provider and 0.05 USDG to the treasury.",
    tx: "0x2759a7759db0c789c7fa9cb659d7c1595a0173010e5003a020bf44fd03266dd1",
  },
  {
    tag: "Attestor-signed fail",
    result: "Hold returned to grant",
    attempt:
      "refundCallAttested accepted a configured-attestor signature over the call, result hash, spec ID, fail verdict, and deadline.",
    detail:
      "The held amount returned to the grant balance. The transaction shows no provider token transfer.",
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
          Deposit USDG into a Robinhood Chain grant with a total cap, per-call
          ceiling, provider allowlist, and expiry. The contract enforces those
          limits before its escrowed funds move. This boundary applies to assets
          in the grant, not to an agent&apos;s other wallets or contracts.
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
            Three historical Robinhood Chain mainnet transactions demonstrate
            specific contract state transitions: an over-limit revert, a release
            on a valid signed pass, and a refund on a valid signed fail. They do
            not establish the semantic quality of either output.
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
            The explorer publishes verified source. Pause status, attestor,
            gateway, and admin roles are live contract state and should be
            checked before relying on the escrow.
          </p>
        </section>

        <section className="mt-14">
          <p className={eyebrow}>exact trust boundary</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            The contract enforces the grant&apos;s numeric, provider, expiry,
            and state-transition rules for the funds it holds. A quality-gated
            release additionally requires the configured attestor&apos;s signature.
            That signature proves the configured key signed the verdict; it does
            not establish the verdict&apos;s truth or prove that the off-chain
            evaluation was correct. Grants without the
            quality gate use the separate spender-or-gateway release path.
          </p>
          <p className={`${paragraph} mt-6`}>
            See{" "}
            <a className={link} href="/guard">
              Covenant Evidence
            </a>{" "}
            for read-only public records. It does not approve trades or
            payments.
          </p>
        </section>
      </main>
      <SiteFooter />
    </>
  );
}
