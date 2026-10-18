#!/usr/bin/env node
import { readFileSync, writeFileSync, mkdirSync, lstatSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { AgentKit } from '@coinbase/agentkit';
import { covenantInferenceActionProvider, CovenantSolanaKeypairWalletProvider } from './agentkit.mjs';
import { SOLANA_GENESIS } from './index.mjs';
import { assert, strictJson } from './proof.mjs';

const args = process.argv.slice(2);
if (args.length !== 3) {
  console.error('Usage: node demo.mjs CONFIG.json REQUEST.json OUTPUT_DIRECTORY');
  console.error('Requires an explicitly funded AgentKit wallet and numeric per-payment/cumulative USDC limits.');
  process.exitCode = 2;
} else {
  const config = strictJson(readFileSync(resolve(args[0]), 'utf8'));
  const request = strictJson(readFileSync(resolve(args[1]), 'utf8'));
  const keyFile = resolve(config.keypairPath); const stat = lstatSync(keyFile);
  assert(stat.isFile() && !stat.isSymbolicLink() && !(stat.mode & 0o077), 'keypair_file_must_be_0600');
  const secret = strictJson(readFileSync(keyFile, 'utf8'));
  assert(Array.isArray(secret) && secret.length === 64 && secret.every(n => Number.isInteger(n) && n >= 0 && n <= 255), 'expected_solana_keypair_json');
  const wallet = new CovenantSolanaKeypairWalletProvider({ keypair: Uint8Array.from(secret), rpcUrl: config.rpcUrl, genesisHash: SOLANA_GENESIS });
  secret.fill(0);
  const provider = covenantInferenceActionProvider(config);
  const kit = await AgentKit.from({ walletProvider: wallet, actionProviders: [provider] });
  const action = kit.getActions().find(action => action.name === 'covenant_inference');
  assert(action, 'agentkit_action_not_registered');
  const output = resolve(args[2]); mkdirSync(output, { recursive: true, mode: 0o700 });
  console.log(JSON.stringify({ event: 'agentkit_ready', wallet: wallet.getAddress(), requestId: request.requestId, action: action.name }));
  try {
    let result;
    for (let attempt = 0; attempt < 40; attempt++) {
      result = strictJson(await action.invoke(request));
      if (result.status === 'completed') break;
      console.log(JSON.stringify({ event: 'inference_pending', requestId: request.requestId }));
      await delay(3000);
    }
    assert(result?.status === 'completed', 'still_pending_resume_with_same_request_and_journal');
    writeFileSync(join(output, 'evidence.json'), JSON.stringify(result, null, 2) + '\n', { mode: 0o600 });
    writeFileSync(join(output, 'response.txt'), result.text, { mode: 0o600 });
    let html = result.text.trim().replace(/^```(?:html)?\s*\n/i, '').replace(/\n```$/, '');
    if (/^(?:<!doctype html>|<html[ >])/i.test(html)) {
      // Preserve the signed response separately. Preview scripts can work but
      // cannot fetch data or open external frames when this file is viewed.
      const csp = '<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; script-src \'unsafe-inline\'; style-src \'unsafe-inline\'; img-src data:; font-src data:; connect-src \'none\'; frame-src \'none\'; form-action \'none\'; base-uri \'none\'">';
      html = /<head(?:\s[^>]*)?>/i.test(html) ? html.replace(/<head(?:\s[^>]*)?>/i, match => match + csp) : html.replace(/<html(?:\s[^>]*)?>/i, match => match + '<head>' + csp + '</head>');
      writeFileSync(join(output, 'page.html'), html, { mode: 0o600 });
    }
    console.log(JSON.stringify({ event: 'purchase_verified', amountMicroUsdc: result.amountMicroUsdc,
      transaction: result.settlement.transaction, jobId: result.result.covenant.job_id,
      receiptSignature: result.result.covenant.receipt.signature_b58, outputDirectory: output }));
  } finally { provider.close(wallet); }
}
