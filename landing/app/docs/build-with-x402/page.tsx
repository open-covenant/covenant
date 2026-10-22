import Link from "next/link";
import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = [
  "build-with-x402",
  "Build with x402",
  "Accept x402 payments in USDC on Solana and Base through Covenant's free facilitator, and give your agents paid AI inference and research reports with signed receipts.",
] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

const sellerInstall = `npm install express @x402/express @x402/core @x402/svm @x402/evm @x402/extensions`;

const sellerCode = `import express from 'express';
import { paymentMiddleware, x402ResourceServer } from '@x402/express';
import { HTTPFacilitatorClient } from '@x402/core/server';
import { ExactSvmScheme } from '@x402/svm/exact/server';
import { ExactEvmScheme } from '@x402/evm/exact/server';
import { declareDiscoveryExtension, bazaarResourceServerExtension } from '@x402/extensions/bazaar';

const app = express();
app.use(express.json());

const facilitator = new HTTPFacilitatorClient({ url: 'https://facilitator.opencovenant.org' });
const server = new x402ResourceServer(facilitator)
  .register('solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp', new ExactSvmScheme())
  .register('eip155:8453', new ExactEvmScheme())
  .registerExtension(bazaarResourceServerExtension);

app.use(paymentMiddleware({
  'POST /api/intel': {
    accepts: [
      { scheme: 'exact', price: '$0.01', network: 'eip155:8453', payTo: '0xYourBaseAddress' },
      { scheme: 'exact', price: '$0.01', network: 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp', payTo: 'YourSolanaAddress' },
    ],
    description: 'Street intel for one district',
    // Optional: lets the facilitator's catalog list this endpoint for agents.
    extensions: {
      ...declareDiscoveryExtension({
        bodyType: 'json',
        input: { district: 'Lowline' },
        inputSchema: { properties: { district: { type: 'string' } }, required: ['district'] },
        output: { example: { district: 'Lowline', heat: 3 } },
      }),
    },
  },
}, server));

app.post('/api/intel', (req, res) => res.json({ district: req.body.district, heat: 3 }));
app.listen(3000);`;

const requestBody = `{
  "model": "claude-haiku-4.5",
  "messages": [{ "role": "user", "content": "Pick tonight's job for a crew of three." }],
  "max_tokens": 512,
  "temperature": 0.2,
  "pricing": "model_tariff_v1"
}`;

const buyerInstall = `npm install @x402/fetch @x402/evm viem`;

const buyerCode = `import { wrapFetchWithPayment, x402Client } from '@x402/fetch';
import { registerExactEvmScheme } from '@x402/evm/exact/client';
import { privateKeyToAccount } from 'viem/accounts';

const client = new x402Client();
registerExactEvmScheme(client, {
  signer: privateKeyToAccount(process.env.AGENT_PRIVATE_KEY),
  // A spending rule: never pay more than $0.04 for one answer.
  policies: [(version, offers) => offers.filter(offer => BigInt(offer.amount) <= 40_000n)],
});
const pay = wrapFetchWithPayment(fetch, client);

const response = await pay('https://compute-inference.opencovenant.org/v1/chat/completions', {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({
    model: 'claude-haiku-4.5',
    messages: [{ role: 'user', content: "Pick tonight's job for a crew of three." }],
    max_tokens: 512,
    pricing: 'model_tariff_v1',
  }),
});
const answer = await response.json();
console.log(answer.choices[0].message.content);`;

const researchInstall = `npm install @x402/fetch@2.28.0 @x402/svm@2.28.0 @solana/kit`;

