import type { Metadata } from "next";
import Image from "next/image";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

const TITLE = "Covenant Guard: run your coding agent unattended";
const DESCRIPTION =
  "A hard per-run spend cap, an OS sandbox, and a signed receipt for Claude Code and Codex runs. Enforced from outside the agent's process, so the agent can't raise its own limit.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/guard" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/guard",
    title: TITLE,
    description: DESCRIPTION,
    images: [{ url: "/guard/card.png", width: 1200, height: 630, alt: "A Covenant Guard receipt: stopped at the spend cap, $3.24 of $3.00" }],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: TITLE,
    description: DESCRIPTION,
    images: "/guard/card.png",
  },
};

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400";
const paragraph = "text-[13px] leading-relaxed text-neutral-300 sm:text-[14px]";
const cmdBlock =
  "block overflow-x-auto whitespace-pre rounded border border-neutral-800 bg-neutral-950 px-4 py-3 font-mono text-[12.5px] leading-relaxed text-neutral-100 sm:text-[13px]";

const RINGS: { title: string; body: string }[] = [
  {
    title: "Hard spend cap",
    body: "Every model call is routed through a local metering proxy that counts spend as the response streams. Cross the cap and the proxy refuses further calls and the guard kills the agent's process group. Overshoot is bounded to the calls already in flight, so it works for headless and interactive runs, and for subscription logins a budget flag can't cover.",
  },
  {
    title: "OS sandbox",
    body: "Writes end at the workspace. Credentials (~/.ssh, ~/.aws, gh, docker, kube) are unreadable, the agent's own config is read-only, and all network egress is denied except the loopback proxy. That last part is what makes the cap real: there is no route to the API that skips the meter. Seatbelt on macOS, bubblewrap on Linux.",
  },
  {
    title: "Signed receipt",
    body: "Every event lands on a SHA-256 hash chain. On exit the guard writes a receipt carrying spend against cap, files changed, models and tokens, and commands, signed ed25519. covguard verify re-checks it from the event log; change one number and it fails.",
  },
];

export default function GuardPage() {
  return (
    <>
      <SiteHeader />
      <main className="mx-auto w-full max-w-4xl px-5 pb-24 pt-14 sm:px-8">
        <p className={eyebrow}>cap &middot; sandbox &middot; receipt</p>
        <h1 className="mt-4 text-2xl font-extralight tracking-[0.18em] text-neutral-50 sm:text-3xl">
          Covenant Guard
        </h1>
        <p className={`${paragraph} mt-5 max-w-2xl`}>
          Run your coding agent unattended. It can&apos;t spend past your cap, can&apos;t touch what
          you didn&apos;t allow, and hands you a signed receipt of everything it did. The guard runs
          as the parent process, outside the sandbox the agent lives in: it holds the credential,
          meters the spend, and pulls the plug, none of which the agent can reach around.
        </p>

        <section className="mt-10">
          <p className={eyebrow}>install</p>
          <code className={`${cmdBlock} mt-3`}>curl -fsSL https://opencovenant.org/guard/install.sh | sh</code>
          <p className={`${paragraph} mt-2 text-neutral-500`}>
            Verifies the release checksums before installing, and fails closed. Also:{" "}
            <span className="font-mono text-[12px] text-neutral-400">brew install open-covenant/tap/covenant-guard</span>{" "}
            or build from source with{" "}
            <span className="font-mono text-[12px] text-neutral-400">cargo install --path agent-os/crates/covenant-guard</span>.
          </p>
        </section>

        <section className="mt-10">
          <p className={eyebrow}>first run</p>
          <code className={`${cmdBlock} mt-3`}>
            {`covguard run --budget 10 -- claude -p "fix the flaky tests" --dangerously-skip-permissions`}
          </code>
          <p className={`${paragraph} mt-2 text-neutral-500`}>
            Works with Claude Code today, including subscription sessions. Codex wiring is included
            and marked experimental.
          </p>
        </section>

        <section className="mt-12 grid gap-4 sm:grid-cols-3">
          {RINGS.map((r) => (
            <div key={r.title} className="rounded border border-neutral-800 bg-neutral-950/60 p-5">
              <h2 className="text-[13px] uppercase tracking-[0.22em] text-neutral-100">{r.title}</h2>
              <p className={`${paragraph} mt-3 text-neutral-400`}>{r.body}</p>
            </div>
          ))}
        </section>

        <section className="mt-12">
          <p className={eyebrow}>the receipt</p>
          <div className="mt-4 overflow-hidden rounded border border-neutral-800">
            <Image
              src="/guard/card.png"
              alt="A Covenant Guard receipt card: stopped at the spend cap, $3.24 of a $3.00 cap, with turns, files, duration, and network, signed and verifiable"
              width={1200}
              height={630}
              className="h-auto w-full"
              priority
            />
          </div>
          <p className={`${paragraph} mt-3 text-neutral-500`}>
            A run that crossed its cap. The tick marks where the cap sat: overshoot is bounded to
            the one call that was in flight, and the receipt shows the true number.{" "}
            <span className="font-mono text-[12px] text-neutral-400">covguard verify</span> re-checks
            the signature and the event chain; tamper with any field and it fails.
          </p>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>honest limits</p>
          <ul className={`${paragraph} mt-3 max-w-2xl list-disc space-y-2 pl-5 text-neutral-400`}>
            <li>Prebuilt binaries: macOS arm64 and Linux x86_64. The Linux sandbox needs bubblewrap installed.</li>
            <li>The cap bounds spend; it does not make the agent&apos;s edits correct. That is what the receipt and your review are for.</li>
            <li>Codex support is wired (Responses API, config generation) but not yet battle-tested.</li>
          </ul>
        </section>

        <section className="mt-12">
          <p className={eyebrow}>source &middot; release</p>
          <p className={`${paragraph} mt-3`}>
            Apache-2.0. The enforcement is open source, so you can audit the thing you trust.{" "}
            <a
              className="underline decoration-neutral-700 underline-offset-4 transition-colors hover:text-neutral-50 hover:decoration-neutral-300"
              href="https://github.com/open-covenant/covenant/releases/tag/covguard-v0.1.0"
            >
              covguard-v0.1.0
            </a>{" "}
            ships cosign-signed tarballs and checksums.
          </p>
        </section>
      </main>
      <SiteFooter />
    </>
  );
}
