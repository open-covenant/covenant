/**
 * An agent hires Mizuki.
 *
 * Mizuki fixes one authorized issue in a public GitHub repository for a fixed
 * price, paid in USDC on Solana, and refunds the price if the pull request does
 * not pass that repository's own checks.
 *
 * The tools here are the published ones: `mizuki-agent-tools/langchain` gives
 * back four LangChain tools, and this script calls them the way an agent would.
 * The only thing it adds is a wallet. `MizukiToolset` accepts a `fetchImpl`, so
 * an x402 payer slots underneath the tools and a priced endpoint answers instead
 * of returning its price.
 *
 *     npm install
 *     SOLANA_KEYPAIR_PATH=./wallet.json node hire-mizuki.mjs
 *
 * The assessment in step 1 costs 0.001 USDC and this script pays it. Hiring
 * Mizuki in step 3 costs the quoted price, and the script will not spend that
 * without MIZUKI_HIRE_FOR_REAL=1. Every step says whether it moved money.
 */
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { createKeyPairSignerFromBytes } from '@solana/kit';
import { x402Client, x402HTTPClient } from '@x402/core/client';
import { registerExactSvmScheme } from '@x402/svm/exact/client';
import { MizukiToolset } from 'mizuki-agent-tools';
import { getMizukiTools } from 'mizuki-agent-tools/langchain';

const TARGET = { owner: 'open-covenant', repo: 'covenant' };
const ISSUE_URL =
  process.env.MIZUKI_ISSUE_URL ?? 'https://github.com/open-covenant/covenant/issues/189';
const RPC_URL = process.env.SOLANA_RPC_URL ?? 'https://api.mainnet-beta.solana.com';

const LABEL_WIDTH = 12;
const usd = (atomic) => `${(Number(atomic) / 1e6).toFixed(6)} USDC`;
const short = (value) => `${value.slice(0, 8)}…${value.slice(-4)}`;
const say = (label, ...rest) => console.log(label.padEnd(LABEL_WIDTH), ':', ...rest);
const indent = (text) => text.replace(/\n/g, `\n${' '.repeat(LABEL_WIDTH + 3)}`);
const heading = (text) => console.log(`\n${text}\n${'-'.repeat(text.length)}`);

// A funded Solana wallet, as a 64-byte JSON array. It needs USDC. It does not
// need SOL, because the fee payer named in each challenge sponsors the network
// fee.
const secret = process.env.SOLANA_KEYPAIR
  ? process.env.SOLANA_KEYPAIR
  : readFileSync(process.env.SOLANA_KEYPAIR_PATH ?? '', 'utf8');
const signer = await createKeyPairSignerFromBytes(new Uint8Array(JSON.parse(secret)));

const rpc = async (method, params) => {
  const response = await fetch(RPC_URL, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
  });
  const { result, error } = await response.json();
  if (error) throw new Error(`${method}: ${error.message}`);
  return result;
};

// Read at confirmed commitment. A payment made seconds ago is confirmed long
// before it is finalized, and the default read would report the balance the
// wallet held before this run started.
const usdcBalance = async (owner, mint) => {
  const { value } = await rpc('getTokenAccountsByOwner', [
    owner,
    { mint },
    { encoding: 'jsonParsed', commitment: 'confirmed' },
  ]);
  return value.reduce(
    (total, account) => total + BigInt(account.account.data.parsed.info.tokenAmount.amount ?? '0'),
    0n,
  );
};

// An x402 payer. On a 402 it reads the challenge, signs an exact USDC transfer
// for the amount named there, and repeats the request carrying the payment.
const payer = registerExactSvmScheme(new x402Client(), { signer });
const http = new x402HTTPClient(payer);
const settlements = [];