const researchCode = `// Run as an ES module (for example research.mjs) with Node 22 or later.
import { readFile, writeFile } from 'node:fs/promises';
import { wrapFetchWithPayment, x402Client } from '@x402/fetch';
import { registerExactSvmScheme } from '@x402/svm/exact/client';
import { createKeyPairSignerFromBytes } from '@solana/kit';

// The agent's 64-byte Solana secret key, for example a keypair file from solana-keygen.
const agentKeyBytes = new Uint8Array(JSON.parse(await readFile(process.env.AGENT_KEYPAIR, 'utf8')));

const client = new x402Client();
client.setSpendControls({ maxAmountPerPayment: '$5' });
registerExactSvmScheme(client, { signer: await createKeyPairSignerFromBytes(agentKeyBytes) });
const pay = wrapFetchWithPayment(fetch, client);

const started = await pay('https://compute-inference.opencovenant.org/v1/jobs/research', {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ objective: 'Which agent wallets let a user cap what an agent can spend, and how?' }),
});
if (started.status !== 202) throw new Error(\`Order failed (\${started.status}): \${await started.text()}\`);
const { status_url } = await started.json();
await writeFile('research-job.txt', status_url); // the way back to the report

let job;
do {
  await new Promise(resolve => setTimeout(resolve, 60_000));
  const response = await fetch(status_url);
  if (!response.ok) continue; // try again next minute
  job = await response.json();
} while (!job || ['paying', 'running', 'refunding'].includes(job.status));

if (job.status === 'completed') console.log(job.result.report);
else console.log(\`The job ended as \${job.status}.\`, job.refund?.transaction ?? '');`;

const sdkInstall = `# JavaScript 0.4.0 (Node 24; includes AgentKit and LangChain/LangGraph tools)
npm install https://github.com/open-covenant/covenant/releases/download/compute-x402-client-v0.4.0/covenant-compute-x402-client-0.4.0.tgz

# Python 0.3.0
pip install https://github.com/open-covenant/covenant/releases/download/compute-x402-python-v0.3.0/covenant_compute_x402-0.3.0-py3-none-any.whl`;

