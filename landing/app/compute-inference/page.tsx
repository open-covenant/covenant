import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";

export const metadata: Metadata = pageMetadata(
  "/compute-inference",
  { title: "Inference for agents", section: "Compute" },
  {
    title: "Inference for agents · Covenant Compute",
    description:
      "Connect Coinbase AgentKit to Covenant inference. Pay per request with USDC on Solana through x402, set spending limits and verify each result with a signed Compute receipt.",
  },
);

const CLIENT_SOURCE =
  "https://github.com/open-covenant/covenant/tree/deploy/compute-coordinator/sdk/compute-x402-client";
const GATEWAY_SOURCE =
  "https://github.com/open-covenant/covenant/tree/deploy/compute-coordinator/services/compute-x402-facilitator";
const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-muted";
const heading = "text-[13px] uppercase tracking-[0.25em] text-fg sm:text-[14px]";
const paragraph = "text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]";
const link =
  "underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg-soft hover:text-fg";

const steps = [
  {
    number: "01",
    title: "Review the quote",
    body: "Send the model, messages and output limit. Covenant returns the price in USDC before your wallet signs a payment. Set a ceiling for each request and for total spending.",
  },
  {
    number: "02",
    title: "Pay with your AgentKit wallet",
    body: "Your agent’s Solana wallet authorizes the payment through x402. The client records that authorization before sending it, so a retry uses the same payment and request.",
  },
  {
    number: "03",
    title: "Verify the result",
    body: "The client checks the signed Compute job, the operator’s receipt and the returned output. It also checks the payment on Solana before reporting the purchase as complete.",
  },
];

const registration = `import { AgentKit } from '@coinbase/agentkit';
import { covenantInferenceActionProvider }
  from '@covenant/compute-x402-client/agentkit';

const kit = await AgentKit.from({
  walletProvider,
  actionProviders: [
    covenantInferenceActionProvider(config),
  ],
});`;

export default function ComputeInferencePage() {
  return (
    <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
      <SiteHeader />
      <div className="page-container">
        <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">
          Compute
        </p>
        <h1 className="type-display max-w-3xl text-balance text-[2.2rem] text-fg sm:text-[2.6rem]">
          Inference for agents
        </h1>
        <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.7] text-fg-soft sm:text-base">
          Connect Coinbase AgentKit to Covenant Compute. Request inference, approve a USDC
          quote and verify the result with a signed receipt. Payments settle on Solana through x402.
        </p>

        <div className="mt-7 flex flex-wrap items-center gap-x-7 gap-y-2">
          <a href="#agentkit" className="inline-flex min-h-11 items-center text-[13px] text-fg underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg">
            Use with AgentKit <span aria-hidden="true" className="ml-2">↓</span>
          </a>
          <a href={CLIENT_SOURCE} target="_blank" rel="noopener noreferrer" className="inline-flex min-h-11 items-center text-[13px] text-muted transition-colors hover:text-fg">
            View source <span aria-hidden="true" className="ml-2">↗</span>
          </a>
        </div>

        <div className="mt-10 flex flex-wrap gap-x-6 gap-y-3 border-y border-line/80 py-5 text-[12px] text-muted sm:mt-14">
          <span>Coinbase AgentKit</span>
          <span>x402 V2</span>
          <span>USDC on Solana</span>
        </div>

        <section aria-labelledby="flow-heading" className="mt-14 sm:mt-20">
          <h2 id="flow-heading" className={heading}>From request to receipt</h2>
          <ol className="mt-9 sm:mt-11">
            {steps.map((step, index) => (
              <li key={step.number} className={`relative border-l border-line/80 pl-8 sm:pl-12 ${index === steps.length - 1 ? "" : "pb-11 sm:pb-12"}`}>
                <span aria-hidden="true" className="absolute -left-[5px] top-[7px] h-[9px] w-[9px] rounded-full border border-line-strong bg-ground" />
                <div className="mb-3 flex flex-wrap items-baseline gap-x-4 gap-y-2">
                  <span className={eyebrow}>{step.number}</span>
                  <h3 className="text-[15px] text-fg">{step.title}</h3>
                </div>
                <p className={`max-w-2xl ${paragraph}`}>{step.body}</p>
              </li>
            ))}
          </ol>
        </section>

        <section id="agentkit" aria-labelledby="agentkit-heading" className="mt-16 scroll-mt-28 border-t border-line/80 pt-9 sm:mt-24 sm:pt-11">
          <h2 id="agentkit-heading" className={heading}>Use your AgentKit wallet</h2>
          <p className={`mt-5 max-w-2xl ${paragraph}`}>
            Add Covenant&apos;s inference action to your AgentKit configuration. The action uses the
            wallet you already supply to AgentKit to sign the USDC payment.
          </p>
          <div className="mt-7 overflow-hidden border border-line/80 bg-raised/30">
            <div className="border-b border-line/80 px-5 py-3 font-mono text-[10px] uppercase tracking-[0.2em] text-muted sm:px-6">
              AgentKit · JavaScript
            </div>
            <pre className="overflow-x-auto px-5 py-6 text-[11px] leading-[1.9] text-fg-soft sm:px-6 sm:text-[12px]" tabIndex={0} aria-label="Register the Covenant inference action with Coinbase AgentKit">
              <code>{registration}</code>
            </pre>
          </div>
          <p className={`mt-5 max-w-2xl ${paragraph}`}>
            Configure the gateway, approved operator keys, spending limits and a persistent
            purchase journal. The example includes installation steps, a registered action and
            receipt verification. Requires Node.js 24 or later and a Solana wallet funded with USDC.
          </p>
          <a href={`${CLIENT_SOURCE}#register-with-agentkit`} target="_blank" rel="noopener noreferrer" className={`mt-4 inline-flex min-h-11 items-center text-[13px] ${link}`}>
            Installation and example <span aria-hidden="true" className="ml-2">↗</span>
          </a>
        </section>

        <section aria-labelledby="payment-heading" className="mt-14 grid gap-9 border-t border-line/80 pt-9 sm:mt-20 sm:grid-cols-2 sm:gap-12 sm:pt-11">
          <div>
            <h2 id="payment-heading" className={heading}>Control what you spend</h2>
            <p className={`mt-5 ${paragraph}`}>
              The client reserves the quoted amount before signing. Its spending ceiling includes
              completed purchases and payments whose outcome is still uncertain. It keeps that
              record across restarts.
            </p>
          </div>
          <div>
            <h2 className={heading}>Inspect what you paid for</h2>
            <p className={`mt-5 ${paragraph}`}>
              Each completed response includes the signed job, operator receipt and exact output
              bytes. The gateway settles the customer payment after it verifies the operator&apos;s
              result. The client verifies the payment separately on Solana.
            </p>
          </div>
        </section>

        <div className="mt-14 flex flex-wrap gap-x-6 gap-y-3 border-t border-line/80 pt-7 sm:mt-20">
          {[
            { label: "Compute status", href: "/compute-status" },
            { label: "Compute", href: "/compute" },
            { label: "Gateway source", href: GATEWAY_SOURCE },
          ].map(({ label, href }) => (
            <a key={href} href={href} className="inline-flex min-h-11 items-center text-[11px] uppercase tracking-[0.2em] text-muted transition-colors hover:text-fg">
              {label} <span aria-hidden="true" className="ml-2">→</span>
            </a>
          ))}
        </div>
      </div>
      <SiteFooter className="pb-8" />
    </main>
  );
}
