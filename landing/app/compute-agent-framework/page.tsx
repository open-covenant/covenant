import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import deployment from "../../public/compute/inference/deployment.json";
import { agentFrameworkRelease as release } from "./_release";

export const metadata: Metadata = pageMetadata(
  "/compute-agent-framework",
  { title: "Compute for Microsoft Agent Framework", section: "Compute" },
  {
    title: "Covenant Compute for Microsoft Agent Framework",
    description: "Add a paid inference tool to Python agents built with Microsoft Agent Framework. Solana USDC payments, application spending limits and verified Compute receipts.",
    ...(!release.accepted ? { robots: { index: false, follow: false } } : {}),
  },
);

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-muted";
const heading = "text-[13px] uppercase tracking-[0.25em] text-fg sm:text-[14px]";
const paragraph = "text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]";
const link = "underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg-soft hover:text-fg";
const price = `${(deployment.priceMicroUsdc / 1_000_000).toFixed(2)} USDC`;

const registration = `import json
from agent_framework import Agent
from covenant_compute_x402.agent_framework import covenant_inference_tool

# Save the request UUID before the first run. Load it again after a restart.
with open("./state/run.json") as file:
    request_id = json.load(file)["requestId"]

compute = covenant_inference_tool(
    client=client,  # Configured wallet, journal and spending limits.
    request_id=request_id,
    model="${deployment.model}",
    max_tokens=2048,
    temperature=0.2,
    system_prompt="Prepare a draft for review. Do not claim actions were performed.",
)

agent = Agent(
    client=planner,  # Your application's separately budgeted planner.
    instructions="Review the incident and identify evidence still needed.",
    tools=[compute],
)

# The model supplies only {"prompt": "..."}.
# Wallet, model, purchase identity and spending limits stay in application code.
result = await agent.run(incident_report)`;

const workflow = `from covenant_compute_x402.workflow_demo import run_demo

# Reuse this configuration, state directory and request UUID in every process.
run = dict(
    client=client,
    state_dir="./state/incident-review",
    request_id=request_id,
    workflow_id=workflow_id,
    model="${deployment.model}",
)

# 1. The planner selects a tool. The workflow checkpoints before payment.
plan = await run_demo("plan", planner=planner, **run)

# 2. A new process resumes the exact saved tool call.
purchase = await run_demo("purchase", **run)
if purchase["purchase"]["status"] != "completed":
    # Keep the same state and identity; resume purchase to reconcile it.
    raise RuntimeError("Purchase pending; no completed answer yet.")

# 3. Replaying the saved call returns the completed purchase from the journal.
replay = await run_demo("replay", **run)

# 4. Continue with the verified answer. This phase has no paid inference tool.
review = await run_demo("finish", planner=planner, **run)`;

function CodeBlock({ label, children }: { label: string; children: string }) {
  return <div className="mt-6 overflow-hidden border border-line/80 bg-raised/30">
    <div className="border-b border-line/80 px-5 py-3 font-mono text-[10px] uppercase tracking-[0.2em] text-muted sm:px-6">{label}</div>
    <pre className="overflow-x-auto px-5 py-6 text-[11px] leading-[1.9] text-fg-soft sm:px-6 sm:text-[12px]" tabIndex={0} aria-label={label}><code>{children}</code></pre>
  </div>;
}

