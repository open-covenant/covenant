import { readFile } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';

type Case = {
  name: string;
  repositoryUrl: string;
  baseSha: string;
  prompt: string;
  validationCommands?: string[];
  maxCostUsd?: number;
};

const path = process.argv[2];
if (!path) throw new Error('usage: pnpm benchmark <cases.json>');
const gateway = process.env.MIZUKI_CODING_GATEWAY_URL ?? 'http://127.0.0.1:8642';
const gatewayToken = process.env.MIZUKI_CODING_GATEWAY_TOKEN;
const cases = JSON.parse(await readFile(path, 'utf8')) as Case[];
if (!Array.isArray(cases) || cases.length === 0)
  throw new Error('benchmark file must contain cases');

const report = [];
for (const item of cases) {
  const started = Date.now();
  try {
    const maxCostUsd = item.maxCostUsd ?? 0.8;
    if (!Number.isFinite(maxCostUsd) || maxCostUsd <= 0 || maxCostUsd > 4) {
      throw new Error('maxCostUsd must be greater than zero and no more than 4');
    }
    const submitted = await fetch(`${gateway}/v1/runs`, {
      method: 'POST',
      headers: gatewayHeaders({ 'content-type': 'application/json' }),
      body: JSON.stringify({
        session_id: `benchmark:${randomUUID()}`,
        input: item.prompt,
        max_cost_usd: maxCostUsd,
        repository_url: item.repositoryUrl,
        base_sha: item.baseSha,
        validation_commands: item.validationCommands ?? [],
      }),
    });
    if (!submitted.ok) throw new Error(`${submitted.status} ${await submitted.text()}`);
    const { run_id: runId } = (await submitted.json()) as { run_id: string };
    const state = await wait(runId);
    const artifacts =
      state.status === 'completed'
        ? await fetch(`${gateway}/v1/runs/${runId}/artifacts`, {
            headers: gatewayHeaders(),
          }).then(async (response) => {
            if (!response.ok) throw new Error(`artifacts ${response.status}`);
            return response.json();
          })
        : undefined;
    report.push({
      name: item.name,
      success: state.status === 'completed',
      status: state.status,
      error: state.error,
      durationSeconds: (Date.now() - started) / 1_000,
      usage: state.usage,
      costUsd: state.costUsd,
      providerReceipts: state.providerReceipts,
      changedFiles: (artifacts as { changedFiles?: string[] } | undefined)?.changedFiles ?? [],
      validations: (artifacts as { validations?: unknown[] } | undefined)?.validations ?? [],
    });
  } catch (cause) {
    report.push({
      name: item.name,
      success: false,
      error: cause instanceof Error ? cause.message : String(cause),
      durationSeconds: (Date.now() - started) / 1_000,
    });
  }
}

console.log(JSON.stringify({ model: process.env.CODER_MODEL, cases: report }, null, 2));
if (report.some((item) => !item.success)) process.exitCode = 1;

async function wait(runId: string) {
  const deadline = Date.now() + 12 * 60_000;
  while (Date.now() < deadline) {
    const response = await fetch(`${gateway}/v1/runs/${runId}`, {
      headers: gatewayHeaders(),
    });
    if (!response.ok) throw new Error(`status ${response.status}`);
    const state = (await response.json()) as {
      status: string;
      error?: string;
      usage?: { inputTokens: number; outputTokens: number };
      costUsd?: number;
      providerReceipts?: Array<{
        model: string;
        route: string;
        providerId?: string;
        requestId?: string;
        providerReportedCostMicrounits?: string;
        accounting?: {
          accountedCostMicrounits: string;
          basis: string;
          inputTokens: number;
          outputTokens: number;
          inputPriceMicrounitsPerMillion: number;
          outputPriceMicrounitsPerMillion: number;
        };
        /** Legacy gateway receipt field. */
        costMicrounits?: string;
      }>;
    };
    if (!['queued', 'running'].includes(state.status)) return state;
    await new Promise((resolve) => setTimeout(resolve, 1_000));
  }
  throw new Error('benchmark case timed out');
}

function gatewayHeaders(headers: Record<string, string> = {}): Record<string, string> {
  return {
    ...headers,
    ...(gatewayToken ? { authorization: `Bearer ${gatewayToken}` } : {}),
  };
}
