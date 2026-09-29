import type { Metadata } from "next";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Covenant Compute: rent a GPU by the second";
const DESCRIPTION =
  "Rent a GPU and pay in USDC for the seconds you use. The meter runs onchain in a MagicBlock ephemeral rollup and settles on Solana mainnet. Operators earn by the second and stake CVNT to take work.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/compute" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/compute",
    title: TITLE,
    description: DESCRIPTION,
    images: [
      {
        url: "/og/compute.jpg",
        width: 1280,
        height: 720,
        alt: "Covenant Compute, GPU time metered by the second, live on Solana mainnet",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: TITLE,
    description: DESCRIPTION,
    images: "/og/compute.jpg",
  },
};

const DOCS = "https://docs.opencovenant.org/compute";
const PROGRAM = "3dTtXH7rah8YyWTsAfSg6qC3iqrJXWGEhAx57uUHXZff";
const FIRST_PAYOUT =
  "35Wdz2k2U1QatsiSVsty3edFnmqUESe9C3Dy6xVpNaKByBi9knFieApuyvrZJgQN9DLmQ4LPqH4xJfm4zw2ou7xR";

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const cmdBlock =
  "block overflow-x-auto whitespace-pre rounded border border-neutral-800 bg-neutral-950 px-4 py-3 font-mono text-[12.5px] leading-relaxed text-neutral-100 sm:text-[13px]";
const link =
  "underline decoration-neutral-700 underline-offset-4 transition-colors hover:text-neutral-50 hover:decoration-neutral-300";

const HOW: { title: string; body: string }[] = [
  {
    title: "Pay for the seconds you use",
    body: "A session holds its ceiling up front: your rate times the window you ask for. You are charged for the session's running time, to the millisecond, and the rest comes back when it closes.",
  },
  {
    title: "The meter runs onchain",
    body: "While a session runs, its meter ticks inside a MagicBlock ephemeral rollup at no cost per tick. At close it commits to Solana in a single write, and the escrow pays the operator from it.",
  },
  {
    title: "Operators stake CVNT",
    body: "Every node stakes 1,000,000 CVNT before it can take work. The stake stays locked onchain while the node serves, and a proven fault costs part of it.",
  },
];

const FIRST_SESSION: { label: string; value: string }[] = [
  { label: "gpu", value: "NVIDIA L40S" },
  { label: "held", value: "3 min 12 s" },
  { label: "paid to the operator", value: "$0.064" },
  { label: "returned to the renter", value: "$0.036" },
];

export default function ComputePage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-7xl px-5 pb-24 pt-[88px] sm:px-8 sm:pt-[120px]">
        <p className={eyebrow}>gpu leases &middot; paid in usdc &middot; solana mainnet</p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">
          Covenant Compute
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Rent a GPU by the second and pay in USDC. You pay for the time you use, and the rest of your
          deposit comes back when the session closes. Live on Solana mainnet.
        </p>

        <video
          className="mt-10 w-full max-w-4xl rounded border border-neutral-800"
          src="/compute/covenant-compute.mp4"
          poster="/compute/poster.jpg"
          controls
          playsInline
          preload="metadata"
        />

        <section className="mt-12">
          <p className={eyebrow}>rent a gpu &middot; from the command line</p>
          <code className={`${cmdBlock} mt-3`}>
            {`covenant-compute lease open --minutes 10 --rate 334 --ssh-key ~/.ssh/id_ed25519.pub`}
          </code>
          <p className={`${paragraph} mt-3 text-neutral-500`}>
            The lease waits for the machine, then prints its SSH address.{" "}
            <span className="font-mono text-[12px] text-neutral-400">lease close</span> ends the session
            and settles it to the second. The{" "}
            <a className={link} href={DOCS}>
              docs
            </a>{" "}
            walk through setup and funding.
          </p>
        </section>

        <section className="mt-12 grid gap-4 sm:grid-cols-3">
          {HOW.map((item) => (
            <div key={item.title} className="rounded border border-neutral-800 bg-neutral-950/60 p-5">
              <h2 className="text-[13px] uppercase tracking-[0.22em] text-neutral-100">{item.title}</h2>
              <p className={`${paragraph} mt-3 text-neutral-400`}>{item.body}</p>
            </div>
          ))}
        </section>

        <section className="mt-12">
          <p className={eyebrow}>run a node</p>
          <p className={`${paragraph} mt-3 max-w-2xl`}>
            Operators earn USDC for every second their GPU is rented. Register a node, stake 1,000,000
            CVNT for it from any wallet you control, and it starts taking leases. Withdraw the stake once
            its lock ends.
          </p>
          <code className={`${cmdBlock} mt-3`}>
            {`covenant-compute-stake stake <node> 1000000 --lock-days 30 --keypair <wallet.json>`}
          </code>
          <p className={`${paragraph} mt-3 text-neutral-500`}>
            The{" "}
            <a className={link} href={`${DOCS}#run-a-node`}>
              operator guide
            </a>{" "}
            covers setup and staking.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>the first mainnet session</p>
          <dl className="mt-3 grid max-w-3xl grid-cols-2 gap-4 sm:grid-cols-4">
            {FIRST_SESSION.map((stat) => (
              <div key={stat.label} className="rounded border border-neutral-800 bg-neutral-950/60 p-4">
                <dt className="font-mono text-[10.5px] uppercase tracking-[0.22em] text-neutral-500">
                  {stat.label}
                </dt>
                <dd className="mt-2 font-mono text-[15px] text-neutral-100">{stat.value}</dd>
              </div>
            ))}
          </dl>
          <p className={`${paragraph} mt-3 text-neutral-500`}>
            Paid from the escrow in{" "}
            <a className={link} href={`https://solscan.io/tx/${FIRST_PAYOUT}`}>
              one transaction
            </a>
            , seconds after the meter closed.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>live on mainnet</p>
          <ul className={`${paragraph} mt-3 max-w-2xl space-y-2`}>
            <li>
              Settlement program{" "}
              <a className={`${link} font-mono text-[12px]`} href={`https://solscan.io/account/${PROGRAM}`}>
                {PROGRAM}
              </a>
            </li>
            <li>
              Live capacity at{" "}
              <a className={`${link} font-mono text-[12px]`} href="https://compute-api.opencovenant.org/federation/capacity">
                compute-api.opencovenant.org
              </a>
            </li>
            <li>Meter on MagicBlock&apos;s ephemeral rollup, settlement on Solana, payments in USDC.</li>
          </ul>
        </section>
      </main>
      <SiteFooter />
    </>
  );
}
