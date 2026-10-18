import type { Metadata } from "next";
import deployment from "../../public/compute/inference/deployment.json";
import publicConfig from "../../public/compute/inference/agentkit-config.example.json";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = pageMetadata(
  "/compute-inference",
  { title: "Inference with Coinbase AgentKit", section: "Compute" },
  {
    title: "Inference with Coinbase AgentKit · Covenant Compute",
    description:
      "Configure a Coinbase AgentKit wallet for Covenant inference with x402 and Solana USDC. Client installation, exact pricing, spending limits and public deployment identities.",
  },
);

const SOURCE = `https://github.com/open-covenant/covenant/tree/${deployment.sourceRevision}`;
const CLIENT_SOURCE = `${SOURCE}/sdk/compute-x402-client`;
const GATEWAY_SOURCE = `https://github.com/open-covenant/covenant/tree/${deployment.gatewayRevision}/agent-os/crates/covenant-compute-gateway`;
const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-muted";
const heading = "text-[13px] uppercase tracking-[0.25em] text-fg sm:text-[14px]";
const paragraph = "text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]";
const link = "underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg-soft hover:text-fg";
const price = `${(deployment.priceMicroUsdc / 1_000_000).toFixed(2)} USDC`;

const steps = [
  { number: "01", title: "Review the quote", body: "Send the model, messages and output limit. Covenant returns the price before your wallet signs. The client checks both the price and the payment destination." },
  { number: "02", title: "Authorize with your AgentKit wallet", body: "Your Solana wallet signs the USDC payment through x402. The client saves that authorization before sending it. Retries keep the same request and payment." },
  { number: "03", title: "Verify the completed purchase", body: "Covenant runs the request from its funded reserve, then settles the customer payment before releasing the result. The client checks the signed job, operator receipt, exact output and confirmed payment." },
];

const packageInstall = `mkdir covenant-inference &&
cd covenant-inference &&
npm init -y &&
curl --fail --location --output ${deployment.clientPackage.filename} \\
  ${deployment.clientPackage.url} &&
echo "${deployment.clientPackage.sha256}  ${deployment.clientPackage.filename}" | shasum -a 256 -c - &&
npm install --ignore-scripts ./${deployment.clientPackage.filename}`;

const sourceInstall = `git clone --filter=blob:none https://github.com/open-covenant/covenant.git
cd covenant
git fetch origin ${deployment.sourceRevision}
git checkout --detach ${deployment.sourceRevision}
cd sdk/compute-x402-client
npm ci --ignore-scripts`;

const registration = `import { AgentKit } from '@coinbase/agentkit';
import { covenantInferenceActionProvider }
  from '@covenant/compute-x402-client/agentkit';

// config is the downloaded example with your local wallet/journal paths.
const provider = covenantInferenceActionProvider(config);
const kit = await AgentKit.from({
  walletProvider, // Your existing AgentKit Solana wallet.
  actionProviders: [provider],
});
const action = kit.getActions()
  .find(({ name }) => name === 'covenant_inference');

// Load a saved request. Keep its UUID and body unchanged when resuming.
const result = JSON.parse(await action.invoke(request));
provider.close(walletProvider);`;

const request = `{
  "requestId": "<generate one UUID and save this file>",
  "model": "${deployment.model}",
  "messages": [{
    "role": "user",
    "content": "Return a complete, compact HTML meeting cost calculator under 1000 tokens. Include working inputs for attendees, hourly rate and meeting minutes. Inline CSS and JavaScript; no external resources. Return HTML only, ending with </html>."
  }],
  "max_tokens": ${deployment.maxOutputTokens},
  "temperature": 0.2
}`;

function CodeBlock({ label, children }: { label: string; children: string }) {
  return <div className="mt-6 overflow-hidden border border-line/80 bg-raised/30">
    <div className="border-b border-line/80 px-5 py-3 font-mono text-[10px] uppercase tracking-[0.2em] text-muted sm:px-6">{label}</div>
    <pre className="overflow-x-auto px-5 py-6 text-[11px] leading-[1.9] text-fg-soft sm:px-6 sm:text-[12px]" tabIndex={0} aria-label={label}><code>{children}</code></pre>
  </div>;
}

