import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import deployment from "../../public/compute/inference/deployment.json";
import { langchainRelease as release } from "./_release";

export const metadata: Metadata = pageMetadata(
  "/compute-langchain",
  { title: "Covenant Compute for LangChain", section: "Compute" },
  {
    title: "Covenant Compute for LangChain and LangGraph",
    description: "Add paid inference to a LangChain agent. Model-selected tool calls, Solana USDC payments, spending limits and signed Compute receipts.",
    ...(!release.accepted ? { robots: { index: false, follow: false } } : {}),
  },
);

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-muted";
const heading = "text-[13px] uppercase tracking-[0.25em] text-fg sm:text-[14px]";
const paragraph = "text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]";
const link = "underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg-soft hover:text-fg";
const price = `${(deployment.priceMicroUsdc / 1_000_000).toFixed(2)} USDC`;

const registration = `import { readFile } from 'node:fs/promises';
import { covenantInferenceTool }
  from '@covenant/compute-x402-client/langchain';

// Your application saves both IDs before the first run.
// Load the same file after a restart; never generate a new ID to retry.
const { requestId, threadId } = JSON.parse(
  await readFile('./state/run.json', 'utf8'),
);

const compute = covenantInferenceTool({
  client, // Configured CovenantX402Client: wallet, journal and spending limits.
  requestId, // One saved UUID for this logical purchase.
  model: '${deployment.model}',
  maxTokens: 2048,
  temperature: 0.2,
  systemPrompt: 'Draft only. Do not claim account actions or promise refunds.',
});

// The model can supply only { prompt: string }.
// Wallet, model, price limits and request identity stay in application code.`;

const demoDependencies = `npm install @langchain/openai@1.6.2 \\
  @langchain/langgraph-checkpoint-sqlite@1.1.0
# If native install scripts were disabled:
npm rebuild better-sqlite3 --ignore-scripts=false`;

function CodeBlock({ label, children }: { label: string; children: string }) {
  return <div className="mt-6 overflow-hidden border border-line/80 bg-raised/30">
    <div className="border-b border-line/80 px-5 py-3 font-mono text-[10px] uppercase tracking-[0.2em] text-muted sm:px-6">{label}</div>
    <pre className="overflow-x-auto px-5 py-6 text-[11px] leading-[1.9] text-fg-soft sm:px-6 sm:text-[12px]" tabIndex={0} aria-label={label}><code>{children}</code></pre>
  </div>;
}

