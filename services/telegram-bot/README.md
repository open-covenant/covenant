# @covenant/telegram-bot

Read-only Telegram status surface for the Covenant project. Today it serves
status + open-task summaries from the SDK's mock data; once the indexer
ships, it will read live state.

## Run

```bash
pnpm --filter @covenant/telegram-bot build
pnpm --filter @covenant/telegram-bot start
```

## Auth model

The bot is **deny-by-default**. Only Telegram users whose numeric IDs are
listed in `TELEGRAM_ALLOWED_USER_IDS` (comma-separated) can invoke any
command. Updates from any other user are dropped silently — no reply, no
error, only a `telegram-bot:denied_allowlist` log entry. Probing the bot
cannot enumerate allowed accounts.

Per-user rate limit: `TELEGRAM_RATE_LIMIT_PER_MIN` commands per minute
(default 5). Over-limit calls are dropped with a `telegram-bot:rate_limited`
log entry. In-memory bucket, single-process scope (the bot is not
horizontally scaled).

All bot replies route through `safeReply()` which explicitly passes
`parse_mode: undefined`. A future change that enables MarkdownV2 cannot
turn a `_` / `*` / `[` inside a task id or agent id into formatting or
injection unless that helper is also changed.

## Env

| Var | Required | Default | Notes |
|---|---|---|---|
| `TELEGRAM_BOT_TOKEN` | yes (for bot) | — | Bot is HTTP-only and serves `/summary`/`/healthz` when unset |
| `TELEGRAM_ALLOWED_USER_IDS` | yes (for bot) | — | Comma-separated numeric user ids; empty set denies all commands |
| `TELEGRAM_RATE_LIMIT_PER_MIN` | no | `5` | Per-user request bucket |
| `TELEGRAM_WEBHOOK_URL` | no | — | If set, skips long-polling (operator wires the webhook externally) |
| `TELEGRAM_PORT` | no | `8788` | Fastify HTTP port |

## Endpoints

| Endpoint | Notes |
|---|---|
| `GET /healthz` | `{ ok, cluster, bot_configured, bot_running, allowlist_size, rate_limit_per_min }` |
| `GET /summary` | Renders the same text the `/status` bot command returns |

## Telegram commands

| Command | Auth | Returns |
|---|---|---|
| `/status` | allowlist + rate limit | Cluster + open-task count + top-agent line |
| `/tasks` | allowlist + rate limit | Newline list of mock tasks (`taskId · status · paymentAmount`) |