const payingFetch = async (input, init = {}) => {
  const first = await fetch(input, init);
  if (first.status !== 402) return first;

  const challenge = await first
    .clone()
    .json()
    .catch(() => ({}));
  const required = await http.getPaymentRequiredResponse(
    (name) => first.headers.get(name),
    challenge,
  );
  const price = required.accepts[0];
  say('  price', `${price.amount} atomic (${usd(price.amount)}) on ${price.network}`);
  say('  signing', `${short(signer.address)} -> ${short(price.payTo)}`);

  // The retry carries a fresh deadline. The toolset started its own timer before
  // the signature, and signing takes long enough to matter.
  const paid = await fetch(input, {
    method: init.method,
    body: init.body,
    headers: {
      ...(init.headers ?? {}),
      ...http.encodePaymentSignatureHeader(await http.createPaymentPayload(required)),
    },
    signal: AbortSignal.timeout(30_000),
  });

  const receipt = paid.headers.get('payment-response');
  if (receipt) {
    const settled = JSON.parse(Buffer.from(receipt, 'base64').toString('utf8'));
    settlements.push({
      label: required.resource.url,
      amount: price.amount,
      transaction: settled.transaction,
    });
    say('  settled', settled.transaction ?? JSON.stringify(settled));
  }
  return paid;
};

const toolset = new MizukiToolset({ fetchImpl: payingFetch, timeoutMs: 30_000 });
const tools = Object.fromEntries(getMizukiTools(toolset).map((tool) => [tool.name, tool]));

heading('An agent hires Mizuki');
say('tools', Object.keys(tools).join(', '));
say('source', 'mizuki-agent-tools@0.1.1 /langchain');
say('wallet', signer.address);

// 1. Does this repository qualify? The answer is priced, and the agent pays for
//    it before committing to anything larger.
heading('1. Assess the repository');
say('call', `mizuki_assess_repository(${TARGET.owner}, ${TARGET.repo})`);
const assessment = await tools.mizuki_assess_repository.invoke(TARGET);
say('answer', indent(assessment));
if (!settlements.length) {
  say('settled', 'nothing');
  console.error(
    '\nThe assessment did not answer, so the wallet was not charged. Step 1 is the' +
      '\npremise of this run, so it stops here rather than skipping past a failure.',
  );
  process.exit(1);
}
say('explorer', `https://solscan.io/tx/${settlements[0].transaction}`);

// 2. What would it cost to fix one issue? A quote is free, names a fixed price,
//    and carries the payment requirements for the job itself.
heading('2. Quote one authorized issue');
say('call', `mizuki_quote(${ISSUE_URL})`);
const quoted = await tools.mizuki_quote.invoke({ githubIssueUrl: ISSUE_URL });
let quote;
try {
  quote = JSON.parse(quoted);
} catch {
  console.error(`\nMizuki did not return a quote:\n${quoted}`);
  process.exit(1);
}
if (!quote.payment) {
  console.error(`\nMizuki declined to quote this issue:\n${quoted}`);
  process.exit(1);
}

const terms = quote.payment.accepts[0];
say('issue', `#${quote.issueNumber} ${quote.issueTitle}`);
say(
  'authorized',
  `label ${quote.authorizationReceipt.label} by ${quote.authorizationReceipt.actorLogin}` +
    ` (${quote.authorizationReceipt.permission}), verified ${quote.authorizationReceipt.verifiedAt}`,
);
say('class', quote.class);
say('price', `${terms.amount} atomic (${usd(terms.amount)}), fixed`);
say('scope', `at most ${quote.maxFiles} files, base ${quote.baseSha.slice(0, 12)}`);
say('validation', quote.validationCommands.join(' && '));
say('pay to', terms.payTo);
say('asset', `${terms.asset} on ${terms.network}`);
say('fee payer', `${terms.extra.feePayer} sponsors the network fee`);
say('expires', quote.expiresAt);

// 3. Hire. This is the step that spends the quoted price, so it is gated: the
//    wallet must hold the price and the operator must ask for it explicitly.
heading('3. Hire Mizuki for that issue');
const held = await usdcBalance(signer.address, terms.asset);
const price = BigInt(terms.amount);
const funded = held >= price;
const confirmed = process.env.MIZUKI_HIRE_FOR_REAL === '1';

// The payer signs nothing above a dollar by default, which is under the price
// of a job. Raise its ceiling to exactly what this quote asks, so a demand for
// more than the quoted price is refused before anything is signed.
payer.setSpendControls({ maxAmountPerPayment: `$${(Number(price) / 1e6).toFixed(2)}` });