export default function ComputeLangChainPage() {
  const accepted = release.accepted && release.acceptance;
  const install = release.packageUrl && release.packageSha256
    ? `curl --fail --location --output covenant-compute-x402-client-${release.version}.tgz \\\n  ${release.packageUrl} &&
echo "${release.packageSha256}  covenant-compute-x402-client-${release.version}.tgz" | shasum -a 256 -c - &&
npm install --ignore-scripts ./covenant-compute-x402-client-${release.version}.tgz`
    : null;
  return <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
    <SiteHeader />
    <div className="page-container">
      <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Compute</p>
      <h1 className="type-display max-w-3xl text-balance text-[2.2rem] text-fg sm:text-[2.6rem]">Add Covenant Compute to LangChain.</h1>
      <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.7] text-fg-soft sm:text-base">Give your agent an inference tool it can choose when a task needs it. Your application sets the spending limits. Covenant handles the USDC purchase and returns a signed Compute receipt.</p>
      <div className="mt-7 flex flex-wrap items-center gap-x-7 gap-y-2">
        <a href="#setup" className={`inline-flex min-h-11 items-center text-[13px] text-fg ${link}`}>Set up the tool <span aria-hidden="true" className="ml-2">↓</span></a>
        <a href="#demo" className="inline-flex min-h-11 items-center text-[13px] text-muted transition-colors hover:text-fg">Inspect the workflow <span aria-hidden="true" className="ml-2">↓</span></a>
      </div>
      {!release.accepted && <aside className="mt-8 max-w-3xl border-l border-line-strong pl-5" aria-label="Unpublished release preview"><p className={eyebrow}>Release preview · not published</p><p className={`mt-2 ${paragraph}`}>The installed package and paid LangGraph workflow are awaiting acceptance. Installation details and production evidence will be added before release.</p></aside>}
      <div className="mt-10 flex flex-wrap gap-x-6 gap-y-3 border-y border-line/80 py-5 text-[12px] text-muted sm:mt-14"><span>LangChain</span><span>LangGraph</span><span>x402</span><span>USDC on Solana</span></div>

      <section aria-labelledby="workflow-heading" className="mt-14 sm:mt-20">
        <h2 id="workflow-heading" className={heading}>The agent chooses. Your application sets the limits.</h2>
        <ol className="mt-8 grid gap-8 sm:grid-cols-3">{[
          ["01", "Select the tool", "The model receives the task and the tool definition. It decides whether to call Covenant and supplies the prompt."],
          ["02", "Check and pay", "The client checks the quote against your configured limits and payment identities before the wallet signs. The model cannot change the wallet, model or spending ceiling."],
          ["03", "Verify and continue", "The client verifies the signed result and payment, then returns the output to the agent. Keep the purchase journal to recover the same request after a restart."],
        ].map(([number,title,body])=><li key={number}><p className={eyebrow}>{number}</p><h3 className="mt-3 text-[15px] text-fg">{title}</h3><p className={`mt-3 ${paragraph}`}>{body}</p></li>)}</ol>
      </section>

      <section id="demo" aria-labelledby="demo-heading" className="mt-14 scroll-mt-28 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
        <h2 id="demo-heading" className={heading}>A support ticket, from triage to a draft reply</h2>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>In the accepted run, a live model chose the Covenant tool for a sample support ticket. The graph saved its checkpoint before payment, received the verified result, and continued with a summary for a human agent.</p>
        {accepted ? <>
          <dl className="mt-7 grid gap-6 sm:grid-cols-3">{[["Customer payment",`${accepted.customerPaymentUsdc} USDC`],["Restart replay","No additional payment"],["Completed",accepted.completedAt]].map(([name,value])=><div key={name}><dt className="text-[12px] text-muted">{name}</dt><dd className="mt-2 font-mono text-[14px] text-fg">{value}</dd></div>)}</dl>
          <div className="mt-8 border border-line/80 bg-raised/30 px-5 py-6 sm:px-6">
            <p className={eyebrow}>Returned draft · sample ticket</p>
            <p className="mt-4 text-[15px] text-fg">Billing / High priority</p>
            <p className={`mt-3 max-w-2xl ${paragraph}`}>The result requests screenshots of both charges, the time of the plan change, and details of the invoice download error. It prepares a reply asking for that evidence.</p>
            <a href="/compute/langchain/response.txt" className={`mt-3 inline-flex min-h-11 items-center text-[13px] ${link}`}>Read the exact returned text ↗</a>
          </div>
          <div className="mt-6 grid gap-x-8 gap-y-1 sm:grid-cols-2">{[
            ["Model selection and graph trace", "/compute/langchain/tool-selection.json"],
            ["Signed purchase and result", "/compute/langchain/purchase.json"],
            ["Finalized payment verification", "/compute/langchain/reconciliation.json"],
            ["Fresh-process replay record", "/compute/langchain/resume.json"],
          ].map(([label, href]) => <a key={href} href={href} className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>{label} ↗</a>)}</div>
          <p className={`mt-5 max-w-2xl ${paragraph}`}>The same saved tool call returned the identical purchase in a fresh process, with zero wallet signatures, gateway requests, RPC calls or planner calls. The original payment was independently checked as finalized on Solana.</p>
          <div className="mt-3 flex flex-wrap gap-x-7 gap-y-2"><a href={accepted.evidenceUrl} className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>Read the acceptance record ↗</a><a href={`https://explorer.solana.com/tx/${accepted.paymentTransaction}`} className={`inline-flex min-h-11 items-center text-[13px] ${link}`} target="_blank" rel="noopener noreferrer">View the USDC payment ↗</a></div>
        </> : <p className={`mt-5 max-w-2xl border-l border-line-strong pl-5 ${paragraph}`}>Acceptance pending. This section will link the recorded model-selected tool call, signed result, confirmed payment and restart replay. No production result is claimed here.</p>}
        <p className={`mt-5 max-w-2xl ${paragraph}`}>The example prepares a draft for review. It does not send a customer message or modify a support system.</p>
      </section>

      <section id="setup" aria-labelledby="setup-heading" className="mt-16 scroll-mt-28 border-t border-line/80 pt-9 sm:mt-24 sm:pt-11">
        <h2 id="setup-heading" className={heading}>Install and register the tool</h2>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>Requires Node.js 24 or later, a funded Solana USDC wallet and a persistent purchase journal. Use your existing Covenant client and planner. Configure the client with the approved service identities and your spending limits before exposing the tool to the model.</p>
        {install ? <CodeBlock label="Verified release package · terminal">{install}</CodeBlock> : <p className={`mt-5 max-w-2xl ${paragraph}`}>The versioned package URL and SHA-256 checksum will appear here after installed-package acceptance.</p>}
        <div className="mt-4 flex flex-wrap gap-x-7 gap-y-2"><a href={release.configurationUrl} className={`inline-flex min-h-11 items-center text-[13px] ${link}`} download>Download client configuration ↓</a><a href={release.documentationUrl} className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>Client setup and API ↗</a><a href={release.releaseUrl} className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>Version {release.version} release ↗</a></div>
        <p className={`mt-4 max-w-2xl ${paragraph}`}>The configuration example allows 0.05 USDC per purchase and 0.50 USDC cumulatively across reserved and completed purchases. Set your own limits and retain the same journal. Planner calls have separate credentials, costs and spending controls.</p>
        <CodeBlock label="Tool registration · JavaScript">{registration}</CodeBlock>
        {release.example ? <CodeBlock label="LangGraph agent · JavaScript">{release.example}</CodeBlock> : <p className={`mt-5 max-w-2xl ${paragraph}`}>The complete LangGraph example will be pinned to the accepted package. It will include model-selected tool invocation and recovery of the saved purchase.</p>}
        <p className={`mt-5 max-w-2xl ${paragraph}`}>Save one request UUID per logical purchase and retain the graph thread ID, persistent checkpointer and payment journal. Reuse the exact tool call when resuming. Changing the prompt, system prompt or generation settings under the same UUID is refused.</p>
        <p className={`mt-4 max-w-2xl ${paragraph}`}>A pending purchase is not a completed answer. Resume the saved tool call with its original identity and authorization. The <a href={release.demoSourceUrl} className={link}>complete LangGraph example</a> handles this with separate plan, purchase, replay and finish phases and an official SQLite checkpointer.</p>
        <details className="mt-7 border-y border-line/80 py-4">
          <summary className="cursor-pointer py-2 text-[13px] text-fg">Optional dependencies for the complete example</summary>
          <p className={`mt-4 max-w-2xl ${paragraph}`}>The packaged support workflow uses SQLite checkpoints. The example planner uses the OpenAI adapter; your application can keep its own compatible planner. These dependencies are optional for the basic tool registration.</p>
          <CodeBlock label="Complete example dependencies · terminal">{demoDependencies}</CodeBlock>
        </details>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>Qualified with LangChain 1.5.18, LangGraph 1.5.1 and LangChain Core 1.2.18: <a href="/compute/langchain/local-qualification.json" className={link}>32 source tests and 32 installed-package tests passed</a>.</p>
      </section>

      <section aria-labelledby="limits-heading" className="mt-14 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
        <h2 id="limits-heading" className={heading}>Model, price and limits</h2>
        <dl className="mt-7 divide-y divide-line/80 border-y border-line/80">{[["Model",deployment.model],["Customer price",`${price} per completed request`],["Maximum output",`${deployment.maxOutputTokens.toLocaleString("en-US")} tokens`],["Input limit",`${deployment.maxInputBytes.toLocaleString("en-US")} UTF-8 bytes across the JSON messages array`],["Payment", "Solana USDC through x402"]].map(([name,value])=><div key={name} className="grid gap-2 py-4 sm:grid-cols-[180px_1fr] sm:gap-7"><dt className="text-[12px] text-muted">{name}</dt><dd className="font-mono text-[12px] leading-[1.8] text-fg-soft">{value}</dd></div>)}</dl>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>The receipt identifies the operator and binds the job to its exact output. Review generated answers before using them; a signature does not establish their accuracy.</p>
        <p className={`mt-4 max-w-2xl ${paragraph}`}>This is an independent integration using the open-source LangChain and LangGraph frameworks.</p>
      </section>
      <div className="mt-14 flex flex-wrap gap-x-6 gap-y-3 border-t border-line/80 pt-7 sm:mt-20">{[{label:"Inference service",href:"/compute-inference"},{label:"Compute status",href:"/compute-status"},{label:"Compute",href:"/compute"},...(release.sourceUrl?[{label:"Integration source",href:release.sourceUrl}]:[])].map(({label,href})=><a key={href} href={href} className="inline-flex min-h-11 items-center text-[11px] uppercase tracking-[0.2em] text-muted transition-colors hover:text-fg">{label}<span aria-hidden="true" className="ml-2">→</span></a>)}</div>
    </div><SiteFooter className="pb-8" />
  </main>;
}
