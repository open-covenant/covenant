# @covenant/telegram-bot

Telegram surface for the Covenant project. Two independent jobs in one process:

1. **Stake announcer** — watches the `$CVNT` stake program on Solana and posts
   a "NEW STAKE" message to a group every time someone opens a lock position.
2. **Command bot** — a deny-by-default `/status` + `/tasks` responder for
   allowlisted operators (demo data today; repoints to live state later).

The service is standalone (no workspace dependency) and deploys like the other
`services/*` on Render.

## Run

```bash
pnpm --filter @covenant/telegram-bot build
pnpm --filter @covenant/telegram-bot start
```

With no `TELEGRAM_BOT_TOKEN` the process still boots and serves the HTTP
endpoints (`/healthz`, `/summary`, `/announce/preview`) — useful for verifying
config and message formatting before wiring a real bot.

## Stake announcer

When `TELEGRAM_BOT_TOKEN` and `TELEGRAM_ANNOUNCE_CHAT_ID` are both set, the bot
polls the stake program (`CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED`) and
decodes its `PositionCreated` events. For each new stake it posts:

```
NEW STAKE

🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥

9,988,818 $CVNT · 7d lock
Total staked: 56,384,774.16 $CVNT
5.6% of supply staked

View on Solscan · Stake $CVNT →
```

- **Amount + lock** come from the on-chain event (`amount`, `multiplier_bps` →
  7d / 30d / 90d / 180d).
- **Total staked** is the live balance of the locked-CVNT vault; **% of supply**
  divides that by the mint supply.
- The icon row scales with the stake size (one per `STAKE_ANNOUNCE_FIRE_UNIT`
  whole tokens). Set `STAKE_ANNOUNCE_EMOJI_ID` to a custom emoji this bot owns
  and the row becomes the spaced Covenant logo (capped at 12); unset, it's 🔥
  (capped at 50).

**How it watches.** It polls `getSignaturesForAddress` from a persisted cursor
rather than using a websocket subscription, so a redeploy or a dropped socket
never silently swallows a stake — the cursor backfills on the next poll and the
bot never double-posts. The cursor lives at
`$STAKE_WATCHER_STATE_DIR/stake-cursor.json` (a Render disk in production).

**Cold start.** On first boot with no cursor the bot anchors to the latest
program signature and does **not** replay history, so deploying it doesn't spam
the group with every past stake. The first stake _after_ deploy is announced.

Run `/stakepreview` (allowlisted) or `GET /announce/preview` to see a sample
message without waiting for a real stake.

## Auth model (command bot)

The command bot is **deny-by-default**. Only Telegram users whose numeric IDs
are listed in `TELEGRAM_ALLOWED_USER_IDS` (comma-separated) can invoke any
command. Updates from anyone else are dropped silently — no reply, no error,
only a `telegram-bot:denied_allowlist` log entry. Probing the bot cannot
enumerate allowed accounts.

Per-user rate limit: `TELEGRAM_RATE_LIMIT_PER_MIN` commands per minute
(default 5). In-memory bucket, single-process scope (the bot is not
horizontally scaled).

The allowlist gates **commands only**. The announcer broadcasts to its group
regardless of the allowlist — it never replies to users.

Command replies route through `safeReply()` (`parse_mode: undefined`) so a task
or agent id containing `_` / `*` / `[` can never become formatting. The
announcer and `/stakepreview` use HTML parse mode, but every interpolated field
is bot-controlled (numbers + our own URLs) and still HTML-escaped.

## Env

### Command bot

| Var | Required | Default | Notes |
|---|---|---|---|
| `TELEGRAM_BOT_TOKEN` | for bot | — | Without it the process is HTTP-only |
| `TELEGRAM_ALLOWED_USER_IDS` | for commands | — | Comma-separated numeric ids; empty = deny all |
| `TELEGRAM_RATE_LIMIT_PER_MIN` | no | `5` | Per-user command bucket |
| `TELEGRAM_WEBHOOK_URL` | no | — | If set, skips long-polling (operator wires the webhook) |
| `TELEGRAM_PORT` | no | `8788` | Fastify HTTP port |

### Stake announcer

| Var | Required | Default | Notes |
|---|---|---|---|
| `TELEGRAM_ANNOUNCE_CHAT_ID` | for announcer | — | Group/channel id (e.g. `-1001234567890`); announcer off when unset |
| `COVENANT_SOLANA_CLUSTER` | no | `mainnet` | `mainnet` \| `devnet` \| `localnet` |
| `COVENANT_SOLANA_RPC_URL` | recommended | public cluster RPC | Use a real RPC; the public endpoint rate-limits |
| `COVNT_MINT` | no | per-cluster default | Mainnet defaults to the live `$CVNT` mint |
| `COVNT_TOKEN_PROGRAM_ID` | no | Token-2022 (mainnet) / SPL (devnet) | Override the mint's token program |
| `COVENANT_TOKEN_SYMBOL` | no | `CVNT` | Ticker shown in the message |
| `STAKE_WATCHER_POLL_MS` | no | `15000` | Poll interval |
| `STAKE_WATCHER_STATE_DIR` | no | `/data/telegram-bot` → tmp | Where the signature cursor is persisted |
| `STAKE_ANNOUNCE_STAKE_URL` | no | `https://opencovenant.org/stake` | "Stake →" link target |
| `STAKE_ANNOUNCE_SOLSCAN_BASE` | no | `https://solscan.io` | Explorer base for the tx link |
| `STAKE_ANNOUNCE_FIRE_UNIT` | no | `250000` | Whole `$CVNT` per bar icon (🔥 cap 50, logo cap 12) |
| `STAKE_ANNOUNCE_EMOJI_ID` | no | — | Telegram custom_emoji_id (from a set this bot owns) used as the bar instead of 🔥 |

## Endpoints

| Endpoint | Notes |
|---|---|
| `GET /healthz` | `{ ok, cluster, bot_configured, bot_running, allowlist_size, rate_limit_per_min, announcer }` |
| `GET /summary` | Renders the same text the `/status` command returns |
| `GET /announce/preview` | `{ text }` — a sample NEW STAKE message (HTML) |

## Telegram commands

| Command | Auth | Returns |
|---|---|---|
| `/status` | allowlist + rate limit | Cluster + open-task count + top-agent line |
| `/tasks` | allowlist + rate limit | Newline list of tasks (`taskId · status · paymentAmount`) |
| `/stakepreview` | allowlist + rate limit | Sample NEW STAKE message (verifies formatting) |