export default function ComputeInferencePage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
      <SiteHeader />
      <div className="page-container">
        <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Compute</p>
        <h1 className="type-display max-w-3xl text-balance text-[2.2rem] text-fg sm:text-[2.6rem]">Pay for inference with USDC.</h1>
        <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.7] text-fg-soft sm:text-base">
          Connect a Coinbase AgentKit wallet to Covenant Compute. Review the price,
          authorize USDC on Solana and verify the result with a signed Compute receipt.
        </p>
        <div className="mt-7 flex flex-wrap items-center gap-x-7 gap-y-2">
          <a href="#agentkit" className={`inline-flex min-h-11 items-center text-[13px] text-fg ${link}`}>Set up AgentKit <span aria-hidden="true" className="ml-2">↓</span></a>
          <a href={CLIENT_SOURCE} target="_blank" rel="noopener noreferrer" className="inline-flex min-h-11 items-center text-[13px] text-muted transition-colors hover:text-fg">View source <span aria-hidden="true" className="ml-2">↗</span></a>
        </div>
        <div className="mt-8 max-w-3xl border-l border-line-strong pl-5">
          <p className={eyebrow}>Verified on Solana mainnet</p>
          <p className={`mt-2 ${paragraph}`}>The first purchase and post-restart replay are verified. Inspect the signed result and customer payment below.</p>
        </div>
        <div className="mt-10 flex flex-wrap gap-x-6 gap-y-3 border-y border-line/80 py-5 text-[12px] text-muted sm:mt-14">
          <span>Coinbase AgentKit</span><span>x402 V2</span><span>USDC on Solana</span>
        </div>

        <section aria-labelledby="service-heading" className="mt-14 sm:mt-20">
          <h2 id="service-heading" className={heading}>Model, price and limits</h2>
          <dl className="mt-7 divide-y divide-line/80 border-y border-line/80">
            {[
              ["Model", deployment.model], ["Customer price", `${price} per completed request`],
              ["Maximum output", `${deployment.maxOutputTokens.toLocaleString("en-US")} tokens`],
              ["Maximum input", `${deployment.maxInputBytes.toLocaleString("en-US")} UTF-8 bytes across the JSON messages array`],
              ["Request format", `${deployment.maxMessages} text messages maximum · non-streaming`],
              ["Endpoint", `POST ${deployment.endpoint}`],
            ].map(([label, value]) => <div key={label} className="grid gap-2 py-4 sm:grid-cols-[180px_1fr] sm:gap-7"><dt className="text-[12px] text-muted">{label}</dt><dd className="break-words font-mono text-[12px] leading-[1.8] text-fg-soft [overflow-wrap:anywhere]">{value}</dd></div>)}
          </dl>
          <p className={`mt-5 max-w-2xl ${paragraph}`}>The input limit includes JSON framing and escaping. The customer price is separate from the operator&apos;s Compute payment. Your wallet checks the actual x402 quote before signing.</p>
        </section>

        <section aria-labelledby="purchase-heading" className="mt-14 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
          <h2 id="purchase-heading" className={heading}>One verified purchase</h2>
          <p className={`mt-5 max-w-2xl ${paragraph}`}>An AgentKit wallet requested a meeting-cost calculator. The returned HTML ran unchanged: Start, Pause and Reset worked, and changing the inputs recalculated the cost.</p>
          <dl className="mt-7 grid gap-6 sm:grid-cols-3">
            {[
              ["Customer payment", `${(deployment.acceptance.customerPaymentMicroUsdc / 1_000_000).toFixed(2)} USDC`],
              ["End to end", `${deployment.acceptance.elapsedSeconds.toFixed(1)} seconds`],
              ["Completed", "18 October 2026 · 01:14 UTC"],
            ].map(([label, value]) => <div key={label}><dt className="text-[12px] text-muted">{label}</dt><dd className="mt-2 font-mono text-[14px] text-fg">{value}</dd></div>)}
          </dl>
          <div className="mt-6 flex flex-wrap gap-x-7 gap-y-2">
            <a href={deployment.acceptance.evidenceUrl} className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>Read the signed purchase record <span aria-hidden="true" className="ml-2">↗</span></a>
            <a href={`https://explorer.solana.com/tx/${deployment.acceptance.paymentTransaction}`} target="_blank" rel="noopener noreferrer" className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>View the USDC payment <span aria-hidden="true" className="ml-2">↗</span></a>
          </div>
          <p className={`mt-3 max-w-2xl ${paragraph}`}>The timing covers wallet initialization, quoting, execution and verification for this purchase.</p>
        </section>

        <section aria-labelledby="flow-heading" className="mt-14 sm:mt-20">
          <h2 id="flow-heading" className={heading}>From request to receipt</h2>
          <ol className="mt-9 sm:mt-11">{steps.map((step, index) => <li key={step.number} className={`relative border-l border-line/80 pl-8 sm:pl-12 ${index === steps.length - 1 ? "" : "pb-11 sm:pb-12"}`}>
            <span aria-hidden="true" className="absolute -left-[5px] top-[7px] h-[9px] w-[9px] rounded-full border border-line-strong bg-ground" />
            <div className="mb-3 flex flex-wrap items-baseline gap-x-4 gap-y-2"><span className={eyebrow}>{step.number}</span><h3 className="text-[15px] text-fg">{step.title}</h3></div><p className={`max-w-2xl ${paragraph}`}>{step.body}</p>
          </li>)}</ol>
        </section>

        <section id="agentkit" aria-labelledby="agentkit-heading" className="mt-16 scroll-mt-28 border-t border-line/80 pt-9 sm:mt-24 sm:pt-11">
          <h2 id="agentkit-heading" className={heading}>Install the client</h2>
          <p className={`mt-5 max-w-2xl ${paragraph}`}>Requires Node.js 24 or later. This release uses AgentKit {deployment.agentKitVersion} and the official x402 SDK {deployment.x402SdkVersion}. Download the versioned release package, verify its checksum and install it with its locked dependencies:</p>
          {!deployment.clientPackage.published && <p className={`mt-4 max-w-2xl ${paragraph}`}>The release package will be published after production acceptance. The source installation below is available now.</p>}
          <CodeBlock label="Release package installation · terminal">{packageInstall}</CodeBlock>
          <details className="mt-5 border-y border-line/80 py-4"><summary className="cursor-pointer text-[13px] text-fg-soft">Install the pinned source instead</summary><CodeBlock label="Source installation · terminal">{sourceInstall}</CodeBlock><p className={`mt-4 ${paragraph}`}>For this source checkout, run <code className="font-mono text-[12px]">node demo.mjs config.private.json request.json ./purchase-evidence</code> from the client directory.</p></details>
          <h3 className="mt-10 text-[15px] text-fg">Configure your wallet and spending limits</h3>
          <p className={`mt-4 max-w-2xl ${paragraph}`}>Download the example, save it as config.private.json and replace its wallet-file placeholder with your own private Solana keypair file. Keep that file on your machine with mode 0600. The example caps each payment at {price} and cumulative spending at 0.50 USDC. Use a persistent purchase journal and a wallet funded with Solana USDC.</p>
          <a className={`mt-4 inline-flex min-h-11 items-center text-[13px] ${link}`} href="/compute/inference/agentkit-config.example.json" download>Download client configuration <span aria-hidden="true" className="ml-2">↓</span></a>
          <details className="mt-3 border-y border-line/80 py-4"><summary className="cursor-pointer text-[13px] text-fg-soft">Inspect the public configuration</summary><CodeBlock label="Client configuration · JSON">{JSON.stringify(publicConfig, null, 2)}</CodeBlock></details>
          <h3 className="mt-10 text-[15px] text-fg">Save one request, then run the demo</h3>
          <p className={`mt-4 max-w-2xl ${paragraph}`}>Generate a UUID once with <code className="font-mono text-[12px]">node -e &apos;console.log(crypto.randomUUID())&apos;</code>, replace the request ID below and save it as <code className="font-mono text-[12px]">request.json</code>. Keep that file unchanged when resuming an interrupted purchase.</p>
          <CodeBlock label="Saved request · JSON">{request}</CodeBlock>
          <CodeBlock label="Run the installed demo · terminal">{"node node_modules/@covenant/compute-x402-client/demo.mjs config.private.json request.json ./purchase-evidence"}</CodeBlock>
          <p className={`mt-5 max-w-2xl ${paragraph}`}>The demo invokes the registered AgentKit action and saves the verified response, payment details and signed receipt. HTML responses also produce a local preview. The gateway redelivers completed output for five minutes; your client retains the result and receipt locally. Payment records remain after the gateway output expires.</p>
          <details className="mt-7 border-y border-line/80 py-4"><summary className="cursor-pointer text-[13px] text-fg-soft">Add the action to an existing AgentKit application</summary><CodeBlock label="AgentKit registration · JavaScript">{registration}</CodeBlock></details>
        </section>

        <section aria-labelledby="identity-heading" className="mt-14 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
          <h2 id="identity-heading" className={heading}>Public deployment identities</h2>
          <dl className="mt-7 divide-y divide-line/80">{[
            ["USDC mint", deployment.asset], ["Merchant", deployment.merchant], ["Gateway buyer", deployment.gatewayBuyer], ["Approved operator", deployment.operator], ["Fee payer", deployment.feePayer], ["Coordinator", deployment.coordinator],
          ].map(([label, value]) => <div key={label} className="grid gap-2 py-4 sm:grid-cols-[180px_1fr] sm:gap-7"><dt className="text-[12px] text-muted">{label}</dt><dd className="break-all font-mono text-[11px] leading-[1.8] text-fg-soft">{value}</dd></div>)}</dl>
          <a href="/compute/inference/deployment.json" className={`mt-5 inline-flex min-h-11 items-center text-[13px] ${link}`}>Download deployment details <span aria-hidden="true" className="ml-2">↓</span></a>
        </section>

        <section aria-labelledby="payment-heading" className="mt-14 grid gap-9 border-t border-line/80 pt-9 sm:mt-20 sm:grid-cols-2 sm:gap-12 sm:pt-11">
          <div><h2 id="payment-heading" className={heading}>Keep spending bounded</h2><p className={`mt-5 ${paragraph}`}>The client reserves the quote before signing. Its cumulative ceiling includes completed purchases and uncertain payments, and persists across restarts. Reuse the same request and journal to recover; do not create a fresh payment for an unresolved purchase.</p></div>
          <div><h2 className={heading}>Know what the receipt verifies</h2><p className={`mt-5 ${paragraph}`}>The receipt binds the operator, job and exact output. It does not establish the quality of an answer or attest to the operator&apos;s hardware. Built with the open-source Coinbase AgentKit and x402 SDKs.</p></div>
        </section>
        <div className="mt-14 flex flex-wrap gap-x-6 gap-y-3 border-t border-line/80 pt-7 sm:mt-20">{[
          { label: "LangChain + LangGraph", href: "/compute-langchain" }, { label: "Compute status", href: "/compute-status" }, { label: "Compute", href: "/compute" }, { label: "Gateway source", href: GATEWAY_SOURCE },
        ].map(({label,href})=><a key={href} href={href} className="inline-flex min-h-11 items-center text-[11px] uppercase tracking-[0.2em] text-muted transition-colors hover:text-fg">{label}<span aria-hidden="true" className="ml-2">→</span></a>)}</div>
      </div>
      <SiteFooter className="pb-8" />
    </main>
  );
}
