import type { Metadata } from "next";
import Image from "next/image";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = {
  title: "The model you paid for is not always the model that ran",
  description:
    "Covenant Compute rents GPU workspaces and inference under a spend limit set before the job starts. Every request returns a signed receipt naming the model that served it. The prompt itself is never stored.",
  alternates: { canonical: "/covenant-compute" },
  openGraph: {
    type: "article",
    url: "https://opencovenant.org/covenant-compute",
    title: "The model you paid for is not always the model that ran",
    description:
      "A signed receipt for every request: which model served it, and hashes of the input and output. The prompt is never stored.",
    images: [
      {
        url: "/og/covenant-compute.jpg",
        width: 1280,
        height: 720,
        alt: "Covenant Compute, verifiable inference network",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    site: "@OpenCovenant",
    creator: "@OpenCovenant",
    title: "The model you paid for is not always the model that ran",
    description: "A signed receipt for every request. The prompt is never stored.",
    images: "/og/covenant-compute.jpg",
  },
};

const backLink =
  "font-mono text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-100";

export default function InferenceReceiptsPost() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-[#030303]">
      <SiteHeader />

      <div className="page-container">
        <a href="/blog" className={backLink}>
          ← Blog
        </a>

        <p className="mt-10 font-mono text-[11px] uppercase tracking-[0.3em] text-neutral-400">
          Protocol · 12 August 2026
        </p>

        <h1 className="mt-5 max-w-3xl text-balance text-[2.2rem] font-extralight uppercase leading-[1.1] tracking-[2px] text-white sm:text-[2.6rem]">
          The model you paid for is not always the model that ran
        </h1>

        <p className="mt-6 max-w-2xl text-pretty text-lg font-light leading-relaxed text-neutral-300 sm:text-xl">
          Every request returns a signed receipt naming the model that served it. The prompt itself
          is never stored.
        </p>

        <Image
          src="/og/covenant-compute.jpg"
          alt="Covenant Compute, verifiable inference network"
          width={1280}
          height={720}
          priority
          className="mt-12 w-full max-w-3xl rounded-sm border border-neutral-800/80"
        />

        <article className="prose-docs mt-12 max-w-2xl">
          <p>
            When you send a prompt to a hosted model API, you are taking two things on faith. That
            they ran the model you paid for, and that they did not keep your prompt afterwards.
            Usually both are true. But usually is not something you can put in an audit, and it is
            not something you can hand to an agent that spends money on your behalf.
          </p>
          <p>
            We are building an inference network where both of those become checkable facts instead
            of promises.
          </p>

          <h2>Update, 28 August 2026: GPU workspaces</h2>
          <p>
            The network now rents whole GPUs alongside inference. You choose from live market offers
            and get a dedicated CUDA and Jupyter workspace with a spend limit fixed before it starts.
            An agent can rent one on its own behalf, under a per-run budget and a launch cap it
            cannot exceed.
          </p>
          <p>
            Billing starts when the machine answers, not when the request went out. Cancel early and
            the unused allowance is released. The receipt names the runtime you were charged for, the
            amount returned, and what the startup window cost us to absorb.
          </p>
          <p>
            The inference network runs on our own nodes. This GPU capacity is rented from an external
            marketplace, so its receipt records our metering of that provider. Onchain settlement for
            these jobs is written and tested, and not yet deployed. Access is a private beta.
          </p>

          <h2>A receipt for every request</h2>
          <p>
            Each completion returns a signed receipt. It names the model identity that served the
            request, and carries hashes of the input and the output, the node that ran it, and a
            timestamp. The receipt hash is the SHA-256 of the canonical JSON of that payload, so two
            parties comparing notes either agree exactly or do not.
          </p>
          <p>
            Model identity is the part that does the work. It is not a label like{" "}
            <code>llama-3-70b</code>, which anyone can print on anything. It is the hash of the
            actual weights file, plus the quantization, the runtime, the runtime version, and the
            sampling parameters. Serve a smaller quantization than the one that was requested and
            the identity in the receipt is a different string. The substitution is not something you
            have to detect by watching quality drift. It is a mismatch.
          </p>
          <p>
            That matters because silent downgrades are the cheapest way to make money in this
            business, and the hardest thing for a customer to catch. Nobody can tell, from one
            answer, whether they got the model they paid for.
          </p>

          <h2>The prompt is never stored</h2>
          <p>
            A receipt carries the hash of the input, never the input. Same for the output. The
            gateway stores hashes, the node, the model, the amount and the timestamp, and nothing
            else. Raw prompts and completions are never written to disk and never logged.
          </p>
          <p>
            This is enforced rather than promised: the test suite greps the receipt log for the
            prompt text after a run and fails if it finds any of it. A regression that started
            retaining prompts would break the build rather than quietly ship.
          </p>

          <h2>Trust classes, derived and not claimed</h2>
          <p>
            Nodes are not equal, so every offer, request and receipt carries a trust class. The
            class is derived by the network from evidence it can check, never asserted by the node
            serving the request, and a request can require a minimum instead of hoping.
          </p>
          <p>
            The top class is the one where a node proves what it is running inside hardware its
            operator cannot inspect. That verification path already works: we bind a subject to an
            Intel TDX enclave, fetch the quote, and check it through DCAP against MagicBlock&apos;s
            mainnet TEE on Solana. The enclave comes back bound, with current TCB, on mainnet, today.
          </p>
          <p>
            What that proves right now is the confidential execution environment, not the GPU doing
            inference. Wiring the attestation to the inference itself is the next piece of work, and
            we would rather say that plainly than let a mainnet screenshot imply more than it shows.
            The network clamps every served class to what it can actually substantiate, so nothing
            is advertised above what is checked.
          </p>

          <h2>What runs today</h2>
          <p>
            The network is end to end on our own nodes. A node serves an OpenAI-compatible surface,
            reaches the gateway through an outbound mTLS tunnel so it needs no inbound ports, and the
            gateway routes by model and minimum trust class. Pointing an OpenAI client at the gateway
            base URL works.
          </p>
          <p>
            Payment is per request over x402. An unpaid request gets a 402 naming the price, asset
            and network; a paid one gets served and returns its receipt. We ran a 30 request soak
            across the full stack: 30 of 30 served, 30 receipts with unique payment references and no
            replay accepted, the payment gate holding on unpaid requests, an average of one second
            per request, and no trace of the prompts in the receipt log.
          </p>
          <p>
            The soak also found a real bug, which is the point of running one. The node&apos;s
            reconnect backoff treated any short tunnel session as a failure, including the successful
            sub-second relays, so under burst it drained its own connection pool and started refusing
            requests. A successful relay now resets the backoff immediately, and three regression
            tests hold that line.
          </p>

          <h2>A result that changed the design</h2>
          <p>
            The original plan was to spot-check nodes by re-running a request elsewhere and comparing
            outputs token for token. We tested whether that assumption survives contact with real
            hardware before building on it.
          </p>
          <p>
            Single-stream greedy decoding is reproducible, with zero divergence. But turn on the
            continuous batching that every serving stack uses for throughput, and three of eight
            prompts stopped matching exactly, on the same model, same machine, same seed. Batch
            composition changes the arithmetic. So exact-match checking would have flagged honest
            nodes as cheats under load, which is worse than not checking at all.
          </p>
          <p>
            Spot-checking has to compare distributions rather than demand identical tokens. Better to
            learn that from a spike than from a slashed operator.
          </p>

          <h2>Who this is for</h2>
          <p>
            Not the race to cheaper tokens. There are networks already running that race, and they
            will win it. This is for the prompts that cannot leave the building today: patient notes,
            case files, unreleased code, anything where we deleted it, promise is not an answer
            anybody will sign off on. If that describes your workload, the receipt is the artifact
            you have been missing.
          </p>
          <p>
            Supply is not open yet. The network runs on our own nodes while the attested tier gets
            wired to the inference itself and the payment rail moves to real settlement. We are
            looking for design partners on the demand side first, because the shape of the receipt
            should be decided by the people who will have to file it.
          </p>
        </article>

        <div className="mt-16 flex flex-wrap gap-x-6 gap-y-3 border-t border-neutral-800/80 pt-8">
          <a
            href="/contact"
            className="text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
          >
            Become a design partner →
          </a>
          <a
            href="https://github.com/open-covenant/covenant"
            target="_blank"
            rel="noopener noreferrer"
            className="text-[12px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-50"
          >
            Source →
          </a>
        </div>
      </div>

      <SiteFooter className="pb-8" />
    </main>
  );
}