export default function ComputeAgentFrameworkPage() {
  const accepted = release.accepted && release.acceptance;
  const wheel = `covenant_compute_x402-${release.version}-py3-none-any.whl`;
  const install = release.packageUrl && release.packageSha256
    ? `curl --fail --location --output ${wheel} \\\n  ${release.packageUrl} &&
echo "${release.packageSha256}  ${wheel}" | shasum -a 256 -c - &&
python -m pip install './${wheel}[agent-framework]'`
    : null;
  return <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
    <SiteHeader />
    <div className="page-container">
      <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Compute</p>
      <h1 className="type-display max-w-3xl text-balance text-[2.2rem] text-fg sm:text-[2.6rem]">Covenant Compute for Microsoft Agent Framework.</h1>
      <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.7] text-fg-soft sm:text-base">Add a paid inference tool to your Python agents. Your application controls the wallet and spending limits. Covenant returns the answer with a signed Compute receipt.</p>
      <div className="mt-7 flex flex-wrap items-center gap-x-7 gap-y-2">
        <a href="#setup" className={`inline-flex min-h-11 items-center text-[13px] text-fg ${link}`}>Set up the tool <span aria-hidden="true" className="ml-2">↓</span></a>
        <a href="#workflow" className="inline-flex min-h-11 items-center text-[13px] text-muted transition-colors hover:text-fg">Inspect the workflow <span aria-hidden="true" className="ml-2">↓</span></a>
      </div>
      {!release.accepted && <aside className="mt-8 max-w-3xl border-l border-line-strong pl-5" aria-label="Unpublished release preview"><p className={eyebrow}>Release preview · not published</p><p className={`mt-2 ${paragraph}`}>The installed Python package and funded workflow are awaiting acceptance. Production evidence and installation details will be added before release.</p></aside>}
      <div className="mt-10 flex flex-wrap gap-x-6 gap-y-3 border-y border-line/80 py-5 text-[12px] text-muted sm:mt-14"><span>Microsoft Agent Framework</span><span>Python</span><span>x402</span><span>USDC on Solana</span></div>

      <section aria-labelledby="how-heading" className="mt-14 sm:mt-20">
        <h2 id="how-heading" className={heading}>An inference tool with application-controlled spending</h2>
        <ol className="mt-8 grid gap-8 sm:grid-cols-3">{[
          ["01", "Select", "The planner sees the task and tool definition. It can choose Covenant and supply a prompt. The wallet, model and payment limits are fixed by your application."],
          ["02", "Pay and verify", "The client checks the quote and payment identities before signing. It verifies the operator’s signed result and the settled USDC payment before returning a completed answer."],
          ["03", "Resume", "Retain the purchase journal and workflow checkpoints. The same completed request returns its saved result after a restart, without another payment."],
        ].map(([number,title,body])=><li key={number}><p className={eyebrow}>{number}</p><h3 className="mt-3 text-[15px] text-fg">{title}</h3><p className={`mt-3 ${paragraph}`}>{body}</p></li>)}</ol>
      </section>

      <section id="workflow" aria-labelledby="workflow-heading" className="mt-14 scroll-mt-28 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
        <h2 id="workflow-heading" className={heading}>An incident report, prepared for review</h2>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>The example gives an agent a sample checkout incident: errors rose after a deployment and fell after a rollback. The task is to separate observations from hypotheses, identify missing evidence and recommend follow-up.</p>
        {accepted ? <>
          <dl className="mt-7 grid gap-6 sm:grid-cols-3">{[["Customer payment",`${accepted.customerPaymentUsdc} USDC`],["Restart replay","No additional payment"],["Verified",accepted.completedAt]].map(([name,value])=><div key={name}><dt className="text-[12px] text-muted">{name}</dt><dd className="mt-2 font-mono text-[14px] text-fg">{value}</dd></div>)}</dl>
          <div className="mt-8 border border-line/80 bg-raised/30 px-5 py-6 sm:px-6"><p className={eyebrow}>Returned analysis · sample incident</p><p className={`mt-4 max-w-2xl ${paragraph}`}>{accepted.resultSummary}</p><div className="mt-3 flex flex-wrap gap-x-7 gap-y-2"><a href="/compute/agent-framework/response.txt" className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>Read the exact returned text ↗</a><a href="/compute/agent-framework/review-notes.json" className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>Read the review notes ↗</a></div><p className={`mt-3 max-w-2xl ${paragraph}`}>The unedited draft and planner summary contain timing, arithmetic and causality errors, documented in the review notes. A verified signature does not establish answer correctness.</p></div>
          <div className="mt-6 grid gap-x-8 gap-y-1 sm:grid-cols-2">{[
            ["Model selection and saved checkpoint", "/compute/agent-framework/tool-selection.json"],
            ["Signed purchase and result", "/compute/agent-framework/purchase.json"],
            ["Finalized payment verification", "/compute/agent-framework/reconciliation.json"],
            ["Fresh-process replay record", "/compute/agent-framework/resume.json"],
            ["Planner continuation and separate cost", "/compute/agent-framework/planner-summary.json"],
          ].map(([label, href]) => <a key={href} href={href} className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>{label} ↗</a>)}</div>
          <p className={`mt-5 max-w-2xl ${paragraph}`}>The model selected the tool before payment. The workflow saved that call, completed the purchase, then replayed the identical result in a fresh process without another signature, gateway request, RPC call or planner call.</p>
          <div className="mt-3 flex flex-wrap gap-x-7 gap-y-2"><a href={accepted.evidenceUrl} className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>Read the acceptance record ↗</a><a href={`https://explorer.solana.com/tx/${accepted.paymentTransaction}`} className={`inline-flex min-h-11 items-center text-[13px] ${link}`} target="_blank" rel="noopener noreferrer">View the USDC payment ↗</a></div>
        </> : <p className={`mt-5 max-w-2xl border-l border-line-strong pl-5 ${paragraph}`}>Acceptance pending. The completed record will include the model-selected tool call, signed result, finalized payment and restart replay.</p>}
        <p className={`mt-5 max-w-2xl ${paragraph}`}>This workflow prepares an analysis for a human reviewer. It does not modify infrastructure, contact an on-call engineer or establish a confirmed root cause.</p>
      </section>

      <section id="setup" aria-labelledby="setup-heading" className="mt-16 scroll-mt-28 border-t border-line/80 pt-9 sm:mt-24 sm:pt-11">
        <h2 id="setup-heading" className={heading}>Install and register the tool</h2>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>Requires Python 3.11 or later, a funded Solana USDC wallet and a persistent purchase journal. Keep your existing planner and configure its spending controls separately. The Covenant adapter registers an official Agent Framework FunctionTool.</p>
        {install ? <CodeBlock label="Verified Python wheel · terminal">{install}</CodeBlock> : <p className={`mt-5 max-w-2xl ${paragraph}`}>The immutable wheel URL and SHA-256 checksum will appear after installed-package acceptance.</p>}
        <div className="mt-4 flex flex-wrap gap-x-7 gap-y-2">{[
          ["Client configuration",release.configurationUrl],["Client setup and API",release.documentationUrl],
          [`Version ${release.version} release`,release.releaseUrl],
        ].map(([label,href])=>href&&<a key={href} href={href} className={`inline-flex min-h-11 items-center text-[13px] ${link}`}>{label} ↗</a>)}</div>
        <CodeBlock label="Tool registration · Python">{registration}</CodeBlock>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>Save one request UUID per logical purchase before dispatch. Reuse the exact prompt, system policy, model and generation settings when resuming. A changed request under the same UUID is refused. Retain the journal even when a payment outcome is uncertain. When switching between Python and TypeScript clients, use the same journal and cumulative ceiling.</p>
        <CodeBlock label="Persistent workflow · Python">{workflow}</CodeBlock>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>The complete example runs on Linux or macOS and uses Agent Framework’s FileCheckpointStorage and WorkflowBuilder. Store checkpoints in a private directory controlled by your application. A pending purchase remains pending; the workflow cannot finish until it has a verified completed result.</p>
        {release.demoSourceUrl && <a href={release.demoSourceUrl} className={`mt-3 inline-flex min-h-11 items-center text-[13px] ${link}`}>Read the complete workflow example ↗</a>}
        <details className="mt-7 border-y border-line/80 py-4"><summary className="cursor-pointer py-2 text-[13px] text-fg">Planner adapter for the complete example</summary><p className={`mt-4 max-w-2xl ${paragraph}`}>The example accepts a configured official chat client. To use the OpenAI-compatible planner adapter, install the optional demo dependencies. Supply your own credentials and a bounded transport; planner calls are billed separately from Covenant inference.</p><CodeBlock label="Optional planner adapter · terminal">{`python -m pip install 'agent-framework-openai==1.17.0'`}</CodeBlock></details>
        {release.qualifiedTests !== null && <p className={`mt-5 max-w-2xl ${paragraph}`}>Qualified with Agent Framework Core 1.22.0 and its OpenAI adapter 1.17.0: <a href="/compute/agent-framework/local-qualification.json" className={link}>{release.qualifiedTests} source tests and {release.qualifiedTests} installed-wheel tests passed</a>.</p>}
      </section>

      <section aria-labelledby="limits-heading" className="mt-14 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
        <h2 id="limits-heading" className={heading}>Model, price and limits</h2>
        <dl className="mt-7 divide-y divide-line/80 border-y border-line/80">{[["Model",deployment.model],["Customer price",`${price} per completed request`],["Maximum output",`${deployment.maxOutputTokens.toLocaleString("en-US")} tokens`],["Input limit",`${deployment.maxInputBytes.toLocaleString("en-US")} UTF-8 bytes across the JSON messages array`],["Payment", "Solana USDC through x402"]].map(([name,value])=><div key={name} className="grid gap-2 py-4 sm:grid-cols-[180px_1fr] sm:gap-7"><dt className="text-[12px] text-muted">{name}</dt><dd className="font-mono text-[12px] leading-[1.8] text-fg-soft">{value}</dd></div>)}</dl>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>The receipt binds the operator and job to the exact output. It verifies provenance and integrity; review generated answers before acting on them.</p>
        <p className={`mt-4 max-w-2xl ${paragraph}`}>This independent integration uses the open-source Microsoft Agent Framework and registers Covenant as an inference tool in your existing agent.</p>
      </section>
      <div className="mt-14 flex flex-wrap gap-x-6 gap-y-3 border-t border-line/80 pt-7 sm:mt-20">{[{label:"Inference service",href:"/compute-inference"},{label:"LangChain + LangGraph",href:"/compute-langchain"},{label:"Compute status",href:"/compute-status"},...(release.sourceUrl?[{label:"Integration source",href:release.sourceUrl}]:[])].map(({label,href})=><a key={href} href={href} className="inline-flex min-h-11 items-center text-[11px] uppercase tracking-[0.2em] text-muted transition-colors hover:text-fg">{label}<span aria-hidden="true" className="ml-2">→</span></a>)}</div>
    </div><SiteFooter className="pb-8" />
  </main>;
}