export default function BuildWithX402Page() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>Build with x402</h1>
      <p>Covenant gives builders two things over x402:</p>
      <ul>
        <li>
          <strong>A free, public facilitator.</strong> If you sell anything over HTTP, such as an
          API, game data or a service an agent calls, the facilitator checks and settles USDC
          payments for you on Solana and Base. You need no account and no API key, and Covenant pays the network fees
          from its own wallets.
        </li>
        <li>
          <strong>Covenant Compute</strong> sells AI inference and research reports that agents
          pay for in USDC. An agent with its own wallet can buy an answer or a report without an
          account or API key, and each one comes with a receipt signed by the operator that handled
          it. An operator is the server that handles your request, and today Covenant runs every
          operator itself.
        </li>
      </ul>
      <p>
        Here is how x402 works: a client calls your endpoint, gets HTTP <code>402</code> with the
        price and where to pay, signs a payment, and calls again with that payment in a header. Your
        server asks a facilitator to verify and settle the payment, then answers. Any x402 v2 client
        can pay, including agent wallets. Payments settle in USDC on Solana or Base, so a product
        whose own economy runs on a different chain can still accept them.
      </p>

      <h2 id="facilitator">Accept payments through the Covenant facilitator</h2>
      <table>
        <thead>
          <tr>
            <th>What</th>
            <th>Value</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>Facilitator</td>
            <td>
              <code>https://facilitator.opencovenant.org</code>
            </td>
          </tr>
          <tr>
            <td>Supported payments</td>
            <td>
              <code>https://facilitator.opencovenant.org/supported</code>
            </td>
          </tr>
          <tr>
            <td>Solana mainnet</td>
            <td>
              <code>solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp</code>, USDC{" "}
              <code>EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v</code>
            </td>
          </tr>
          <tr>
            <td>Base mainnet</td>
            <td>
              <code>eip155:8453</code>, USDC <code>0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913</code>
            </td>
          </tr>
          <tr>
            <td>Scheme</td>
            <td>
              <code>exact</code>: the buyer pays the price you set
            </td>
          </tr>
          <tr>
            <td>Source</td>
            <td>
              <a href="https://github.com/open-covenant/covenant/tree/main/services/x402-facilitator">
                github.com/open-covenant/covenant/tree/main/services/x402-facilitator
              </a>
            </td>
          </tr>
        </tbody>
      </table>
      <p>It runs on mainnet only; there is no test network.</p>

      <h3>Set up a seller</h3>
      <p>
        With Node 24 and Express, install the official x402 packages, then point the middleware at
        the facilitator. We ran this setup against the live facilitator, and it returns a{" "}
        <code>402</code> that offers payment on both networks.
      </p>
      <pre>
        <code>{sellerInstall}</code>
      </pre>
      <pre>
        <code>{sellerCode}</code>
      </pre>
      <ul>
        <li>
          The Solana fee payer is filled in automatically from <code>/supported</code>; you never
          handle it. On Base, the facilitator submits the buyer&apos;s signed USDC transfer and pays
          the gas.
        </li>
        <li>
          <code>payTo</code> is your own receiving address. On Solana it needs a USDC token account
          before the first payment.
        </li>
        <li>
          The same works in other frameworks: the x402 project publishes <code>@x402/hono</code>{" "}
          and <code>@x402/next</code>, and <code>HTTPFacilitatorClient</code> with the URL above
          works in any of them.
        </li>
        <li>
          If the facilitator cannot be reached, the middleware answers <code>502</code>; if a
          settlement fails, it answers <code>402</code>. In both cases your route&apos;s response is
          not sent, so nothing is served without payment.
        </li>
      </ul>

      <h3>Get listed so agents can find you</h3>
      <p>
        If your route declares the Bazaar discovery extension (the{" "}
        <code>declareDiscoveryExtension</code>{" "}
        block above), the facilitator adds your endpoint to
        Covenant&apos;s public catalog after its first settled payment:
      </p>
      <pre>
        <code>{`https://facilitator.opencovenant.org/discovery/resources
https://facilitator.opencovenant.org/discovery/search?query=your+words`}</code>
      </pre>
      <p>
        Each listing is tied to your URL together with your receiving address, so another seller
        cannot overwrite it. A later paid request with an updated declaration updates your listing;
        to remove one, write to{" "}
        <a href="mailto:partnerships@opencovenant.org">partnerships@opencovenant.org</a>.
      </p>
      <p>
        The Bazaar extension is a shared x402 format. Declaring it lists you in Covenant&apos;s
        catalog, not in Coinbase&apos;s x402 Bazaar. Coinbase&apos;s catalog lists only endpoints
        that its own facilitator settles, so getting listed there is a separate setup.
      </p>

      <h3>Limits</h3>
      <p>These keep a free service from being abused:</p>
      <ul>
        <li>USDC only, exact scheme only, and nothing under $0.001.</li>
        <li>
          A payment to the payer&apos;s own address is refused, so test from a different wallet than
          your <code>payTo</code> address.
        </li>
        <li>
          Each receiving address can settle up to 500 payments per network per UTC day. Across all
          sellers, the facilitator settles at most 2,000 payments per network per UTC day. When a cap
          is reached, the facilitator returns the error <code>pay_to_daily_limit_reached</code> or{" "}
          <code>facilitator_daily_limit_reached</code>, your middleware refuses the request, and the
          buyer is not charged. The counts reset at midnight UTC. The facilitator does not publish
          how much of the shared daily cap is left.
        </li>
        <li>
          On Solana, a payment may ask for at most 100,000 compute units at a priority fee of at most
          0.1 lamports per unit; standard x402 clients stay well under both. Payments from the
          Phantom wallet, which adds its own safety checks, may spend at most 20,000 lamports in
          priority fees. On Base, a smart wallet must already be deployed before it can pay.
        </li>
        <li>
          Your server can make up to 120 verify and settle calls per minute from one IP address. A
          payment uses one of each, so that is about 60 paid requests a minute per server.
        </li>
      </ul>
      <p>
        If you expect more volume, write to{" "}
        <a href="mailto:partnerships@opencovenant.org">partnerships@opencovenant.org</a> before you
        launch. Test with small real payments; the minimum is $0.001, and any x402 v2 client can act
        as the test buyer, including the one below.
      </p>

      <h2 id="compute">Give your agents paid AI inference</h2>
      <table>
        <thead>
          <tr>
            <th>What</th>
            <th>Value</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>Endpoint</td>
            <td>
              <code>POST https://compute-inference.opencovenant.org/v1/chat/completions</code>
            </td>
          </tr>
          <tr>
            <td>Models and prices (free)</td>
            <td>
              <code>GET https://compute-inference.opencovenant.org/v1/models</code>
            </td>
          </tr>
          <tr>
            <td>Discovery</td>
            <td>
              <code>/.well-known/x402.json</code> and <code>/openapi.json</code> on the same host
            </td>
          </tr>
          <tr>
            <td>Pay with</td>
            <td>USDC on Solana or Base; no API key, account or SDK required</td>
          </tr>
          <tr>
            <td>Listed on</td>
            <td>Coinbase&apos;s x402 Bazaar and x402scan</td>
          </tr>
        </tbody>
      </table>
      <ul>
        <li>Model: <code>claude-haiku-4.5</code>, text only. No streaming, tools or images.</li>
        <li>
          Input: 1 to 32 messages with the roles <code>system</code>, <code>user</code> or{" "}
          <code>assistant</code>. The messages array, as compact JSON, may be at most 4,096 bytes.
        </li>
        <li>Output: up to 2,048 tokens per request.</li>
        <li>
          Covenant&apos;s operator sends your messages to the model through a third-party model
          provider, and Covenant keeps the model&apos;s response and a hash of your request as the
          record behind your receipt. Send only what you would send to a hosted model API.
        </li>
      </ul>

      <h3>Price</h3>
      <p>
        You pay a fixed price before the request runs. It depends on the <code>max_tokens</code> you
        ask for, not on how many tokens the answer actually uses. Send{" "}
        <code>&quot;pricing&quot;: &quot;model_tariff_v1&quot;</code> and the price is $0.012 plus
        $0.000009 per token of <code>max_tokens</code>:
      </p>
      <table>
        <thead>
          <tr>
            <th>
              <code>max_tokens</code>
            </th>
            <th>Price</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>256</td>
            <td>$0.014304</td>
          </tr>
          <tr>
            <td>512</td>
            <td>$0.016608</td>
          </tr>
          <tr>
            <td>2,048</td>
            <td>$0.030432</td>
          </tr>
        </tbody>
      </table>
      <p>
        Requests without <code>pricing</code> cost a flat $0.05, which is more than the tariff costs
        at any <code>max_tokens</code>, so always send it. Current prices, and how long each price
        stays valid, are at <code>GET /v1/models</code>.
      </p>

      <h3>How a paid request works</h3>
      <pre>
        <code>{requestBody}</code>
      </pre>
      <ol>
        <li>
          Your first <code>POST</code> returns <code>402</code>. The body, and the{" "}
          <code>Payment-Required</code> header, offer two ways to pay the same price: USDC on Solana
          or on Base.
        </li>
        <li>
          Your x402 client signs one payment and sends the identical body again with the{" "}
          <code>Payment-Signature</code> header.
        </li>
        <li>
          The model runs. Only once the answer exists is your payment settled, so a failed run is
          never charged: the payment is simply never submitted.
        </li>
        <li>
          The answer arrives as an OpenAI-style chat completion, usually 5 to 10 seconds after
          payment in our tests. The <code>Payment-Response</code> header carries the settled
          transaction.
        </li>
        <li>
          Covenant&apos;s gateway, the service that takes your payment, holds a paid request for up
          to 25 seconds while the payment settles. If it needs longer, you get <code>202</code> with
          a <code>Retry-After</code> header: wait, then send the identical request again with the same{" "}
          <code>Payment-Signature</code>. You are not charged twice. The Covenant SDKs do this for
          you; with a plain x402 client, keep the <code>Payment-Signature</code> you sent so you can
          repeat it.
        </li>
        <li>
          Optional: add an <code>Idempotency-Key</code> header containing a UUID. If you resend the
          same body with the same key before paying, you get the same <code>402</code> offer back,
          so a retry keeps the original price and payment details.
        </li>
        <li>
          Covenant does not limit requests per wallet. When the service is at capacity it answers{" "}
          <code>503</code>; retry with backoff.
        </li>
      </ol>

      <h3>Pay from an agent</h3>
      <p>With the official x402 client, paying on Base from Node:</p>
      <pre>
        <code>{buyerInstall}</code>
      </pre>
      <pre>
        <code>{buyerCode}</code>
      </pre>
      <p>
        For Solana, use <code>@x402/svm</code> instead:{" "}
        <code>registerExactSvmScheme(client, {"{ signer }"})</code> with a signer from{" "}
        <code>@solana/kit</code> (<code>createKeyPairSignerFromBytes</code>). The agent&apos;s wallet
        needs only USDC. It needs no SOL on Solana and no ETH on Base, because the network fees are
        paid for it.
      </p>

      <h3 id="receipts">Check a receipt</h3>
      <p>
        Every answer includes a <code>covenant</code> object. Its main fields are{" "}
        <code>envelope</code> (Covenant&apos;s signed record of your paid request, called the job),{" "}
        <code>output</code> and <code>output_json</code> (the model&apos;s output), and{" "}
        <code>receipt</code> (the operator&apos;s signed receipt), along with <code>job_id</code>,{" "}
        <code>quote_id</code> and <code>request_sha256</code>. The receipt lists the status, job
        hash, result hash, price and time.
      </p>
      <p>
        The receipt is the operator&apos;s signed statement that it produced this output for this
        job. You can check it later to confirm what you paid for and that the output has not been
        changed. To check one in any language:
      </p>
      <ol>
        <li>
          The job is signed with Ed25519 by the gateway key over the text{" "}
          <code>covenant.compute.job.v1</code> and a newline, followed by{" "}
          <code>envelope.payload_json</code>.
        </li>
        <li>
          The receipt is signed with Ed25519 by the operator key over{" "}
          <code>covenant.compute.receipt.v1</code> and a newline, followed by{" "}
          <code>receipt.receipt_json</code>. Signatures and keys are base58 (
          <code>signature_b58</code>, <code>signer_pubkey_b58</code>).
        </li>
        <li>
          <code>receipt.job_hash_hex</code> is the SHA-256 of <code>envelope.payload_json</code>, and{" "}
          <code>receipt.result_hash_hex</code> is the SHA-256 of <code>output_json</code>.
        </li>
        <li>Pin the signing keys below rather than trusting the ones inside a response.</li>
      </ol>
      <table>
        <thead>
          <tr>
            <th>Key</th>
            <th>Value</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>Operator (inference)</td>
            <td>
              <code>9fefASd52qfR2Q8P81yXtkHu9E4q9KvW58x2ixBAAvLR</code>
            </td>
          </tr>
          <tr>
            <td>Gateway (inference)</td>
            <td>
              <code>F9KjruGNU9f9tr5RXiBZfENeD6f8qAMUX5LQDEt9qdn1</code>
            </td>
          </tr>
          <tr>
            <td>Operator (research reports)</td>
            <td>
              <code>fqtEZWvgFENXwAh6Wd4448iX3jaVghUSqGZ5LcvJ6Yq</code>
            </td>
          </tr>
          <tr>
            <td>Gateway (research reports)</td>
            <td>
              <code>D6kFBPgnVuiXiYeVsTj1JuqySN7ukRkoZBRvtkkLMWL</code>
            </td>
          </tr>
        </tbody>
      </table>
      <p>
        When a key changes, the new key is listed here with the date it took effect, and the
        previous key stays listed so earlier receipts can still be checked. For a complete
        reference implementation, see{" "}
        <a href="https://github.com/open-covenant/covenant/blob/deploy/compute-coordinator/sdk/compute-x402-client/proof.mjs">
          proof.mjs
        </a>{" "}
        in the JavaScript SDK.
      </p>

      <h3>Covenant SDKs</h3>
      <p>
        The SDKs are optional. They add receipt checks, spending caps and a local record of
        purchases, so an agent that crashes and restarts never pays twice for the same request. They
        currently pay only in USDC on Solana; to pay on Base, use the official x402 client above.
        Releases and checksums are on{" "}
        <a href="https://github.com/open-covenant/covenant/releases">GitHub</a>.
      </p>
      <pre>
        <code>{sdkInstall}</code>
      </pre>

      <h2 id="research">Buy a research report</h2>
      <p>
        An agent can also order a research report that answers a question of its own choosing and
        cites its sources. The research is done by IMD, a network of AI agents that are paid to
        carry out research and other tasks; you can learn more at{" "}
        <a href="https://identity.md">identity.md</a>. Covenant places the order with IMD for you,
        and the Covenant operator that handled your job signs a receipt that records the job and
        the exact report it returned.
      </p>
      <p>
        A report costs 5 USDC on Solana mainnet (USDC mint{" "}
        <code>EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v</code>) and usually takes a few minutes.
        The paying wallet needs no SOL, because Covenant pays the network fees.
      </p>
      <ol>
        <li>
          Send a POST request to{" "}
          <code>https://compute-inference.opencovenant.org/v1/jobs/research</code> with the header{" "}
          <code>Content-Type: application/json</code> and a body such as{" "}
          <code>{"{\"objective\": \"...\"}"}</code>. The objective is the question the report
          should answer, in 20 to 4,000 characters. A request without such an objective is refused
          with HTTP <code>400</code>, and you are not charged. Otherwise the server replies with
          HTTP <code>402</code> and the payment requirements, including the 5 USDC price.
        </li>
        <li>
          Pay with an x402 version 2 client that supports the <code>exact</code> scheme on Solana,
          resending the same body. The server replies with HTTP <code>202</code> and the job&apos;s
          status, including an absolute <code>status_url</code>. Work starts only after your
          payment settles.
        </li>
        <li>
          Poll the <code>status_url</code> about once a minute; these requests are free. The{" "}
          <code>status</code> is one of <code>paying</code>, <code>running</code>,{" "}
          <code>completed</code>, <code>refunding</code>, <code>refunded</code> or{" "}
          <code>unpaid</code>. Unpaid means the payment did not go through and nothing was charged.
          A <code>message</code> field explains the status in words.
        </li>
        <li>
          When the status is <code>completed</code>, <code>result.report</code> holds the report in
          Markdown. The result also includes <code>result.files</code>, which lists the files that
          make up the report, each with its size, SHA-256 hash and a link;{" "}
          <code>result.imd.job_id</code>, which is IMD&apos;s ID for the order; and{" "}
          <code>result.covenant</code>, which holds the signed job, the exact output and the
          operator&apos;s signed receipt. You can check these as described in{" "}
          <a href="#receipts">Check a receipt</a>.
        </li>
      </ol>
      <p>
        If the job cannot be completed, the 5 USDC is refunded to the wallet that paid, usually
        within a few minutes. The status changes to <code>refunded</code>, and{" "}
        <code>refund.transaction</code> identifies the Solana transaction that returned your USDC.
      </p>
      <p>
        Each job has a two-hour deadline. A job that cannot start within an hour of payment is
        refunded, and so is a job that has neither completed nor been refunded a day after its
        deadline. Most jobs reach a final status within minutes; the longest possible wait is
        about 27 hours.
      </p>
      <p>
        Save the <code>status_url</code> as soon as you receive it and keep it private. Anyone who
        has the URL can read the report. If the <code>202</code> response is lost, send the same
        request again with the same <code>Payment-Signature</code> header: you get the job&apos;s
        status and its URL back, and you are not charged again. Reports are deleted seven days
        after they complete, so save a copy before then.
      </p>
      <p>
        By default, <code>@x402/fetch</code> 2.28.0 refuses any single payment over $1, so raise
        its limit to $5 before you buy a report. The example below does that, sends the request,
        saves the <code>status_url</code>, and then polls it once a minute until the job is
        completed, refunded or unpaid.
      </p>
      <pre>
        <code>{researchInstall}</code>
      </pre>
      <pre>
        <code>{researchCode}</code>
      </pre>

      <h2 id="agent-games">Use it in an agent game</h2>
      <p>
        Games where AI agents play beside people, each agent with its own wallet, can use both
        services:
      </p>
      <ul>
        <li>
          <strong>Agents that pay for their own thinking.</strong>{" "}
          Each agent wallet holds a little
          USDC and buys its reasoning per call: choosing a move, solving a daily puzzle, answering a
          challenge. You don&apos;t hand out API keys or carry one large inference bill. Five puzzle
          attempts a day at a 512-token cap cost about $0.08 per agent per day.
        </li>
        <li>
          <strong>Receipts as proof of play.</strong>{" "}
          A tournament can ask agents to submit the
          receipt behind an answer, then check the operator&apos;s signature and that the
          receipt&apos;s result hash matches the submitted answer. That shows the operator signed for
          this answer at a given time and that it hasn&apos;t been edited since. It does not show
          that the agent had no other help, or that this was its only attempt.
        </li>
        <li>
          <strong>Game-run characters with a record.</strong>{" "}
          The game&apos;s own server can buy
          inference for its host characters, broadcasts or clues, and publish each receipt, so
          players can see the text came from a signed model run.
        </li>
        <li>
          <strong>Selling to agents.</strong>{" "}
          If the game wants agents to pay for something, such as
          premium data, hints or entry to side events, it can put x402 on those API routes with the
          seller setup above. Agents pay in USDC on Base or Solana with no account, and the routes
          appear in the facilitator&apos;s catalog for other agents to find.
        </li>
      </ul>
      <p>What to know first:</p>
      <ul>
        <li>
          The facilitator settles USDC on Solana and Base only. A game whose own economy runs on
          another chain, such as Robinhood Chain, keeps it there; x402 payments in USDC sit beside
          it rather than replacing it.
        </li>
        <li>
          Compute is one text model with short inputs (4 KB) and outputs of up to 2,048 tokens. That
          suits decisions, puzzle attempts and short dialogue, not long documents or images.
        </li>
        <li>
          Each request takes several seconds and is paid on its own, so it fits turn decisions and
          daily challenges better than real-time action.
        </li>
      </ul>

      <h2>Links</h2>
      <ul>
        <li>
          <Link href="/x402">Covenant x402</Link>: how agents built on Covenant pay for and sell
          services over x402.
        </li>
        <li>
          Covenant Compute also offers GPU rental and coding agents; see{" "}
          <Link href="/compute">Compute</Link>.
        </li>
        <li>
          <a href="https://opencovenant.org/compute-status">Compute status</a>,{" "}
          <a href="https://opencovenant.org/terms">terms</a> and{" "}
          <a href="https://opencovenant.org/privacy">privacy</a>.
        </li>
        <li>
          Questions and volume plans:{" "}
          <a href="mailto:partnerships@opencovenant.org">partnerships@opencovenant.org</a>.
        </li>
        <li>
          The x402 protocol: <a href="https://x402.org">x402.org</a>.
        </li>
      </ul>
    </>
  );
}
