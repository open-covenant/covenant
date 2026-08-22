# ClawPump setup

Use the documented local Agent MCP server. Keep the API key in the MCP client's secret storage and use `mcp.json.example` only as a template.

Official reference: <https://clawpump.tech/docs>

```sh
npx @clawpump/agents --claude
```

For any other stdio MCP client, configure `npx @clawpump/agents` and set `CLAWPUMP_API_KEY`. If the package is unavailable, stop and follow the local-checkout fallback in the official ClawPump documentation. Do not substitute an undocumented HTTP endpoint.

## Agent creation

Use the MCP tools interactively so the installed server supplies the current argument schema:

1. `get_account_status` to verify the API key and linked account.
2. `get_model_catalog` to select a currently available model within the daily budget.
3. `create_agent` with name `Mizuki`, a male persona, and no trading skill enabled.
4. `get_agent` to record the agent ID and wallet address in the operator password manager.
5. `create_custom_skill` using `custom-skill.md` as the content.
6. `list_custom_skills` and `get_custom_skill` to verify the skill is enabled and unmodified.
7. `get_budget`, `get_wallet_summaries`, and `get_balance` to confirm conservative limits before any paid ClawPump action.

Do not put the ClawPump API key, agent ID, wallet private material, or local MCP configuration in Git.

## Automations

ClawPump documents schedule-triggered prompt automations through `create_automation`. Create them through the installed MCP tool or the guided `setup-automations` prompt so the live server validates the trigger and action schema. Do not handcraft requests to guessed endpoints.

Create these prompt actions one at a time. Immediately pause each new rule with `update_automation`, manually fire it once with `trigger_automation`, inspect the run, then arm it:

| Name                  | Schedule              | Prompt action                                                                                                                                                                                                                                                                                                                       | Safety                                                                             |
| --------------------- | --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| Mizuki morning gate   | Daily 08:00 UTC       | Read the published Mizuki activity, treasury, capability, and unit-economics views. Report paid jobs, PRs, merges, external maintainers, refund obligations, gross-margin status and cost coverage, signer protection status, liability reconciliation, modeled allocation gaps, pending signer operations, and the next hard gate. | Read-only. If a source is unavailable, report unavailable; never infer a pass.     |
| Mizuki stream brief   | Tue and Fri 14:00 UTC | Prepare the 15-minute stream brief from public receipts: one real paid job, refund-to-bounty evidence, variable execution estimate, omitted costs, gross-margin status, and current traction gates.                                                                                                                                 | Read-only. Exclude secrets and non-public contributor data.                        |
| Mizuki incident watch | Hourly                | Check public health and activity for refund obligations over five minutes, escrow-pending over ten minutes, unverified signer protection, liability mismatch, duplicate financial events, or a negative recognized-revenue-less-partial estimate. Produce an alert with receipt IDs and the matching runbook section.               | Alert only. Never retry, transfer, rotate, deploy, or change policy.               |
| Mizuki outreach queue | Weekdays 09:00 UTC    | List the next consent-based public maintainer contacts already entered by an operator, the exact eligible issue, and follow-ups due.                                                                                                                                                                                                | Never scrape private contacts or send without an operator-reviewed recipient list. |

Automation prompts may read Mizuki's public API and dashboard only if the enabled agent tools provide ordinary web access. If they do not, connect a documented skill or keep the automation as an operator reminder; do not invent a ClawPump capability.

## AgentMail

After the first successful paid canary, use `agent_mail_get_address`. If no inbox exists, obtain explicit budget approval and use `agent_mail_create`, which ClawPump documents as an approximately $2 USDC x402 action. Use `agent_mail_send` only for consent-based outreach from the tracked queue. Never bulk-send or include payment links before a maintainer agrees to the specific issue.

## Token gate

Do not launch a token to manufacture activity. After both operator-funded mainnet canaries pass, use `get_launch_status` and the guided `launch-token` prompt early enough to satisfy the 19 September entry deadline. Require an operator to review name, symbol, mint authority, image, description, payout wallet, and public risk disclosure before invoking a launch tool. Record the returned mint and dashboard URLs through `get_dashboard_urls`. If commercial traction targets remain open, publish the exact gap instead of delaying the required entry step or fabricating activity.

The token narrative follows the maintenance evidence. It does not substitute for paid jobs, merged PRs, external maintainers, refunds, or verified positive gross margin with complete cost coverage.