say('holds', `${usd(held)} (${held} atomic)`);
say('needs', `${usd(price)} (${price} atomic)`);
say('spend cap', `${usd(price)} per payment, the quoted price exactly`);
say('funded', funded ? 'yes' : `no, short ${usd(price - held)}`);
say('confirmed', confirmed ? 'yes, MIZUKI_HIRE_FOR_REAL=1' : 'no, MIZUKI_HIRE_FOR_REAL is not set');

const submission = {
  method: 'POST',
  url: `${process.env.MIZUKI_API_URL ?? 'https://mizuki.opencovenant.org/api/mizuki'}/v1/jobs`,
  headers: {
    'content-type': 'application/json',
    'idempotency-key': randomUUID(),
    'payment-signature': '<base64 x402 payload signed over the requirements below>',
  },
  body: { quote_id: quote.id },
  signedOver: quote.payment,
};

if (funded && confirmed) {
  const response = await fetch(submission.url, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'idempotency-key': submission.headers['idempotency-key'],
      // The quote body carries the same x402 declaration a 402 would have put
      // in a header, so the payer signs it directly.
      ...http.encodePaymentSignatureHeader(await http.createPaymentPayload(quote.payment)),
    },
    body: JSON.stringify(submission.body),
    signal: AbortSignal.timeout(60_000),
  });
  const job = await response.json();
  say('submitted', `HTTP ${response.status}`);
  if (response.status >= 400) {
    console.error(`\nMizuki rejected the submission:\n${JSON.stringify(job, null, 2)}`);
    process.exit(1);
  }
  const receipt = response.headers.get('payment-response');
  const settled = receipt ? JSON.parse(Buffer.from(receipt, 'base64').toString('utf8')) : {};
  settlements.push({
    label: `${quote.owner}/${quote.repo}#${quote.issueNumber}, job ${job.id}`,
    amount: terms.amount,
    transaction: settled.transaction ?? 'settlement not reported in the response header',
  });
  say('job', job.id);

  // Track it. Mizuki opens a pull request, runs the repository's own checks,
  // and refunds the price if they do not pass.
  const deadline = Date.now() + 15 * 60_000;
  let state = '';
  while (Date.now() < deadline) {
    const status = JSON.parse(await tools.mizuki_job_status.invoke({ jobId: job.id }));
    if (status.state !== state) {
      state = status.state;
      say('state', `${state}${status.pullRequestUrl ? ` ${status.pullRequestUrl}` : ''}`);
    }
    if (['delivered', 'validated', 'refunded', 'failed', 'cancelled'].includes(state)) break;
    await new Promise((resolve) => setTimeout(resolve, 15_000));
  }
} else {
  console.log('\nNOT EXECUTED. No transaction was created, signed, or sent for this step.');
  console.log(
    funded
      ? 'The wallet holds the price. This run did not set MIZUKI_HIRE_FOR_REAL=1, so nothing was spent.'
      : 'The wallet does not hold the price, so this step could not run.',
  );
  console.log('\nThe request it would send:\n');
  console.log(JSON.stringify(submission, null, 2));
  console.log(
    '\nThe signed payload is deliberately not printed. It authorizes an exact USDC' +
      '\ntransfer and anyone holding it could submit it.',
  );
  console.log('\nAfter that, mizuki_job_status(jobId) reports the pull request, the');
  console.log('repository checks, and the refund if those checks do not pass.');
}

heading('What this run moved');
for (const settled of settlements) {
  console.log(`  paid      ${usd(settled.amount)}  ${settled.label}`);
  console.log(`            ${settled.transaction}`);
}
if (!(funded && confirmed)) {
  console.log(`  not paid  ${usd(price)}  ${quote.owner}/${quote.repo}#${quote.issueNumber}`);
  console.log('            no transaction was built, signed, or sent');
}
console.log('\nQuoting costs nothing. Every charge this run made is listed above, and');
console.log('each signature can be checked on any Solana explorer.');
