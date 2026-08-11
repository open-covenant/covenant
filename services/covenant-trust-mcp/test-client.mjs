// End-to-end MCP test: spawn the server over stdio with a real client, list the
// tools, and call each against live Solana mainnet. Verify uses a locally-signed
// attestation to prove PASS and a tampered copy to prove FAIL.
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StdioClientTransport} from '@modelcontextprotocol/sdk/client/stdio.js';
import {generateKeyPairSync, createHash, sign as edSign} from 'node:crypto';
import bs58 from 'bs58';

const RPC = 'https://mainnet.helius-rpc.com/?api-key=fe1af088-e142-478c-a228-20c1f56888a3';

const canonical = (v) => {
  if (v === null || typeof v !== 'object') return JSON.stringify(v);
  if (Array.isArray(v)) return `[${v.map(canonical).join(',')}]`;
  return `{${Object.entries(v).filter(([, x]) => x !== undefined).sort(([a], [b]) => (a < b ? -1 : 1)).map(([k, x]) => `${JSON.stringify(k)}:${canonical(x)}`).join(',')}}`;
};
function makeAttestation(subject, claim) {
  const {publicKey, privateKey} = generateKeyPairSync('ed25519');
  const payload = {subject, claim, ts: 1751000000};
  const digest = createHash('sha256').update(canonical(payload), 'utf8').digest('hex');
  const sig = edSign(null, Buffer.from(`covenant.attest.v1\n${digest}`, 'utf8'), privateKey);
  const jwk = publicKey.export({format: 'jwk'});
  return {
    alg: 'ed25519', domain: 'covenant.attest.v1',
    canonicalization: 'JSON, recursively key-sorted, no insignificant whitespace, UTF-8',
    payload, digest_sha256_hex: digest,
    pubkey_b58: bs58.encode(Buffer.from(jwk.x, 'base64url')),
    signature_b58: bs58.encode(sig),
  };
}

const transport = new StdioClientTransport({
  command: 'node', args: ['dist/server.js', '--stdio'],
  env: {...process.env, COVENANT_SOLANA_MAINNET_RPC_URL: RPC},
});
const client = new Client({name: 'trust-mcp-test', version: '1.0.0'});
await client.connect(transport);

const tools = await client.listTools();
console.log('TOOLS:', tools.tools.map((t) => `${t.name} (readOnly=${t.annotations?.readOnlyHint})`).join(', '));

const call = async (name, args) => (await client.callTool({name, arguments: args})).content[0].text;

console.log('\n--- covenant_reputation (Covenant treasury) ---');
console.log(await call('covenant_reputation', {wallet: '8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM'}));

console.log('\n--- covenant_agent_passport (featured attestation asset) ---');
console.log(await call('covenant_agent_passport', {asset: 'AHZE6uSSnQ2Y1rLLCi7Pv86m6JgTzpcD8s2DEhzfrm3U'}));

console.log('\n--- covenant_verify (valid) ---');
const att = makeAttestation('did:example:agent', {role: 'trader', ok: true});
console.log(await call('covenant_verify', {attestation: att}));

console.log('\n--- covenant_verify (tampered -> must FAIL) ---');
const forged = {...att, payload: {...att.payload, subject: 'did:example:attacker'}};
console.log(await call('covenant_verify', {attestation: forged}));

console.log('\n--- covenant_reputation (bad input -> must error) ---');
console.log(await call('covenant_reputation', {wallet: 'not-an-address'}));

const envelope = 'EvjTAQqJAQgPGAIqQCa9fK2mRxH'.repeat(20);
console.log('\n--- covenant_scan_reasoning (three providers -> must FIND 3) ---');
console.log(await call('covenant_scan_reasoning', {
  content: [
    `{"type":"thinking","thinking":"","signature":"${envelope}"}`,
    `{"type":"reasoning","encrypted_content":"${envelope}"}`,
    `{"thoughtSignature":"${envelope}"}`,
  ].join('\n'),
}));

console.log('\n--- covenant_scan_reasoning (on-chain sigs + prose -> must be CLEAN) ---');
console.log(await call('covenant_scan_reasoning', {
  content: [
    `{"thinking":"about the trade","signature":"0x${'ab'.repeat(65)}"}`,
    '{"signature":"3Qw8Uh2mKpLxYvZnR7tBdF1aWcE4sJgHiN6oPqTrXyMz9bCkDvSfAuGeHjKlMnBpQrStUvWxYz12"}',
    'the paper decodes an encrypted_content field to recover hidden reasoning',
    '{"type": "thinking", "thinking": "scratch", "signature": "..."}',
  ].join('\n'),
}));

await client.close();
