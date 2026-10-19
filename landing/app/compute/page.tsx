import type { Metadata } from "next";
import { pageMetadata } from "@/app/_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { workspaceRelease } from "../compute-workspaces/_release";

const TITLE = "Covenant Compute: rent a GPU by the second";
const DESCRIPTION =
  "Rent a GPU and pay in USDC for the seconds you use. The meter runs onchain in a MagicBlock ephemeral rollup and settles on Solana mainnet. Operators earn by the second and stake CVNT to take work.";

export const metadata: Metadata = pageMetadata("/compute", { title: "Covenant Compute" }, {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/compute" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/compute",
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
});

const DOCS = "https://docs.opencovenant.org/compute";
const RELEASE = "https://github.com/open-covenant/covenant/releases/tag/compute-v0.3.0";
const PROGRAM = "3dTtXH7rah8YyWTsAfSg6qC3iqrJXWGEhAx57uUHXZff";
const FIRST_PAYOUT =
  "35Wdz2k2U1QatsiSVsty3edFnmqUESe9C3Dy6xVpNaKByBi9knFieApuyvrZJgQN9DLmQ4LPqH4xJfm4zw2ou7xR";

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-muted";
const paragraph = "text-[13px] leading-relaxed text-fg-soft sm:text-[14px]";
const cmdBlock =
  "block overflow-x-auto whitespace-pre rounded border border-line bg-raised px-4 py-3 font-mono text-[12.5px] leading-relaxed text-fg sm:text-[13px]";
const link =
  "underline decoration-line-strong underline-offset-4 transition-colors hover:text-fg hover:decoration-fg-soft";

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
        <h1 className="page-title mt-4 text-fg">
          Covenant Compute
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Rent a GPU by the second and pay in USDC. When you open a session, the most it can cost is
          held from your balance. You pay only for the seconds from when an operator accepts it until
          it closes, and the rest goes back to your balance. Live on Solana mainnet.
        </p>

        <video
          className="mt-10 w-full max-w-4xl rounded border border-line"
          src="/compute/covenant-compute-film.mp4"
          poster="/compute/poster.jpg"
          controls
          playsInline
          preload="metadata"
        />

        <section className="mt-12">
          <p className={eyebrow}>install &middot; macos and linux</p>
          <code className={`${cmdBlock} mt-3`}>{`brew install open-covenant/tap/covenant-compute`}</code>
          <p className={`${paragraph} mt-3 text-muted-2`}>Or use the install script:</p>
          <code className={`${cmdBlock} mt-3`}>
            {`curl -fsSL https://opencovenant.org/install/compute.sh | sh`}
          </code>
          <p className={`${paragraph} mt-3 text-muted-2`}>
            Either way you get the command-line tools built for your machine, for Apple silicon and
            Intel Macs and for x86-64 and ARM64 Linux. Every release archive is signed, and the{" "}
            <a className={link} href={RELEASE}>
              release page
            </a>{" "}
            shows how to verify it.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>rent a gpu &middot; from the command line</p>
          <code className={`${cmdBlock} mt-3`}>
            {`covenant-compute lease open --minutes 10 --rate 334 --ssh-key ~/.ssh/id_ed25519.pub`}
          </code>
          <p className={`${paragraph} mt-3 text-muted-2`}>
            The lease waits for the machine, then prints its SSH address.{" "}
            <span className="font-mono text-[12px] text-muted">lease close</span> ends the session
            and settles it to the second. The{" "}
            <a className={link} href={DOCS}>
              docs
            </a>{" "}
            walk through setup and funding.
          </p>
        </section>

        <section className="mt-12 grid gap-4 sm:grid-cols-3">
          {HOW.map((item) => (
            <div key={item.title} className="rounded border border-line bg-raised/60 p-5">
              <h2 className="text-[13px] uppercase tracking-[0.22em] text-fg">{item.title}</h2>
              <p className={`${paragraph} mt-3 text-muted`}>{item.body}</p>
            </div>
          ))}
        </section>

        <p className={`${paragraph} mt-6 max-w-2xl text-muted`}>
          Compare sampled provider quotes and settled rental rates in the{" "}
          <a className={link} href="/compute-prices">public GPU price history</a>, with timestamps and sample counts.
          {" "}View capacity, settlement checks and reviewed incidents on the <a className={link} href="/compute-status">Compute status page</a>.
        </p>

        {workspaceRelease.accepted && <section className="mt-12 max-w-2xl">
          <h2 className={eyebrow}>Encrypted workspaces</h2>
          <p className={`${paragraph} mt-3`}>Save an encrypted checkpoint before closing a GPU lease, then restore the same file for a later lease. The key stays on your client.</p>
          <a className={`${link} mt-3 inline-flex min-h-11 items-center text-[13px]`} href="/compute-workspaces">Workspace workflow and release evidence →</a>
        </section>}

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
          <p className={`${paragraph} mt-3 text-muted-2`}>
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
              <div key={stat.label} className="rounded border border-line bg-raised/60 p-4">
                <dt className="font-mono text-[10.5px] uppercase tracking-[0.22em] text-muted-2">
                  {stat.label}
                </dt>
                <dd className="mt-2 font-mono text-[15px] text-fg">{stat.value}</dd>
              </div>
            ))}
          </dl>
          <p className={`${paragraph} mt-3 text-muted-2`}>
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
