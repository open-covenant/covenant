/**
 * Mizuki tools for LangChain and LangGraph agents.
 *
 *     import { createReactAgent } from '@langchain/langgraph/prebuilt';
 *     import { getMizukiTools } from 'mizuki-agent-tools/langchain';
 *
 *     const agent = createReactAgent({ llm, tools: getMizukiTools() });
 *
 * Quoting an issue and reading bounties work with no configuration. A maintainer
 * token, passed here or as MIZUKI_API_TOKEN, also unlocks the repository reads.
 */

import { DynamicStructuredTool } from '@langchain/core/tools';
import { z } from 'zod';
import { MizukiToolset, type MizukiToolsetOptions } from './index.js';

export { MizukiToolset };

/**
 * Build the Mizuki tool list for a LangChain agent.
 *
 * Pass a toolset to control the API URL or token, or let it read the environment.
 */
export function getMizukiTools(
  toolset: MizukiToolset | MizukiToolsetOptions = {},
): DynamicStructuredTool[] {
  const t = toolset instanceof MizukiToolset ? toolset : new MizukiToolset(toolset);
  return [
    new DynamicStructuredTool({
      name: 'mizuki_quote',
      description:
        'Quote fixed-price maintenance for one open issue in a public GitHub repository. Returns the price and the x402 payment requirements. Does not pay for or start any work.',
      schema: z.object({
        githubIssueUrl: z.string().url().describe('URL of an open GitHub issue'),
      }),
      func: async ({ githubIssueUrl }) => t.quote(githubIssueUrl),
    }),
    new DynamicStructuredTool({
      name: 'mizuki_assess_repository',
      description:
        'Report whether a public GitHub repository qualifies for Mizuki maintenance and which command Mizuki would run to validate a change. Not a quote, and reserves nothing. Worth calling before quoting.',
      schema: z.object({
        owner: z.string().describe('GitHub owner or organisation'),
        repo: z.string().describe('GitHub repository name'),
      }),
      func: async ({ owner, repo }) => t.assess(owner, repo),
    }),
    new DynamicStructuredTool({
      name: 'mizuki_job_status',
      description:
        'Read delivery, pull request, validation, and refund state for a Mizuki maintenance job.',
      schema: z.object({ jobId: z.string().describe('Job identifier') }),
      func: async ({ jobId }) => t.jobStatus(jobId),
    }),
    new DynamicStructuredTool({
      name: 'mizuki_bounties',
      description:
        'List open public Mizuki maintenance bounties. A bounty is opened after an eligible job is fully refunded, so the work is still wanted.',
      schema: z.object({}),
      func: async () => t.bounties(),
    }),
  ];
}
